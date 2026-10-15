//! Version-specific OpenAPI schemas from the connected server and CRDs.
use crate::{ApiResource, ClusterSession, DynamicObject, GroupVersionKind};
use kube::Api;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Debug)]
pub struct Schema {
    pub root: Value,
    pub definitions: Map<String, Value>,
    pub source: String,
    pub version: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub pointer: String,
    pub message: String,
}
#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub schema: Value,
    pub required: bool,
}

pub async fn load(session: &ClusterSession, resource: &ApiResource) -> Result<Arc<Schema>, String> {
    let client = session.client();
    let group_path = if resource.group.is_empty() {
        format!("api/{}", resource.version)
    } else {
        format!("apis/{}/{}", resource.group, resource.version)
    };
    let mut failures = Vec::new();
    match get(client, "/openapi/v3").await {
        Ok(index) => {
            let uri = index
                .get("paths")
                .and_then(|paths| paths.get(&group_path))
                .and_then(|entry| entry["serverRelativeURL"].as_str());
            if let Some(uri) =
                uri.filter(|uri| uri.starts_with("/openapi/v3/") && !uri.starts_with("//"))
            {
                match get(client, uri).await {
                    Ok(document) => {
                        if let Some(schema) = from_openapi(
                            &document,
                            resource,
                            "Server OpenAPI v3",
                            session.version(),
                        ) {
                            return Ok(Arc::new(schema));
                        }
                    }
                    Err(error) => failures.push(error),
                }
            } else {
                failures.push(format!("OpenAPI v3 has no schema for {group_path}"));
            }
        }
        Err(error) => failures.push(error),
    }
    // Older API servers publish a single v2 document. Its definitions remain
    // useful for completion/type checks; CRD-only features use the CRD below.
    match get(client, "/openapi/v2").await {
        Ok(document) => {
            if let Some(schema) =
                from_openapi(&document, resource, "Server OpenAPI v2", session.version())
            {
                return Ok(Arc::new(schema));
            }
        }
        Err(error) => failures.push(error),
    }
    if !resource.group.is_empty() {
        let crd_resource = ApiResource::from_gvk_with_plural(
            &GroupVersionKind::gvk("apiextensions.k8s.io", "v1", "CustomResourceDefinition"),
            "customresourcedefinitions",
        );
        let api: Api<DynamicObject> = Api::all_with(client.clone(), &crd_resource);
        match api
            .get(&format!("{}.{}", resource.plural, resource.group))
            .await
        {
            Ok(crd) => {
                if let Some(root) = crd
                    .data
                    .pointer("/spec/versions")
                    .and_then(Value::as_array)
                    .and_then(|versions| {
                        versions
                            .iter()
                            .find(|version| version["name"] == resource.version)
                    })
                    .and_then(|version| version.pointer("/schema/openAPIV3Schema"))
                {
                    let mut root = root.clone();
                    if root.get("properties").is_none() {
                        root["properties"] = json!({});
                    }
                    let props = root["properties"]
                        .as_object_mut()
                        .ok_or("CRD schema has no object properties.")?;
                    // These universally present resource fields are validated by
                    // the API server, and omitted from many CRD spec schemas.
                    props
                        .entry("apiVersion")
                        .or_insert(json!({"type":"string"}));
                    props.entry("kind").or_insert(json!({"type":"string"}));
                    props.entry("metadata").or_insert(
                        json!({"type":"object","x-kubernetes-preserve-unknown-fields":true}),
                    );
                    return Ok(Arc::new(Schema {
                        root,
                        definitions: Map::new(),
                        source: format!(
                            "CRD {}.{} / {}",
                            resource.plural, resource.group, resource.version
                        ),
                        version: session.version().into(),
                    }));
                }
            }
            Err(error) => failures.push(crate::Error::Api(error).user_message()),
        }
    }
    Err(format!(
        "Schema unavailable for {} {} on {}. {}",
        resource.kind,
        resource.api_version,
        session.version(),
        failures.join(" · ")
    ))
}
async fn get(client: &kube::Client, uri: &str) -> Result<Value, String> {
    let request = http::Request::builder()
        .uri(uri)
        .header(http::header::ACCEPT, "application/json")
        .body(Vec::new())
        .map_err(|error| error.to_string())?;
    client
        .request(request)
        .await
        .map_err(|error| crate::Error::Api(error).user_message())
}
pub fn from_openapi(
    document: &Value,
    resource: &ApiResource,
    source: &str,
    version: &str,
) -> Option<Schema> {
    let definitions = document
        .pointer("/components/schemas")
        .or_else(|| document.get("definitions"))?
        .as_object()?
        .clone();
    let root = definitions
        .values()
        .find(|schema| {
            schema["x-kubernetes-group-version-kind"]
                .as_array()
                .is_some_and(|gvks| {
                    gvks.iter().any(|gvk| {
                        gvk["group"] == resource.group
                            && gvk["version"] == resource.version
                            && gvk["kind"] == resource.kind
                    })
                })
        })?
        .clone();
    Some(Schema {
        root,
        definitions,
        source: source.into(),
        version: version.into(),
    })
}
impl Schema {
    pub fn resolve<'a>(&'a self, schema: &'a Value) -> &'a Value {
        let mut value = schema;
        let mut seen = BTreeSet::new();
        while let Some(reference) = value["$ref"].as_str() {
            let Some(name) = reference
                .strip_prefix("#/components/schemas/")
                .or_else(|| reference.strip_prefix("#/definitions/"))
            else {
                break;
            };
            if !seen.insert(name) {
                break;
            }
            let Some(resolved) = self.definitions.get(name) else {
                break;
            };
            value = resolved;
        }
        value
    }
    pub fn at(&self, path: &[String]) -> Option<&Value> {
        let mut schema = &self.root;
        for segment in path {
            schema = self.child(schema, segment, 0)?;
        }
        Some(self.resolve(schema))
    }
    fn child<'a>(&'a self, schema: &'a Value, segment: &str, depth: usize) -> Option<&'a Value> {
        if depth > 24 {
            return None;
        }
        let schema = self.resolve(schema);
        if segment.parse::<usize>().is_ok()
            && let Some(items) = schema.get("items")
        {
            return Some(items);
        }
        if let Some(property) = schema["properties"].get(segment) {
            return Some(property);
        }
        if let Some(parts) = schema["allOf"].as_array()
            && let Some(property) = parts
                .iter()
                .find_map(|part| self.child(part, segment, depth + 1))
        {
            return Some(property);
        }
        schema
            .get("additionalProperties")
            .filter(|value| value.is_object())
    }
    pub fn fields(&self, path: &[String]) -> Vec<Field> {
        let Some(schema) = self.at(path) else {
            return vec![];
        };
        let required = schema["required"].as_array();
        let mut fields = BTreeMap::new();
        self.collect_fields(schema, required, &mut fields, 0);
        fields.into_values().collect()
    }
    fn collect_fields(
        &self,
        schema: &Value,
        required: Option<&Vec<Value>>,
        fields: &mut BTreeMap<String, Field>,
        depth: usize,
    ) {
        if depth > 24 {
            return;
        }
        let schema = self.resolve(schema);
        if let Some(props) = schema["properties"].as_object() {
            for (name, value) in props {
                fields.insert(
                    name.clone(),
                    Field {
                        name: name.clone(),
                        schema: self.resolve(value).clone(),
                        required: required
                            .is_some_and(|values| values.iter().any(|value| value == name))
                            || schema["required"]
                                .as_array()
                                .is_some_and(|values| values.iter().any(|value| value == name)),
                    },
                );
            }
        }
        if let Some(parts) = schema["allOf"].as_array() {
            for part in parts {
                self.collect_fields(part, required, fields, depth + 1);
            }
        }
    }
    pub fn validate(&self, value: &Value) -> Vec<Issue> {
        let mut issues = Vec::new();
        self.check(&self.root, value, "", &mut issues, 0);
        issues.truncate(200);
        issues
    }
    fn accepts_null(&self, schema: &Value) -> bool {
        let resolved = self.resolve(schema);
        if [schema, resolved]
            .iter()
            .any(|schema| schema["nullable"] == true || schema["x-kubernetes-nullable"] == true)
        {
            return true;
        }
        // metav1.Time and MicroTime marshal their zero value as null, although
        // Kubernetes publishes them as non-nullable date-time strings. Keep
        // this compatibility rule scoped to those references, not CRD strings.
        schema["$ref"].as_str().is_some_and(|reference| {
            reference
                .strip_prefix("#/components/schemas/")
                .or_else(|| reference.strip_prefix("#/definitions/"))
                .is_some_and(|name| {
                    matches!(
                        name,
                        "io.k8s.apimachinery.pkg.apis.meta.v1.Time"
                            | "io.k8s.apimachinery.pkg.apis.meta.v1.MicroTime"
                    )
                })
                && resolved["type"] == "string"
                && resolved["format"] == "date-time"
        })
    }
    fn check(
        &self,
        schema: &Value,
        value: &Value,
        pointer: &str,
        issues: &mut Vec<Issue>,
        depth: usize,
    ) {
        if depth > 64 || issues.len() >= 200 {
            return;
        }
        if value.is_null() && self.accepts_null(schema) {
            return;
        }
        let schema = self.resolve(schema);
        let issue = |message: String| Issue {
            pointer: pointer.into(),
            message,
        };
        let int_or_string = schema["x-kubernetes-int-or-string"] == true;
        let expected = schema["type"].as_str();
        let valid = if int_or_string {
            value.is_string() || value.is_i64() || value.is_u64()
        } else {
            match expected {
                Some("object") => value.is_object(),
                Some("array") => value.is_array(),
                Some("string") => value.is_string(),
                Some("integer") => value.is_i64() || value.is_u64(),
                Some("number") => value.is_number(),
                Some("boolean") => value.is_boolean(),
                _ => true,
            }
        };
        if !valid {
            issues.push(issue(format!(
                "Expected {}.",
                if int_or_string {
                    "integer or string"
                } else {
                    expected.unwrap_or("valid value")
                }
            )));
            return;
        }
        if let Some(choices) = schema["enum"].as_array()
            && !choices.contains(value)
        {
            issues.push(issue(format!(
                "Choose one of: {}.",
                choices
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if let Some(required) = schema["required"].as_array()
            && let Some(object) = value.as_object()
        {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    issues.push(issue(format!("Required field '{name}' is missing.")));
                }
            }
        }
        if let Some(object) = value.as_object() {
            for (name, child) in object {
                let next = format!("{pointer}/{}", name.replace('~', "~0").replace('/', "~1"));
                if let Some(property) = schema["properties"].get(name) {
                    self.check(property, child, &next, issues, depth + 1);
                } else if let Some(additional) = schema
                    .get("additionalProperties")
                    .filter(|value| value.is_object())
                {
                    self.check(additional, child, &next, issues, depth + 1);
                } else if schema["additionalProperties"] == false {
                    issues.push(Issue {
                        pointer: next,
                        message: "Unknown field.".into(),
                    });
                }
            }
        }
        if let Some(array) = value.as_array()
            && let Some(items) = schema.get("items")
        {
            for (index, child) in array.iter().enumerate() {
                self.check(
                    items,
                    child,
                    &format!("{pointer}/{index}"),
                    issues,
                    depth + 1,
                );
            }
        }
        if let Some(parts) = schema["allOf"].as_array() {
            for part in parts {
                self.check(part, value, pointer, issues, depth + 1);
            }
        }
        for combinator in ["anyOf", "oneOf"] {
            if let Some(parts) = schema[combinator].as_array() {
                let valid_count = parts
                    .iter()
                    .filter(|part| {
                        let mut failures = Vec::new();
                        self.check(part, value, pointer, &mut failures, depth + 1);
                        failures.is_empty()
                    })
                    .count();
                if valid_count == 0 || (combinator == "oneOf" && valid_count != 1) {
                    issues.push(issue(format!(
                        "Value does not match {combinator} alternatives."
                    )));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn resource() -> ApiResource {
        ApiResource::from_gvk_with_plural(
            &GroupVersionKind::gvk("apps", "v1", "Deployment"),
            "deployments",
        )
    }
    fn document() -> Value {
        json!({"components":{"schemas":{"Deployment":{"x-kubernetes-group-version-kind":[{"group":"apps","version":"v1","kind":"Deployment"}],"type":"object","required":["spec"],"properties":{"spec":{"$ref":"#/components/schemas/Spec"}}},"Spec":{"type":"object","required":["replicas"],"properties":{"replicas":{"type":"integer","description":"Desired replicas."},"mode":{"type":"string","enum":["Rolling","Recreate"]},"port":{"x-kubernetes-int-or-string":true},"nullable":{"type":"string","nullable":true},"tags":{"type":"object","additionalProperties":{"type":"string"}},"items":{"type":"array","items":{"type":"boolean"}}}}}}})
    }
    #[test]
    fn resolves_versioned_refs_types_required_enums_maps_arrays_and_kubernetes_extensions() {
        let schema =
            from_openapi(&document(), &resource(), "Server OpenAPI v3", "v1.35.7").unwrap();
        assert_eq!(schema.version, "v1.35.7");
        assert!(
            schema
                .fields(&["spec".into()])
                .iter()
                .find(|field| field.name == "replicas")
                .unwrap()
                .required
        );
        assert!(schema.validate(&json!({"spec":{"replicas":2,"mode":"Rolling","port":80,"nullable":null,"tags":{"app":"api"},"items":[true]}})).is_empty());
        let issues = schema.validate(
            &json!({"spec":{"mode":"bad","tags":{"app":4},"items":["wrong"],"port":false}}),
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.message.contains("replicas"))
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.pointer == "/spec/mode" && issue.message.contains("Choose"))
        );
        assert!(issues.iter().any(|issue| issue.pointer == "/spec/tags/app"));
        assert!(issues.iter().any(|issue| issue.pointer == "/spec/items/0"));
        assert!(issues.iter().any(|issue| issue.pointer == "/spec/port"));
    }
    #[test]
    fn kubernetes_zero_timestamps_accept_null_without_relaxing_custom_strings() {
        for prefix in ["#/components/schemas/", "#/definitions/"] {
            let time =
                json!({"$ref": format!("{prefix}io.k8s.apimachinery.pkg.apis.meta.v1.Time")});
            let micro_time =
                json!({"$ref": format!("{prefix}io.k8s.apimachinery.pkg.apis.meta.v1.MicroTime")});
            let schema = Schema {
                root: json!({"type":"object","properties":{
                    "metadata":{"type":"object","properties":{"creationTimestamp":time.clone()}},
                    "status":{"type":"object","properties":{"conditions":{"type":"array","items":{"type":"object","properties":{
                        "lastProbeTime":{"allOf":[time.clone()]},"lastTransitionTime":time
                    }}}}},
                    "eventTime":micro_time,
                    "customDate":{"type":"string","format":"date-time"},
                    "name":{"type":"string"}
                }}),
                definitions: serde_json::from_value(json!({
                    "io.k8s.apimachinery.pkg.apis.meta.v1.Time":{"type":"string","format":"date-time"},
                    "io.k8s.apimachinery.pkg.apis.meta.v1.MicroTime":{"type":"string","format":"date-time"}
                })).unwrap(),
                source: "Server OpenAPI".into(),
                version: "v1.33.3".into(),
            };
            let value = json!({"metadata":{"creationTimestamp":null},"status":{"conditions":[
                {"lastProbeTime":null,"lastTransitionTime":"2026-10-15T00:00:00Z"},
                {"lastProbeTime":null,"lastTransitionTime":null}
            ]},"eventTime":null,"customDate":"2026-10-15T00:00:00Z","name":"demo"});
            assert!(schema.validate(&value).is_empty());
            let mut invalid = value;
            invalid["customDate"] = Value::Null;
            invalid["name"] = Value::Null;
            invalid["status"]["conditions"][0]["lastProbeTime"] = json!(42);
            let issues = schema.validate(&invalid);
            assert_eq!(issues.len(), 3);
            for pointer in ["/customDate", "/name", "/status/conditions/0/lastProbeTime"] {
                assert!(issues.iter().any(|issue| issue.pointer == pointer));
            }
        }
    }
    #[test]
    fn nullable_annotations_on_a_reference_are_preserved() {
        let schema = Schema {
            root: json!({"type":"object","properties":{
                "first":{"$ref":"#/components/schemas/Text","nullable":true},
                "second":{"$ref":"#/definitions/Text","x-kubernetes-nullable":true},
                "strict":{"$ref":"#/components/schemas/Text"}
            }}),
            definitions: serde_json::from_value(json!({"Text":{"type":"string"}})).unwrap(),
            source: "Fixture OpenAPI".into(),
            version: "v1".into(),
        };
        assert!(
            schema
                .validate(&json!({"first":null,"second":null,"strict":"text"}))
                .is_empty()
        );
        assert_eq!(schema.validate(&json!({"strict":null})).len(), 1);
    }
    async fn server(routes: Vec<(String, Value)>) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            for (path, body) in routes {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    header.push(socket.read_u8().await.unwrap());
                }
                let header = String::from_utf8(header).unwrap();
                assert_eq!(header.split_whitespace().nth(1).unwrap(), path);
                let text = body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    text.len(),
                    text
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{address}"), task)
    }
    #[tokio::test]
    async fn fetches_the_connected_servers_group_version_hash_without_a_local_version_guess() {
        let (url, task) = server(vec![("/openapi/v3".into(), json!({"paths":{"apis/apps/v1":{"serverRelativeURL":"/openapi/v3/apis/apps/v1?hash=exact-version"}}})), ("/openapi/v3/apis/apps/v1?hash=exact-version".into(), document())]).await;
        let session = ClusterSession::for_testing(crate::ClusterId::new("schema-api"), url, vec![]);
        let schema = session.schema(&resource(), false).await.unwrap();
        let cached = session.schema(&resource(), false).await.unwrap();
        assert!(Arc::ptr_eq(&schema, &cached));
        assert!(schema.source.contains("v3"));
        assert_eq!(schema.version, session.version());
        assert_eq!(
            schema.at(&["spec".into(), "replicas".into()]).unwrap()["type"],
            "integer"
        );
        task.await.unwrap();
    }
    #[tokio::test]
    async fn crd_fallback_uses_only_the_requested_served_version_and_keeps_metadata() {
        let resource = ApiResource::from_gvk_with_plural(
            &GroupVersionKind::gvk("example.io", "v1", "Widget"),
            "widgets",
        );
        let crd = json!({"apiVersion":"apiextensions.k8s.io/v1","kind":"CustomResourceDefinition","metadata":{"name":"widgets.example.io"},"spec":{"versions":[{"name":"v2","schema":{"openAPIV3Schema":{"type":"object","properties":{"newOnly":{"type":"string"}}}}},{"name":"v1","schema":{"openAPIV3Schema":{"type":"object","properties":{"spec":{"type":"object","required":["size"],"properties":{"size":{"type":"integer"}}}}}}}]}});
        let (url, task) = server(vec![
            ("/openapi/v3".into(), json!({"paths":{}})),
            ("/openapi/v2".into(), json!({"definitions":{}})),
            (
                "/apis/apiextensions.k8s.io/v1/customresourcedefinitions/widgets.example.io".into(),
                crd,
            ),
        ])
        .await;
        let session = ClusterSession::for_testing(crate::ClusterId::new("schema-crd"), url, vec![]);
        let schema = load(&session, &resource).await.unwrap();
        assert!(schema.source.contains("CRD"));
        assert!(schema.at(&["metadata".into()]).is_some());
        assert!(schema.at(&["newOnly".into()]).is_none());
        assert!(
            schema
                .validate(&json!({"spec":{}}))
                .iter()
                .any(|issue| issue.message.contains("size"))
        );
        task.await.unwrap();
    }
}
