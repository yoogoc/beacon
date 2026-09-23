//! kubectl's own columns, for the kinds everybody looks at.
//!
//! A CRD describes its own table; a built-in resource does not. The API server
//! knows how to print one, but only through the `Table` content type, which has
//! no watch -- and Beacon is built on watches. So the tables kubectl has
//! compiled in are ported here, from
//! `k8s.io/kubernetes/pkg/printers/internalversion`.
//!
//! Accuracy is the whole point. These columns are read next to a terminal, and
//! a Service whose `PORT(S)` disagrees with `kubectl get svc` is a bug report,
//! not a style difference. The cases that look like details -- a Node that is
//! `Ready,SchedulingDisabled`, a Service with no external IP, a PVC's access
//! modes as `RWO` -- are exactly the ones that make a column look wrong.
//!
//! Anything not here falls through to a CRD's own columns, and then to
//! Name / Namespace / Age.

use serde_json::Value;

use crate::{Cell, CellValue, ColumnDef, ColumnSet, ColumnSource, ColumnWidth, pod};

/// kubectl's columns for a resource we know by name.
///
/// Keyed on group as well as kind: a `Pod` in a third-party API group is
/// somebody else's resource that happens to share the name.
pub fn column_set(group: &str, kind: &str, namespaced: bool) -> Option<ColumnSet> {
    let columns = match (group, kind) {
        ("", "Pod") => pod_columns(),
        ("", "Service") => service(),
        ("", "Node") => node(),
        ("", "Namespace") => namespace(),
        ("", "ConfigMap") => config_map(),
        ("", "Secret") => secret(),
        ("", "PersistentVolumeClaim") => persistent_volume_claim(),
        ("", "ServiceAccount") => service_account(),
        ("apps", "Deployment") => deployment(),
        ("apps", "StatefulSet") => stateful_set(),
        ("apps", "DaemonSet") => daemon_set(),
        ("apps", "ReplicaSet") => replica_set(),
        ("batch", "Job") => job(),
        ("batch", "CronJob") => cron_job(),
        ("networking.k8s.io", "Ingress") => ingress(),
        ("rbac.authorization.k8s.io", "RoleBinding" | "ClusterRoleBinding") => role_binding(),
        _ => return None,
    };

    Some(assemble(namespaced, columns))
}

/// Name, then Namespace when the list spans namespaces, then the kind's own
/// columns, then Age. That shape is kubectl's, and it is what makes a column
/// set recognisable before it is read.
///
/// Age is appended unless the kind placed one itself. `Node` is the one table
/// where kubectl does not put it last -- it sits before `Version` -- and a
/// kind that says where its Age goes is taken at its word.
fn assemble(namespaced: bool, rest: Vec<ColumnDef>) -> ColumnSet {
    let mut columns = vec![ColumnDef::new(
        "Name",
        ColumnWidth::Flex(3.0),
        ColumnSource::Name,
    )];

    if namespaced {
        columns.push(ColumnDef::new(
            "Namespace",
            ColumnWidth::Flex(1.5),
            ColumnSource::Namespace,
        ));
    }

    let places_its_own_age = rest
        .iter()
        .any(|column| matches!(column.source, ColumnSource::Age));

    columns.extend(rest);
    if !places_its_own_age {
        columns.push(age());
    }

    ColumnSet { columns }
}

fn age() -> ColumnDef {
    ColumnDef::new("Age", ColumnWidth::Fixed(72.0), ColumnSource::Age)
}

fn computed(header: &str, width: f32, compute: fn(&Cell<'_>) -> CellValue) -> ColumnDef {
    ColumnDef::new(
        header,
        ColumnWidth::Fixed(width),
        ColumnSource::Computed(compute),
    )
}

fn flexible(header: &str, share: f32, compute: fn(&Cell<'_>) -> CellValue) -> ColumnDef {
    ColumnDef::new(
        header,
        ColumnWidth::Flex(share),
        ColumnSource::Computed(compute),
    )
}

// MARK: workloads

fn pod_columns() -> Vec<ColumnDef> {
    // The three Pod columns are computed together; see `pod::summarize`.
    vec![
        computed("Ready", 68.0, |cell| {
            CellValue::text(pod::summarize(cell.metadata, cell.data, cell.now).ready)
        }),
        computed("Status", 170.0, |cell| {
            CellValue::text(pod::summarize(cell.metadata, cell.data, cell.now).status)
        }),
        computed("Restarts", 120.0, |cell| {
            CellValue::text(pod::summarize(cell.metadata, cell.data, cell.now).restarts)
        }),
    ]
}

fn deployment() -> Vec<ColumnDef> {
    vec![
        computed("Ready", 72.0, |cell| {
            ratio(
                number(cell.data, &["status", "readyReplicas"]),
                desired(cell),
            )
        }),
        computed("Up-to-date", 96.0, |cell| {
            count(cell.data, &["status", "updatedReplicas"])
        }),
        computed("Available", 88.0, |cell| {
            count(cell.data, &["status", "availableReplicas"])
        }),
    ]
}

fn stateful_set() -> Vec<ColumnDef> {
    vec![computed("Ready", 72.0, |cell| {
        ratio(
            number(cell.data, &["status", "readyReplicas"]),
            desired(cell),
        )
    })]
}

fn replica_set() -> Vec<ColumnDef> {
    vec![
        computed("Desired", 76.0, |cell| {
            CellValue::text(desired(cell).to_string())
        }),
        computed("Current", 76.0, |cell| {
            count(cell.data, &["status", "replicas"])
        }),
        computed("Ready", 68.0, |cell| {
            count(cell.data, &["status", "readyReplicas"])
        }),
    ]
}

fn daemon_set() -> Vec<ColumnDef> {
    vec![
        computed("Desired", 76.0, |cell| {
            count(cell.data, &["status", "desiredNumberScheduled"])
        }),
        computed("Current", 76.0, |cell| {
            count(cell.data, &["status", "currentNumberScheduled"])
        }),
        computed("Ready", 68.0, |cell| {
            count(cell.data, &["status", "numberReady"])
        }),
        computed("Up-to-date", 96.0, |cell| {
            count(cell.data, &["status", "updatedNumberScheduled"])
        }),
        computed("Available", 88.0, |cell| {
            count(cell.data, &["status", "numberAvailable"])
        }),
        flexible("Node Selector", 1.0, |cell| {
            match at(cell.data, &["spec", "template", "spec", "nodeSelector"]) {
                Some(Value::Object(selector)) if !selector.is_empty() => {
                    CellValue::text(join(selector.iter().map(|(k, v)| match v {
                        Value::String(text) => format!("{k}={text}"),
                        other => format!("{k}={other}"),
                    })))
                }
                _ => CellValue::Missing,
            }
        }),
    ]
}

fn job() -> Vec<ColumnDef> {
    vec![
        computed("Status", 104.0, |cell| {
            // kubectl reads the terminal conditions first, then falls back to
            // whether anything is running. A suspended job is neither.
            let conditions = conditions(cell.data);
            for (kind, status) in &conditions {
                if status == "True" && (kind == "Complete" || kind == "Failed") {
                    return CellValue::text(kind.clone());
                }
            }
            if at(cell.data, &["spec", "suspend"]) == Some(&Value::Bool(true)) {
                return CellValue::text("Suspended");
            }
            CellValue::text("Running")
        }),
        computed("Completions", 104.0, |cell| {
            let succeeded = number(cell.data, &["status", "succeeded"]);
            match number(cell.data, &["spec", "completions"]) {
                // A job with no `completions` runs until one pod succeeds, and
                // kubectl prints the count alone rather than a ratio.
                None => CellValue::text(format!("{}/1", succeeded.unwrap_or(0))),
                Some(completions) => ratio(succeeded, completions),
            }
        }),
        computed("Duration", 88.0, |cell| {
            let Some(started) = timestamp(cell.data, &["status", "startTime"]) else {
                return CellValue::Missing;
            };
            let finished = timestamp(cell.data, &["status", "completionTime"]).unwrap_or(cell.now);
            CellValue::text(crate::format_duration(
                finished.duration_since(started).as_secs(),
            ))
        }),
    ]
}

fn cron_job() -> Vec<ColumnDef> {
    vec![
        flexible("Schedule", 1.0, |cell| {
            string(cell.data, &["spec", "schedule"]).into()
        }),
        computed("Suspend", 80.0, |cell| {
            match at(cell.data, &["spec", "suspend"]) {
                Some(Value::Bool(value)) => CellValue::text(value.to_string()),
                _ => CellValue::text("False"),
            }
        }),
        computed("Active", 72.0, |cell| {
            match at(cell.data, &["status", "active"]) {
                Some(Value::Array(active)) => CellValue::text(active.len().to_string()),
                _ => CellValue::text("0"),
            }
        }),
        computed("Last Schedule", 104.0, |cell| {
            match timestamp(cell.data, &["status", "lastScheduleTime"]) {
                Some(last) => CellValue::text(crate::format_duration(
                    cell.now.duration_since(last).as_secs(),
                )),
                None => CellValue::Missing,
            }
        }),
    ]
}

// MARK: networking

fn service() -> Vec<ColumnDef> {
    vec![
        computed("Type", 112.0, |cell| {
            string(cell.data, &["spec", "type"]).into()
        }),
        computed("Cluster-IP", 120.0, |cell| {
            string(cell.data, &["spec", "clusterIP"]).into()
        }),
        flexible("External-IP", 1.0, |cell| external_ips(cell.data)),
        flexible("Port(s)", 1.2, |cell| service_ports(cell.data)),
    ]
}

/// What kubectl's `getServiceExternalIP` produces.
///
/// The four service types answer this from four different places, and an
/// external IP is the field people are actually looking at when they run
/// `kubectl get svc`.
fn external_ips(data: &Value) -> CellValue {
    let declared = strings(at(data, &["spec", "externalIPs"]));

    match string(data, &["spec", "type"]) {
        Some("ExternalName") => string(data, &["spec", "externalName"]).into(),
        Some("LoadBalancer") => {
            let mut addresses: Vec<String> = Vec::new();
            if let Some(Value::Array(ingress)) = at(data, &["status", "loadBalancer", "ingress"]) {
                addresses.extend(ingress.iter().filter_map(|entry| {
                    entry
                        .get("ip")
                        .or_else(|| entry.get("hostname"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }));
            }
            addresses.extend(declared);
            if addresses.is_empty() {
                // A load balancer that has not been given an address yet.
                CellValue::text("<pending>")
            } else {
                CellValue::text(join(addresses))
            }
        }
        // ClusterIP and NodePort have nothing but whatever was declared.
        _ if declared.is_empty() => CellValue::Missing,
        _ => CellValue::text(join(declared)),
    }
}

/// `80/TCP`, or `80:31234/TCP` once a node port is allocated.
fn service_ports(data: &Value) -> CellValue {
    let Some(Value::Array(ports)) = at(data, &["spec", "ports"]) else {
        return CellValue::Missing;
    };

    let rendered = ports.iter().filter_map(|port| {
        let number = port.get("port")?.as_i64()?;
        let protocol = port
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or("TCP");
        Some(match port.get("nodePort").and_then(Value::as_i64) {
            Some(node_port) => format!("{number}:{node_port}/{protocol}"),
            None => format!("{number}/{protocol}"),
        })
    });

    let rendered: Vec<_> = rendered.collect();
    if rendered.is_empty() {
        CellValue::Missing
    } else {
        CellValue::text(join(rendered))
    }
}

fn ingress() -> Vec<ColumnDef> {
    vec![
        computed("Class", 104.0, |cell| {
            string(cell.data, &["spec", "ingressClassName"]).into()
        }),
        flexible("Hosts", 1.2, |cell| {
            let Some(Value::Array(rules)) = at(cell.data, &["spec", "rules"]) else {
                return CellValue::Missing;
            };
            let hosts: Vec<String> = rules
                .iter()
                .map(|rule| {
                    rule.get("host")
                        .and_then(Value::as_str)
                        // A rule with no host matches everything, which kubectl
                        // prints as `*`.
                        .unwrap_or("*")
                        .to_string()
                })
                .collect();
            if hosts.is_empty() {
                CellValue::Missing
            } else {
                CellValue::text(join(hosts))
            }
        }),
        flexible("Address", 1.0, |cell| {
            let Some(Value::Array(ingress)) = at(cell.data, &["status", "loadBalancer", "ingress"])
            else {
                return CellValue::Missing;
            };
            let addresses: Vec<String> = ingress
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("ip")
                        .or_else(|| entry.get("hostname"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect();
            if addresses.is_empty() {
                CellValue::Missing
            } else {
                CellValue::text(join(addresses))
            }
        }),
        computed("Ports", 80.0, |cell| {
            // kubectl does not read the backends: an Ingress serves 80, and 443
            // as well once it has any TLS configuration.
            match at(cell.data, &["spec", "tls"]) {
                Some(Value::Array(tls)) if !tls.is_empty() => CellValue::text("80, 443"),
                _ => CellValue::text("80"),
            }
        }),
    ]
}

// MARK: cluster and config

fn node() -> Vec<ColumnDef> {
    vec![
        computed("Status", 152.0, |cell| node_status(cell.data)),
        flexible("Roles", 1.0, node_roles),
        // kubectl puts Age before Version here, and only here.
        age(),
        computed("Version", 120.0, |cell| {
            string(cell.data, &["status", "nodeInfo", "kubeletVersion"]).into()
        }),
    ]
}

/// `Ready`, `NotReady`, or either of those with `,SchedulingDisabled`.
///
/// A cordoned node is the case that matters: it is still Ready, and a column
/// that only says `Ready` hides the reason nothing is being scheduled on it.
fn node_status(data: &Value) -> CellValue {
    let ready = conditions(data)
        .into_iter()
        .find(|(kind, _)| kind == "Ready")
        .map(|(_, status)| status);

    let mut status = match ready.as_deref() {
        Some("True") => "Ready".to_string(),
        Some("False") => "NotReady".to_string(),
        _ => "Unknown".to_string(),
    };

    if at(data, &["spec", "unschedulable"]) == Some(&Value::Bool(true)) {
        status.push_str(",SchedulingDisabled");
    }

    CellValue::text(status)
}

/// The roles a node advertises, which live in its labels rather than anywhere
/// more obvious.
fn node_roles(cell: &Cell<'_>) -> CellValue {
    const PREFIX: &str = "node-role.kubernetes.io/";
    const LEGACY: &str = "kubernetes.io/role";

    let Some(labels) = &cell.metadata.labels else {
        return CellValue::Missing;
    };

    let mut roles: Vec<&str> = labels
        .iter()
        .filter_map(|(key, value)| match key.strip_prefix(PREFIX) {
            Some(role) if !role.is_empty() => Some(role),
            _ if key == LEGACY && !value.is_empty() => Some(value.as_str()),
            _ => None,
        })
        .collect();

    if roles.is_empty() {
        return CellValue::Missing;
    }
    roles.sort_unstable();
    CellValue::text(join(roles))
}

/// A binding's whole point is what it binds to, and kubectl writes that as
/// `Role/name` -- the kind matters, because a RoleBinding may point at a
/// ClusterRole.
fn role_binding() -> Vec<ColumnDef> {
    vec![flexible("Role", 2.0, |cell| {
        let Some(role) = at(cell.data, &["roleRef"]) else {
            return CellValue::Missing;
        };
        match (
            role.get("kind").and_then(Value::as_str),
            role.get("name").and_then(Value::as_str),
        ) {
            (Some(kind), Some(name)) => CellValue::text(format!("{kind}/{name}")),
            _ => CellValue::Missing,
        }
    })]
}

fn namespace() -> Vec<ColumnDef> {
    vec![computed("Status", 96.0, |cell| {
        string(cell.data, &["status", "phase"]).into()
    })]
}

fn config_map() -> Vec<ColumnDef> {
    vec![computed("Data", 68.0, |cell| {
        // Both maps count, which is how a ConfigMap holding a binary blob shows
        // as non-empty.
        let entries = entries(cell.data, "data") + entries(cell.data, "binaryData");
        CellValue::text(entries.to_string())
    })]
}

fn secret() -> Vec<ColumnDef> {
    vec![
        flexible("Type", 1.2, |cell| string(cell.data, &["type"]).into()),
        computed("Data", 68.0, |cell| {
            CellValue::text(entries(cell.data, "data").to_string())
        }),
    ]
}

fn service_account() -> Vec<ColumnDef> {
    vec![computed("Secrets", 80.0, |cell| {
        match at(cell.data, &["secrets"]) {
            Some(Value::Array(secrets)) => CellValue::text(secrets.len().to_string()),
            _ => CellValue::text("0"),
        }
    })]
}

fn persistent_volume_claim() -> Vec<ColumnDef> {
    vec![
        computed("Status", 96.0, |cell| {
            string(cell.data, &["status", "phase"]).into()
        }),
        flexible("Volume", 1.5, |cell| {
            string(cell.data, &["spec", "volumeName"]).into()
        }),
        computed("Capacity", 88.0, |cell| {
            string(cell.data, &["status", "capacity", "storage"]).into()
        }),
        computed("Access Modes", 112.0, |cell| {
            let modes = strings(at(cell.data, &["status", "accessModes"]));
            if modes.is_empty() {
                return CellValue::Missing;
            }
            CellValue::Text(modes.iter().map(|mode| access_mode(mode)).collect())
        }),
        flexible("Storageclass", 1.0, |cell| {
            string(cell.data, &["spec", "storageClassName"]).into()
        }),
        computed("VolumeAttributesClass", 168.0, |cell| {
            // kubectl's placeholder here is `<unset>`, not the `<none>` it uses
            // everywhere else: the field is recent, and unset is its ordinary
            // state rather than something missing.
            match string(cell.data, &["status", "currentVolumeAttributesClassName"])
                .or_else(|| string(cell.data, &["spec", "volumeAttributesClassName"]))
            {
                Some(class) => CellValue::text(class),
                None => CellValue::text("<unset>"),
            }
        }),
    ]
}

/// The abbreviations kubectl prints, concatenated with no separator -- a volume
/// that is both is `RWORWX`.
fn access_mode(mode: &str) -> &'static str {
    match mode {
        "ReadWriteOnce" => "RWO",
        "ReadOnlyMany" => "ROX",
        "ReadWriteMany" => "RWX",
        "ReadWriteOncePod" => "RWOP",
        _ => "",
    }
}

// MARK: reading the object

/// Walks a path of object keys.
///
/// Built-in columns use this rather than [`crate::path`]: the paths are known
/// at compile time, this runs for every visible cell, and there is nothing to
/// parse.
fn at<'a>(data: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(data, |node, key| node.get(key))
}

fn string<'a>(data: &'a Value, path: &[&str]) -> Option<&'a str> {
    at(data, path)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

fn number(data: &Value, path: &[&str]) -> Option<i64> {
    at(data, path).and_then(Value::as_i64)
}

/// A replica count that is absent means zero, not unknown -- the status
/// subresource omits counters that are at zero.
fn count(data: &Value, path: &[&str]) -> CellValue {
    CellValue::text(number(data, path).unwrap_or(0).to_string())
}

/// `spec.replicas`, which defaults to 1 when it is not set.
fn desired(cell: &Cell<'_>) -> i64 {
    number(cell.data, &["spec", "replicas"]).unwrap_or(1)
}

fn ratio(have: Option<i64>, want: i64) -> CellValue {
    CellValue::text(format!("{}/{want}", have.unwrap_or(0)))
}

fn entries(data: &Value, field: &str) -> usize {
    match data.get(field) {
        Some(Value::Object(map)) => map.len(),
        _ => 0,
    }
}

fn strings(node: Option<&Value>) -> Vec<String> {
    match node {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn timestamp(data: &Value, path: &[&str]) -> Option<crate::Timestamp> {
    string(data, path)?.parse().ok()
}

/// `status.conditions` as `(type, status)` pairs.
fn conditions(data: &Value) -> Vec<(String, String)> {
    let Some(Value::Array(conditions)) = at(data, &["status", "conditions"]) else {
        return Vec::new();
    };

    conditions
        .iter()
        .filter_map(|condition| {
            Some((
                condition.get("type")?.as_str()?.to_string(),
                condition.get("status")?.as_str()?.to_string(),
            ))
        })
        .collect()
}

/// kubectl's separator for a list inside one cell.
fn join(values: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    values
        .into_iter()
        .map(|value| value.as_ref().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Timestamp;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn now() -> Timestamp {
        "2026-09-23T12:00:00Z".parse().expect("fixed clock")
    }

    /// Every cell of one object, in column order, the way the table renders it.
    fn row(group: &str, kind: &str, data: Value) -> Vec<String> {
        row_with(group, kind, ObjectMeta::default(), data)
    }

    fn row_with(group: &str, kind: &str, metadata: ObjectMeta, data: Value) -> Vec<String> {
        let columns = column_set(group, kind, false).expect("a built-in table");
        let cell = Cell {
            metadata: &metadata,
            data: &data,
            now: now(),
        };
        columns
            .columns
            .iter()
            // Name and Age come from metadata the fixtures do not set.
            .filter(|column| !matches!(column.source, ColumnSource::Name | ColumnSource::Age))
            .map(|column| column.resolve(&cell).display().to_string())
            .collect()
    }

    fn headers(group: &str, kind: &str, namespaced: bool) -> Vec<String> {
        column_set(group, kind, namespaced)
            .expect("a built-in table")
            .headers()
            .iter()
            .map(|header| header.to_string())
            .collect()
    }

    #[test]
    fn every_table_starts_with_name_and_ends_with_age() {
        for (group, kind) in [
            ("", "Pod"),
            ("", "Service"),
            ("apps", "Deployment"),
            ("batch", "Job"),
            ("networking.k8s.io", "Ingress"),
        ] {
            let headers = headers(group, kind, true);
            assert_eq!(headers.first().map(String::as_str), Some("Name"), "{kind}");
            assert_eq!(
                headers.get(1).map(String::as_str),
                Some("Namespace"),
                "{kind}"
            );
            assert_eq!(headers.last().map(String::as_str), Some("Age"), "{kind}");
        }
    }

    /// The exception, and the reason `assemble` does not simply append: a Node
    /// table reads `STATUS ROLES AGE VERSION`.
    #[test]
    fn node_puts_age_before_version() {
        assert_eq!(
            headers("", "Node", false),
            ["Name", "Status", "Roles", "Age", "Version"]
        );
    }

    /// A cluster-scoped kind has no Namespace column even when one is asked for
    /// -- the caller decides, and Node is never namespaced.
    #[test]
    fn a_kind_we_do_not_know_has_no_built_in_table() {
        assert!(column_set("example.com", "Widget", true).is_none());
        assert!(column_set("example.com", "Pod", true).is_none());
    }

    #[test]
    fn deployment_matches_kubectl() {
        assert_eq!(
            headers("apps", "Deployment", true),
            [
                "Name",
                "Namespace",
                "Ready",
                "Up-to-date",
                "Available",
                "Age"
            ]
        );
        assert_eq!(
            row(
                "apps",
                "Deployment",
                json!({
                    "spec": { "replicas": 3 },
                    "status": { "readyReplicas": 2, "updatedReplicas": 3, "availableReplicas": 2 }
                })
            ),
            ["2/3", "3", "2"]
        );
    }

    /// A deployment scaled to zero reports no counters at all. Empty cells
    /// there would read as "unknown" when the answer is zero.
    #[test]
    fn absent_replica_counters_are_zero() {
        assert_eq!(
            row(
                "apps",
                "Deployment",
                json!({ "spec": { "replicas": 0 }, "status": {} })
            ),
            ["0/0", "0", "0"]
        );
    }

    /// `spec.replicas` defaults to 1 when it is omitted.
    #[test]
    fn an_omitted_replica_count_is_one() {
        assert_eq!(
            row(
                "apps",
                "StatefulSet",
                json!({ "spec": {}, "status": { "readyReplicas": 1 } })
            ),
            ["1/1"]
        );
    }

    #[test]
    fn service_ports_include_the_node_port() {
        assert_eq!(
            headers("", "Service", false),
            [
                "Name",
                "Type",
                "Cluster-IP",
                "External-IP",
                "Port(s)",
                "Age"
            ]
        );

        assert_eq!(
            row(
                "",
                "Service",
                json!({ "spec": {
                    "type": "ClusterIP", "clusterIP": "10.43.36.48",
                    "ports": [{ "port": 2746, "protocol": "TCP" }]
                }})
            ),
            ["ClusterIP", "10.43.36.48", "<none>", "2746/TCP"]
        );

        assert_eq!(
            row(
                "",
                "Service",
                json!({ "spec": {
                    "type": "NodePort", "clusterIP": "10.43.1.1",
                    "ports": [
                        { "port": 80, "nodePort": 31234, "protocol": "TCP" },
                        { "port": 443, "nodePort": 31235, "protocol": "TCP" }
                    ]
                }})
            ),
            [
                "NodePort",
                "10.43.1.1",
                "<none>",
                "80:31234/TCP,443:31235/TCP"
            ]
        );
    }

    /// A load balancer with no address yet is `<pending>`, which is a different
    /// statement from having none.
    #[test]
    fn a_load_balancer_without_an_address_is_pending() {
        assert_eq!(
            row(
                "",
                "Service",
                json!({ "spec": { "type": "LoadBalancer", "clusterIP": "10.43.1.1" } })
            )[2],
            "<pending>"
        );

        assert_eq!(
            row(
                "",
                "Service",
                json!({
                    "spec": { "type": "LoadBalancer", "clusterIP": "10.43.1.1" },
                    "status": { "loadBalancer": { "ingress": [{ "ip": "203.0.113.5" }] } }
                })
            )[2],
            "203.0.113.5"
        );
    }

    #[test]
    fn an_external_name_service_shows_what_it_points_at() {
        assert_eq!(
            row(
                "",
                "Service",
                json!({ "spec": { "type": "ExternalName", "externalName": "db.example.com" } })
            )[2],
            "db.example.com"
        );
    }

    #[test]
    fn node_matches_kubectl() {
        let metadata = ObjectMeta {
            labels: Some(BTreeMap::from([
                (
                    "node-role.kubernetes.io/control-plane".into(),
                    "true".into(),
                ),
                ("node-role.kubernetes.io/master".into(), "true".into()),
                ("kubernetes.io/hostname".into(), "k3s".into()),
            ])),
            ..Default::default()
        };

        assert_eq!(
            row_with(
                "",
                "Node",
                metadata,
                json!({
                    "status": {
                        "conditions": [{ "type": "Ready", "status": "True" }],
                        "nodeInfo": { "kubeletVersion": "v1.33.3+k3s1" }
                    }
                })
            ),
            ["Ready", "control-plane,master", "v1.33.3+k3s1"]
        );
    }

    /// A cordoned node is still Ready. A column that says only `Ready` hides
    /// the reason nothing is being scheduled onto it.
    #[test]
    fn a_cordoned_node_says_so() {
        assert_eq!(
            row(
                "",
                "Node",
                json!({
                    "spec": { "unschedulable": true },
                    "status": { "conditions": [{ "type": "Ready", "status": "True" }] }
                })
            )[0],
            "Ready,SchedulingDisabled"
        );
    }

    /// A node whose kubelet stopped reporting has `status: Unknown`, which is
    /// neither Ready nor NotReady.
    #[test]
    fn a_node_that_stopped_reporting_is_unknown() {
        assert_eq!(
            row(
                "",
                "Node",
                json!({ "status": { "conditions": [{ "type": "Ready", "status": "Unknown" }] } })
            )[0],
            "Unknown"
        );
        assert_eq!(row("", "Node", json!({ "status": {} }))[0], "Unknown");
    }

    #[test]
    fn a_node_with_no_role_labels_has_none() {
        assert_eq!(row("", "Node", json!({ "status": {} }))[1], "<none>");
    }

    #[test]
    fn job_reads_its_terminal_condition() {
        assert_eq!(
            headers("batch", "Job", true),
            [
                "Name",
                "Namespace",
                "Status",
                "Completions",
                "Duration",
                "Age"
            ]
        );

        assert_eq!(
            row(
                "batch",
                "Job",
                json!({
                    "spec": { "completions": 1 },
                    "status": {
                        "succeeded": 1,
                        "startTime": "2026-09-23T11:58:51Z",
                        "completionTime": "2026-09-23T12:00:00Z",
                        "conditions": [{ "type": "Complete", "status": "True" }]
                    }
                })
            ),
            ["Complete", "1/1", "69s"]
        );
    }

    /// A running job's duration is measured against now, not left empty.
    #[test]
    fn a_running_job_has_a_duration_so_far() {
        assert_eq!(
            row(
                "batch",
                "Job",
                json!({
                    "spec": { "completions": 1 },
                    "status": { "startTime": "2026-09-23T11:50:00Z" }
                })
            ),
            ["Running", "0/1", "10m"]
        );
    }

    #[test]
    fn a_suspended_job_is_neither_running_nor_complete() {
        assert_eq!(
            row(
                "batch",
                "Job",
                json!({ "spec": { "suspend": true, "completions": 1 }, "status": {} })
            )[0],
            "Suspended"
        );
    }

    #[test]
    fn a_pvc_abbreviates_its_access_modes() {
        assert_eq!(
            headers("", "PersistentVolumeClaim", true),
            [
                "Name",
                "Namespace",
                "Status",
                "Volume",
                "Capacity",
                "Access Modes",
                "Storageclass",
                "VolumeAttributesClass",
                "Age"
            ]
        );

        assert_eq!(
            row(
                "",
                "PersistentVolumeClaim",
                json!({
                    "spec": { "volumeName": "pvc-abc", "storageClassName": "local-path" },
                    "status": {
                        "phase": "Bound",
                        "capacity": { "storage": "10Gi" },
                        "accessModes": ["ReadWriteOnce"]
                    }
                })
            ),
            ["Bound", "pvc-abc", "10Gi", "RWO", "local-path", "<unset>"]
        );
    }

    #[test]
    fn an_ingress_without_a_host_matches_everything() {
        assert_eq!(
            row(
                "networking.k8s.io",
                "Ingress",
                json!({ "spec": { "ingressClassName": "traefik", "rules": [{}] } })
            ),
            ["traefik", "*", "<none>", "80"]
        );
    }

    /// TLS anywhere in the spec adds 443, which is what kubectl reports without
    /// looking at the backends at all.
    #[test]
    fn tls_adds_a_port_to_an_ingress() {
        assert_eq!(
            row(
                "networking.k8s.io",
                "Ingress",
                json!({ "spec": {
                    "rules": [{ "host": "app.example.com" }],
                    "tls": [{ "hosts": ["app.example.com"] }]
                }})
            )[3],
            "80, 443"
        );
    }

    /// A RoleBinding may bind a ClusterRole, so the kind is part of the answer
    /// rather than something to infer from the binding's own kind.
    #[test]
    fn a_binding_names_what_it_binds() {
        assert_eq!(
            headers("rbac.authorization.k8s.io", "RoleBinding", true),
            ["Name", "Namespace", "Role", "Age"]
        );

        assert_eq!(
            row(
                "rbac.authorization.k8s.io",
                "RoleBinding",
                json!({ "roleRef": { "kind": "ClusterRole", "name": "admin" } })
            ),
            ["ClusterRole/admin"]
        );

        assert_eq!(
            row(
                "rbac.authorization.k8s.io",
                "ClusterRoleBinding",
                json!({ "roleRef": { "kind": "ClusterRole", "name": "cluster-admin" } })
            ),
            ["ClusterRole/cluster-admin"]
        );
    }

    #[test]
    fn a_config_map_counts_both_of_its_maps() {
        assert_eq!(
            row(
                "",
                "ConfigMap",
                json!({ "data": { "a": "1", "b": "2" }, "binaryData": { "c": "AA==" } })
            ),
            ["3"]
        );
        assert_eq!(row("", "ConfigMap", json!({})), ["0"]);
    }

    #[test]
    fn a_daemon_set_shows_its_node_selector() {
        assert_eq!(
            row(
                "apps",
                "DaemonSet",
                json!({
                    "spec": { "template": { "spec": { "nodeSelector": { "disk": "ssd" } } } },
                    "status": {
                        "desiredNumberScheduled": 3, "currentNumberScheduled": 3,
                        "numberReady": 2, "updatedNumberScheduled": 3, "numberAvailable": 2
                    }
                })
            ),
            ["3", "3", "2", "3", "2", "disk=ssd"]
        );
    }

    #[test]
    fn a_cron_job_reports_when_it_last_ran() {
        assert_eq!(
            headers("batch", "CronJob", true),
            [
                "Name",
                "Namespace",
                "Schedule",
                "Suspend",
                "Active",
                "Last Schedule",
                "Age"
            ]
        );

        assert_eq!(
            row(
                "batch",
                "CronJob",
                json!({
                    "spec": { "schedule": "*/5 * * * *" },
                    "status": { "lastScheduleTime": "2026-09-23T11:57:00Z", "active": [{}] }
                })
            ),
            ["*/5 * * * *", "False", "1", "3m"]
        );
    }
}
