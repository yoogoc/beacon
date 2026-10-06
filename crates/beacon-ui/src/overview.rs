//! Structured projections for the Overview: keep nested values readable and selectable.
use std::collections::BTreeSet;

use beacon_columns::{Timestamp, builtin::deployment_status, pod};
use beacon_kube::DynamicObject;
use serde_json::Value;

use crate::theme::Tone;

pub(crate) type FieldRows = Vec<(String, Option<String>)>;
pub(crate) type Sections = Vec<(String, FieldRows)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reference {
    pub group: &'static str,
    pub kind: &'static str,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Cell {
    pub value: Option<String>,
    pub reference: Option<Reference>,
}

impl Cell {
    pub(crate) fn text(value: impl Into<String>) -> Self {
        Self {
            value: Some(value.into()),
            reference: None,
        }
    }

    pub(crate) fn of(value: Option<&Value>) -> Self {
        Self {
            value: value.and_then(text),
            reference: None,
        }
    }

    pub(crate) fn link(mut self, group: &'static str, kind: &'static str) -> Self {
        self.reference = self
            .value
            .as_ref()
            .filter(|v| !v.is_empty())
            .map(|name| Reference {
                group,
                kind,
                name: name.clone(),
            });
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Table {
    pub headers: Vec<&'static str>,
    pub rows: Vec<Vec<Cell>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Group {
    pub title: String,
    pub fields: Vec<(String, Cell)>,
    pub table: Option<Table>,
    pub owner: bool,
    pub collapsed: bool,
}

impl Group {
    pub(crate) fn new(title: impl Into<String>, fields: Vec<(&str, Cell)>) -> Self {
        Self {
            title: title.into(),
            fields: fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            table: None,
            owner: false,
            collapsed: false,
        }
    }
}

pub(crate) struct Projection {
    pub groups: Vec<Group>,
    pub conditions: FieldRows,
    pub additional: Sections,
}

pub(crate) struct Summary {
    pub label: String,
    pub tone: Tone,
    pub health: bool,
    pub hints: Vec<String>,
    pub message: Option<String>,
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(v) if v.is_empty() => None,
        Value::String(v) => Some(v.clone()),
        Value::Array(v) => Some(v.iter().filter_map(text).collect::<Vec<_>>().join(", ")),
        v => Some(v.to_string()),
    }
}

fn at(data: &Value, path: &str) -> Cell {
    Cell::of(data.pointer(path))
}

fn defaulted(data: &Value, path: &str, default: &str) -> Cell {
    let cell = at(data, path);
    if cell.value.is_some() {
        cell
    } else {
        Cell::text(default)
    }
}

fn items<'a>(data: &'a Value, path: &str) -> impl Iterator<Item = &'a Value> {
    data.pointer(path)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn pairs(data: &Value, path: &str) -> Cell {
    Cell {
        value: data
            .pointer(path)
            .and_then(Value::as_object)
            .filter(|v| !v.is_empty())
            .map(|v| {
                v.iter()
                    .map(|(k, v)| format!("{k}={}", text(v).unwrap_or_default()))
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
        reference: None,
    }
}

/// Summary colours come from actual status, never the existence of an object.
pub(crate) fn summary(group: &str, kind: &str, object: &DynamicObject, now: Timestamp) -> Summary {
    let data = &object.data;
    let mut hints = Vec::new();
    let (label, health) = match (group, kind) {
        ("", "Pod") => {
            let pod = pod::summarize(&object.metadata, data, now);
            hints.extend([
                format!("{} ready", pod.ready),
                format!("{} restarts", pod.restarts),
            ]);
            (pod.status, true)
        }
        ("apps", "Deployment") => {
            hints.push(format!(
                "{}/{} ready",
                defaulted(data, "/status/readyReplicas", "0").value.unwrap(),
                defaulted(data, "/spec/replicas", "1").value.unwrap()
            ));
            (deployment_status(&object.metadata, data), true)
        }
        ("", "Node") => {
            let ready = items(data, "/status/conditions").find(|v| v["type"] == "Ready");
            let status = match ready.and_then(|v| v["status"].as_str()) {
                Some("True") => "Ready",
                Some("False") => "NotReady",
                _ => "Unknown",
            };
            if data.pointer("/spec/unschedulable").and_then(Value::as_bool) == Some(true) {
                hints.push("Scheduling disabled".into());
            }
            (status.into(), true)
        }
        ("", "PersistentVolumeClaim") => (
            at(data, "/status/phase")
                .value
                .unwrap_or_else(|| "Unknown".into()),
            true,
        ),
        ("", "Service") => (
            defaulted(data, "/spec/type", "ClusterIP").value.unwrap(),
            false,
        ),
        ("networking.k8s.io", "Ingress") => (
            at(data, "/spec/ingressClassName")
                .value
                .or_else(|| {
                    object
                        .metadata
                        .annotations
                        .as_ref()?
                        .get("kubernetes.io/ingress.class")
                        .cloned()
                })
                .unwrap_or_else(|| "Ingress".into()),
            false,
        ),
        ("", "Secret") => (defaulted(data, "/type", "Opaque").value.unwrap(), false),
        ("", "ConfigMap") => ("ConfigMap".into(), false),
        _ => (
            at(data, "/status/phase")
                .value
                .unwrap_or_else(|| kind.into()),
            false,
        ),
    };
    let tone = if health {
        crate::status::tone(&label)
    } else {
        Tone::Unknown
    };
    let message = at(data, "/status/message")
        .value
        .or_else(|| {
            items(data, "/status/conditions")
                .find(|v| {
                    v["status"] == "False"
                        && matches!(
                            v["type"].as_str(),
                            Some("Ready" | "Progressing" | "Available")
                        )
                        || v["status"] == "True"
                            && matches!(
                                v["type"].as_str(),
                                Some(
                                    "ReplicaFailure"
                                        | "MemoryPressure"
                                        | "DiskPressure"
                                        | "PIDPressure"
                                )
                            )
                })
                .and_then(|v| text(&v["message"]).or_else(|| text(&v["reason"])))
        })
        .or_else(|| {
            if (group, kind) != ("", "Pod") {
                return None;
            }
            [
                "initContainerStatuses",
                "containerStatuses",
                "ephemeralContainerStatuses",
            ]
            .into_iter()
            .flat_map(|key| data["status"][key].as_array().into_iter().flatten())
            .find_map(|container| {
                at(container, "/state/waiting/message")
                    .value
                    .or_else(|| at(container, "/state/terminated/message").value)
            })
        });
    Summary {
        label,
        tone,
        health,
        hints,
        message,
    }
}

/// Project common resources once per object update. The full safe field tree
/// stays available below the curated groups, including fields added by newer APIs.
pub(crate) fn project(group: &str, kind: &str, object: &DynamicObject) -> Projection {
    if beacon_kube::argo::rank(group, kind).is_some() {
        return crate::argo::project(kind, object);
    }
    let data = &object.data;
    let mut groups = Vec::new();
    match (group, kind) {
        ("", "Pod") => {
            let mut runtime = Group::new(
                "Runtime",
                vec![
                    ("Node", at(data, "/spec/nodeName").link("", "Node")),
                    (
                        "Pod IPs",
                        at(data, "/status/podIPs")
                            .value
                            .filter(|v| !v.is_empty())
                            .map_or_else(
                                || at(data, "/status/podIP"),
                                |_| {
                                    Cell::text(
                                        items(data, "/status/podIPs")
                                            .filter_map(|v| text(&v["ip"]))
                                            .collect::<Vec<_>>()
                                            .join(", "),
                                    )
                                },
                            ),
                    ),
                    ("Host IP", at(data, "/status/hostIP")),
                    ("QoS class", at(data, "/status/qosClass")),
                    (
                        "Service account",
                        defaulted(data, "/spec/serviceAccountName", "default")
                            .link("", "ServiceAccount"),
                    ),
                    (
                        "Restart policy",
                        defaulted(data, "/spec/restartPolicy", "Always"),
                    ),
                ],
            );
            runtime.owner = true;
            groups.push(runtime);
        }
        ("apps", "Deployment") => {
            let mut rollout =
                Group::new(
                    "Replicas & rollout",
                    vec![
                        ("Desired", defaulted(data, "/spec/replicas", "1")),
                        ("Ready", defaulted(data, "/status/readyReplicas", "0")),
                        ("Updated", defaulted(data, "/status/updatedReplicas", "0")),
                        (
                            "Available",
                            defaulted(data, "/status/availableReplicas", "0"),
                        ),
                        (
                            "Strategy",
                            defaulted(data, "/spec/strategy/type", "RollingUpdate"),
                        ),
                        (
                            "Revision",
                            Cell {
                                value: object.metadata.annotations.as_ref().and_then(|v| {
                                    v.get("deployment.kubernetes.io/revision").cloned()
                                }),
                                reference: None,
                            },
                        ),
                        (
                            "Progress deadline",
                            at(data, "/spec/progressDeadlineSeconds"),
                        ),
                        (
                            "Revision history limit",
                            at(data, "/spec/revisionHistoryLimit"),
                        ),
                    ],
                );
            if data.pointer("/spec/strategy/type").and_then(Value::as_str) != Some("Recreate") {
                rollout.fields.extend([
                    (
                        "Max surge".into(),
                        defaulted(data, "/spec/strategy/rollingUpdate/maxSurge", "25%"),
                    ),
                    (
                        "Max unavailable".into(),
                        defaulted(data, "/spec/strategy/rollingUpdate/maxUnavailable", "25%"),
                    ),
                ]);
            }
            rollout.owner = !object
                .metadata
                .owner_references
                .as_ref()
                .is_none_or(Vec::is_empty);
            groups.push(rollout);
            groups.push(Group::new(
                "Selector",
                vec![
                    ("Match labels", pairs(data, "/spec/selector/matchLabels")),
                    (
                        "Match expressions",
                        at(data, "/spec/selector/matchExpressions"),
                    ),
                ],
            ));
        }
        ("", "Service") => {
            groups.push(Group::new(
                "Networking",
                vec![
                    (
                        "Cluster IPs",
                        at(data, "/spec/clusterIPs")
                            .value
                            .filter(|v| !v.is_empty())
                            .map_or_else(|| at(data, "/spec/clusterIP"), Cell::text),
                    ),
                    ("IP families", at(data, "/spec/ipFamilies")),
                    ("IP family policy", at(data, "/spec/ipFamilyPolicy")),
                    ("External IPs", at(data, "/spec/externalIPs")),
                    ("Load balancer", addresses(data)),
                    ("External name", at(data, "/spec/externalName")),
                    (
                        "Session affinity",
                        defaulted(data, "/spec/sessionAffinity", "None"),
                    ),
                    (
                        "External traffic policy",
                        at(data, "/spec/externalTrafficPolicy"),
                    ),
                    (
                        "Internal traffic policy",
                        at(data, "/spec/internalTrafficPolicy"),
                    ),
                    (
                        "Publish not ready addresses",
                        defaulted(data, "/spec/publishNotReadyAddresses", "false"),
                    ),
                ],
            ));
            let mut ports = Group::new("Ports", vec![]);
            ports.table = Some(Table {
                headers: vec!["Name", "Port", "Target", "Node port"],
                rows: items(data, "/spec/ports")
                    .map(|v| {
                        vec![
                            at(v, "/name"),
                            Cell::text(format!(
                                "{} / {}",
                                at(v, "/port").value.unwrap_or_else(|| "<none>".into()),
                                defaulted(v, "/protocol", "TCP").value.unwrap()
                            )),
                            at(v, "/targetPort"),
                            at(v, "/nodePort"),
                        ]
                    })
                    .collect(),
            });
            groups.push(ports);
            groups.push(Group::new(
                "Selector",
                vec![("Labels", pairs(data, "/spec/selector"))],
            ));
        }
        ("networking.k8s.io", "Ingress") => {
            let mut class = at(data, "/spec/ingressClassName");
            if class.value.is_none() {
                class.value = object
                    .metadata
                    .annotations
                    .as_ref()
                    .and_then(|v| v.get("kubernetes.io/ingress.class").cloned());
            }
            groups.push(Group::new(
                "Routing",
                vec![
                    ("Class", class.link("networking.k8s.io", "IngressClass")),
                    ("Addresses", addresses(data)),
                ],
            ));
            for (index, rule) in items(data, "/spec/rules").enumerate() {
                let mut routes = Group::new(
                    format!(
                        "Rules · {}",
                        text(&rule["host"]).unwrap_or_else(|| "All hosts".into())
                    ),
                    vec![],
                );
                routes.table = Some(Table {
                    headers: vec!["Path", "Type", "Service", "Port"],
                    rows: items(rule, "/http/paths")
                        .map(|v| {
                            vec![
                                at(v, "/path"),
                                at(v, "/pathType"),
                                at(v, "/backend/service/name").link("", "Service"),
                                backend_port(&v["backend"]),
                            ]
                        })
                        .collect(),
                });
                // Preserve non-Service backends in the route fields as well.
                routes.fields.extend(
                    items(rule, "/http/paths")
                        .filter_map(|v| v.pointer("/backend/resource"))
                        .map(|v| {
                            (
                                format!("Resource backend · rule {}", index + 1),
                                Cell::of(Some(v)),
                            )
                        }),
                );
                groups.push(routes);
            }
            if let Some(backend) = data.pointer("/spec/defaultBackend") {
                groups.push(Group::new(
                    "Default backend",
                    vec![
                        ("Service", at(backend, "/service/name").link("", "Service")),
                        ("Port", backend_port(backend)),
                        ("Resource", at(backend, "/resource")),
                    ],
                ));
            }
            for (index, tls) in items(data, "/spec/tls").enumerate() {
                groups.push(Group::new(
                    format!("TLS · {}", index + 1),
                    vec![
                        ("Secret", at(tls, "/secretName").link("", "Secret")),
                        ("Hosts", at(tls, "/hosts")),
                    ],
                ));
            }
        }
        ("", "PersistentVolumeClaim") => groups.push(Group::new(
            "Storage",
            vec![
                ("Requested", at(data, "/spec/resources/requests/storage")),
                ("Capacity", at(data, "/status/capacity/storage")),
                ("Access modes", at(data, "/spec/accessModes")),
                (
                    "Volume mode",
                    defaulted(data, "/spec/volumeMode", "Filesystem"),
                ),
                (
                    "Storage class",
                    at(data, "/spec/storageClassName").link("storage.k8s.io", "StorageClass"),
                ),
                (
                    "Volume",
                    at(data, "/spec/volumeName").link("", "PersistentVolume"),
                ),
            ],
        )),
        ("", "ConfigMap" | "Secret") => {
            let secret = kind == "Secret";
            let entries = beacon_kube::data::read(object, secret);
            let mut config = Group::new(
                if secret { "Secret" } else { "Configuration" },
                vec![
                    ("Immutable", defaulted(data, "/immutable", "false")),
                    ("Keys", Cell::text(entries.len().to_string())),
                ],
            );
            if secret {
                config
                    .fields
                    .insert(0, ("Type".into(), defaulted(data, "/type", "Opaque")));
            }
            groups.push(config);
            let mut keys = Group::new("Data keys", vec![]);
            keys.table = Some(Table {
                headers: if secret {
                    vec!["Key"]
                } else {
                    vec!["Key", "Format", "Size"]
                },
                rows: entries
                    .iter()
                    .map(|v| {
                        let mut row = vec![Cell::text(v.key.clone())];
                        if !secret {
                            row.extend([
                                Cell::text(if v.field == beacon_kube::DataField::BinaryData {
                                    "Binary"
                                } else {
                                    "Text"
                                }),
                                Cell::text(format!("{} bytes", v.bytes)),
                            ]);
                        }
                        row
                    })
                    .collect(),
            });
            groups.push(keys);
        }
        ("", "Node") => {
            let mut capacity = Group::new("Capacity & allocatable", vec![]);
            capacity.table = Some(resource_table(
                &data["status"]["capacity"],
                &data["status"]["allocatable"],
                ["Resource", "Capacity", "Allocatable"],
            ));
            groups.push(capacity);
            let mut system = Group::new(
                "System",
                vec![
                    ("Kubelet", at(data, "/status/nodeInfo/kubeletVersion")),
                    (
                        "Container runtime",
                        at(data, "/status/nodeInfo/containerRuntimeVersion"),
                    ),
                    ("OS image", at(data, "/status/nodeInfo/osImage")),
                    (
                        "Operating system",
                        at(data, "/status/nodeInfo/operatingSystem"),
                    ),
                    ("Architecture", at(data, "/status/nodeInfo/architecture")),
                    ("Kernel", at(data, "/status/nodeInfo/kernelVersion")),
                    ("Provider ID", at(data, "/spec/providerID")),
                ],
            );
            system
                .fields
                .extend(items(data, "/status/addresses").map(|v| {
                    (
                        text(&v["type"]).unwrap_or_else(|| "Address".into()),
                        at(v, "/address"),
                    )
                }));
            if let Some(instance) = object
                .metadata
                .labels
                .as_ref()
                .and_then(|v| v.get("node.kubernetes.io/instance-type"))
            {
                system
                    .fields
                    .push(("Instance type".into(), Cell::text(instance.clone())));
            }
            groups.push(system);
        }
        _ => {
            let mut owner = Group::new("Relationships", vec![]);
            owner.owner = true;
            groups.push(owner);
        }
    }
    if object
        .metadata
        .owner_references
        .as_ref()
        .is_some_and(|v| !v.is_empty())
        && !groups.iter().any(|g| g.owner)
        && let Some(first) = groups.first_mut()
    {
        first.owner = true;
    }
    Projection {
        groups,
        conditions: data
            .pointer("/status/conditions")
            .map(rows)
            .unwrap_or_default(),
        additional: sections(group, kind, data),
    }
}

fn addresses(data: &Value) -> Cell {
    let addresses = items(data, "/status/loadBalancer/ingress")
        .filter_map(|v| text(&v["ip"]).or_else(|| text(&v["hostname"])))
        .collect::<Vec<_>>();
    Cell {
        value: (!addresses.is_empty()).then(|| addresses.join("\n")),
        reference: None,
    }
}

fn backend_port(backend: &Value) -> Cell {
    let value = backend
        .pointer("/service/port/name")
        .or_else(|| backend.pointer("/service/port/number"));
    Cell::of(value)
}

/// Union of the two maps, including extended resources such as GPUs.
pub(crate) fn resource_table(first: &Value, second: &Value, headers: [&'static str; 3]) -> Table {
    let keys: BTreeSet<_> = [first, second]
        .into_iter()
        .filter_map(Value::as_object)
        .flat_map(|v| v.keys())
        .collect();
    let mut keys: Vec<_> = keys.into_iter().collect();
    keys.sort_by_key(|key| {
        (
            match key.as_str() {
                "cpu" => 0,
                "memory" => 1,
                "pods" => 2,
                "ephemeral-storage" => 3,
                _ => 4,
            },
            key.as_str(),
        )
    });
    Table {
        headers: headers.into(),
        rows: keys
            .into_iter()
            .map(|key| {
                vec![
                    Cell::text(
                        match key.as_str() {
                            "cpu" => "CPU",
                            "memory" => "Memory",
                            "pods" => "Pods",
                            "ephemeral-storage" => "Ephemeral storage",
                            other => other,
                        }
                        .to_string(),
                    ),
                    Cell::of(first.get(key)),
                    Cell::of(second.get(key)),
                ]
            })
            .collect(),
    }
}

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
        .filter(|(key, _)| {
            !(group.is_empty()
                && matches!(kind, "ConfigMap" | "Secret")
                && matches!(key.as_str(), "data" | "binaryData" | "stringData"))
        })
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
    fn object(data: Value) -> DynamicObject {
        let mut object: DynamicObject = serde_json::from_value(json!({"apiVersion":"v1", "kind":"Pod", "metadata":{"name":"example", "namespace":"default"}})).unwrap();
        object.data = data;
        object
    }

    fn value<'a>(projection: &'a Projection, title: &str, label: &str) -> &'a Cell {
        &projection
            .groups
            .iter()
            .find(|g| g.title == title)
            .unwrap()
            .fields
            .iter()
            .find(|(k, _)| k == label)
            .unwrap()
            .1
    }

    #[test]
    fn pod_summary_reports_waiting_reason_and_runtime_references() {
        let object = object(
            json!({"spec":{"nodeName":"worker", "containers":[{"name":"api"}]}, "status":{"phase":"Running", "podIPs":[{"ip":"10.0.0.1"},{"ip":"::1"}], "containerStatuses":[{"name":"api", "ready":false,"restartCount":3,"state":{"waiting":{"reason":"CrashLoopBackOff","message":"Back-off restarting api"}}}]}}),
        );
        let summary = summary("", "Pod", &object, Timestamp::now());
        assert_eq!(summary.label, "CrashLoopBackOff");
        assert_eq!(summary.tone, Tone::Critical);
        assert_eq!(summary.message.as_deref(), Some("Back-off restarting api"));
        assert!(summary.health);
        let projection = project("", "Pod", &object);
        assert_eq!(
            value(&projection, "Runtime", "Pod IPs").value.as_deref(),
            Some("10.0.0.1, ::1")
        );
        assert_eq!(
            value(&projection, "Runtime", "Node")
                .reference
                .as_ref()
                .unwrap()
                .kind,
            "Node"
        );
        assert_eq!(
            value(&projection, "Runtime", "Service account")
                .value
                .as_deref(),
            Some("default")
        );
        assert!(projection.groups[0].owner);
    }

    #[test]
    fn deployment_defaults_and_recreate_rollouts_are_distinct() {
        let object = object(
            json!({"spec":{"strategy":{"type":"Recreate"}}, "status":{"conditions":[{"type":"Progressing","status":"False","message":"Deadline exceeded"}]}}),
        );
        let projection = project("apps", "Deployment", &object);
        assert_eq!(
            value(&projection, "Replicas & rollout", "Desired")
                .value
                .as_deref(),
            Some("1")
        );
        assert_eq!(
            value(&projection, "Replicas & rollout", "Ready")
                .value
                .as_deref(),
            Some("0")
        );
        assert!(
            !projection.groups[0]
                .fields
                .iter()
                .any(|(k, _)| k == "Max surge")
        );
        let summary = summary("apps", "Deployment", &object, Timestamp::now());
        assert_eq!(summary.label, "Failed");
        assert_eq!(summary.message.as_deref(), Some("Deadline exceeded"));
    }

    #[test]
    fn networking_summaries_never_invent_health() {
        let service = object(
            json!({"spec":{"type":"ExternalName","externalName":"database.example","ports":[{"port":443,"targetPort":"https"}]}}),
        );
        let summary = summary("", "Service", &service, Timestamp::now());
        assert_eq!(summary.label, "ExternalName");
        assert!(!summary.health);
        assert_eq!(summary.tone, Tone::Unknown);
        let projection = project("", "Service", &service);
        assert_eq!(
            value(&projection, "Networking", "External name")
                .value
                .as_deref(),
            Some("database.example")
        );
        assert_eq!(
            projection.groups[1].table.as_ref().unwrap().rows[0][2]
                .value
                .as_deref(),
            Some("https")
        );
    }

    #[test]
    fn ingress_keeps_named_ports_resource_backends_and_tls_links() {
        let ingress = object(
            json!({"spec":{"rules":[{"host":"example.com","http":{"paths":[{"path":"/", "pathType":"Prefix","backend":{"service":{"name":"api","port":{"name":"http"}}}}, {"path":"/assets","backend":{"resource":{"apiGroup":"example.com","kind":"Bucket","name":"assets"}}}]}}], "tls":[{"secretName":"api-tls","hosts":["example.com"]}]}, "status":{"loadBalancer":{"ingress":[{"hostname":"lb.example.com"}]}}}),
        );
        let projection = project("networking.k8s.io", "Ingress", &ingress);
        let routes = projection.groups[1].table.as_ref().unwrap();
        assert_eq!(routes.rows[0][2].reference.as_ref().unwrap().name, "api");
        assert_eq!(routes.rows[0][3].value.as_deref(), Some("http"));
        assert!(
            projection.groups[1].fields[0]
                .1
                .value
                .as_ref()
                .unwrap()
                .contains("Bucket")
        );
        assert_eq!(
            value(&projection, "TLS · 1", "Secret")
                .reference
                .as_ref()
                .unwrap()
                .kind,
            "Secret"
        );
        assert_eq!(
            value(&projection, "Routing", "Addresses").value.as_deref(),
            Some("lb.example.com")
        );
    }

    #[test]
    fn pvc_capacity_and_volume_references_keep_the_reported_values() {
        let pvc = object(
            json!({"spec":{"resources":{"requests":{"storage":"10Gi"}},"accessModes":["ReadWriteOnce"],"storageClassName":"fast","volumeName":"pv-1"},"status":{"phase":"Bound","capacity":{"storage":"12Gi"}}}),
        );
        let projection = project("", "PersistentVolumeClaim", &pvc);
        assert_eq!(
            value(&projection, "Storage", "Requested").value.as_deref(),
            Some("10Gi")
        );
        assert_eq!(
            value(&projection, "Storage", "Capacity").value.as_deref(),
            Some("12Gi")
        );
        assert_eq!(
            value(&projection, "Storage", "Volume")
                .reference
                .as_ref()
                .unwrap()
                .kind,
            "PersistentVolume"
        );
        assert_eq!(
            value(&projection, "Storage", "Volume mode")
                .value
                .as_deref(),
            Some("Filesystem")
        );
    }

    #[test]
    fn data_key_tables_do_not_reveal_values_and_sizes_are_decoded() {
        let config = object(json!({"data":{"config":"你好"},"binaryData":{"blob":"AQID"}}));
        let projection = project("", "ConfigMap", &config);
        let keys = projection.groups[1].table.as_ref().unwrap();
        assert_eq!(keys.rows[0][2].value.as_deref(), Some("3 bytes"));
        assert_eq!(keys.rows[1][2].value.as_deref(), Some("6 bytes"));
        assert!(!format!("{:?}", projection.groups).contains("你好"));
        let secret = object(
            json!({"type":"kubernetes.io/tls","data":{"tls.key":"UFJJVkFURSBTRUNSRVQ=","tls.crt":"Q0VSVA=="}, "stringData":{"password":"sensitive"}}),
        );
        let projection = project("", "Secret", &secret);
        assert_eq!(
            projection.groups[1].table.as_ref().unwrap().headers,
            ["Key"]
        );
        let rendered = format!("{:?}{:?}", projection.groups, projection.additional);
        for private in [
            "UFJJVkFURSBTRUNSRVQ=",
            "PRIVATE SECRET",
            "sensitive",
            "Q0VSVA==",
        ] {
            assert!(!rendered.contains(private));
        }
    }

    #[test]
    fn extended_resources_and_unknown_api_fields_remain_accessible() {
        let table = resource_table(
            &json!({"cpu":"100m","nvidia.com/gpu":"1"}),
            &json!({"memory":"1Gi","nvidia.com/gpu":"2"}),
            ["Resource", "Requests", "Limits"],
        );
        assert_eq!(table.rows.len(), 3);
        assert_eq!(table.rows[2][0].value.as_deref(), Some("nvidia.com/gpu"));
        assert_eq!(table.rows[2][2].value.as_deref(), Some("2"));
        let custom = object(json!({"data":{"userField":"visible"}, "spec":{"futureField":42}}));
        let projection = project("example.com", "Pod", &custom);
        assert_eq!(projection.groups[0].title, "Relationships");
        assert!(
            projection
                .additional
                .iter()
                .any(|(k, fields)| k == "data" && fields[0].1.as_deref() == Some("visible"))
        );
        assert!(
            projection
                .additional
                .iter()
                .any(|(_, fields)| fields.iter().any(|(k, _)| k == "futureField"))
        );
    }

    #[test]
    fn curated_resources_keep_owner_navigation_and_legacy_ingress_classes() {
        let mut owned = object(json!({}));
        owned.metadata.owner_references = Some(serde_json::from_value(json!([{"apiVersion":"apps/v1","kind":"Deployment","name":"parent","uid":"parent-id"}])).unwrap());
        for (group, kind) in [
            ("", "ConfigMap"),
            ("", "Secret"),
            ("", "Service"),
            ("", "Node"),
            ("", "PersistentVolumeClaim"),
            ("networking.k8s.io", "Ingress"),
        ] {
            assert!(
                project(group, kind, &owned).groups.iter().any(|g| g.owner),
                "{kind} must retain its owner"
            );
        }
        owned.metadata.annotations =
            Some([("kubernetes.io/ingress.class".into(), "legacy-nginx".into())].into());
        let ingress = project("networking.k8s.io", "Ingress", &owned);
        assert_eq!(
            value(&ingress, "Routing", "Class").value.as_deref(),
            Some("legacy-nginx")
        );
        assert_eq!(
            summary("networking.k8s.io", "Ingress", &owned, Timestamp::now()).label,
            "legacy-nginx"
        );
    }

    #[test]
    fn node_pressure_is_explained_and_capacity_includes_extended_resources() {
        let node = object(
            json!({"spec":{"unschedulable":true},"status":{"conditions":[{"type":"Ready","status":"True"},{"type":"MemoryPressure","status":"True","message":"Low available memory"}],"capacity":{"cpu":"8","example.com/device":"4"},"allocatable":{"cpu":"7"},"addresses":[{"type":"InternalIP","address":"10.0.0.2"}]}}),
        );
        let summary = summary("", "Node", &node, Timestamp::now());
        assert_eq!(summary.label, "Ready");
        assert_eq!(summary.message.as_deref(), Some("Low available memory"));
        assert_eq!(summary.hints, ["Scheduling disabled"]);
        let projection = project("", "Node", &node);
        assert_eq!(projection.groups[0].table.as_ref().unwrap().rows.len(), 2);
        assert_eq!(
            value(&projection, "System", "InternalIP").value.as_deref(),
            Some("10.0.0.2")
        );
    }

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
