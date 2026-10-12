//! Cluster-wide dashboard snapshots. Each read can fail independently: missing
//! permissions or metrics never turn into invented zeroes for healthy sections.

use crate::{
    ClusterSession, DynamicObject, ObjectRef, WatchKey,
    metrics::{self, Metrics, Usage},
};
use futures::future::join_all;
use k8s_openapi::jiff::Timestamp;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Range {
    #[default]
    HalfHour,
    Hour,
    SixHours,
}
impl Range {
    pub fn seconds(self) -> i64 {
        match self {
            Self::HalfHour => 1800,
            Self::Hour => 3600,
            Self::SixHours => 21600,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::HalfHour => "30m",
            Self::Hour => "1h",
            Self::SixHours => "6h",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ResourceSnapshot {
    pub objects: Vec<Arc<DynamicObject>>,
    pub error: Option<String>,
}
impl ResourceSnapshot {
    pub fn available(&self) -> bool {
        self.error.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub nodes: ResourceSnapshot,
    pub pods: ResourceSnapshot,
    pub events: ResourceSnapshot,
    pub workloads: ResourceSnapshot,
    pub metrics: metrics::Report,
    pub history: Result<Vec<Sample>, String>,
    pub at: Timestamp,
}

pub async fn fetch(session: Arc<ClusterSession>, range: Range) -> Snapshot {
    let list = |group: &'static str, kind: &'static str| {
        let session = session.clone();
        async move {
            let Some(kind) = session
                .discovery()
                .kinds()
                .iter()
                .find(|k| {
                    k.resource.group == group && k.resource.kind == kind && k.supports("list")
                })
                .cloned()
            else {
                return ResourceSnapshot {
                    error: Some(format!("{kind} list is unavailable for this connection.")),
                    ..Default::default()
                };
            };
            match tokio::time::timeout(
                Duration::from_secs(15),
                session.list_objects(WatchKey::all(kind.resource)),
            )
            .await
            {
                Ok(Ok(objects)) => ResourceSnapshot {
                    objects,
                    error: None,
                },
                Ok(Err(error)) => ResourceSnapshot {
                    error: Some(error.user_message()),
                    ..Default::default()
                },
                Err(_) => ResourceSnapshot {
                    error: Some("The resource list timed out. Refresh to retry.".into()),
                    ..Default::default()
                },
            }
        }
    };
    let (nodes, pods, events, workloads, metrics) = tokio::join!(
        list("", "Node"),
        list("", "Pod"),
        list("", "Event"),
        join_all([
            list("apps", "Deployment"),
            list("apps", "StatefulSet"),
            list("apps", "DaemonSet")
        ]),
        async {
            tokio::time::timeout(Duration::from_secs(15), session.metrics_report())
                .await
                .unwrap_or_else(|_| metrics::Report {
                    error: Some(
                        "Metrics refresh timed out. Check the configured metrics source.".into(),
                    ),
                    ..Default::default()
                })
        },
    );
    let mut workload_snapshot = ResourceSnapshot::default();
    let mut workload_errors = Vec::new();
    for (kind, data) in ["Deployment", "StatefulSet", "DaemonSet"]
        .into_iter()
        .zip(workloads)
    {
        workload_snapshot.objects.extend(data.objects);
        if let Some(error) = data.error {
            workload_errors.push(format!("{kind}: {error}"));
        }
    }
    workload_snapshot.error = (!workload_errors.is_empty()).then(|| workload_errors.join("\n"));
    let at = Timestamp::now();
    let history = if let crate::connection::MetricsSource::Prometheus(config) =
        &session.connection_options().metrics
    {
        if nodes.available() && !nodes.objects.is_empty() {
            prometheus_history(
                &session.connection_options().proxy,
                config,
                &nodes.objects,
                range,
                at.as_second(),
            )
            .await
        } else {
            Err("Node capacity is required to display cluster trends.".into())
        }
    } else {
        Ok(Vec::new())
    };
    Snapshot {
        nodes,
        pods,
        events,
        workloads: workload_snapshot,
        metrics,
        history,
        at,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Resources {
    pub cpu: f64,
    pub memory: f64,
    pub pods: f64,
}
impl Resources {
    fn add(&mut self, other: Self) {
        self.cpu += other.cpu;
        self.memory += other.memory;
        self.pods += other.pods;
    }
    fn max(self, other: Self) -> Self {
        Self {
            cpu: self.cpu.max(other.cpu),
            memory: self.memory.max(other.memory),
            pods: self.pods.max(other.pods),
        }
    }
}

fn resources(data: &Value) -> Resources {
    let read = |key: &str| {
        data[key]
            .as_str()
            .and_then(metrics::parse_quantity)
            .filter(|v| v.is_finite() && *v >= 0.)
            .unwrap_or(0.)
    };
    Resources {
        cpu: read("cpu"),
        memory: read("memory"),
        pods: read("pods"),
    }
}

/// The scheduler takes the max of steady-state containers and sequential init
/// phases. Restartable init containers continue in every subsequent phase.
pub fn pod_resources(spec: &Value, field: &str) -> Resources {
    let mut steady = Resources::default();
    for container in array(spec, "/containers") {
        steady.add(resources(&container["resources"][field]));
    }
    let mut sidecars = Resources::default();
    let mut init_peak = Resources::default();
    for container in array(spec, "/initContainers") {
        let amount = resources(&container["resources"][field]);
        if container["restartPolicy"] == "Always" {
            sidecars.add(amount);
            init_peak = init_peak.max(sidecars);
        } else {
            let mut phase = amount;
            phase.add(sidecars);
            init_peak = init_peak.max(phase);
        }
    }
    steady.add(sidecars);
    let mut effective = steady.max(init_peak);
    // Pod-level resource budgets supersede container sums for the named resource.
    let budget = &spec["resources"][field];
    let values = resources(budget);
    if budget.get("cpu").is_some() {
        effective.cpu = values.cpu;
    }
    if budget.get("memory").is_some() {
        effective.memory = values.memory;
    }
    let overhead = resources(&spec["overhead"]);
    effective.cpu += if field == "requests" || effective.cpu > 0. {
        overhead.cpu
    } else {
        0.
    };
    effective.memory += if field == "requests" || effective.memory > 0. {
        overhead.memory
    } else {
        0.
    };
    effective
}

fn array<'a>(data: &'a Value, path: &str) -> impl Iterator<Item = &'a Value> {
    data.pointer(path)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}
pub fn node_ready(data: &Value) -> bool {
    array(data, "/status/conditions").any(|c| c["type"] == "Ready" && c["status"] == "True")
}
fn pod_active(object: &DynamicObject) -> bool {
    !matches!(
        object.data.pointer("/status/phase").and_then(Value::as_str),
        Some("Succeeded" | "Failed")
    )
}
fn pod_ready(object: &DynamicObject) -> bool {
    object.metadata.deletion_timestamp.is_none()
        && array(&object.data, "/status/conditions")
            .any(|c| c["type"] == "Ready" && c["status"] == "True")
}
fn workload_available(object: &DynamicObject) -> bool {
    let data = &object.data;
    if object.metadata.generation.is_some_and(|g| {
        data.pointer("/status/observedGeneration")
            .and_then(Value::as_i64)
            .unwrap_or(-1)
            < g
    }) {
        return false;
    }
    let (desired, ready) = match object.types.as_ref().map(|t| t.kind.as_str()) {
        Some("DaemonSet") => (
            data.pointer("/status/desiredNumberScheduled")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            data.pointer("/status/numberAvailable")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ),
        Some("StatefulSet") => (
            data.pointer("/spec/replicas")
                .and_then(Value::as_u64)
                .unwrap_or(1),
            data.pointer("/status/readyReplicas")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ),
        _ => (
            data.pointer("/spec/replicas")
                .and_then(Value::as_u64)
                .unwrap_or(1),
            data.pointer("/status/availableReplicas")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        ),
    };
    ready >= desired
}

pub fn event_time(object: &DynamicObject) -> Option<Timestamp> {
    [
        "/series/lastObservedTime",
        "/lastTimestamp",
        "/eventTime",
        "/firstTimestamp",
    ]
    .iter()
    .find_map(|p| {
        object
            .data
            .pointer(p)
            .and_then(Value::as_str)
            .and_then(|v| v.parse().ok())
    })
    .or_else(|| object.metadata.creation_timestamp.as_ref().map(|t| t.0))
}
pub fn warning_events(snapshot: &Snapshot) -> Vec<Arc<DynamicObject>> {
    let mut events: Vec<_> = snapshot
        .events
        .objects
        .iter()
        .filter(|event| {
            event.data["type"] == "Warning"
                && event_time(event).is_some_and(|time| {
                    let age = snapshot.at.as_second() - time.as_second();
                    (0..=3600).contains(&age)
                })
        })
        .cloned()
        .collect();
    events.sort_by_key(|e| std::cmp::Reverse(event_time(e)));
    events
}

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub nodes: Option<(usize, usize)>,
    pub pods: Option<(usize, usize)>,
    pub workloads: Option<(usize, usize)>,
    pub warnings: Option<usize>,
    pub capacity: Option<Resources>,
    pub allocatable: Option<Resources>,
    pub usage: Option<Resources>,
    pub requests: Option<Resources>,
    pub limits: Option<Resources>,
    pub scheduled: Option<usize>,
    pub pending: Option<usize>,
    pub terminal: Option<usize>,
    pub partial: bool,
    pub attention: bool,
}
impl Summary {
    pub fn read(snapshot: &Snapshot) -> Self {
        let mut out = Self {
            partial: [
                &snapshot.nodes,
                &snapshot.pods,
                &snapshot.events,
                &snapshot.workloads,
            ]
            .iter()
            .any(|s| !s.available()),
            ..Default::default()
        };
        if snapshot.nodes.available() {
            out.nodes = Some((
                snapshot
                    .nodes
                    .objects
                    .iter()
                    .filter(|o| node_ready(&o.data))
                    .count(),
                snapshot.nodes.objects.len(),
            ));
            out.capacity = sum_node_resources(&snapshot.nodes.objects, "capacity");
            out.allocatable = sum_node_resources(&snapshot.nodes.objects, "allocatable");
            if !snapshot.nodes.objects.is_empty() {
                out.usage = snapshot.nodes.objects.iter().try_fold(
                    Resources::default(),
                    |mut total, node| {
                        let Usage {
                            cpu_millis,
                            memory_bytes,
                        } = snapshot
                            .metrics
                            .metrics
                            .complete_node(node.metadata.name.as_deref()?)?;
                        total.cpu += cpu_millis as f64 / 1000.;
                        total.memory += memory_bytes as f64;
                        Some(total)
                    },
                );
            }
        }
        if snapshot.pods.available() {
            let active: Vec<_> = snapshot
                .pods
                .objects
                .iter()
                .filter(|o| pod_active(o))
                .collect();
            out.pods = Some((active.iter().filter(|o| pod_ready(o)).count(), active.len()));
            out.pending = Some(
                active
                    .iter()
                    .filter(|o| {
                        o.data.pointer("/status/phase").and_then(Value::as_str) == Some("Pending")
                    })
                    .count(),
            );
            out.terminal = Some(snapshot.pods.objects.len() - active.len());
            let mut requests = Resources::default();
            let mut limits = Resources::default();
            let mut scheduled = 0;
            for pod in active {
                if pod
                    .data
                    .pointer("/spec/nodeName")
                    .and_then(Value::as_str)
                    .is_some_and(|n| !n.is_empty())
                {
                    requests.add(pod_resources(&pod.data["spec"], "requests"));
                    limits.add(pod_resources(&pod.data["spec"], "limits"));
                    scheduled += 1;
                }
            }
            out.requests = Some(requests);
            out.limits = Some(limits);
            out.scheduled = Some(scheduled);
        }
        if snapshot.workloads.available() {
            out.workloads = Some((
                snapshot
                    .workloads
                    .objects
                    .iter()
                    .filter(|o| workload_available(o))
                    .count(),
                snapshot.workloads.objects.len(),
            ));
        }
        if snapshot.events.available() {
            out.warnings = Some(warning_events(snapshot).len());
        }
        out.attention = [out.nodes, out.pods, out.workloads]
            .into_iter()
            .flatten()
            .any(|(ready, total)| ready < total)
            || out.warnings.is_some_and(|n| n > 0)
            || snapshot.nodes.objects.iter().any(|o| {
                array(&o.data, "/status/conditions").any(|c| {
                    matches!(
                        c["type"].as_str(),
                        Some(
                            "MemoryPressure"
                                | "DiskPressure"
                                | "PIDPressure"
                                | "NetworkUnavailable"
                        )
                    ) && c["status"] == "True"
                })
            });
        out
    }
}

fn sum_node_resources(nodes: &[Arc<DynamicObject>], field: &str) -> Option<Resources> {
    if nodes.is_empty() {
        return None;
    }
    nodes
        .iter()
        .try_fold(Resources::default(), |mut sum, node| {
            let values = &node.data["status"][field];
            for key in ["cpu", "memory", "pods"] {
                values[key]
                    .as_str()
                    .and_then(metrics::parse_quantity)
                    .filter(|v| v.is_finite() && *v >= 0.)?;
            }
            sum.add(resources(values));
            Some(sum)
        })
}

/// Built off the foreground thread so rendering/hovering never scans all Pods.
pub fn node_pod_counts(snapshot: &Snapshot) -> Option<HashMap<String, usize>> {
    if !snapshot.pods.available() {
        return None;
    }
    let mut counts = HashMap::new();
    for pod in snapshot.pods.objects.iter().filter(|pod| pod_active(pod)) {
        if let Some(node) = pod
            .data
            .pointer("/spec/nodeName")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
        {
            *counts.entry(node.to_owned()).or_default() += 1;
        }
    }
    Some(counts)
}
pub fn node_usage(metrics: &Metrics, node: &DynamicObject) -> Option<(f64, f64)> {
    let used = metrics.complete_node(node.metadata.name.as_deref()?)?;
    let alloc = resources(&node.data["status"]["allocatable"]);
    (alloc.cpu > 0. && alloc.memory > 0.).then(|| {
        (
            used.cpu_millis as f64 / 10. / alloc.cpu,
            used.memory_bytes as f64 / alloc.memory * 100.,
        )
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub at: i64,
    pub cpu: Option<f64>,
    pub memory: Option<f64>,
}
pub fn session_sample(snapshot: &Snapshot, summary: &Summary) -> Sample {
    Sample {
        at: snapshot.at.as_second(),
        cpu: summary.usage.map(|v| v.cpu),
        memory: summary.usage.map(|v| v.memory),
    }
}

async fn prometheus_history(
    proxy: &crate::connection::Proxy,
    config: &crate::connection::Prometheus,
    nodes: &[Arc<DynamicObject>],
    range: Range,
    end: i64,
) -> Result<Vec<Sample>, String> {
    config.validate()?;
    let client = proxy
        .http_client()
        .map_err(|_| "Could not create the metrics HTTP client.".to_owned())?;
    let start = end - range.seconds();
    let step = (range.seconds() / 120).max(15);
    let query = |expression: &str| {
        let mut request = client
            .get(format!(
                "{}/api/v1/query_range",
                config.url.trim_end_matches('/')
            ))
            .query(&[
                ("query", expression.to_owned()),
                ("start", start.to_string()),
                ("end", end.to_string()),
                ("step", step.to_string()),
            ]);
        if !config.bearer_token.is_empty() {
            request = request.bearer_auth(&config.bearer_token);
        }
        async move {
            let response = request.send().await.map_err(|_| "Could not reach Prometheus for history. Check its URL, proxy and TLS certificate.".to_owned())?;
            if !response.status().is_success() {
                return Err(format!(
                    "Prometheus history returned HTTP {}.",
                    response.status().as_u16()
                ));
            }
            let data: Value = response
                .json()
                .await
                .map_err(|_| "Prometheus history returned invalid JSON.".to_owned())?;
            if data["status"] != "success" || data["data"]["resultType"] != "matrix" {
                return Err("Prometheus history queries must return range vectors.".into());
            }
            Ok(data)
        }
    };
    let (cpu, memory) = tokio::join!(query(&config.node_cpu), query(&config.node_memory));
    let names: Vec<_> = nodes
        .iter()
        .filter_map(|n| n.metadata.name.clone())
        .collect();
    history_samples(&cpu?, &memory?, &names)
}

fn history_samples(cpu: &Value, memory: &Value, nodes: &[String]) -> Result<Vec<Sample>, String> {
    let parse = |response: &Value| -> Result<BTreeMap<i64, HashMap<String, f64>>, String> {
        let entries = response["data"]["result"]
            .as_array()
            .ok_or("Prometheus history has no result matrix.")?;
        let mut values: BTreeMap<i64, HashMap<String, f64>> = BTreeMap::new();
        for entry in entries {
            let node = entry["metric"]["node"]
                .as_str()
                .filter(|v| !v.is_empty())
                .ok_or("Node history queries must return a node label.")?;
            if !nodes.iter().any(|n| n == node) {
                continue;
            }
            for pair in entry["values"]
                .as_array()
                .ok_or("Prometheus history has no sample values.")?
            {
                let (Some(at), Some(value)) = (
                    pair[0].as_f64(),
                    pair[1].as_str().and_then(|v| v.parse::<f64>().ok()),
                ) else {
                    continue;
                };
                if at.is_finite() && value.is_finite() && value >= 0. {
                    *values
                        .entry(at as i64)
                        .or_default()
                        .entry(node.to_owned())
                        .or_default() += value;
                }
            }
        }
        Ok(values)
    };
    let cpu = parse(cpu)?;
    let memory = parse(memory)?;
    let mut times: Vec<_> = cpu.keys().chain(memory.keys()).copied().collect();
    times.sort();
    times.dedup();
    let sum = |values: &BTreeMap<i64, HashMap<String, f64>>, at: i64| {
        values
            .get(&at)
            .filter(|v| nodes.iter().all(|node| v.contains_key(node)))
            .map(|v| v.values().sum())
    };
    Ok(times
        .into_iter()
        .map(|at| Sample {
            at,
            cpu: sum(&cpu, at),
            memory: sum(&memory, at),
        })
        .collect())
}

pub fn event_target(event: &DynamicObject) -> Option<(&str, &str, ObjectRef)> {
    let object = event
        .data
        .get("involvedObject")
        .or_else(|| event.data.get("regarding"))?;
    let version = object["apiVersion"].as_str().unwrap_or("v1");
    let group = version.split_once('/').map_or("", |(group, _)| group);
    Some((
        group,
        object["kind"].as_str()?,
        ObjectRef::new(
            object["namespace"].as_str().map(str::to_owned),
            object["name"].as_str()?,
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object(value: Value) -> Arc<DynamicObject> {
        Arc::new(serde_json::from_value(value).unwrap())
    }
    fn snapshot() -> Snapshot {
        Snapshot {
            nodes: ResourceSnapshot {
                objects: vec![object(
                    json!({"apiVersion":"v1","kind":"Node","metadata":{"name":"worker"},"status":{"capacity":{"cpu":"4","memory":"8Gi","pods":"110"},"allocatable":{"cpu":"3800m","memory":"7Gi","pods":"100"},"conditions":[{"type":"Ready","status":"True"}]}}),
                )],
                error: None,
            },
            pods: ResourceSnapshot::default(),
            events: ResourceSnapshot::default(),
            workloads: ResourceSnapshot::default(),
            metrics: metrics::Report::default(),
            history: Ok(vec![]),
            at: "2026-10-12T12:00:00Z".parse().unwrap(),
        }
    }
    #[test]
    fn init_sidecars_overhead_and_pod_budgets_follow_scheduling_rules() {
        let spec = json!({"containers":[{"resources":{"requests":{"cpu":"500m","memory":"500Mi"}}}],
            "initContainers":[{"restartPolicy":"Always","resources":{"requests":{"cpu":"100m","memory":"100Mi"}}},{"resources":{"requests":{"cpu":"2","memory":"200Mi"}}}],"overhead":{"cpu":"50m","memory":"10Mi"}});
        let resources = pod_resources(&spec, "requests");
        assert!((resources.cpu - 2.15).abs() < 1e-9);
        assert_eq!(resources.memory, 610. * 1024. * 1024.);
        let mut budget = spec;
        budget["resources"] = json!({"requests":{"cpu":"3"}});
        assert!((pod_resources(&budget, "requests").cpu - 3.05).abs() < 1e-9);
        assert_eq!(
            pod_resources(&budget, "limits").cpu,
            0.,
            "overhead does not invent a declared limit"
        );
    }
    #[test]
    fn totals_exclude_terminal_and_unscheduled_pods_and_do_not_invent_usage() {
        let mut snapshot = snapshot();
        let pod = |name: &str, phase: &str, node: Option<&str>, ready: bool| {
            object(
                json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"default"},"spec":{"nodeName":node,"containers":[{"resources":{"requests":{"cpu":"250m"},"limits":{"cpu":"5"}}}]},"status":{"phase":phase,"conditions":[{"type":"Ready","status":if ready{"True"}else{"False"}}]}}),
            )
        };
        snapshot.pods.objects = vec![
            pod("app", "Running", Some("worker"), true),
            pod("queued", "Pending", None, false),
            pod("completed", "Succeeded", Some("worker"), false),
            pod("failed", "Failed", Some("worker"), false),
        ];
        let summary = Summary::read(&snapshot);
        assert_eq!(summary.nodes, Some((1, 1)));
        assert_eq!(summary.pods, Some((1, 2)));
        assert_eq!(summary.scheduled, Some(1));
        assert_eq!(node_pod_counts(&snapshot).unwrap().get("worker"), Some(&1));
        assert_eq!(summary.terminal, Some(2));
        assert_eq!(summary.pending, Some(1));
        assert_eq!(summary.requests.unwrap().cpu, 0.25);
        assert_eq!(summary.limits.unwrap().cpu, 5.);
        assert!((summary.allocatable.unwrap().cpu - 3.8).abs() < 1e-9);
        assert!(summary.usage.is_none());
        assert!(summary.attention);
        snapshot.nodes.error = Some("Forbidden".into());
        snapshot.pods.error = Some("Forbidden".into());
        let summary = Summary::read(&snapshot);
        assert!(summary.partial);
        assert!(summary.nodes.is_none());
        assert!(summary.capacity.is_none());
        assert!(summary.requests.is_none());
        assert!(summary.pods.is_none());
        assert!(node_pod_counts(&snapshot).is_none());
    }
    #[test]
    fn warnings_are_recent_occurrences_and_workloads_require_observed_generation() {
        let mut snapshot = snapshot();
        let event = |name: &str, last: &str, series: Option<&str>| {
            object(
                json!({"apiVersion":"v1","kind":"Event","metadata":{"name":name},"type":"Warning","lastTimestamp":last,"series":{"lastObservedTime":series},"involvedObject":{"apiVersion":"apps/v1","kind":"Deployment","namespace":"default","name":"api"}}),
            )
        };
        snapshot.events.objects = vec![
            event("old", "2026-10-12T09:00:00Z", None),
            event(
                "repeated",
                "2026-10-11T09:00:00Z",
                Some("2026-10-12T11:59:00Z"),
            ),
            event("recent", "2026-10-12T11:55:00Z", None),
        ];
        let deployment = |observed: i64, desired: i64| {
            object(
                json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"api","generation":3},"spec":{"replicas":desired},"status":{"observedGeneration":observed,"availableReplicas":desired}}),
            )
        };
        snapshot.workloads.objects = vec![deployment(2, 3), deployment(3, 0), deployment(3, 2)];
        assert_eq!(Summary::read(&snapshot).warnings, Some(2));
        assert_eq!(Summary::read(&snapshot).workloads, Some((2, 3)));
        assert_eq!(
            warning_events(&snapshot)[0].metadata.name.as_deref(),
            Some("repeated")
        );
        let (group, kind, target) = event_target(&snapshot.events.objects[0]).unwrap();
        assert_eq!(
            (group, kind, target.to_string()),
            ("apps", "Deployment", "default/api".into())
        );
    }
    #[test]
    fn history_preserves_missing_samples_and_filters_other_clusters() {
        let matrix = |entries: Value| json!({"data":{"resultType":"matrix","result":entries}});
        let cpu = matrix(
            json!([{"metric":{"node":"a"},"values":[[100,"1"],[115,"NaN"],[130,"2"]]},{"metric":{"node":"b"},"values":[[100,"2"],[130,"3"]]},{"metric":{"node":"other"},"values":[[100,"1000"]]}]),
        );
        let memory = matrix(
            json!([{"metric":{"node":"a"},"values":[[100,"10"],[115,"20"]]},{"metric":{"node":"b"},"values":[[100,"20"],[115,"30"]]}]),
        );
        let samples = history_samples(&cpu, &memory, &["a".into(), "b".into()]).unwrap();
        assert_eq!(
            samples,
            vec![
                Sample {
                    at: 100,
                    cpu: Some(3.),
                    memory: Some(30.)
                },
                Sample {
                    at: 115,
                    cpu: None,
                    memory: Some(50.)
                },
                Sample {
                    at: 130,
                    cpu: Some(5.),
                    memory: None
                }
            ]
        );
        assert!(
            history_samples(
                &matrix(json!([{"metric":{},"values":[]} ])),
                &memory,
                &["a".into()]
            )
            .is_err()
        );
    }
    #[tokio::test]
    async fn history_requests_use_configured_proxy_auth_base_path_and_range() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 16384];
                let n = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..n]).to_lowercase();
                assert!(request.starts_with("get /prometheus/api/v1/query_range?"));
                assert!(request.contains("start=1800"));
                assert!(request.contains("end=3600"));
                assert!(request.contains("step=15"));
                assert!(request.contains("authorization: bearer fixture-token"));
                let body = r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{"node":"worker"},"values":[[3600,"1"]]}]}}"#;
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let config = crate::connection::Prometheus {
            url: format!("http://{addr}/prometheus"),
            bearer_token: "fixture-token".into(),
            ..Default::default()
        };
        let samples = prometheus_history(
            &crate::connection::Proxy::Direct,
            &config,
            &snapshot().nodes.objects,
            Range::HalfHour,
            3600,
        )
        .await
        .unwrap();
        assert_eq!(samples[0].cpu, Some(1.));
        assert_eq!(samples[0].memory, Some(1.));
        server.await.unwrap();
    }
    #[tokio::test]
    async fn independent_list_failures_leave_other_sections_available_and_only_read() {
        let fixture = crate::test_support::Fixture::start(vec![]).await;
        fixture.deny("nodes");
        let kinds = [
            ("", "Node", "nodes"),
            ("", "Pod", "pods"),
            ("", "Event", "events"),
            ("apps", "Deployment", "deployments"),
            ("apps", "StatefulSet", "statefulsets"),
            ("apps", "DaemonSet", "daemonsets"),
        ]
        .into_iter()
        .map(|(group, kind, plural)| crate::Kind {
            resource: crate::ApiResource::from_gvk_with_plural(
                &crate::GroupVersionKind::gvk(group, "v1", kind),
                plural,
            ),
            namespaced: kind != "Node",
            verbs: vec!["list".into()],
        })
        .collect();
        let session = Arc::new(ClusterSession::for_testing(
            crate::ClusterId::new("overview-fixture"),
            fixture.url.clone(),
            kinds,
        ));
        let data = fetch(session, Range::HalfHour).await;
        let summary = Summary::read(&data);
        assert!(data.nodes.error.as_ref().unwrap().contains("403"));
        assert!(data.pods.available());
        assert!(data.workloads.available());
        assert!(summary.nodes.is_none());
        assert_eq!(summary.pods, Some((0, 0)));
        assert!(summary.usage.is_none());
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(request, _)| request.starts_with("GET "))
        );
    }
}
