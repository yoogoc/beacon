//! What kubectl prints in a Pod's `READY`, `STATUS` and `RESTARTS` columns.
//!
//! This is a port of `printPod` in `k8s.io/kubernetes/pkg/printers/internalversion`.
//! None of it can be derived from a single field: `STATUS` is assembled from
//! the phase, the deletion timestamp, the pod conditions and the state of every
//! container, with init containers taking precedence over the rest. A pod stuck
//! pulling its second init container reads `Init:1/3`, and nothing in the API
//! response says that.
//!
//! Getting it exactly right matters because these columns are read next to a
//! terminal running `kubectl get pods`. A cell that disagrees is a bug report.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, Time};
use k8s_openapi::jiff::Timestamp;
use serde::Deserialize;

use crate::age::format_duration;

/// The three derived columns, computed together.
///
/// They share a traversal of the container statuses, and two of them feed each
/// other -- `ready` counts containers that the `status` pass walks anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodSummary {
    /// `2/3`: ready containers over the pod's container count.
    pub ready: String,
    /// `Running`, `Init:1/2`, `CrashLoopBackOff`, `Terminating`...
    pub status: String,
    /// `0`, or `5 (8d ago)` when a container has restarted.
    pub restarts: String,
}

/// The phase the API server reported, before any of the corrections below.
///
/// The caller uses this to pick a colour: the corrected `status` string has too
/// many values to match on, but they all collapse onto a phase.
pub fn phase(data: &serde_json::Value) -> Option<String> {
    let pod: PodView = view(data);
    pod.status.phase
}

/// Computes all three columns for one pod.
///
/// `data` is the object minus `apiVersion`, `kind` and `metadata` -- that is,
/// `{"spec": ..., "status": ...}`.
pub fn summarize(metadata: &ObjectMeta, data: &serde_json::Value, now: Timestamp) -> PodSummary {
    let pod: PodView = view(data);
    let status = &pod.status;

    let total_containers = pod.spec.containers.len();
    let mut ready_containers = 0usize;

    let mut reason = status
        .reason
        .clone()
        .or_else(|| status.phase.clone())
        .unwrap_or_default();

    // A pod held by a scheduling gate is Pending with nothing to explain it.
    if status.conditions.iter().any(|condition| {
        condition.kind == "PodScheduled" && condition.reason.as_deref() == Some(SCHEDULING_GATED)
    }) {
        reason = SCHEDULING_GATED.to_string();
    }

    let mut restarts = 0i64;
    let mut last_restart: Option<Timestamp> = None;
    // Sidecars (init containers with `restartPolicy: Always`) keep running for
    // the pod's whole life, so their restarts belong to the steady-state count
    // rather than to initialization.
    let mut sidecar_restarts = 0i64;
    let mut last_sidecar_restart: Option<Timestamp> = None;
    let mut initializing = false;

    for (index, container) in status.init_container_statuses.iter().enumerate() {
        let is_sidecar = pod
            .spec
            .init_containers
            .get(index)
            .is_some_and(|spec| spec.restart_policy.as_deref() == Some("Always"));

        restarts += container.restart_count;
        merge_later(&mut last_restart, container.last_terminated_at());
        if is_sidecar {
            sidecar_restarts += container.restart_count;
            merge_later(&mut last_sidecar_restart, container.last_terminated_at());
        }

        match &container.state {
            // Finished successfully: this one is done, look at the next.
            state if state.terminated.as_ref().is_some_and(|t| t.exit_code == 0) => continue,
            // A running sidecar does not hold up initialization, and it counts
            // towards READY like an ordinary container.
            _ if is_sidecar && container.started == Some(true) => {
                if container.ready {
                    ready_containers += 1;
                }
                continue;
            }
            ContainerState {
                terminated: Some(terminated),
                ..
            } => {
                reason = format!("Init:{}", terminated.describe());
                initializing = true;
            }
            ContainerState {
                waiting: Some(waiting),
                ..
            } if waiting
                .reason
                .as_deref()
                .is_some_and(|reason| !reason.is_empty() && reason != "PodInitializing") =>
            {
                reason = format!("Init:{}", waiting.reason.clone().unwrap_or_default());
                initializing = true;
            }
            _ => {
                reason = format!("Init:{index}/{}", pod.spec.init_containers.len());
                initializing = true;
            }
        }
        break;
    }

    // Once the pod is past initialization -- or has finished for good -- the
    // ordinary containers decide what it says.
    if !initializing || is_terminal(status.phase.as_deref()) {
        restarts = sidecar_restarts;
        last_restart = last_sidecar_restart;
        let mut has_running = false;

        // Backwards, so that the first container's state wins when several
        // disagree. kubectl does the same, and matching it is the point.
        for container in status.container_statuses.iter().rev() {
            restarts += container.restart_count;
            merge_later(&mut last_restart, container.last_terminated_at());

            match &container.state {
                ContainerState {
                    waiting: Some(waiting),
                    ..
                } if waiting.reason.as_deref().is_some_and(|r| !r.is_empty()) => {
                    reason = waiting.reason.clone().unwrap_or_default();
                }
                ContainerState {
                    terminated: Some(terminated),
                    ..
                } => {
                    reason = terminated.describe();
                }
                ContainerState {
                    running: Some(_), ..
                } if container.ready => {
                    has_running = true;
                    ready_containers += 1;
                }
                _ => {}
            }
        }

        // A pod whose last container exited while others still run is not
        // Completed; whether it is Running depends on its Ready condition.
        if reason == "Completed" && has_running {
            reason = if has_ready_condition(&status.conditions) {
                "Running".to_string()
            } else {
                "NotReady".to_string()
            };
        }
    }

    // Deletion wins over everything: a terminating pod still reports Running.
    if metadata.deletion_timestamp.is_some() {
        reason = if status.reason.as_deref() == Some(NODE_UNREACHABLE) {
            "Unknown".to_string()
        } else if !is_terminal(status.phase.as_deref()) {
            "Terminating".to_string()
        } else {
            reason
        };
    }

    PodSummary {
        ready: format!("{ready_containers}/{total_containers}"),
        status: reason,
        restarts: format_restarts(restarts, last_restart, now),
    }
}

/// `5 (8d ago)` -- the count plus how long ago the most recent one was.
fn format_restarts(restarts: i64, last: Option<Timestamp>, now: Timestamp) -> String {
    match last {
        Some(last) if restarts != 0 => {
            let ago = format_duration(now.duration_since(last).as_secs());
            format!("{restarts} ({ago} ago)")
        }
        _ => restarts.to_string(),
    }
}

/// A pod that has reached a terminal phase will not change again on its own.
fn is_terminal(phase: Option<&str>) -> bool {
    matches!(phase, Some("Failed" | "Succeeded"))
}

fn has_ready_condition(conditions: &[Condition]) -> bool {
    conditions
        .iter()
        .any(|condition| condition.kind == "Ready" && condition.status == "True")
}

fn merge_later(current: &mut Option<Timestamp>, candidate: Option<Timestamp>) {
    if let Some(candidate) = candidate
        && current.is_none_or(|current| candidate > current)
    {
        *current = Some(candidate);
    }
}

fn view(data: &serde_json::Value) -> PodView {
    // A pod whose shape we cannot read is a pod with no containers and no
    // phase, which renders as `0/0` with an empty status -- visibly wrong,
    // rather than a row that silently disappears.
    serde_json::from_value(data.clone()).unwrap_or_default()
}

/// `apiv1.PodReasonSchedulingGated`.
const SCHEDULING_GATED: &str = "SchedulingGated";
/// `node.NodeUnreachablePodReason`.
const NODE_UNREACHABLE: &str = "NodeLost";

// The projection of a Pod that these columns read.
//
// Deliberately not `k8s_openapi::api::core::v1::Pod`: this runs for every
// visible row, and deserializing a full PodSpec to count containers is far more
// work than the few fields below.

#[derive(Debug, Default, Deserialize)]
struct PodView {
    #[serde(default)]
    spec: PodSpecView,
    #[serde(default)]
    status: PodStatusView,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PodSpecView {
    #[serde(default)]
    containers: Vec<ContainerSpecView>,
    #[serde(default)]
    init_containers: Vec<ContainerSpecView>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContainerSpecView {
    #[serde(default)]
    restart_policy: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PodStatusView {
    phase: Option<String>,
    reason: Option<String>,
    #[serde(default)]
    conditions: Vec<Condition>,
    #[serde(default)]
    container_statuses: Vec<ContainerStatusView>,
    #[serde(default)]
    init_container_statuses: Vec<ContainerStatusView>,
}

#[derive(Debug, Default, Deserialize)]
struct Condition {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContainerStatusView {
    #[serde(default)]
    ready: bool,
    /// Only meaningful for init containers, where it separates a sidecar that
    /// has started from one that has not.
    #[serde(default)]
    started: Option<bool>,
    #[serde(default)]
    restart_count: i64,
    #[serde(default)]
    state: ContainerState,
    #[serde(default)]
    last_state: ContainerState,
}

impl ContainerStatusView {
    fn last_terminated_at(&self) -> Option<Timestamp> {
        self.last_state
            .terminated
            .as_ref()
            .and_then(|terminated| terminated.finished_at.as_ref())
            .map(|time| time.0)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContainerState {
    running: Option<Running>,
    terminated: Option<Terminated>,
    waiting: Option<Waiting>,
}

#[derive(Debug, Default, Deserialize)]
struct Running {}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Terminated {
    #[serde(default)]
    exit_code: i32,
    #[serde(default)]
    signal: i32,
    #[serde(default)]
    reason: Option<String>,
    finished_at: Option<Time>,
}

impl Terminated {
    /// How kubectl names this termination: the reason if the kubelet gave one,
    /// otherwise the signal or exit code that has to stand in for it.
    fn describe(&self) -> String {
        match self.reason.as_deref() {
            Some(reason) if !reason.is_empty() => reason.to_string(),
            _ if self.signal != 0 => format!("Signal:{}", self.signal),
            _ => format!("ExitCode:{}", self.exit_code),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Waiting {
    #[serde(default)]
    reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> Timestamp {
        "2026-09-20T12:00:00Z".parse().expect("fixed clock")
    }

    fn summary(data: serde_json::Value) -> PodSummary {
        summarize(&ObjectMeta::default(), &data, now())
    }

    fn terminating(data: serde_json::Value) -> PodSummary {
        let metadata = ObjectMeta {
            deletion_timestamp: Some(Time(now())),
            ..Default::default()
        };
        summarize(&metadata, &data, now())
    }

    fn running_pod(containers: usize, ready: usize) -> serde_json::Value {
        json!({
            "spec": { "containers": vec![json!({}); containers] },
            "status": {
                "phase": "Running",
                "conditions": [{ "type": "Ready", "status": "True" }],
                "containerStatuses": (0..containers)
                    .map(|i| json!({
                        "ready": i < ready,
                        "restartCount": 0,
                        "state": { "running": { "startedAt": "2026-09-01T00:00:00Z" } }
                    }))
                    .collect::<Vec<_>>()
            }
        })
    }

    #[test]
    fn a_healthy_pod() {
        let summary = summary(running_pod(2, 2));
        assert_eq!(summary.ready, "2/2");
        assert_eq!(summary.status, "Running");
        assert_eq!(summary.restarts, "0");
    }

    /// A container that is running but not ready still counts against READY.
    #[test]
    fn a_container_that_is_not_ready_is_not_counted() {
        assert_eq!(summary(running_pod(2, 1)).ready, "1/2");
    }

    #[test]
    fn a_crash_looping_container_shows_the_waiting_reason() {
        let summary = summary(json!({
            "spec": { "containers": [{}] },
            "status": {
                "phase": "Running",
                "containerStatuses": [{
                    "ready": false,
                    "restartCount": 5,
                    "state": { "waiting": { "reason": "CrashLoopBackOff" } },
                    "lastState": { "terminated": {
                        "exitCode": 1,
                        "finishedAt": "2026-09-12T12:00:00Z"
                    }}
                }]
            }
        }));

        assert_eq!(summary.ready, "0/1");
        assert_eq!(summary.status, "CrashLoopBackOff");
        assert_eq!(summary.restarts, "5 (8d ago)");
    }

    /// The exact shape of the local cluster's completed Argo workflow pods.
    #[test]
    fn a_completed_pod() {
        let summary = summary(json!({
            "spec": { "containers": [{}, {}] },
            "status": {
                "phase": "Succeeded",
                "containerStatuses": [
                    { "ready": false, "restartCount": 0,
                      "state": { "terminated": { "exitCode": 0, "reason": "Completed" } } },
                    { "ready": false, "restartCount": 0,
                      "state": { "terminated": { "exitCode": 0, "reason": "Completed" } } }
                ]
            }
        }));

        assert_eq!(summary.ready, "0/2");
        assert_eq!(summary.status, "Completed");
    }

    #[test]
    fn a_failed_pod_reports_its_exit_code_when_there_is_no_reason() {
        let summary = summary(json!({
            "spec": { "containers": [{}] },
            "status": {
                "phase": "Failed",
                "containerStatuses": [{
                    "ready": false, "restartCount": 0,
                    "state": { "terminated": { "exitCode": 137 } }
                }]
            }
        }));

        assert_eq!(summary.status, "ExitCode:137");
    }

    #[test]
    fn a_pod_killed_by_a_signal_reports_the_signal() {
        let summary = summary(json!({
            "spec": { "containers": [{}] },
            "status": {
                "phase": "Failed",
                "containerStatuses": [{
                    "ready": false, "restartCount": 0,
                    "state": { "terminated": { "exitCode": 0, "signal": 9 } }
                }]
            }
        }));

        assert_eq!(summary.status, "Signal:9");
    }

    /// Init containers take over the status line, and the index is how far
    /// along initialization got.
    #[test]
    fn an_initializing_pod_counts_its_init_containers() {
        let summary = summary(json!({
            "spec": { "containers": [{}], "initContainers": [{}, {}, {}] },
            "status": {
                "phase": "Pending",
                "initContainerStatuses": [
                    { "ready": true, "restartCount": 0,
                      "state": { "terminated": { "exitCode": 0, "reason": "Completed" } } },
                    { "ready": false, "restartCount": 0,
                      "state": { "waiting": { "reason": "PodInitializing" } } },
                    { "ready": false, "restartCount": 0, "state": {} }
                ]
            }
        }));

        assert_eq!(summary.status, "Init:1/3");
        assert_eq!(summary.ready, "0/1");
    }

    #[test]
    fn a_failing_init_container_names_its_reason() {
        let summary = summary(json!({
            "spec": { "containers": [{}], "initContainers": [{}] },
            "status": {
                "phase": "Pending",
                "initContainerStatuses": [{
                    "ready": false, "restartCount": 3,
                    "state": { "waiting": { "reason": "ImagePullBackOff" } }
                }]
            }
        }));

        assert_eq!(summary.status, "Init:ImagePullBackOff");
        assert_eq!(summary.restarts, "3");
    }

    /// A sidecar keeps running while the pod does, so it neither blocks
    /// initialization nor gets left out of READY.
    #[test]
    fn a_started_sidecar_does_not_block_initialization() {
        let summary = summary(json!({
            "spec": {
                "containers": [{}],
                "initContainers": [{ "restartPolicy": "Always" }]
            },
            "status": {
                "phase": "Running",
                "conditions": [{ "type": "Ready", "status": "True" }],
                "initContainerStatuses": [{
                    "ready": true, "started": true, "restartCount": 0,
                    "state": { "running": { "startedAt": "2026-09-01T00:00:00Z" } }
                }],
                "containerStatuses": [{
                    "ready": true, "restartCount": 0,
                    "state": { "running": { "startedAt": "2026-09-01T00:00:00Z" } }
                }]
            }
        }));

        assert_eq!(summary.status, "Running");
        // READY counts the sidecar, but the denominator is `spec.containers`,
        // exactly as kubectl reports it.
        assert_eq!(summary.ready, "2/1");
    }

    #[test]
    fn a_deleted_pod_is_terminating() {
        assert_eq!(terminating(running_pod(1, 1)).status, "Terminating");
    }

    /// A pod on a node that stopped reporting shows Unknown, not Terminating:
    /// nothing is actually tearing it down.
    #[test]
    fn a_pod_on_a_lost_node_is_unknown() {
        let summary = terminating(json!({
            "spec": { "containers": [{}] },
            "status": { "phase": "Running", "reason": "NodeLost" }
        }));

        assert_eq!(summary.status, "Unknown");
    }

    /// A pod that already finished keeps its final status when deleted.
    #[test]
    fn deleting_a_finished_pod_does_not_say_terminating() {
        let summary = terminating(json!({
            "spec": { "containers": [{}] },
            "status": {
                "phase": "Succeeded",
                "containerStatuses": [{
                    "ready": false, "restartCount": 0,
                    "state": { "terminated": { "exitCode": 0, "reason": "Completed" } }
                }]
            }
        }));

        assert_eq!(summary.status, "Completed");
    }

    /// One container exiting does not finish a pod whose others still run.
    #[test]
    fn a_pod_with_one_finished_container_is_still_running() {
        let summary = summary(json!({
            "spec": { "containers": [{}, {}] },
            "status": {
                "phase": "Running",
                "conditions": [{ "type": "Ready", "status": "True" }],
                "containerStatuses": [
                    { "ready": true, "restartCount": 0,
                      "state": { "running": { "startedAt": "2026-09-01T00:00:00Z" } } },
                    { "ready": false, "restartCount": 0,
                      "state": { "terminated": { "exitCode": 0, "reason": "Completed" } } }
                ]
            }
        }));

        assert_eq!(summary.status, "Running");
        assert_eq!(summary.ready, "1/2");
    }

    #[test]
    fn a_scheduling_gate_is_the_reason_a_pod_is_pending() {
        let summary = summary(json!({
            "spec": { "containers": [{}] },
            "status": {
                "phase": "Pending",
                "conditions": [{
                    "type": "PodScheduled", "status": "False", "reason": "SchedulingGated"
                }]
            }
        }));

        assert_eq!(summary.status, "SchedulingGated");
    }

    /// An object we cannot parse must still produce a row.
    #[test]
    fn an_unreadable_pod_renders_as_empty_rather_than_panicking() {
        let summary = summary(json!("not a pod"));
        assert_eq!(summary.ready, "0/0");
        assert_eq!(summary.restarts, "0");
    }
}
