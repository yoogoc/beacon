//! The operations that change a cluster.
//!
//! Create and delete objects, restart workloads, scale them, and apply edited
//! manifests.
//!
//! Applying goes through Server-Side Apply with a field manager of `beacon`,
//! which is what makes a conflict *detectable* rather than a silent overwrite.
//! When another manager owns a field you are changing, the API server refuses
//! and says which fields and which manager — and that refusal is more useful
//! than the write, so it is surfaced rather than retried.

use kube::{
    Api,
    api::{
        ApiResource, DeleteParams, DynamicObject, Patch, PatchParams, PostParams, Preconditions,
    },
};
use serde_json::{Value, json};

use crate::{Error, Kind, ObjectRef, Result, access::verbs};

/// An object selected for deletion, pinned to the UID that was shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteTarget {
    pub reference: ObjectRef,
    pub uid: String,
}

/// The field manager Beacon writes under. It appears in `managedFields`, and in
/// somebody else's conflict message later, so it is worth being a name a person
/// would recognise.
pub const FIELD_MANAGER: &str = "beacon";

/// Something the user can do to an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    Delete,
    /// A rollout restart: what `kubectl rollout restart` does, which is to
    /// stamp the pod template so the controller rolls it.
    Restart,
    Scale {
        replicas: i32,
    },
    Apply,
}

impl Operation {
    /// The RBAC verb this needs, for the permission preflight.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Delete => verbs::DELETE,
            // Everything else is a PATCH, including Server-Side Apply.
            Self::Restart | Self::Scale { .. } | Self::Apply => verbs::PATCH,
        }
    }

    /// The resource the verb applies to, given a kind's plural name.
    ///
    /// Scaling is a subresource, and RBAC names it separately -- permission to
    /// patch a Deployment is not permission to scale one.
    pub fn resource(&self, plural: &str) -> String {
        match self {
            Self::Scale { .. } => format!("{plural}/scale"),
            _ => plural.to_string(),
        }
    }

    /// Whether this operation is meaningful for a kind at all.
    ///
    /// Scaling and restarting are workload ideas; offering them on a ConfigMap
    /// would be a menu item that cannot work.
    pub fn applies_to(&self, group: &str, kind: &str) -> bool {
        match self {
            Self::Delete | Self::Apply => true,
            Self::Restart => matches!(
                (group, kind),
                ("apps", "Deployment" | "StatefulSet" | "DaemonSet")
            ),
            Self::Scale { .. } => matches!(
                (group, kind),
                ("apps", "Deployment" | "StatefulSet" | "ReplicaSet")
                    | ("", "ReplicationController")
            ),
        }
    }

    /// What a confirmation prompt says.
    pub fn describe(&self) -> String {
        match self {
            Self::Delete => "Delete".to_string(),
            Self::Restart => "Restart".to_string(),
            Self::Scale { replicas } => format!("Scale to {replicas}"),
            Self::Apply => "Apply".to_string(),
        }
    }
}

fn api(
    client: &kube::Client,
    resource: &ApiResource,
    namespace: Option<&str>,
) -> Api<DynamicObject> {
    match namespace {
        Some(namespace) => Api::namespaced_with(client.clone(), namespace, resource),
        None => Api::all_with(client.clone(), resource),
    }
}

/// Checks the identity and scope before using the selected kind's endpoint.
pub fn prepare_create(kind: &Kind, manifest: Value) -> Result<DynamicObject> {
    let invalid = |message: &str| Error::Manifest(message.to_string());
    if !manifest.is_object() {
        return Err(invalid("provide one resource object"));
    }
    if manifest.get("apiVersion").and_then(Value::as_str)
        != Some(kind.resource.api_version.as_str())
        || manifest.get("kind").and_then(Value::as_str) != Some(kind.resource.kind.as_str())
    {
        return Err(invalid(&format!(
            "expected apiVersion {} and kind {} for this resource tab",
            kind.resource.api_version, kind.resource.kind
        )));
    }
    let object: DynamicObject =
        serde_json::from_value(manifest).map_err(|error| invalid(&error.to_string()))?;
    let present = |value: &Option<String>| {
        value
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    };
    if !present(&object.metadata.name) && !present(&object.metadata.generate_name) {
        return Err(invalid(
            "metadata.name or metadata.generateName is required",
        ));
    }
    if kind.namespaced {
        if !present(&object.metadata.namespace) {
            return Err(invalid("metadata.namespace is required for this resource"));
        }
    } else if object.metadata.namespace.is_some() {
        return Err(invalid(
            "cluster-scoped resources must not specify metadata.namespace",
        ));
    }
    if object.metadata.uid.is_some() || object.metadata.resource_version.is_some() {
        return Err(invalid(
            "remove metadata.uid and metadata.resourceVersion when creating a resource",
        ));
    }
    Ok(object)
}

/// Uses POST rather than Apply: an existing name must be refused, never updated.
pub async fn create(
    client: &kube::Client,
    resource: &ApiResource,
    object: &DynamicObject,
    dry_run: bool,
) -> Result<DynamicObject> {
    let params = PostParams {
        dry_run,
        field_manager: Some(FIELD_MANAGER.to_string()),
    };
    let created = api(client, resource, object.metadata.namespace.as_deref())
        .create(&params, object)
        .await?;
    if !dry_run {
        tracing::info!(kind = %resource.kind, target = %ObjectRef::of(&created), "created");
    }
    Ok(created)
}

pub async fn delete(
    client: &kube::Client,
    resource: &ApiResource,
    namespace: Option<&str>,
    name: &str,
) -> Result<()> {
    api(client, resource, namespace)
        .delete(name, &DeleteParams::default())
        .await?;
    tracing::info!(kind = %resource.kind, name, namespace = ?namespace, "deleted");
    Ok(())
}

/// Delete only the exact incarnation selected by the user. If the object was
/// replaced under the same name, Kubernetes rejects the UID precondition.
pub async fn delete_with_uid(
    client: &kube::Client,
    resource: &ApiResource,
    target: &DeleteTarget,
) -> Result<()> {
    let params = DeleteParams {
        preconditions: Some(Preconditions {
            uid: Some(target.uid.clone()),
            ..Default::default()
        }),
        ..Default::default()
    };
    api(client, resource, target.reference.namespace.as_deref())
        .delete(&target.reference.name, &params)
        .await?;
    tracing::info!(kind = %resource.kind, target = %target.reference, "deleted");
    Ok(())
}

/// Stamps the pod template so the controller rolls its pods.
///
/// This is what `kubectl rollout restart` does. There is no restart verb in the
/// API; a workload restarts because its template changed.
pub async fn restart(
    client: &kube::Client,
    resource: &ApiResource,
    namespace: Option<&str>,
    name: &str,
    now: &str,
) -> Result<()> {
    let patch = json!({
        "spec": { "template": { "metadata": { "annotations": {
            "kubectl.kubernetes.io/restartedAt": now
        }}}}
    });

    api(client, resource, namespace)
        .patch(name, &PatchParams::default(), &Patch::Merge(patch))
        .await?;
    tracing::info!(kind = %resource.kind, name, namespace = ?namespace, "restarted");
    Ok(())
}

pub async fn scale(
    client: &kube::Client,
    resource: &ApiResource,
    namespace: Option<&str>,
    name: &str,
    replicas: i32,
) -> Result<()> {
    let patch = json!({ "spec": { "replicas": replicas } });

    api(client, resource, namespace)
        .patch_scale(name, &PatchParams::default(), &Patch::Merge(patch))
        .await?;
    tracing::info!(kind = %resource.kind, name, namespace = ?namespace, replicas, "scaled");
    Ok(())
}

/// What came back from an apply.
#[derive(Debug)]
pub enum Applied {
    Ok(Box<DynamicObject>),
    /// Somebody else owns fields this apply would change. Nothing was written.
    Conflict(Conflict),
}

/// A Server-Side Apply refusal, parsed into what a person needs to decide.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Conflict {
    /// The field paths the API server named, as it wrote them.
    pub fields: Vec<String>,
    /// The field managers that own them.
    pub managers: Vec<String>,
    /// The original message, in case the parse missed something.
    pub message: String,
}

impl Conflict {
    pub fn summary(&self) -> String {
        match (self.fields.len(), self.managers.as_slice()) {
            (0, _) => "This apply conflicts with another field manager.".to_string(),
            (count, []) => format!(
                "{count} field{} owned by another manager",
                if count == 1 { "" } else { "s" }
            ),
            (count, [manager]) => format!(
                "{count} field{} owned by {manager}",
                if count == 1 { "" } else { "s" }
            ),
            (count, managers) => format!(
                "{count} field{} owned by {} managers",
                if count == 1 { "" } else { "s" },
                managers.len()
            ),
        }
    }
}

/// Applies an edited object.
///
/// `force` takes ownership of the conflicting fields, which is what
/// `kubectl apply --force-conflicts` does. It is never the default: the first
/// attempt is always the one that can be refused.
pub async fn apply(
    client: &kube::Client,
    resource: &ApiResource,
    namespace: Option<&str>,
    name: &str,
    object: &Value,
    force: bool,
    dry_run: bool,
) -> Result<Applied> {
    let mut params = PatchParams::apply(FIELD_MANAGER);
    if force {
        params = params.force();
    }
    if dry_run {
        params = params.dry_run();
    }

    // GET responses include field ownership records, but Kubernetes forbids
    // submitting them in a Server-Side Apply request.
    let object = prepare_apply(object.clone());

    match api(client, resource, namespace)
        .patch(name, &params, &Patch::Apply(&object))
        .await
    {
        Ok(applied) => {
            if !dry_run {
                tracing::info!(kind = %resource.kind, name, namespace = ?namespace, "applied");
            }
            Ok(Applied::Ok(Box::new(applied)))
        }
        Err(kube::Error::Api(status)) if status.code == 409 => {
            Ok(Applied::Conflict(parse_conflict(&status.message)))
        }
        Err(error) => Err(Error::Api(error)),
    }
}

/// The exact manifest submitted by Apply, also used by the confirmation diff.
pub fn prepare_apply(mut object: Value) -> Value {
    if let Some(metadata) = object.get_mut("metadata").and_then(Value::as_object_mut) {
        metadata.remove("managedFields");
    }
    object
}

/// Reads the API server's conflict message.
///
/// There are two shapes and the difference is not documented anywhere; it was
/// found by pointing this at a cluster. A single conflict is one line, with the
/// field after the colon:
///
/// ```text
/// Apply failed with 1 conflict: conflict with "argocd-controller" using apps/v1: .spec.replicas
/// ```
///
/// Several conflicts put the fields on their own lines:
///
/// ```text
/// Apply failed with 2 conflicts: conflicts with "kubectl-client-side-apply" using v1:
/// - .spec.replicas
/// - .metadata.labels.team
/// ```
///
/// Parsed rather than shown raw because the field list is the part a person
/// acts on, and in the one-line form it is at the end of a sentence about
/// something else.
fn parse_conflict(message: &str) -> Conflict {
    let mut fields = Vec::new();
    let mut managers = Vec::new();

    for line in message.lines() {
        let line = line.trim();

        if let Some(field) = line.strip_prefix("- ") {
            push_unique(&mut fields, field.trim());
            continue;
        }

        // `conflict with` and `conflicts with` are both used, depending on the
        // count, and neither contains the other. Normalising first means one
        // split rather than two passes.
        let normalised = line.replace("conflicts with ", "conflict with ");
        for fragment in normalised.split("conflict with ").skip(1) {
            let Some((manager, rest)) = manager_and_rest(fragment) else {
                continue;
            };
            push_unique(&mut managers, &manager);

            // ` using apps/v1: .spec.replicas` -- anything after the second
            // colon is the field, and it is absent in the multi-line form.
            if let Some((_, trailing)) = rest.split_once(':') {
                let trailing = trailing.trim();
                if trailing.starts_with('.') {
                    push_unique(&mut fields, trailing);
                }
            }
        }
    }

    Conflict {
        fields,
        managers,
        message: message.to_string(),
    }
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !value.is_empty() && !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

/// Splits `"name" using v1: rest` into the quoted name and what follows it.
fn manager_and_rest(fragment: &str) -> Option<(String, &str)> {
    let rest = fragment.strip_prefix('"')?;
    let (manager, after) = rest.split_once('"')?;
    (!manager.is_empty()).then(|| (manager.to_string(), after))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Model the API server's Apply rejection, including manifests copied from
    /// a GET response or pasted into the YAML editor.
    #[tokio::test]
    async fn apply_omits_managed_fields_from_the_request() {
        use std::time::Duration;
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };

        for (group, kind, plural, namespace, managed_fields, force, dry_run) in [
            (
                "",
                "ConfigMap",
                "configmaps",
                Some("default"),
                Some(json!([{
                    "manager": "kubectl",
                    "operation": "Apply",
                    "apiVersion": "v1",
                    "fieldsType": "FieldsV1",
                    "fieldsV1": {"f:data": {"f:setting": {}}}
                }])),
                false,
                false,
            ),
            (
                "",
                "ConfigMap",
                "configmaps",
                Some("default"),
                Some(json!([])),
                true,
                false,
            ),
            (
                "example.io",
                "Widget",
                "widgets",
                None,
                Some(json!([{"manager": "controller", "operation": "Update"}])),
                false,
                true,
            ),
            (
                "example.io",
                "Widget",
                "widgets",
                Some("default"),
                Some(Value::Null),
                true,
                true,
            ),
            (
                "",
                "ConfigMap",
                "configmaps",
                Some("default"),
                None,
                false,
                true,
            ),
        ] {
            let resource = ApiResource::from_gvk_with_plural(
                &kube::core::GroupVersionKind::gvk(group, "v1", kind),
                plural,
            );
            let mut object = json!({
                "apiVersion": resource.api_version,
                "kind": kind,
                "metadata": {
                    "name": "apply-test",
                    "uid": "original-uid",
                    "resourceVersion": "42",
                    "labels": {"app": "test"},
                    "annotations": {"note": "edited"},
                    "ownerReferences": [{
                        "apiVersion": "v1", "kind": "ConfigMap",
                        "name": "owner", "uid": "owner-uid"
                    }]
                }
            });
            if let Some(namespace) = namespace {
                object["metadata"]["namespace"] = json!(namespace);
            }
            if let Some(managed_fields) = managed_fields {
                object["metadata"]["managedFields"] = managed_fields;
            }
            if kind == "ConfigMap" {
                object["data"] = json!({"setting": "edited value", "untouched": "keep"});
            } else {
                // A similarly named field inside a CRD's payload is user data.
                object["spec"] = json!({
                    "replicas": 3,
                    "metadata": {"managedFields": ["custom value"]}
                });
                object["status"] = json!({"ready": true});
            }
            let original = object.clone();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    bytes.push(stream.read_u8().await.unwrap());
                    assert!(bytes.len() < 8192);
                }
                let header = String::from_utf8(bytes).unwrap();
                let length: usize = header
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .unwrap()
                    .1
                    .trim()
                    .parse()
                    .unwrap();
                let mut body = vec![0; length];
                stream.read_exact(&mut body).await.unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                let rejected = body
                    .pointer("/metadata/managedFields")
                    .is_some_and(|value| !value.is_null());
                let (status, response) = if rejected {
                    (
                        "422 Unprocessable Entity",
                        json!({
                            "apiVersion": "v1", "kind": "Status", "status": "Failure",
                            "reason": "Invalid", "code": 422,
                            "message": "metadata.managedFields must be nil"
                        }),
                    )
                } else {
                    ("200 OK", body.clone())
                };
                let response = response.to_string();
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                            response.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                (header, body)
            });
            let config = kube::Config::new(format!("http://{address}").parse().unwrap());
            let client = kube::Client::try_from(config).unwrap();
            let applied = tokio::time::timeout(
                Duration::from_secs(5),
                apply(
                    &client,
                    &resource,
                    namespace,
                    "apply-test",
                    &object,
                    force,
                    dry_run,
                ),
            )
            .await
            .unwrap()
            .expect("Apply must omit server-managed fields");
            assert!(matches!(applied, Applied::Ok(_)));
            let (header, sent) = server.await.unwrap();
            let mut request_line = header.lines().next().unwrap().split_whitespace();
            assert_eq!(request_line.next(), Some("PATCH"));
            let url = url::Url::parse(&format!("http://{address}{}", request_line.next().unwrap()))
                .unwrap();
            let prefix = if group.is_empty() {
                "/api/v1".to_string()
            } else {
                format!("/apis/{group}/v1")
            };
            let scope = namespace
                .map(|ns| format!("/namespaces/{ns}"))
                .unwrap_or_default();
            assert_eq!(url.path(), format!("{prefix}{scope}/{plural}/apply-test"));
            let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
            assert_eq!(
                query.get("fieldManager").map(String::as_str),
                Some(FIELD_MANAGER)
            );
            assert_eq!(
                query.get("force").map(String::as_str) == Some("true"),
                force
            );
            assert_eq!(
                query.get("dryRun").map(String::as_str) == Some("All"),
                dry_run
            );
            assert!(
                header
                    .to_lowercase()
                    .contains("content-type: application/apply-patch+yaml")
            );
            let mut expected = original.clone();
            expected["metadata"]
                .as_object_mut()
                .unwrap()
                .remove("managedFields");
            assert_eq!(sent, expected);
            assert_eq!(
                object, original,
                "Applying must not change the editor's object"
            );
        }
    }

    fn create_kind(namespaced: bool) -> Kind {
        Kind {
            resource: ApiResource {
                group: "".into(),
                version: "v1".into(),
                api_version: "v1".into(),
                kind: "ConfigMap".into(),
                plural: "configmaps".into(),
            },
            namespaced,
            verbs: vec!["create".into()],
        }
    }

    #[test]
    fn creation_checks_endpoint_identity_and_namespace() {
        let kind = create_kind(true);
        let valid = json!({"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": "test", "namespace": "qa"}});
        assert_eq!(
            prepare_create(&kind, valid.clone())
                .unwrap()
                .metadata
                .namespace
                .as_deref(),
            Some("qa")
        );
        for (path, value) in [("kind", json!("Secret")), ("apiVersion", json!("apps/v1"))] {
            let mut wrong = valid.clone();
            wrong[path] = value;
            assert!(prepare_create(&kind, wrong).is_err());
        }
        let mut missing = valid.clone();
        missing["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("namespace");
        assert!(prepare_create(&kind, missing.clone()).is_err());
        assert!(prepare_create(&create_kind(false), valid).is_err());
        assert!(prepare_create(&create_kind(false), missing).is_ok());
    }

    #[test]
    fn creation_accepts_generated_names_but_rejects_existing_object_identity() {
        let kind = create_kind(true);
        let generated = json!({"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"generateName": "test-", "namespace": "qa"}});
        assert!(prepare_create(&kind, generated.clone()).is_ok());
        for field in ["uid", "resourceVersion"] {
            let mut existing = generated.clone();
            existing["metadata"][field] = json!("existing");
            assert!(prepare_create(&kind, existing).is_err());
        }
        let mut nameless = generated;
        nameless["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("generateName");
        assert!(prepare_create(&kind, nameless).is_err());
    }

    #[test]
    fn operations_need_the_verb_the_api_checks() {
        assert_eq!(Operation::Delete.verb(), "delete");
        assert_eq!(Operation::Apply.verb(), "patch");
        assert_eq!(Operation::Restart.verb(), "patch");
        assert_eq!(Operation::Scale { replicas: 3 }.verb(), "patch");
    }

    /// Permission to patch a Deployment is not permission to scale one -- RBAC
    /// names the subresource separately.
    #[test]
    fn scaling_asks_about_the_scale_subresource() {
        assert_eq!(
            Operation::Scale { replicas: 3 }.resource("deployments"),
            "deployments/scale"
        );
        assert_eq!(Operation::Delete.resource("deployments"), "deployments");
        assert_eq!(Operation::Apply.resource("pods"), "pods");
    }

    /// Offering "Scale" on a ConfigMap is a menu item that cannot work.
    #[test]
    fn scale_and_restart_only_apply_to_workloads() {
        let scale = Operation::Scale { replicas: 1 };
        assert!(scale.applies_to("apps", "Deployment"));
        assert!(scale.applies_to("apps", "StatefulSet"));
        assert!(
            !scale.applies_to("apps", "DaemonSet"),
            "a DaemonSet has no replicas"
        );
        assert!(!scale.applies_to("", "ConfigMap"));

        assert!(Operation::Restart.applies_to("apps", "DaemonSet"));
        assert!(!Operation::Restart.applies_to("", "Pod"));

        // Anything can be deleted or applied, including a CRD's objects.
        assert!(Operation::Delete.applies_to("argoproj.io", "Application"));
        assert!(Operation::Apply.applies_to("argoproj.io", "Application"));
    }

    /// Verbatim from an API server refusing an apply.
    const TWO_CONFLICTS: &str = r#"Apply failed with 2 conflicts: conflicts with "kubectl-client-side-apply" using v1:
- .spec.replicas
- .metadata.labels.team"#;

    /// Also verbatim, from a k3s 1.33 apiserver refusing a one-field apply.
    /// The single-conflict form puts the field on the same line, which the
    /// multi-line parse alone silently missed.
    const ONE_CONFLICT: &str = r#"Apply failed with 1 conflict: conflict with "argocd-controller" using apps/v1: .spec.replicas"#;

    #[test]
    fn a_single_conflict_is_one_line() {
        let conflict = parse_conflict(ONE_CONFLICT);

        assert_eq!(conflict.fields, [".spec.replicas"]);
        assert_eq!(conflict.managers, ["argocd-controller"]);
        assert_eq!(conflict.summary(), "1 field owned by argocd-controller");
    }

    #[test]
    fn a_conflict_names_its_fields_and_its_owner() {
        let conflict = parse_conflict(TWO_CONFLICTS);

        assert_eq!(conflict.fields, [".spec.replicas", ".metadata.labels.team"]);
        assert_eq!(conflict.managers, ["kubectl-client-side-apply"]);
        assert_eq!(
            conflict.summary(),
            "2 fields owned by kubectl-client-side-apply"
        );
    }

    #[test]
    fn a_conflict_with_several_owners_counts_them() {
        let message = r#"Apply failed with 3 conflicts: conflicts with "argocd-controller" using apps/v1:
- .spec.replicas
conflicts with "kubectl-client-side-apply" using apps/v1:
- .spec.template.spec.containers[name="app"].image
- .metadata.annotations.note"#;

        let conflict = parse_conflict(message);
        assert_eq!(conflict.fields.len(), 3);
        assert_eq!(
            conflict.managers,
            ["argocd-controller", "kubectl-client-side-apply"]
        );
        assert_eq!(conflict.summary(), "3 fields owned by 2 managers");
    }

    #[test]
    fn one_conflicting_field_reads_as_one() {
        let message = r#"Apply failed with 1 conflict: conflicts with "argocd-controller" using apps/v1:
- .spec.replicas"#;
        assert_eq!(
            parse_conflict(message).summary(),
            "1 field owned by argocd-controller"
        );
    }

    /// A field must not be counted twice because it appeared in both forms.
    #[test]
    fn a_field_is_never_listed_twice() {
        let message = r#"conflict with "a" using v1: .spec.replicas
- .spec.replicas"#;
        assert_eq!(parse_conflict(message).fields, [".spec.replicas"]);
    }

    /// The message format is not a contract. An unparseable one still has to
    /// produce something a person can read, and the original is kept for that.
    #[test]
    fn an_unrecognised_message_is_still_reported() {
        let conflict = parse_conflict("something else went wrong");

        assert!(conflict.fields.is_empty());
        assert!(conflict.managers.is_empty());
        assert_eq!(conflict.message, "something else went wrong");
        assert_eq!(
            conflict.summary(),
            "This apply conflicts with another field manager."
        );
    }

    #[test]
    fn a_manager_is_never_listed_twice() {
        let message = r#"conflicts with "same" using v1:
- .a
conflicts with "same" using v1:
- .b"#;
        assert_eq!(parse_conflict(message).managers, ["same"]);
    }
}
