//! Prepared off the UI thread. Runtime fields and concealed Secrets share one
//! normalization policy across the field summary and the YAML viewer.
use super::Options;
use gpui_kit::component::diff::DiffFile;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;

pub(super) fn normalized(mut value: Value, options: Options) -> Value {
    if !options.status {
        value.as_object_mut().map(|object| object.remove("status"));
    }
    if !options.metadata
        && let Some(metadata) = value.get_mut("metadata").and_then(Value::as_object_mut)
    {
        for key in [
            "uid",
            "resourceVersion",
            "generation",
            "creationTimestamp",
            "deletionTimestamp",
            "deletionGracePeriodSeconds",
            "managedFields",
            "selfLink",
            "namespace",
        ] {
            metadata.remove(key);
        }
        if let Some(annotations) = metadata
            .get_mut("annotations")
            .and_then(Value::as_object_mut)
        {
            annotations.remove("kubectl.kubernetes.io/last-applied-configuration");
            if annotations.is_empty() {
                metadata.remove("annotations");
            }
        }
        if let Some(owners) = metadata
            .get_mut("ownerReferences")
            .and_then(Value::as_array_mut)
        {
            for owner in owners {
                if let Some(owner) = owner.as_object_mut() {
                    owner.remove("uid");
                }
            }
        }
    }
    if value["kind"] == "Secret" && !options.reveal {
        for key in ["data", "stringData"] {
            if let Some(data) = value.get_mut(key).and_then(Value::as_object_mut) {
                for value in data.values_mut() {
                    let digest = Sha256::digest(value.as_str().unwrap_or_default().as_bytes());
                    *value = Value::String(format!("[concealed · sha256:{digest:x}]"));
                }
            }
        }
        if let Some(annotations) = value
            .pointer_mut("/metadata/annotations")
            .and_then(Value::as_object_mut)
        {
            annotations.remove("kubectl.kubernetes.io/last-applied-configuration");
        }
    }
    value
}

#[derive(Clone)]
pub(super) struct Change {
    pub path: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}
impl Change {
    pub fn label(&self) -> &'static str {
        match (&self.before, &self.after) {
            (None, _) => "Added",
            (_, None) => "Removed",
            _ => "Changed",
        }
    }
    pub fn text(value: &Option<Value>) -> String {
        match value {
            None => "Not present".into(),
            Some(Value::String(text)) if text.starts_with("[concealed · sha256:") => {
                "Hidden Secret value".into()
            }
            Some(Value::String(text)) => text.clone(),
            Some(value) => serde_json::to_string_pretty(value).unwrap_or_default(),
        }
    }
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        self.path.to_lowercase().contains(&query)
            || Self::text(&self.before).to_lowercase().contains(&query)
            || Self::text(&self.after).to_lowercase().contains(&query)
    }
}

pub(super) struct Prepared {
    pub changes: Vec<Change>,
    pub files: Vec<DiffFile>,
    pub added: usize,
    pub removed: usize,
}

fn changes(a: Option<&Value>, b: Option<&Value>, path: String, out: &mut Vec<Change>) {
    if a == b {
        return;
    }
    match (a, b) {
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            let keys: BTreeSet<_> = a.keys().chain(b.keys()).collect();
            for key in keys {
                let segment = if key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    }
                } else {
                    format!("{path}[{}]", serde_json::to_string(key).unwrap())
                };
                changes(a.get(key), b.get(key), segment, out);
            }
        }
        (Some(Value::Array(a)), Some(Value::Array(b))) => {
            for i in 0..a.len().max(b.len()) {
                changes(a.get(i), b.get(i), format!("{path}[{i}]"), out);
            }
        }
        _ => out.push(Change {
            path,
            before: a.cloned(),
            after: b.cloned(),
        }),
    }
}

pub(super) fn prepare(a: Value, b: Value, options: Options) -> Result<Prepared, String> {
    let a = normalized(a, options);
    let b = normalized(b, options);
    let mut fields = vec![];
    changes(Some(&a), Some(&b), String::new(), &mut fields);
    let before = crate::yaml_review::canonical_yaml(a)?;
    let after = crate::yaml_review::canonical_yaml(b)?;
    let text_diff = similar::TextDiff::from_lines(&before, &after);
    let mut added = 0;
    let mut removed = 0;
    for change in text_diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => removed += 1,
            _ => {}
        }
    }
    let files = if before == after {
        vec![DiffFile::unchanged("resource.yaml", &before).with_language("yaml")]
    } else {
        let patch = text_diff
            .unified_diff()
            .context_radius(before.lines().count().max(after.lines().count()))
            .header("source.yaml", "target.yaml")
            .to_string();
        DiffFile::parse(&patch)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|file| file.with_language("yaml"))
            .collect()
    };
    Ok(Prepared {
        changes: fields,
        files,
        added,
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summary_tracks_field_paths_additions_removals_and_missing_values() {
        let a = json!({"metadata":{"annotations":{"example.com/key":"old"}},"spec":{"replicas":2,"containers":[{"image":"web:v1"}],"removed":true}});
        let b = json!({"metadata":{"annotations":{"example.com/key":"new"}},"spec":{"replicas":4,"containers":[{"image":"web:v2"}, {"image":"sidecar:v1"}],"added":null}});
        let result = prepare(a, b, Options::default()).unwrap();
        assert_eq!(
            result
                .changes
                .iter()
                .map(|change| (change.path.as_str(), change.label()))
                .collect::<Vec<_>>(),
            vec![
                ("metadata.annotations[\"example.com/key\"]", "Changed"),
                ("spec.added", "Added"),
                ("spec.containers[0].image", "Changed"),
                ("spec.containers[1]", "Added"),
                ("spec.removed", "Removed"),
                ("spec.replicas", "Changed"),
            ]
        );
        assert_eq!(Change::text(&result.changes[1].before), "Not present");
        assert_eq!(Change::text(&result.changes[1].after), "null");
        assert!(result.changes[2].matches("WEB:V2"));
        assert!(result.changes[5].matches("replicas"));
        assert!(result.added > 0 && result.removed > 0);
    }

    #[test]
    fn summary_and_yaml_ignore_runtime_noise_unless_requested() {
        let a = json!({"kind":"Deployment","metadata":{"name":"web","namespace":"dev","uid":"1","annotations":{"kubectl.kubernetes.io/last-applied-configuration":"old"}},"status":{"readyReplicas":1}});
        let b = json!({"kind":"Deployment","metadata":{"name":"web","namespace":"prod","uid":"2"},"status":{"readyReplicas":3}});
        let result = prepare(a.clone(), b.clone(), Options::default()).unwrap();
        assert!(result.changes.is_empty());
        assert_eq!((result.added, result.removed), (0, 0));
        let result = prepare(
            a,
            b,
            Options {
                status: true,
                metadata: true,
                reveal: false,
            },
        )
        .unwrap();
        assert!(
            result
                .changes
                .iter()
                .any(|change| change.path == "metadata.namespace")
        );
        assert!(
            result
                .changes
                .iter()
                .any(|change| change.path == "status.readyReplicas")
        );
        assert!(result.added > 0 && result.removed > 0);
    }

    #[test]
    fn secret_values_are_detected_and_filterable_without_being_exposed() {
        let a = json!({"kind":"Secret","data":{"password":"c2VjcmV0"},"metadata":{"annotations":{"kubectl.kubernetes.io/last-applied-configuration":"c2VjcmV0"}}});
        let b = json!({"kind":"Secret","data":{"password":"bmV3LXNlY3JldA=="},"metadata":{}});
        let result = prepare(a.clone(), b.clone(), Options::default()).unwrap();
        assert_eq!(result.changes.len(), 1);
        let change = &result.changes[0];
        assert_eq!(change.path, "data.password");
        assert_eq!(Change::text(&change.before), "Hidden Secret value");
        assert_ne!(change.before, change.after);
        assert!(!change.matches("c2VjcmV0"));
        let result = prepare(
            a,
            b,
            Options {
                reveal: true,
                ..Options::default()
            },
        )
        .unwrap();
        assert_eq!(Change::text(&result.changes[0].before), "c2VjcmV0");
        assert_eq!(Change::text(&result.changes[0].after), "bmV3LXNlY3JldA==");
    }
}
