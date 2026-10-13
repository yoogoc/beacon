//! Configuration references from Pod specs and workload templates. Values are never inspected.
use crate::{DynamicObject, ObjectRef};
use serde_json::Value;
use std::collections::BTreeSet;

pub const CONSUMERS: &[(&str, &str)] = &[
    ("", "Pod"),
    ("apps", "Deployment"),
    ("apps", "StatefulSet"),
    ("apps", "DaemonSet"),
    ("apps", "ReplicaSet"),
    ("", "ReplicationController"),
    ("batch", "Job"),
    ("batch", "CronJob"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigurationKind {
    ConfigMap,
    Secret,
}
impl ConfigurationKind {
    pub fn for_kind(group: &str, kind: &str) -> Option<Self> {
        match (group, kind) {
            ("", "ConfigMap") => Some(Self::ConfigMap),
            ("", "Secret") => Some(Self::Secret),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::ConfigMap => "ConfigMap",
            Self::Secret => "Secret",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Reference {
    pub kind: ConfigurationKind,
    pub name: String,
    pub key: Option<String>,
    pub container: Option<String>,
    pub via: String,
    pub field: String,
    pub mount_path: Option<String>,
    pub optional: bool,
}
impl Reference {
    pub fn target(&self, namespace: Option<String>) -> ObjectRef {
        ObjectRef::new(namespace, &self.name)
    }
    pub fn description(&self) -> String {
        let mut pieces = vec![
            self.key.as_deref().unwrap_or("All keys").to_string(),
            self.via.clone(),
        ];
        if let Some(container) = &self.container {
            pieces.push(format!("container {container}"));
        }
        if let Some(path) = &self.mount_path {
            pieces.push(path.clone());
        }
        if self.optional {
            pieces.push("optional".into());
        }
        pieces.join(" · ")
    }
    pub fn missing_key(&self, object: &DynamicObject) -> bool {
        self.key.as_ref().is_some_and(|key| {
            !["data", "binaryData", "stringData"].iter().any(|field| {
                object
                    .data
                    .get(field)
                    .and_then(Value::as_object)
                    .is_some_and(|map| map.contains_key(key))
            })
        })
    }
}

pub fn supported(group: &str, kind: &str) -> bool {
    CONSUMERS.contains(&(group, kind))
}

fn pod_spec<'a>(
    group: &str,
    kind: &str,
    object: &'a DynamicObject,
) -> Option<(&'a Value, &'static str)> {
    match (group, kind) {
        ("", "Pod") => Some((object.data.get("spec")?, "/spec")),
        ("batch", "CronJob") => Some((
            object
                .data
                .pointer("/spec/jobTemplate/spec/template/spec")?,
            "/spec/jobTemplate/spec/template/spec",
        )),
        _ if supported(group, kind) => Some((
            object.data.pointer("/spec/template/spec")?,
            "/spec/template/spec",
        )),
        _ => None,
    }
}

pub fn keys(object: &DynamicObject) -> BTreeSet<String> {
    ["data", "binaryData", "stringData"]
        .into_iter()
        .filter_map(|field| object.data.get(field)?.as_object())
        .flat_map(|map| map.keys().cloned())
        .collect()
}

fn array<'a>(value: &'a Value, name: &str) -> std::slice::Iter<'a, Value> {
    value
        .get(name)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
}
fn string(value: &Value, name: &str) -> Option<String> {
    value
        .get(name)?
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Workload kind is supplied separately because watch list items can omit their type metadata.
pub fn references(group: &str, kind: &str, object: &DynamicObject) -> Vec<Reference> {
    let Some((spec, prefix)) = pod_spec(group, kind, object) else {
        return vec![];
    };
    let mut result = Vec::new();
    for category in ["containers", "initContainers", "ephemeralContainers"] {
        for (index, container) in array(spec, category).enumerate() {
            let container_name = string(container, "name");
            for (env_index, env) in array(container, "env").enumerate() {
                for (field, config_kind) in [
                    ("configMapKeyRef", ConfigurationKind::ConfigMap),
                    ("secretKeyRef", ConfigurationKind::Secret),
                ] {
                    let Some(target) = env.get("valueFrom").and_then(|value| value.get(field))
                    else {
                        continue;
                    };
                    if let Some(name) = string(target, "name") {
                        result.push(Reference {
                            kind: config_kind,
                            name,
                            key: string(target, "key"),
                            container: container_name.clone(),
                            via: format!("env {}", env["name"].as_str().unwrap_or("?")),
                            field: format!(
                                "{prefix}/{category}/{index}/env/{env_index}/valueFrom/{field}"
                            ),
                            mount_path: None,
                            optional: target["optional"].as_bool().unwrap_or(false),
                        });
                    }
                }
            }
            for (env_index, env) in array(container, "envFrom").enumerate() {
                for (field, config_kind) in [
                    ("configMapRef", ConfigurationKind::ConfigMap),
                    ("secretRef", ConfigurationKind::Secret),
                ] {
                    let Some(target) = env.get(field) else {
                        continue;
                    };
                    if let Some(name) = string(target, "name") {
                        result.push(Reference {
                            kind: config_kind,
                            name,
                            key: None,
                            container: container_name.clone(),
                            via: format!(
                                "envFrom{}",
                                string(env, "prefix")
                                    .map(|prefix| format!(" (prefix {prefix})"))
                                    .unwrap_or_default()
                            ),
                            field: format!(
                                "{prefix}/{category}/{index}/envFrom/{env_index}/{field}"
                            ),
                            mount_path: None,
                            optional: target["optional"].as_bool().unwrap_or(false),
                        });
                    }
                }
            }
        }
    }
    for (index, volume) in array(spec, "volumes").enumerate() {
        let name = volume["name"].as_str().unwrap_or("?");
        let mounts: Vec<_> = ["containers", "initContainers", "ephemeralContainers"]
            .into_iter()
            .flat_map(|category| array(spec, category))
            .flat_map(|container| {
                array(container, "volumeMounts")
                    .filter(move |mount| mount["name"] == name)
                    .map(move |mount| (string(container, "name"), mount))
            })
            .collect();
        let mut projections: Vec<_> = [
            ("configMap", ConfigurationKind::ConfigMap, "name"),
            ("secret", ConfigurationKind::Secret, "secretName"),
        ]
        .into_iter()
        .filter_map(|(field, kind, name_field)| {
            Some((
                volume.get(field)?,
                kind,
                name_field,
                format!("{prefix}/volumes/{index}/{field}"),
            ))
        })
        .collect();
        if let Some(projected) = volume.get("projected") {
            for (projection, source) in array(projected, "sources").enumerate() {
                for (field, config_kind) in [
                    ("configMap", ConfigurationKind::ConfigMap),
                    ("secret", ConfigurationKind::Secret),
                ] {
                    if let Some(target) = source.get(field) {
                        projections.push((
                            target,
                            config_kind,
                            "name",
                            format!(
                                "{prefix}/volumes/{index}/projected/sources/{projection}/{field}"
                            ),
                        ));
                    }
                }
            }
        }
        for (target, config_kind, name_field, field) in projections {
            let Some(config_name) = string(target, name_field) else {
                continue;
            };
            let items: Vec<_> = array(target, "items").collect();
            let entries: Vec<_> = if items.is_empty() {
                vec![(None, None)]
            } else {
                items
                    .iter()
                    .map(|item| (string(item, "key"), string(item, "path")))
                    .collect()
            };
            let locations: Vec<_> = if mounts.is_empty() {
                vec![(None, None)]
            } else {
                mounts
                    .iter()
                    .map(|(container, mount)| (container.clone(), Some(*mount)))
                    .collect()
            };
            for (key, file) in entries {
                for (container, mount) in &locations {
                    if let (Some(mount), Some(file)) = (mount, file.as_deref())
                        && let Some(sub_path) = mount.get("subPath").and_then(Value::as_str)
                        && sub_path != file
                    {
                        continue;
                    }
                    let mount_path = mount.and_then(|mount| {
                        let path = mount["mountPath"].as_str()?;
                        if mount.get("subPath").is_some() || mount.get("subPathExpr").is_some() {
                            Some(path.to_string())
                        } else {
                            Some(
                                file.as_ref()
                                    .map(|file| format!("{}/{file}", path.trim_end_matches('/')))
                                    .unwrap_or_else(|| path.to_string()),
                            )
                        }
                    });
                    result.push(Reference {
                        kind: config_kind,
                        name: config_name.clone(),
                        key: key.clone(),
                        container: container.clone(),
                        via: format!("volume {name}"),
                        field: field.clone(),
                        mount_path,
                        optional: target["optional"].as_bool().unwrap_or(false),
                    });
                }
            }
        }
        if let Some(target) = volume.pointer("/csi/nodePublishSecretRef")
            && let Some(name) = string(target, "name")
        {
            result.push(Reference {
                kind: ConfigurationKind::Secret,
                name,
                key: None,
                container: None,
                via: "CSI nodePublishSecretRef".into(),
                field: format!("{prefix}/volumes/{index}/csi/nodePublishSecretRef"),
                mount_path: None,
                optional: false,
            });
        }
    }
    for (index, secret) in array(spec, "imagePullSecrets").enumerate() {
        if let Some(name) = string(secret, "name") {
            result.push(Reference {
                kind: ConfigurationKind::Secret,
                name,
                key: None,
                container: None,
                via: "imagePullSecrets".into(),
                field: format!("{prefix}/imagePullSecrets/{index}"),
                mount_path: None,
                optional: false,
            });
        }
    }
    result.sort();
    result.dedup();
    result
}

pub fn used_by(
    kind: ConfigurationKind,
    source: &DynamicObject,
    group: &str,
    consumer_kind: &str,
    consumer: &DynamicObject,
) -> Vec<Reference> {
    if source.metadata.namespace != consumer.metadata.namespace {
        return vec![];
    }
    references(group, consumer_kind, consumer)
        .into_iter()
        .filter(|reference| {
            reference.kind == kind
                && Some(reference.name.as_str()) == source.metadata.name.as_deref()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn object(kind: &str, data: Value) -> DynamicObject {
        let mut object: DynamicObject = serde_json::from_value(json!({"apiVersion":"v1", "kind":kind,"metadata":{"name":"config","namespace":"default"}})).unwrap();
        object.data = data;
        object
    }

    #[test]
    fn parses_actual_references_keys_paths_and_init_containers_without_reading_values() {
        let consumer = object(
            "Pod",
            json!({"spec":{"containers":[{"name":"app","env":[{"name":"PORT","valueFrom":{"configMapKeyRef":{"name":"config","key":"port"}}}],"envFrom":[{"secretRef":{"name":"credentials","optional":true}}],"volumeMounts":[{"name":"settings","mountPath":"/etc/app"}]}],"initContainers":[{"name":"init","env":[{"name":"TOKEN","valueFrom":{"secretKeyRef":{"name":"config","key":"token"}}}]}],"volumes":[{"name":"settings","projected":{"sources":[{"configMap":{"name":"config","items":[{"key":"app.yml","path":"settings.yml"}]}},{"secret":{"name":"credentials"}}]}}],"imagePullSecrets":[{"name":"registry"}]}}),
        );
        let references = references("", "Pod", &consumer);
        assert_eq!(references.len(), 6);
        assert!(
            references
                .iter()
                .any(|r| r.key.as_deref() == Some("app.yml")
                    && r.mount_path.as_deref() == Some("/etc/app/settings.yml"))
        );
        assert!(
            references.iter().any(
                |r| r.key.as_deref() == Some("token") && r.container.as_deref() == Some("init")
            )
        );
        assert!(
            references
                .iter()
                .any(|r| r.name == "credentials" && r.optional)
        );
        assert!(
            references
                .iter()
                .any(|r| r.name == "registry" && r.via == "imagePullSecrets")
        );
        let source = object(
            "ConfigMap",
            json!({"data":{"port":"80"},"binaryData":{"app.yml":"AA=="}}),
        );
        let used = used_by(ConfigurationKind::ConfigMap, &source, "", "Pod", &consumer);
        assert_eq!(used.len(), 2);
        assert!(used.iter().all(|r| !r.missing_key(&source)));
    }

    #[test]
    fn limits_usage_to_namespace_and_real_kind_and_handles_workload_templates() {
        let source = object("Secret", json!({"data":{"present":"AA=="}}));
        let spec = json!({"containers":[{"name":"app","env":[{"name":"VALUE","valueFrom":{"secretKeyRef":{"name":"config","key":"missing"}}}]}]});
        let deployment = object("Deployment", json!({"spec":{"template":{"spec":spec}}}));
        let cron = object(
            "CronJob",
            json!({"spec":{"jobTemplate":{"spec":{"template":{"spec":spec}}}}}),
        );
        for (group, kind, consumer) in [
            ("apps", "Deployment", deployment.clone()),
            ("batch", "CronJob", cron),
        ] {
            let refs = used_by(ConfigurationKind::Secret, &source, group, kind, &consumer);
            assert_eq!(refs.len(), 1);
            assert!(refs[0].missing_key(&source));
        }
        let mut other = deployment.clone();
        other.metadata.namespace = Some("elsewhere".into());
        assert!(
            used_by(
                ConfigurationKind::Secret,
                &source,
                "apps",
                "Deployment",
                &other
            )
            .is_empty()
        );
        assert!(
            used_by(
                ConfigurationKind::ConfigMap,
                &source,
                "apps",
                "Deployment",
                &deployment
            )
            .is_empty()
        );
        assert!(references("custom.io", "Deployment", &deployment).is_empty());
    }

    #[test]
    fn subpath_only_reports_the_selected_volume_file_and_preserves_optional_refs() {
        let consumer = object(
            "Pod",
            json!({"spec":{"containers":[{"name":"app","volumeMounts":[{"name":"settings","mountPath":"/config","subPath":"selected"}]}],"volumes":[{"name":"settings","secret":{"secretName":"config","optional":true,"items":[{"key":"yes","path":"selected"},{"key":"no","path":"other"}]}}]}}),
        );
        let refs = references("", "Pod", &consumer);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].key.as_deref(), Some("yes"));
        assert_eq!(refs[0].mount_path.as_deref(), Some("/config"));
        assert!(refs[0].optional);
    }
}
