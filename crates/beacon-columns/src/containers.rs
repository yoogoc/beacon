//! Per-container states, matched by name rather than API response order.
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerHealth {
    Ready,
    Running,
    Waiting,
    Failed,
    Completed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSummary {
    pub name: String,
    pub init: bool,
    pub ephemeral: bool,
    pub state: String,
    pub health: ContainerHealth,
    pub ready: bool,
    pub restarts: i64,
    pub message: Option<String>,
}

impl ContainerSummary {
    pub fn description(&self) -> String {
        format!(
            "{}{}: {} · {} · {} restarts{}",
            if self.init {
                "Init "
            } else if self.ephemeral {
                "Ephemeral "
            } else {
                ""
            },
            self.name,
            self.state,
            if self.ready { "Ready" } else { "Not ready" },
            self.restarts,
            self.message
                .as_ref()
                .map(|s| format!(" — {s}"))
                .unwrap_or_default()
        )
    }
}

pub fn summarize(data: &Value) -> Vec<ContainerSummary> {
    let mut result = Vec::new();
    for (spec_key, status_key, init, ephemeral) in [
        ("initContainers", "initContainerStatuses", true, false),
        ("containers", "containerStatuses", false, false),
        (
            "ephemeralContainers",
            "ephemeralContainerStatuses",
            false,
            true,
        ),
    ] {
        for spec in data["spec"][spec_key].as_array().into_iter().flatten() {
            let name = spec["name"].as_str().unwrap_or_default();
            let status = data["status"][status_key]
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| s["name"].as_str() == Some(name));
            let ready = status.and_then(|s| s["ready"].as_bool()).unwrap_or(false);
            let restarts = status.and_then(|s| s["restartCount"].as_i64()).unwrap_or(0);
            let (state, health, detail) = if let Some(s) = status {
                if let Some(t) = s["state"].get("terminated") {
                    let code = t["exitCode"].as_i64();
                    let state = t["reason"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            if code == Some(0) {
                                "Completed".into()
                            } else {
                                format!(
                                    "ExitCode:{}",
                                    code.map(|v| v.to_string())
                                        .unwrap_or_else(|| "unknown".into())
                                )
                            }
                        });
                    (
                        state,
                        if code == Some(0) {
                            ContainerHealth::Completed
                        } else {
                            ContainerHealth::Failed
                        },
                        Some(t),
                    )
                } else if let Some(w) = s["state"].get("waiting") {
                    let reason = w["reason"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("Waiting");
                    let failed = matches!(
                        reason,
                        "CrashLoopBackOff"
                            | "ImagePullBackOff"
                            | "ErrImagePull"
                            | "ErrImageNeverPull"
                            | "CreateContainerConfigError"
                            | "CreateContainerError"
                            | "InvalidImageName"
                            | "RunContainerError"
                    );
                    (
                        reason.into(),
                        if failed {
                            ContainerHealth::Failed
                        } else {
                            ContainerHealth::Waiting
                        },
                        Some(w),
                    )
                } else if s["state"].get("running").is_some() {
                    (
                        "Running".into(),
                        if ready {
                            ContainerHealth::Ready
                        } else {
                            ContainerHealth::Running
                        },
                        None,
                    )
                } else {
                    ("Unknown".into(), ContainerHealth::Unknown, None)
                }
            } else {
                ("Pending".into(), ContainerHealth::Waiting, None)
            };
            result.push(ContainerSummary {
                name: name.into(),
                init,
                ephemeral,
                state,
                health,
                ready,
                restarts,
                message: detail
                    .and_then(|d| d["message"].as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            });
        }
    }
    result
}

pub fn description(data: &Value) -> String {
    let containers = summarize(data);
    if containers.is_empty() {
        return "<none>".into();
    }
    containers
        .iter()
        .map(ContainerSummary::description)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn includes_missing_statuses_and_matches_names() {
        let data = json!({"spec":{"containers":[{"name":"api"},{"name":"worker"}],"initContainers":[{"name":"migrate"}]},
            "status":{"containerStatuses":[{"name":"worker","ready":false,"state":{"waiting":{"reason":"CrashLoopBackOff"}}},{"name":"api","ready":true,"state":{"running":{}}}]}});
        let states = summarize(&data);
        assert!(states[0].init);
        assert_eq!(states[0].health, ContainerHealth::Waiting);
        assert_eq!(states[1].health, ContainerHealth::Ready);
        assert_eq!(states[2].health, ContainerHealth::Failed);
    }
    #[test]
    fn completed_init_and_running_sidecar_are_not_failed() {
        let data = json!({"spec":{"initContainers":[{"name":"migrate"},{"name":"sidecar","restartPolicy":"Always"}]},
            "status":{"initContainerStatuses":[{"name":"migrate","state":{"terminated":{"exitCode":0}}},{"name":"sidecar","ready":true,"state":{"running":{}}}]}});
        let states = summarize(&data);
        assert_eq!(states[0].health, ContainerHealth::Completed);
        assert_eq!(states[1].health, ContainerHealth::Ready);
        assert!(states.iter().all(|s| s.init));
    }
}
