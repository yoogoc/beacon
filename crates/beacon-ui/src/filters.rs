//! Exact resource filters, independent of the fuzzy name search.
use beacon_columns::{Timestamp, builtin, pod};
use beacon_kube::{DynamicObject, Kind};
use serde_json::Value;

/// Search the choices inside a picker without changing its selected values.
pub(crate) struct OptionSearch(String);

impl OptionSearch {
    pub(crate) fn new(query: &str) -> Self {
        Self(query.trim().to_lowercase())
    }

    pub(crate) fn matches(&self, value: &str) -> bool {
        self.0.is_empty() || value.to_lowercase().contains(&self.0)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Field {
    SecretType,
    PodStatus,
    DeploymentStatus,
    ServiceType,
    IngressClass,
    ClaimStatus,
    Volume,
    AccessMode,
    StorageClass,
    VolumeMode,
    Scope,
}

impl Field {
    pub(crate) fn for_kind(kind: &Kind) -> Vec<Self> {
        use Field::*;
        match (kind.resource.group.as_str(), kind.resource.kind.as_str()) {
            ("", "Secret") => vec![SecretType],
            ("", "Pod") => vec![PodStatus],
            ("apps", "Deployment") => vec![DeploymentStatus],
            ("", "Service") => vec![ServiceType],
            ("networking.k8s.io", "Ingress") => vec![IngressClass],
            ("", "PersistentVolumeClaim") => {
                vec![ClaimStatus, Volume, AccessMode, StorageClass, VolumeMode]
            }
            ("apiextensions.k8s.io", "CustomResourceDefinition") => vec![Scope],
            _ => vec![],
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::SecretType | Self::ServiceType => "Type",
            Self::PodStatus | Self::DeploymentStatus | Self::ClaimStatus => "Status",
            Self::IngressClass => "Class",
            Self::Volume => "Volume",
            Self::AccessMode => "Access mode",
            Self::StorageClass => "Storage class",
            Self::VolumeMode => "Volume mode",
            Self::Scope => "Scope",
        }
    }

    pub(crate) fn values(self, object: &DynamicObject, now: Timestamp) -> Vec<String> {
        let data = &object.data;
        let text = |path: &str, default: &str| {
            data.pointer(path)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .unwrap_or(default)
                .to_string()
        };
        vec![match self {
            Self::SecretType => text("/type", "Opaque"),
            Self::PodStatus => pod::summarize(&object.metadata, data, now).status,
            Self::DeploymentStatus => builtin::deployment_status(&object.metadata, data),
            Self::ServiceType => text("/spec/type", "ClusterIP"),
            Self::IngressClass => data
                .pointer("/spec/ingressClassName")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    object
                        .metadata
                        .annotations
                        .as_ref()?
                        .get("kubernetes.io/ingress.class")
                        .cloned()
                })
                .unwrap_or_else(|| "<none>".into()),
            Self::ClaimStatus => text("/status/phase", "Pending"),
            Self::Volume => text("/spec/volumeName", "<none>"),
            Self::StorageClass => data
                .pointer("/spec/storageClassName")
                .and_then(Value::as_str)
                .map(|s| if s.is_empty() { "<none>" } else { s }.to_string())
                .or_else(|| {
                    object
                        .metadata
                        .annotations
                        .as_ref()?
                        .get("volume.beta.kubernetes.io/storage-class")
                        .cloned()
                })
                .unwrap_or_else(|| "<none>".into()),
            Self::VolumeMode => text("/spec/volumeMode", "Filesystem"),
            Self::Scope => text("/spec/scope", "<none>"),
            Self::AccessMode => {
                let modes: Vec<_> = data
                    .pointer("/spec/accessModes")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                return if modes.is_empty() {
                    vec!["<none>".into()]
                } else {
                    modes
                };
            }
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn object(data: Value) -> DynamicObject {
        serde_json::from_value(
            json!({"apiVersion":"v1", "kind":"Pod", "metadata":{"name":"demo"}, "spec":data}),
        )
        .unwrap()
    }
    #[test]
    fn modes_are_individual_and_absent_values_have_defaults() {
        let o = object(json!({"accessModes":["ReadWriteOnce","ReadOnlyMany"]}));
        assert_eq!(
            Field::AccessMode.values(&o, Timestamp::now()),
            ["ReadWriteOnce", "ReadOnlyMany"]
        );
        assert_eq!(
            Field::ServiceType.values(&o, Timestamp::now()),
            ["ClusterIP"]
        );
        assert_eq!(Field::StorageClass.values(&o, Timestamp::now()), ["<none>"]);
    }
    #[test]
    fn ingress_legacy_class_and_explicit_class() {
        let mut o = object(json!({}));
        o.metadata.annotations = Some(std::collections::BTreeMap::from([(
            "kubernetes.io/ingress.class".into(),
            "nginx".into(),
        )]));
        assert_eq!(Field::IngressClass.values(&o, Timestamp::now()), ["nginx"]);
        o.data["spec"]["ingressClassName"] = json!("traefik");
        assert_eq!(
            Field::IngressClass.values(&o, Timestamp::now()),
            ["traefik"]
        );
    }
}
