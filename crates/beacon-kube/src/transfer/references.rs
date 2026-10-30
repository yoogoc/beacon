use super::*;

#[derive(Clone)]
struct Reference {
    group: &'static str,
    kind: String,
    name: String,
    pointer: String,
    key: Option<String>,
    optional: bool,
    namespace: Option<String>,
}
fn references(kind: &Kind, object: &DynamicObject) -> Vec<Reference> {
    let namespace = object.metadata.namespace.clone();
    let mut refs =
        crate::config_refs::references(&kind.resource.group, &kind.resource.kind, object)
            .into_iter()
            .map(|r| {
                let key_field = object
                    .data
                    .pointer(&r.field)
                    .and_then(|v| v.get("secretName"))
                    .map_or("name", |_| "secretName");
                Reference {
                    group: "",
                    kind: r.kind.name().into(),
                    name: r.name,
                    pointer: format!("{}/{key_field}", r.field),
                    key: r.key,
                    optional: r.optional,
                    namespace: namespace.clone(),
                }
            })
            .collect::<Vec<_>>();
    let value = &object.data;
    let prefix = match kind.resource.kind.as_str() {
        "Pod" => "/spec",
        "CronJob" => "/spec/jobTemplate/spec/template/spec",
        _ => "/spec/template/spec",
    };
    if let Some(name) = value
        .pointer(&format!("{prefix}/serviceAccountName"))
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty() && *n != "default")
    {
        refs.push(Reference {
            group: "",
            kind: "ServiceAccount".into(),
            name: name.into(),
            pointer: format!("{prefix}/serviceAccountName"),
            key: None,
            optional: false,
            namespace: namespace.clone(),
        });
    }
    fn add_service(
        value: &Value,
        pointer: &str,
        namespace: &Option<String>,
        refs: &mut Vec<Reference>,
    ) {
        if let Some(name) = value.pointer(pointer).and_then(Value::as_str) {
            refs.push(Reference {
                group: "",
                kind: "Service".into(),
                name: name.into(),
                pointer: pointer.into(),
                key: None,
                optional: false,
                namespace: namespace.clone(),
            });
        }
    }
    if kind.resource.group == "networking.k8s.io" && kind.resource.kind == "Ingress" {
        add_service(
            value,
            "/spec/defaultBackend/service/name",
            &namespace,
            &mut refs,
        );
        if let Some(rules) = value.pointer("/spec/rules").and_then(Value::as_array) {
            for (i, rule) in rules.iter().enumerate() {
                for j in 0..rule
                    .pointer("/http/paths")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
                {
                    add_service(
                        value,
                        &format!("/spec/rules/{i}/http/paths/{j}/backend/service/name"),
                        &namespace,
                        &mut refs,
                    );
                }
            }
        }
        if let Some(tls) = value.pointer("/spec/tls").and_then(Value::as_array) {
            for (i, tls) in tls.iter().enumerate() {
                if let Some(name) = tls["secretName"].as_str().filter(|n| !n.is_empty()) {
                    refs.push(Reference {
                        group: "",
                        kind: "Secret".into(),
                        name: name.into(),
                        pointer: format!("/spec/tls/{i}/secretName"),
                        key: None,
                        optional: false,
                        namespace: namespace.clone(),
                    });
                }
            }
        }
    }
    if kind.resource.group == "rbac.authorization.k8s.io"
        && matches!(
            kind.resource.kind.as_str(),
            "RoleBinding" | "ClusterRoleBinding"
        )
    {
        if let (Some(name), Some(role)) = (
            value.pointer("/roleRef/name").and_then(Value::as_str),
            value.pointer("/roleRef/kind").and_then(Value::as_str),
        ) {
            refs.push(Reference {
                group: "rbac.authorization.k8s.io",
                kind: role.into(),
                name: name.into(),
                pointer: "/roleRef/name".into(),
                key: None,
                optional: false,
                namespace: (role == "Role").then(|| namespace.clone()).flatten(),
            });
        }
        if let Some(subjects) = value["subjects"].as_array() {
            for (i, subject) in subjects
                .iter()
                .enumerate()
                .filter(|(_, s)| s["kind"] == "ServiceAccount")
            {
                if let (Some(name), Some(ns)) =
                    (subject["name"].as_str(), subject["namespace"].as_str())
                {
                    refs.push(Reference {
                        group: "",
                        kind: "ServiceAccount".into(),
                        name: name.into(),
                        pointer: format!("/subjects/{i}/name"),
                        key: None,
                        optional: false,
                        namespace: Some(ns.into()),
                    });
                }
            }
        }
    }
    if kind.resource.group.is_empty()
        && kind.resource.kind == "ServiceAccount"
        && let Some(secrets) = value["imagePullSecrets"].as_array()
    {
        for (i, secret) in secrets.iter().enumerate() {
            if let Some(name) = secret["name"].as_str() {
                refs.push(Reference {
                    group: "",
                    kind: "Secret".into(),
                    name: name.into(),
                    pointer: format!("/imagePullSecrets/{i}/name"),
                    key: None,
                    optional: false,
                    namespace: namespace.clone(),
                });
            }
        }
    }
    refs
}
impl Batch {
    pub(super) async fn check_references(&mut self) {
        let selected = self
            .items
            .iter()
            .filter_map(|item| {
                Some((
                    item.selection.kind.resource.group.clone(),
                    item.selection.kind.resource.kind.clone(),
                    item.selection.reference.clone(),
                    item.selection.name.clone(),
                    item.source.clone()?,
                ))
            })
            .collect::<Vec<_>>();
        let mut missing = HashSet::new();
        for item in &mut self.items {
            let Some(source) = &item.source else {
                continue;
            };
            for reference in references(&item.selection.kind, source) {
                let candidate = selected.iter().find(|(group, kind, target, _, _)| {
                    group == reference.group
                        && kind == &reference.kind
                        && target.name == reference.name
                        && target.namespace == reference.namespace
                });
                let source_kind = self
                    .source
                    .discovery()
                    .kinds()
                    .iter()
                    .find(|kind| {
                        kind.resource.group == reference.group
                            && kind.resource.kind == reference.kind
                    })
                    .cloned();
                let source_ns = reference.namespace.clone();
                let target_ns = if reference.namespace == item.selection.reference.namespace {
                    self.options.namespace.clone()
                } else {
                    reference.namespace.clone()
                };
                if let Some((_, _, _, new_name, object)) = candidate {
                    if let Some(value) = item
                        .manifest
                        .as_mut()
                        .and_then(|value| value.pointer_mut(&reference.pointer))
                    {
                        *value = Value::String(new_name.clone());
                    }
                    // ServiceAccount subjects carry an explicit namespace.
                    if reference.pointer.starts_with("/subjects/")
                        && let Some(value) = item.manifest.as_mut().and_then(|v| {
                            v.pointer_mut(
                                reference
                                    .pointer
                                    .trim_end_matches("name")
                                    .trim_end_matches('/'),
                            )
                        })
                        && let Some(map) = value.as_object_mut()
                        && let Some(ns) = &self.options.namespace
                    {
                        map.insert("namespace".into(), Value::String(ns.clone()));
                    }
                    if reference
                        .key
                        .as_ref()
                        .is_some_and(|key| !crate::config_refs::keys(object).contains(key))
                        && !reference.optional
                    {
                        item.problem = Some(format!(
                            "Referenced key is missing in {}/{}.",
                            reference.kind, reference.name
                        ));
                    }
                    continue;
                }
                let target_kind = self.destination.discovery().kinds().iter().find(|kind| {
                    kind.resource.group == reference.group && kind.resource.kind == reference.kind
                });
                let exists = if let Some(kind) = target_kind {
                    match api(&self.destination, kind, target_ns.as_deref(), false)
                        .get(&reference.name)
                        .await
                    {
                        Ok(object) => reference
                            .key
                            .as_ref()
                            .is_none_or(|key| crate::config_refs::keys(&object).contains(key)),
                        Err(error) if api_missing(&error) => false,
                        Err(error) => {
                            item.problem = Some(Error::from(error).user_message());
                            continue;
                        }
                    }
                } else {
                    false
                };
                if exists {
                    if reference.pointer.starts_with("/subjects/")
                        && source_ns == item.selection.reference.namespace
                        && let Some(ns) = &target_ns
                        && let Some(map) = item
                            .manifest
                            .as_mut()
                            .and_then(|v| {
                                v.pointer_mut(
                                    reference
                                        .pointer
                                        .trim_end_matches("name")
                                        .trim_end_matches('/'),
                                )
                            })
                            .and_then(Value::as_object_mut)
                    {
                        map.insert("namespace".into(), Value::String(ns.clone()));
                    }
                    continue;
                }
                if reference.optional {
                    self.warnings.push(format!(
                        "Optional reference {}/{} is absent at the destination.",
                        reference.kind, reference.name
                    ));
                    continue;
                }
                let message = format!(
                    "Required reference {}/{}{} is missing at the destination.",
                    reference.kind,
                    reference.name,
                    reference
                        .key
                        .as_ref()
                        .map_or(String::new(), |key| format!(" key {key}"))
                );
                if let Some(kind) = source_kind {
                    let target = ObjectRef::new(source_ns.clone(), reference.name.clone());
                    if missing.insert((
                        kind.resource.group.clone(),
                        kind.resource.kind.clone(),
                        target.clone(),
                    )) {
                        let original = api(&self.source, &kind, source_ns.as_deref(), false)
                            .get(&reference.name)
                            .await
                            .ok();
                        self.dependencies.push(Dependency {
                            kind,
                            reference: target,
                            uid: original.and_then(|object| object.metadata.uid),
                            message,
                        });
                    }
                } else {
                    item.problem = Some(message);
                }
            }
        }
    }
}
