//! Structured projections for the Overview: keep nested values readable and selectable.
use serde_json::Value;

pub(crate) type FieldRows = Vec<(String, Option<String>)>;
pub(crate) type Sections = Vec<(String, FieldRows)>;

pub(crate) fn rows(value: &Value) -> Vec<(String, Option<String>)> {
    fn visit(value: &Value, path: String, rows: &mut Vec<(String, Option<String>)>) {
        match value {
            Value::Object(fields) if !fields.is_empty() => {
                for (key, value) in fields {
                    visit(
                        value,
                        if path.is_empty() {
                            key.clone()
                        } else {
                            format!("{path} / {key}")
                        },
                        rows,
                    );
                }
            }
            Value::Array(values) if !values.is_empty() => {
                for (index, value) in values.iter().enumerate() {
                    // Names identify containers, conditions, ports and volume entries better than indices.
                    let name = value
                        .get("name")
                        .or_else(|| value.get("type"))
                        .and_then(Value::as_str);
                    let label = name
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("[{}]", index + 1));
                    visit(value, format!("{path} / {label}"), rows);
                }
            }
            Value::Null => rows.push((path, None)),
            Value::String(text) => rows.push((path, Some(text.clone()))),
            other => rows.push((path, Some(other.to_string()))),
        }
    }
    let mut result = Vec::new();
    if value.as_object().is_some_and(|fields| fields.is_empty()) {
        return result;
    }
    visit(value, String::new(), &mut result);
    result
}

pub(crate) fn pod_spec_path(group: &str, kind: &str) -> Option<&'static str> {
    match (group, kind) {
        ("", "Pod") => Some("/spec"),
        ("apps", "Deployment" | "ReplicaSet" | "StatefulSet" | "DaemonSet")
        | ("batch", "Job")
        | ("", "ReplicationController") => Some("/spec/template/spec"),
        ("batch", "CronJob") => Some("/spec/jobTemplate/spec/template/spec"),
        ("", "PodTemplate") => Some("/template/spec"),
        _ => None,
    }
}

pub(crate) fn sections(group: &str, kind: &str, data: &Value) -> Sections {
    let mut data = data.clone();
    // Containers get their own cards, so do not repeat them in the spec tree.
    if let Some(path) = pod_spec_path(group, kind)
        && let Some(Value::Object(spec)) = data.pointer_mut(path)
    {
        for key in ["containers", "initContainers", "ephemeralContainers"] {
            spec.remove(key);
        }
    }
    if group.is_empty()
        && kind == "Pod"
        && let Some(Value::Object(status)) = data.get_mut("status")
    {
        for key in [
            "containerStatuses",
            "initContainerStatuses",
            "ephemeralContainerStatuses",
        ] {
            status.remove(key);
        }
    }
    data.as_object()
        .into_iter()
        .flatten()
        // ConfigMap and Secret data remain in their dedicated Data page. Never reveal secret data here.
        .filter(|(key, _)| !matches!(key.as_str(), "data" | "binaryData" | "stringData"))
        .map(|(key, value)| {
            (
                match key.as_str() {
                    "spec" => "Spec".into(),
                    "status" => "Status".into(),
                    other => other.to_string(),
                },
                rows(value),
            )
        })
        .filter(|(_, rows)| !rows.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn nested_specs_keep_ports_resources_probes_and_empty_values() {
        let rows = rows(
            &json!({"ports":[{"name":"http","containerPort":8080}],"resources":{"requests":{"cpu":"100m"}}, "readinessProbe":{"httpGet":{"path":"/health"}}, "args":[],"tty":false}),
        );
        assert!(rows.contains(&("ports / http / containerPort".into(), Some("8080".into()))));
        assert!(rows.contains(&("resources / requests / cpu".into(), Some("100m".into()))));
        assert!(
            rows.iter()
                .any(|(key, _)| key == "readinessProbe / httpGet / path")
        );
        assert!(rows.contains(&("args".into(), Some("[]".into()))));
    }
    #[test]
    fn secrets_are_not_exposed_and_workload_containers_are_not_repeated() {
        let secret = sections(
            "",
            "Secret",
            &json!({"type":"Opaque","data":{"password":"encoded"}}),
        );
        assert_eq!(secret.len(), 1);
        let deployment = sections(
            "apps",
            "Deployment",
            &json!({"spec":{"replicas":2,"template":{"spec":{"containers":[{"name":"api"}],"nodeSelector":{"disk":"ssd"}}}}}),
        );
        assert!(
            deployment[0]
                .1
                .iter()
                .all(|(key, _)| !key.contains("containers"))
        );
        assert!(
            deployment[0]
                .1
                .iter()
                .any(|(key, _)| key.contains("nodeSelector"))
        );
    }
}
