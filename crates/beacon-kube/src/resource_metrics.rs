//! Resource metrics with real timestamps, explicit missing values and source
//! provenance. Metrics API snapshots become session history in the UI;
//! Prometheus matrices retain the server's historical sample times.
use crate::{
    ApiResource, ClusterSession, DynamicObject, GroupVersionKind, WatchKey,
    connection::{MetricsSource, Prometheus, Proxy},
    metrics::parse_quantity,
    overview,
};
use k8s_openapi::jiff::Timestamp;
use kube::Api;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    pub at: f64,
    pub cpu: Option<f64>,
    pub memory: Option<f64>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Amount {
    pub cpu: Option<f64>,
    pub memory: Option<f64>,
}
#[derive(Clone, Debug, Default)]
pub struct Reference {
    pub requests: Amount,
    pub limits: Amount,
    pub allocatable: Amount,
}
#[derive(Clone, Debug)]
pub struct Marker {
    pub id: String,
    pub at: f64,
    pub reason: String,
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub name: String,
    pub group: &'static str,
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub observed_at: f64,
    pub sample: Sample,
    pub reference: Reference,
    pub history: Vec<Sample>,
    pub markers: Vec<Marker>,
    pub restarts: BTreeMap<String, i64>,
    pub containers: Vec<Container>,
    pub errors: Vec<String>,
    pub history_start: f64,
}
fn now() -> f64 {
    Timestamp::now().as_millisecond() as f64 / 1000.
}
fn time(value: &str) -> Option<f64> {
    value
        .parse::<Timestamp>()
        .ok()
        .map(|time| time.as_millisecond() as f64 / 1000.)
}
fn quantity(value: &Value) -> Option<f64> {
    value
        .as_str()
        .and_then(parse_quantity)
        .filter(|value| value.is_finite() && *value >= 0.)
}
fn amount(value: &Value) -> Amount {
    Amount {
        cpu: quantity(&value["cpu"]),
        memory: quantity(&value["memory"]),
    }
}
fn arrays<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value[key].as_array().into_iter().flatten()
}
pub fn containers(object: &DynamicObject) -> Vec<Container> {
    [
        ("containers", "Containers"),
        ("initContainers", "Init containers"),
        ("ephemeralContainers", "Ephemeral containers"),
    ]
    .into_iter()
    .flat_map(|(field, group)| {
        arrays(&object.data["spec"], field).filter_map(move |container| {
            container["name"].as_str().map(|name| Container {
                name: name.into(),
                group,
            })
        })
    })
    .collect()
}
fn pod_amount(spec: &Value, field: &str, selected: Option<&str>) -> Amount {
    if let Some(selected) = selected {
        return ["containers", "initContainers", "ephemeralContainers"]
            .into_iter()
            .flat_map(|field| arrays(spec, field))
            .find(|container| container["name"] == selected)
            .map(|container| amount(&container["resources"][field]))
            .unwrap_or_default();
    }
    let effective = overview::pod_resources(spec, field);
    let declared = |name: &str| {
        spec["resources"][field].get(name).is_some()
            || ["containers", "initContainers"]
                .into_iter()
                .flat_map(|field| arrays(spec, field))
                .any(|container| container["resources"][field].get(name).is_some())
    };
    Amount {
        cpu: (field == "requests" || declared("cpu")).then_some(effective.cpu),
        memory: (field == "requests" || declared("memory")).then_some(effective.memory),
    }
}
pub fn reference(
    object: &DynamicObject,
    pods: &[Arc<DynamicObject>],
    selected: Option<&str>,
) -> Reference {
    if object
        .types
        .as_ref()
        .is_some_and(|types| types.kind == "Node")
    {
        let sum = |field| {
            let mut cpu = 0.;
            let mut memory = 0.;
            let mut cpu_defined = false;
            let mut memory_defined = false;
            for pod in pods.iter().filter(|pod| {
                !matches!(
                    pod.data.pointer("/status/phase").and_then(Value::as_str),
                    Some("Succeeded" | "Failed")
                )
            }) {
                let value = pod_amount(&pod.data["spec"], field, None);
                if let Some(value) = value.cpu {
                    cpu += value;
                    cpu_defined = true;
                }
                if let Some(value) = value.memory {
                    memory += value;
                    memory_defined = true;
                }
            }
            Amount {
                cpu: (field == "requests" || cpu_defined).then_some(cpu),
                memory: (field == "requests" || memory_defined).then_some(memory),
            }
        };
        Reference {
            requests: sum("requests"),
            limits: sum("limits"),
            allocatable: amount(&object.data["status"]["allocatable"]),
        }
    } else {
        Reference {
            requests: pod_amount(&object.data["spec"], "requests", selected),
            limits: pod_amount(&object.data["spec"], "limits", selected),
            allocatable: Amount::default(),
        }
    }
}
fn statuses(object: &DynamicObject) -> impl Iterator<Item = &Value> {
    [
        "containerStatuses",
        "initContainerStatuses",
        "ephemeralContainerStatuses",
    ]
    .into_iter()
    .flat_map(|field| arrays(&object.data["status"], field))
}
pub fn state_markers(
    objects: &[Arc<DynamicObject>],
    selected: Option<&str>,
) -> (Vec<Marker>, BTreeMap<String, i64>) {
    let mut markers = Vec::new();
    let mut restarts = BTreeMap::new();
    for object in objects {
        for status in statuses(object)
            .filter(|status| selected.is_none_or(|selected| status["name"] == selected))
        {
            let container = status["name"].as_str().unwrap_or("unknown");
            let identity = format!(
                "{}/{}/{}",
                object.metadata.uid.as_deref().unwrap_or(""),
                object.metadata.name.as_deref().unwrap_or(""),
                container
            );
            restarts.insert(
                identity.clone(),
                status["restartCount"].as_i64().unwrap_or(0),
            );
            for field in ["state", "lastState"] {
                if let Some(terminated) = status[field].get("terminated")
                    && let Some(at) = terminated["finishedAt"].as_str().and_then(time)
                {
                    let reason = terminated["reason"].as_str().unwrap_or("Terminated");
                    if reason == "OOMKilled" || field == "lastState" {
                        markers.push(Marker {
                            id: format!("{identity}/{at}/{reason}"),
                            at,
                            reason: reason.into(),
                            message: format!(
                                "{} / {container}: {reason}{}",
                                object.metadata.name.as_deref().unwrap_or("Pod"),
                                terminated["exitCode"]
                                    .as_i64()
                                    .map(|exit| format!(" · exit {exit}"))
                                    .unwrap_or_default()
                            ),
                        });
                    }
                }
            }
        }
    }
    (markers, restarts)
}
async fn metrics_api(
    session: &ClusterSession,
    object: &DynamicObject,
    node: bool,
    selected: Option<&str>,
) -> Result<Sample, String> {
    let resource = ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk(
            "metrics.k8s.io",
            "v1beta1",
            if node { "NodeMetrics" } else { "PodMetrics" },
        ),
        if node { "nodes" } else { "pods" },
    );
    let api: Api<DynamicObject> = if node {
        Api::all_with(session.client().clone(), &resource)
    } else {
        Api::namespaced_with(
            session.client().clone(),
            object.metadata.namespace.as_deref().unwrap_or("default"),
            &resource,
        )
    };
    let metric = api
        .get(object.metadata.name.as_deref().unwrap_or(""))
        .await
        .map_err(|error| crate::Error::Api(error).user_message())?;
    let at = metric.data["timestamp"]
        .as_str()
        .and_then(time)
        .unwrap_or_else(now);
    let used = if node {
        amount(&metric.data["usage"])
    } else {
        let entries = arrays(&metric.data, "containers")
            .filter(|entry| selected.is_none_or(|selected| entry["name"] == selected))
            .collect::<Vec<_>>();
        let sum = |field: &str| {
            if entries.is_empty() {
                None
            } else {
                entries
                    .iter()
                    .map(|entry| quantity(&entry["usage"][field]))
                    .collect::<Option<Vec<_>>>()
                    .map(|values| values.into_iter().sum())
            }
        };
        Amount {
            cpu: sum("cpu"),
            memory: sum("memory"),
        }
    };
    Ok(Sample {
        at,
        cpu: used.cpu,
        memory: used.memory,
    })
}

pub async fn fetch(
    session: Arc<ClusterSession>,
    resource: ApiResource,
    namespace: Option<String>,
    name: String,
    uid: Option<String>,
    selected: Option<String>,
    seconds: i64,
) -> Result<Snapshot, String> {
    let object = session
        .clone()
        .get_object(resource.clone(), namespace, name)
        .await
        .map_err(|error| error.user_message())?;
    if uid
        .as_ref()
        .is_some_and(|uid| object.metadata.uid.as_ref() != Some(uid))
    {
        return Err(
            "This resource was replaced. Reopen its details to start a new metrics session.".into(),
        );
    }
    let node = resource.kind == "Node" && resource.group.is_empty();
    let mut errors = Vec::new();
    let mut pods_available = true;
    let pods = if node {
        let mut key = WatchKey::all(crate::resources::pod());
        key.fields = Some(format!(
            "spec.nodeName={}",
            object.metadata.name.as_deref().unwrap_or("")
        ));
        match session.clone().list_objects(key).await {
            Ok(pods) => pods
                .into_iter()
                .filter(|pod| {
                    pod.data["spec"]["nodeName"].as_str() == object.metadata.name.as_deref()
                })
                .collect(),
            Err(error) => {
                pods_available = false;
                errors.push(format!("Scheduled Pods: {}", error.user_message()));
                vec![]
            }
        }
    } else {
        vec![Arc::new(object.clone())]
    };
    let mut reference = reference(&object, &pods, selected.as_deref());
    if !pods_available {
        reference.requests = Amount::default();
        reference.limits = Amount::default();
    }
    let (mut markers, restarts) = state_markers(&pods, selected.as_deref());
    let observed_at = now();
    let mut sample = Sample {
        at: observed_at,
        ..Default::default()
    };
    let mut history = Vec::new();
    let created = object
        .metadata
        .creation_timestamp
        .as_ref()
        .map_or(0., |time| time.0.as_second() as f64);
    let history_start = (sample.at - seconds as f64).max(created);
    match &session.connection_options().metrics {
        MetricsSource::Disabled => errors.push("Metrics are disabled in cluster settings.".into()),
        MetricsSource::Kubernetes => {
            match metrics_api(&session, &object, node, selected.as_deref()).await {
                Ok(value) => sample = value,
                Err(error) => errors.push(format!("Kubernetes Metrics API: {error}")),
            }
        }
        MetricsSource::Prometheus(config) => match prometheus_history(
            &session.connection_options().proxy,
            config,
            &object,
            node,
            selected.as_deref(),
            (history_start, sample.at),
            seconds,
        )
        .await
        {
            Ok((values, partial_errors)) => {
                errors.extend(partial_errors);
                if let Some(value) = values.last() {
                    sample = *value;
                }
                history = values;
            }
            Err(error) => errors.push(error),
        },
    }
    if let Some(kind) = session.discovery().find("Event") {
        let mut key = WatchKey::all(kind.resource.clone());
        key.fields = Some(format!(
            "involvedObject.uid={}",
            object.metadata.uid.as_deref().unwrap_or("")
        ));
        match session.clone().list_objects(key).await {
            Ok(events) => {
                for event in events {
                    let involved = &event.data["involvedObject"];
                    if object
                        .metadata
                        .uid
                        .as_deref()
                        .is_some_and(|uid| involved["uid"] != uid)
                    {
                        continue;
                    }
                    if let Some(at) = overview::event_time(&event) {
                        let reason = event.data["reason"].as_str().unwrap_or("Event");
                        if event.data["type"] == "Warning"
                            || reason.to_ascii_lowercase().contains("kill")
                            || reason.contains("BackOff")
                        {
                            markers.push(Marker {
                                id: event
                                    .metadata
                                    .uid
                                    .clone()
                                    .unwrap_or_else(|| format!("{reason}/{}", at.as_second())),
                                at: at.as_second() as f64,
                                reason: reason.into(),
                                message: event.data["message"].as_str().unwrap_or("").into(),
                            });
                        }
                    }
                }
            }
            Err(error) => errors.push(format!("Events: {}", error.user_message())),
        }
    }
    Ok(Snapshot {
        observed_at,
        sample,
        reference,
        history,
        markers,
        restarts,
        containers: containers(&object),
        errors,
        history_start,
    })
}
async fn prometheus_history(
    proxy: &Proxy,
    config: &Prometheus,
    object: &DynamicObject,
    node: bool,
    selected: Option<&str>,
    bounds: (f64, f64),
    seconds: i64,
) -> Result<(Vec<Sample>, Vec<String>), String> {
    let (start, end) = bounds;
    config.validate()?;
    let client = proxy.http_client().map_err(|error| error.to_string())?;
    let (cpu, memory) = if node {
        (&config.node_cpu, &config.node_memory)
    } else if selected.is_some() {
        (&config.container_cpu, &config.container_memory)
    } else {
        (&config.pod_cpu, &config.pod_memory)
    };
    let query = |expression: String| {
        // Restrict returned series at the server without rewriting the user's
        // arbitrary aggregation. `and` keeps values/labels from the left vector.
        let mut labels = if node {
            vec![("node", object.metadata.name.as_deref().unwrap_or(""))]
        } else {
            vec![
                (
                    "namespace",
                    object.metadata.namespace.as_deref().unwrap_or("default"),
                ),
                ("pod", object.metadata.name.as_deref().unwrap_or("")),
            ]
        };
        if let Some(selected) = selected {
            labels.push(("container", selected));
        }
        let mut identity = "vector(1)".to_owned();
        for (label, value) in &labels {
            let value = serde_json::to_string(value).expect("string serialization");
            identity =
                format!("label_replace({identity}, \"{label}\", {value}, \"__name__\", \".*\")");
        }
        let keys = labels
            .iter()
            .map(|(label, _)| *label)
            .collect::<Vec<_>>()
            .join(", ");
        let expression = format!("({expression}) and on ({keys}) ({identity})");
        let mut request = client
            .get(format!(
                "{}/api/v1/query_range",
                config.url.trim_end_matches('/')
            ))
            .query(&[
                ("query", expression),
                ("start", start.to_string()),
                ("end", end.to_string()),
                ("step", (seconds / 240).max(15).to_string()),
            ])
            .timeout(std::time::Duration::from_secs(30));
        if !config.bearer_token.is_empty() {
            request = request.bearer_auth(&config.bearer_token);
        }
        async move {
            let response = request.send().await.map_err(|_| {
                "Could not reach Prometheus history. Check its URL, proxy and TLS certificate."
                    .to_owned()
            })?;
            if !response.status().is_success() {
                return Err(format!(
                    "Prometheus history returned HTTP {}.",
                    response.status().as_u16()
                ));
            }
            let data: Value = response
                .json()
                .await
                .map_err(|_| "Prometheus returned invalid history JSON.".to_owned())?;
            if data["status"] != "success" || data["data"]["resultType"] != "matrix" {
                return Err(format!(
                    "Prometheus history requires a range vector. {}",
                    data["error"]
                        .as_str()
                        .unwrap_or("Check the configured query.")
                ));
            }
            Ok(data)
        }
    };
    let (cpu, memory) = tokio::join!(query(cpu.clone()), query(memory.clone()));
    let mut errors = Vec::new();
    let empty = serde_json::json!({"data":{"result":[]}});
    let mut usable = |result: Result<Value, String>, dimension| match result {
        Ok(value) => value,
        Err(error) => {
            errors.push(format!("Prometheus {dimension}: {error}"));
            empty.clone()
        }
    };
    let cpu = usable(cpu, "CPU");
    let memory = usable(memory, "memory");
    Ok((
        matrices(&cpu, &memory, object, node, selected, start, end)?,
        errors,
    ))
}
pub fn matrices(
    cpu: &Value,
    memory: &Value,
    object: &DynamicObject,
    node: bool,
    selected: Option<&str>,
    start: f64,
    end: f64,
) -> Result<Vec<Sample>, String> {
    let parse = |response: &Value| -> Result<BTreeMap<i64, f64>, String> {
        let entries = response["data"]["result"]
            .as_array()
            .ok_or("Prometheus has no history matrix.")?;
        let mut values = BTreeMap::new();
        for entry in entries {
            let metric = &entry["metric"];
            let matches = if node {
                if metric["node"].as_str().is_none() {
                    return Err("Node history queries must return a node label.".into());
                }
                metric["node"].as_str() == object.metadata.name.as_deref()
            } else {
                if metric["namespace"].as_str().is_none()
                    || metric["pod"].as_str().is_none()
                    || (selected.is_some() && metric["container"].as_str().is_none())
                {
                    return Err(
                        "Pod/container history queries require namespace/pod[/container] labels."
                            .into(),
                    );
                }
                metric["namespace"].as_str() == object.metadata.namespace.as_deref()
                    && metric["pod"].as_str() == object.metadata.name.as_deref()
                    && selected.is_none_or(|selected| metric["container"] == selected)
            };
            if !matches {
                continue;
            }
            for pair in entry["values"]
                .as_array()
                .ok_or("History series has no sample values.")?
            {
                if let (Some(at), Some(value)) = (
                    pair[0].as_f64(),
                    pair[1].as_str().and_then(|value| value.parse::<f64>().ok()),
                ) && at.is_finite()
                    && at >= start
                    && at <= end
                    && value.is_finite()
                    && value >= 0.
                {
                    *values.entry((at * 1000.).round() as i64).or_default() += value;
                }
            }
        }
        Ok(values)
    };
    let cpu = parse(cpu)?;
    let memory = parse(memory)?;
    let times = cpu
        .keys()
        .chain(memory.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    Ok(times
        .into_iter()
        .map(|at| Sample {
            at: at as f64 / 1000.,
            cpu: cpu.get(&at).copied(),
            memory: memory.get(&at).copied(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClusterId, Kind, test_support::Fixture};
    use serde_json::json;
    fn object(value: Value) -> DynamicObject {
        serde_json::from_value(value).unwrap()
    }
    fn pod() -> Value {
        json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"api","namespace":"default","uid":"api"},"spec":{"nodeName":"worker","containers":[{"name":"app","resources":{"requests":{"cpu":"250m","memory":"128Mi"},"limits":{"cpu":"1","memory":"1Gi"}}},{"name":"side","resources":{"requests":{"cpu":"50m"}}}],"initContainers":[{"name":"init","resources":{"requests":{"cpu":"500m"}}}]},"status":{"phase":"Running","containerStatuses":[{"name":"app","restartCount":2,"lastState":{"terminated":{"reason":"OOMKilled","exitCode":137,"finishedAt":"2026-10-13T00:00:00Z"}}}]}})
    }
    fn node() -> Value {
        json!({"apiVersion":"v1","kind":"Node","metadata":{"name":"worker","uid":"worker"},"status":{"allocatable":{"cpu":"4","memory":"8Gi"}}})
    }
    fn session(fixture: &Fixture) -> Arc<ClusterSession> {
        let kinds = [
            ("Pod", "pods", true),
            ("Node", "nodes", false),
            ("Event", "events", true),
        ]
        .into_iter()
        .map(|(kind, plural, namespaced)| Kind {
            resource: ApiResource::from_gvk_with_plural(
                &GroupVersionKind::gvk("", "v1", kind),
                plural,
            ),
            namespaced,
            verbs: vec!["list".into(), "get".into()],
        })
        .collect();
        Arc::new(ClusterSession::for_testing(
            ClusterId::new("resource-metrics"),
            fixture.url.clone(),
            kinds,
        ))
    }
    fn matrix(series: Value) -> Value {
        json!({"status":"success","data":{"resultType":"matrix","result":series}})
    }
    #[test]
    fn effective_requests_current_limits_and_container_scope() {
        let pod = object(pod());
        let total = reference(&pod, &[], None);
        assert_eq!(total.requests.cpu, Some(0.5));
        assert_eq!(total.requests.memory, Some(128. * 1024. * 1024.));
        assert_eq!(total.limits.cpu, Some(1.));
        let side = reference(&pod, &[], Some("side"));
        assert_eq!(side.requests.cpu, Some(0.05));
        assert_eq!(side.requests.memory, None);
        assert_eq!(side.limits.cpu, None);
        assert_eq!(reference(&pod, &[], Some("gone")).requests.cpu, None);
        let mut terminal = pod.clone();
        terminal.data["status"]["phase"] = json!("Succeeded");
        let total = reference(&object(node()), &[Arc::new(pod), Arc::new(terminal)], None);
        assert_eq!(total.requests.cpu, Some(0.5));
        assert_eq!(total.allocatable.cpu, Some(4.));
    }
    #[test]
    fn restart_markers_keep_exact_time_and_ignore_missing_termination_time() {
        let mut pod = object(pod());
        pod.data["status"]["initContainerStatuses"] =
            json!([{"name":"init","restartCount":3,"lastState":{"terminated":{"reason":"Error"}}}]);
        let (markers, restarts) = state_markers(&[Arc::new(pod.clone())], None);
        assert_eq!(markers.len(), 1);
        assert_eq!(markers[0].reason, "OOMKilled");
        assert_eq!(markers[0].at, time("2026-10-13T00:00:00Z").unwrap());
        assert_eq!(restarts.values().sum::<i64>(), 5);
        assert!(state_markers(&[Arc::new(pod)], Some("side")).0.is_empty());
    }
    #[test]
    fn history_filters_exact_identity_preserves_gaps_and_sums_matching_series() {
        let cpu = matrix(json!([
            {"metric":{"namespace":"other","pod":"api","container":"app"},"values":[[100,"999"]]},
            {"metric":{"namespace":"default","pod":"api","container":"side"},"values":[[100,"10"]]},
            {"metric":{"namespace":"default","pod":"api","container":"app"},"values":[[90,"3"],[100,"0.2"],[120,"NaN"],[130,"-1"]]},
            {"metric":{"namespace":"default","pod":"api","container":"app"},"values":[[100,"0.3"]]}
        ]));
        let memory = matrix(
            json!([{"metric":{"namespace":"default","pod":"api","container":"app"},"values":[[110,"128"]]}]),
        );
        let values = matrices(
            &cpu,
            &memory,
            &object(pod()),
            false,
            Some("app"),
            100.,
            140.,
        )
        .unwrap();
        assert_eq!(
            values,
            vec![
                Sample {
                    at: 100.,
                    cpu: Some(0.5),
                    memory: None
                },
                Sample {
                    at: 110.,
                    cpu: None,
                    memory: Some(128.)
                }
            ]
        );
        assert!(
            matrices(
                &matrix(json!([{"metric":{"pod":"api"},"values":[]} ])),
                &memory,
                &object(pod()),
                false,
                None,
                0.,
                200.
            )
            .unwrap_err()
            .contains("labels")
        );
    }
    #[tokio::test]
    async fn pod_api_uses_exact_metrics_target_and_keeps_partial_failures_missing() {
        let metrics = json!({"apiVersion":"metrics.k8s.io/v1beta1","kind":"PodMetrics","metadata":{"name":"api","namespace":"default"},"timestamp":"2026-10-13T00:00:15Z","containers":[{"name":"app","usage":{"cpu":"100m","memory":"64Mi"}},{"name":"side","usage":{"cpu":"25m"}}]});
        let fixture=Fixture::start(vec![pod(),metrics,json!({"apiVersion":"v1","kind":"Event","metadata":{"name":"oom","namespace":"default","uid":"oom"},"involvedObject":{"uid":"api"},"type":"Warning","reason":"BackOff","message":"restarting","lastTimestamp":"2026-10-13T00:00:00Z"})]).await;
        let session = session(&fixture);
        let snapshot = fetch(
            session.clone(),
            crate::resources::pod(),
            Some("default".into()),
            "api".into(),
            Some("api".into()),
            None,
            1800,
        )
        .await
        .unwrap();
        assert_eq!(snapshot.sample.cpu, Some(0.125));
        assert_eq!(snapshot.sample.memory, None);
        assert_eq!(snapshot.sample.at, time("2026-10-13T00:00:15Z").unwrap());
        assert_eq!(snapshot.markers.len(), 2);
        let selected = fetch(
            session.clone(),
            crate::resources::pod(),
            Some("default".into()),
            "api".into(),
            Some("api".into()),
            Some("app".into()),
            1800,
        )
        .await
        .unwrap();
        assert_eq!(selected.sample.memory, Some(64. * 1024. * 1024.));
        let missing = fetch(
            session.clone(),
            crate::resources::pod(),
            Some("default".into()),
            "api".into(),
            Some("api".into()),
            Some("init".into()),
            1800,
        )
        .await
        .unwrap();
        assert_eq!(missing.sample.cpu, None);
        assert!(
            fetch(
                session,
                crate::resources::pod(),
                Some("default".into()),
                "api".into(),
                Some("old-uid".into()),
                None,
                1800
            )
            .await
            .unwrap_err()
            .contains("replaced")
        );
        let requests = fixture.requests.lock().unwrap();
        assert!(requests.iter().any(|(line, _)| {
            line.starts_with("GET /apis/metrics.k8s.io/v1beta1/namespaces/default/pods/api ")
        }));
        assert!(requests.iter().all(|(line, _)| line.starts_with("GET ")));
    }
    #[tokio::test]
    async fn node_partial_permissions_preserve_usage_but_do_not_invent_requests() {
        let fixture=Fixture::start(vec![node(),json!({"apiVersion":"metrics.k8s.io/v1beta1","kind":"NodeMetrics","metadata":{"name":"worker"},"timestamp":"2026-10-13T00:00:15Z","usage":{"cpu":"1","memory":"2Gi"}})]).await;
        fixture.deny("pods");
        let snapshot = fetch(
            session(&fixture),
            ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk("", "v1", "Node"), "nodes"),
            None,
            "worker".into(),
            Some("worker".into()),
            None,
            1800,
        )
        .await
        .unwrap();
        assert_eq!(snapshot.sample.cpu, Some(1.));
        assert_eq!(snapshot.reference.requests.cpu, None);
        assert_eq!(snapshot.reference.allocatable.cpu, Some(4.));
        assert!(snapshot.errors.iter().any(|error| error.contains("403")));
    }
    #[tokio::test]
    async fn node_scheduled_requests_filter_node_and_terminal_pods() {
        let mut other = pod();
        other["metadata"]["name"] = json!("other");
        other["spec"]["nodeName"] = json!("other");
        let fixture = Fixture::start(vec![node(), pod(), other]).await;
        let snapshot = fetch(
            session(&fixture),
            ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk("", "v1", "Node"), "nodes"),
            None,
            "worker".into(),
            Some("worker".into()),
            None,
            1800,
        )
        .await
        .unwrap();
        assert_eq!(snapshot.reference.requests.cpu, Some(0.5));
        assert!(snapshot.sample.cpu.is_none());
        assert!(!snapshot.errors.is_empty());
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.contains("fieldSelector=spec.nodeName%3Dworker"))
        );
    }
    #[tokio::test]
    async fn prometheus_container_queries_keep_auth_base_path_range_and_error_messages() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut seen = vec![];
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 16384];
                let n = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..n]).to_lowercase();
                assert!(request.starts_with("get /prom/api/v1/query_range?"));
                assert!(request.contains("start=100"));
                assert!(request.contains("end=200"));
                assert!(request.contains("step=15"));
                assert!(request.contains("authorization: bearer fixture-token"));
                seen.push(request);
                let body = r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{"namespace":"default","pod":"api","container":"app"},"values":[[150,"1"]]}]}}"#;
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
            assert!(seen.iter().any(|r| r.contains("container_cpu")));
            assert!(seen.iter().any(|r| r.contains("container_mem")));
            assert!(seen.iter().all(|r| r.contains("label_replace")
                && r.contains("namespace")
                && r.contains("container")));
        });
        let config = Prometheus {
            url: format!("http://{address}/prom"),
            bearer_token: "fixture-token".into(),
            container_cpu: "container_cpu".into(),
            container_memory: "container_mem".into(),
            ..Default::default()
        };
        let (values, errors) = prometheus_history(
            &Proxy::Direct,
            &config,
            &object(pod()),
            false,
            Some("app"),
            (100., 200.),
            1800,
        )
        .await
        .unwrap();
        assert!(errors.is_empty());
        assert_eq!(
            values[0],
            Sample {
                at: 150.,
                cpu: Some(1.),
                memory: Some(1.)
            }
        );
        server.await.unwrap();
    }
    #[tokio::test]
    async fn prometheus_partial_history_keeps_cpu_when_memory_query_fails() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 16384];
                let n = socket.read(&mut request).await.unwrap();
                let request = String::from_utf8_lossy(&request[..n]);
                let (status, body) = if request.contains("container_cpu") {
                    (
                        "200 OK",
                        r#"{"status":"success","data":{"resultType":"matrix","result":[{"metric":{"namespace":"default","pod":"api","container":"app"},"values":[[150,"0.2"]]}]}}"#,
                    )
                } else {
                    ("403 Forbidden", "{}")
                };
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let config = Prometheus {
            url: format!("http://{address}"),
            container_cpu: "container_cpu".into(),
            container_memory: "container_memory".into(),
            ..Default::default()
        };
        let (values, errors) = prometheus_history(
            &Proxy::Direct,
            &config,
            &object(pod()),
            false,
            Some("app"),
            (100., 200.),
            1800,
        )
        .await
        .unwrap();
        assert_eq!(
            values,
            vec![Sample {
                at: 150.,
                cpu: Some(0.2),
                memory: None
            }]
        );
        assert!(errors[0].contains("memory"));
        assert!(errors[0].contains("403"));
        server.await.unwrap();
    }
    #[test]
    fn legacy_prometheus_settings_get_container_defaults() {
        let legacy: Prometheus = serde_json::from_value(
            json!({"url":"https://prom.example","pod_cpu":"custom_cpu","node_cpu":"custom_node"}),
        )
        .unwrap();
        assert_eq!(legacy.pod_cpu, "custom_cpu");
        assert!(legacy.container_cpu.contains("container"));
        assert!(legacy.container_memory.contains("namespace"));
        assert!(legacy.validate().is_ok());
        let mut broken = legacy;
        broken.container_memory.clear();
        assert!(broken.validate().unwrap_err().contains("six"));
    }
}
