//! What can be done to the selected object, and whether this user may do it.
//!
//! Beacon asks the cluster before it offers. An action that would be refused is
//! shown disabled with the reason attached, rather than enabled and then
//! failing — the difference between a client that knows where you stand and one
//! that lets you find out by trying.
//!
//! Two reasons an action may be missing, and they are not the same thing. A
//! `ConfigMap` has no replicas, so `Scale` is not offered at all. A user
//! without `delete` on pods sees `Delete`, greyed, saying so.

use beacon_kube::{DynamicObject, Kind, Operation, Rules, workflow_ops::Action};

/// One entry in the actions menu.
#[derive(Debug, Clone)]
pub struct Choice {
    pub operation: Operation,
    pub label: String,
    pub allowed: bool,
    /// Why not, for the tooltip. `None` when it is allowed.
    pub blocked_because: Option<String>,
}

impl Choice {
    /// What the palette and the context menu both show.
    pub fn tooltip(&self) -> Option<&str> {
        self.blocked_because.as_deref()
    }
}

/// The actions that make sense for a kind, each marked with whether this user
/// may perform it.
///
/// `rules` is `None` while the permission answer is still in flight. Everything
/// is enabled until it arrives: a menu that flickers from disabled to enabled
/// teaches people to distrust the greying.
///
/// `replicas` is the object's current replica count, so that a scale prompt can
/// open on the number that is actually set.
pub fn available(kind: &Kind, rules: Option<&Rules>, replicas: i32) -> Vec<Choice> {
    available_for(kind, rules, replicas, false)
}

/// Schedule actions reflect the object's current suspension state.
pub fn available_for(
    kind: &Kind,
    rules: Option<&Rules>,
    replicas: i32,
    suspended: bool,
) -> Vec<Choice> {
    available_for_target(kind, rules, replicas, suspended, None)
}

pub fn available_for_target(
    kind: &Kind,
    rules: Option<&Rules>,
    replicas: i32,
    suspended: bool,
    target: Option<&str>,
) -> Vec<Choice> {
    let group = kind.resource.group.as_str();
    let name = kind.resource.kind.as_str();

    [
        Operation::Restart,
        Operation::Scale { replicas },
        Operation::RunJob,
        Operation::CronJobSuspend {
            suspend: !suspended,
        },
        Operation::NodeCordon {
            unschedulable: !suspended,
        },
        Operation::NodeDrain,
        Operation::Delete,
    ]
    .into_iter()
    .filter(|operation| operation.applies_to(group, name))
    .map(|operation| {
        let allowed = rules.is_none_or(|rules| {
            if matches!(
                operation,
                Operation::CronJobSuspend { .. }
                    | Operation::NodeCordon { .. }
                    | Operation::NodeDrain
            ) && let Some(target) = target
            {
                return rules.allows_named(
                    operation.verb(),
                    group,
                    &operation.resource(&kind.resource.plural),
                    target,
                );
            }
            rules.allows(
                operation.verb(),
                group,
                &operation.resource(&kind.resource.plural),
            )
        });

        Choice {
            label: if operation == Operation::RunJob && name == "CronJob" {
                "Run now…".into()
            } else {
                label(&operation)
            },
            blocked_because: (!allowed).then(|| refusal(&operation, kind)),
            allowed,
            operation,
        }
    })
    .collect()
}

/// Canonical resource actions, filtered using the selected object's live state.
pub fn available_for_object(
    kind: &Kind,
    rules: Option<&Rules>,
    replicas: i32,
    object: &DynamicObject,
) -> Vec<Choice> {
    let suspended = object
        .data
        .pointer(
            if kind.resource.group.is_empty() && kind.resource.kind == "Node" {
                "/spec/unschedulable"
            } else {
                "/spec/suspend"
            },
        )
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let mut choices = available_for_target(
        kind,
        rules,
        replicas,
        suspended,
        object.metadata.name.as_deref(),
    );
    let mut workflow = Vec::new();
    for action in [
        Action::Submit,
        Action::Retry,
        Action::Resubmit,
        Action::Suspend,
        Action::Resume,
        Action::Stop,
        Action::Terminate,
    ] {
        if !action.applies(&kind.resource.group, &kind.resource.kind)
            || !action.available(&kind.resource.kind, object)
        {
            continue;
        }
        let allowed = rules.is_none_or(|rules| {
            // The cluster template's destination namespace is chosen in the
            // dialog. Cluster-scope rules cannot answer that namespace question.
            if action.creates() {
                kind.resource.kind == "ClusterWorkflowTemplate"
                    || rules.allows("create", "argoproj.io", "workflows")
            } else {
                rules.allows_named(
                    "patch",
                    "argoproj.io",
                    "workflows",
                    object.metadata.name.as_deref().unwrap_or(""),
                )
            }
        });
        workflow.push(Choice {
            operation: Operation::Workflow { action },
            label: format!("{}…", action.label()),
            allowed,
            blocked_because: (!allowed).then(|| {
                format!(
                    "You do not have {} on argoproj.io/workflows in this namespace",
                    action.verb()
                )
            }),
        });
    }
    workflow.append(&mut choices);
    workflow
}

/// Whether this user may apply an edited manifest for a kind.
///
/// Separate from [`available`] because Apply is reached from the YAML pane
/// rather than from a menu.
pub fn may_apply(kind: &Kind, rules: Option<&Rules>) -> bool {
    rules.is_none_or(|rules| {
        rules.allows(
            Operation::Apply.verb(),
            &kind.resource.group,
            &kind.resource.plural,
        )
    })
}

/// Whether this user may open a shell or run a command in a pod.
///
/// `pods/exec` is a subresource with its own rule, so being able to read a
/// pod says nothing about it -- which is exactly why a refusal arrives as a
/// WebSocket that will not upgrade rather than as anything legible.
pub fn may_exec(rules: Option<&Rules>) -> bool {
    // Older API servers accept GET alone for WebSocket exec; SPDY uses
    // CREATE. This is only a hint: incomplete EKS webhook rules cannot prove
    // permission, and the API server remains authoritative.
    rules.is_none_or(|rules| {
        rules.allows("get", "", "pods/exec") || rules.allows("create", "", "pods/exec")
    })
}

/// The container ports a pod declares, deduplicated and in order.
///
/// Read from the spec rather than asked for, because the pod already said.
pub fn ports(data: &serde_json::Value) -> Vec<u16> {
    let Some(containers) = data
        .get("spec")
        .and_then(|spec| spec.get("containers"))
        .and_then(|containers| containers.as_array())
    else {
        return Vec::new();
    };

    let mut ports = Vec::new();
    for container in containers {
        let Some(declared) = container.get("ports").and_then(|ports| ports.as_array()) else {
            continue;
        };
        for port in declared {
            let Some(number) = port
                .get("containerPort")
                .and_then(serde_json::Value::as_i64)
                .and_then(|number| u16::try_from(number).ok())
            else {
                continue;
            };
            if !ports.contains(&number) {
                ports.push(number);
            }
        }
    }
    ports
}

fn label(operation: &Operation) -> String {
    match operation {
        Operation::Delete => "Delete".to_string(),
        Operation::Restart => "Restart".to_string(),
        // The number is asked for, so the label ends in an ellipsis the way a
        // menu item that opens a prompt should.
        Operation::Scale { .. } => "Scale…".to_string(),
        Operation::Apply => "Apply".to_string(),
        Operation::RunJob => "Rerun as new Job…".to_string(),
        Operation::CronJobSuspend { suspend: true } => "Suspend schedule…".to_string(),
        Operation::CronJobSuspend { suspend: false } => "Resume schedule…".to_string(),
        Operation::NodeCordon {
            unschedulable: true,
        } => "Cordon…".into(),
        Operation::NodeCordon {
            unschedulable: false,
        } => "Uncordon…".into(),
        Operation::NodeDrain => "Drain…".into(),
        Operation::Workflow { action } => format!("{}…", action.label()),
    }
}

/// The sentence shown on a disabled action.
///
/// Phrased as the RBAC question that was asked, because that is what somebody
/// has to take to whoever grants them access.
fn refusal(operation: &Operation, kind: &Kind) -> String {
    format!(
        "You do not have {} on {} in {}",
        operation.verb(),
        operation.resource(&kind.resource.plural),
        if kind.namespaced {
            "this namespace"
        } else {
            "this cluster"
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use beacon_kube::{ApiResource, GroupVersionKind};

    fn kind(group: &str, name: &str, plural: &str) -> Kind {
        Kind {
            resource: ApiResource::from_gvk_with_plural(
                &GroupVersionKind::gvk(group, "v1", name),
                plural,
            ),
            namespaced: true,
            verbs: vec!["list".into(), "watch".into()],
        }
    }

    fn labels(choices: &[Choice]) -> Vec<&str> {
        choices.iter().map(|choice| choice.label.as_str()).collect()
    }

    #[test]
    fn argo_actions_follow_live_phase_and_named_workflow_permissions() {
        let workflow = kind("argoproj.io", "Workflow", "workflows");
        let mut object: DynamicObject = serde_json::from_value(serde_json::json!({"apiVersion":"argoproj.io/v1alpha1","kind":"Workflow","metadata":{"name":"run","namespace":"default"},"spec":{},"status":{"phase":"Running"}})).unwrap();
        let patch = rules(&[("patch", "argoproj.io", "workflows")]);
        let choices = available_for_object(&workflow, Some(&patch), 1, &object);
        assert!(labels(&choices).contains(&"Suspend Workflow…"));
        assert!(labels(&choices).contains(&"Stop Workflow…"));
        assert!(!labels(&choices).contains(&"Retry failed steps…"));
        object.data["spec"]["suspend"] = serde_json::json!(true);
        let choices = available_for_object(&workflow, Some(&patch), 1, &object);
        assert!(labels(&choices).contains(&"Resume Workflow…"));
        assert!(!labels(&choices).contains(&"Suspend Workflow…"));
        object.data["status"]["phase"] = serde_json::json!("Failed");
        let choices = available_for_object(&workflow, Some(&patch), 1, &object);
        assert!(
            choices
                .iter()
                .find(|choice| choice.label == "Retry failed steps…")
                .unwrap()
                .allowed
        );
        assert!(
            !choices
                .iter()
                .find(|choice| choice.label == "Resubmit Workflow…")
                .unwrap()
                .allowed
        );
        assert!(!labels(&choices).contains(&"Stop Workflow…"));
        let template = kind("argoproj.io", "WorkflowTemplate", "workflowtemplates");
        let create = rules(&[("create", "argoproj.io", "workflows")]);
        assert!(
            available_for_object(&template, Some(&create), 1, &object)
                .iter()
                .find(|choice| choice.label == "Submit Workflow…")
                .unwrap()
                .allowed
        );
        let unrelated = kind("example.test", "Workflow", "workflows");
        assert!(
            !available_for_object(&unrelated, Some(&create), 1, &object)
                .iter()
                .any(|choice| matches!(choice.operation, Operation::Workflow { .. }))
        );
    }

    /// Built from a `SelfSubjectRulesReview` answer, the way the real one is.
    fn rules(grants: &[(&str, &str, &str)]) -> Rules {
        let json = serde_json::json!({
            "incomplete": false,
            "nonResourceRules": [],
            "resourceRules": grants.iter().map(|(verb, group, resource)| {
                serde_json::json!({
                    "verbs": [verb],
                    "apiGroups": [group],
                    "resources": [resource],
                })
            }).collect::<Vec<_>>()
        });
        serde_json::from_value(json).expect("a rules answer")
    }

    #[test]
    fn a_deployment_can_be_restarted_scaled_and_deleted() {
        let kind = kind("apps", "Deployment", "deployments");
        let all = rules(&[
            ("patch", "apps", "deployments"),
            ("patch", "apps", "deployments/scale"),
            ("delete", "apps", "deployments"),
        ]);

        let choices = available(&kind, Some(&all), 3);
        assert_eq!(labels(&choices), ["Restart", "Scale…", "Delete"]);
        assert!(choices.iter().all(|choice| choice.allowed));
    }

    /// A ConfigMap has no replicas and no pod template. Those actions are
    /// absent, not disabled -- there is nothing to explain.
    #[test]
    fn actions_that_cannot_apply_are_not_offered() {
        let choices = available(&kind("", "ConfigMap", "configmaps"), None, 0);
        assert_eq!(labels(&choices), ["Delete"]);
    }

    /// The same subresource trap as scaling: reading pods is not exec-ing
    /// into them, and the cluster only says so when the connection fails.
    #[test]
    fn exec_needs_its_own_rule() {
        let reader = rules(&[("get", "", "pods"), ("list", "", "pods")]);
        assert!(!super::may_exec(Some(&reader)));

        let allowed = rules(&[("create", "", "pods/exec")]);
        assert!(super::may_exec(Some(&allowed)));
        let websocket = rules(&[("get", "", "pods/exec")]);
        assert!(super::may_exec(Some(&websocket)));

        // Unknown until the answer arrives, and unknown means offer it.
        assert!(super::may_exec(None));
    }

    /// Scaling is a subresource, so patching a Deployment does not grant it.
    /// This is the case the greying exists for.
    #[test]
    fn scaling_is_disabled_without_the_scale_subresource() {
        let kind = kind("apps", "Deployment", "deployments");
        let partial = rules(&[
            ("patch", "apps", "deployments"),
            ("delete", "apps", "deployments"),
        ]);

        let choices = available(&kind, Some(&partial), 3);
        let scale = choices
            .iter()
            .find(|choice| choice.label == "Scale…")
            .expect("scale is offered");

        assert!(!scale.allowed);
        assert_eq!(
            scale.tooltip(),
            Some("You do not have patch on deployments/scale in this namespace")
        );

        let restart = choices
            .iter()
            .find(|choice| choice.label == "Restart")
            .expect("restart is offered");
        assert!(restart.allowed, "patch on the resource itself is granted");
    }

    #[test]
    fn a_reader_can_do_nothing() {
        let kind = kind("", "Pod", "pods");
        let read_only = rules(&[("get", "", "pods"), ("list", "", "pods")]);

        let choices = available(&kind, Some(&read_only), 0);
        assert_eq!(labels(&choices), ["Delete"]);
        assert!(!choices[0].allowed);
        assert!(!may_apply(&kind, Some(&read_only)));
    }

    /// Until the answer arrives, nothing is greyed. A menu that flickers from
    /// disabled to enabled teaches people to ignore the greying.
    #[test]
    fn everything_is_enabled_while_the_answer_is_in_flight() {
        let kind = kind("apps", "Deployment", "deployments");
        let choices = available(&kind, None, 1);

        assert!(choices.iter().all(|choice| choice.allowed));
        assert!(choices.iter().all(|choice| choice.tooltip().is_none()));
        assert!(may_apply(&kind, None));
    }

    /// A pod's ports come from its spec, so the menu can offer them by number
    /// instead of asking somebody to go and read the manifest.
    #[test]
    fn a_pods_declared_ports_are_offered() {
        let spec = serde_json::json!({ "spec": { "containers": [
            { "ports": [{ "containerPort": 8080 }, { "containerPort": 8443 }] },
            { "ports": [{ "containerPort": 9090 }, { "containerPort": 8080 }] },
            { }
        ]}});

        assert_eq!(
            ports(&spec),
            [8080, 8443, 9090],
            "in order, without repeats"
        );
        assert!(ports(&serde_json::json!({})).is_empty());
        assert!(ports(&serde_json::json!({ "spec": { "containers": [{}] } })).is_empty());
    }

    /// The prompt opens on the number that is set, not on zero.
    #[test]
    fn scale_carries_the_current_replica_count() {
        let choices = available(&kind("apps", "Deployment", "deployments"), None, 7);
        let scale = choices
            .iter()
            .find(|choice| choice.label == "Scale…")
            .expect("scale is offered");

        assert_eq!(scale.operation, Operation::Scale { replicas: 7 });
    }

    #[test]
    fn job_actions_use_job_creation_permission_and_reflect_schedule_state() {
        let cron = kind("batch", "CronJob", "cronjobs");
        let patch_only = rules(&[("patch", "batch", "cronjobs")]);
        let choices = available_for(&cron, Some(&patch_only), 1, true);
        let run = choices
            .iter()
            .find(|choice| choice.operation == Operation::RunJob)
            .unwrap();
        assert_eq!(run.label, "Run now…");
        assert!(!run.allowed);
        assert_eq!(
            run.tooltip(),
            Some("You do not have create on jobs in this namespace")
        );
        let schedule = choices
            .iter()
            .find(|choice| matches!(choice.operation, Operation::CronJobSuspend { .. }))
            .unwrap();
        assert_eq!(
            schedule.operation,
            Operation::CronJobSuspend { suspend: false }
        );
        assert!(schedule.allowed);
        assert_eq!(schedule.label, "Resume schedule…");
        let job = kind("batch", "Job", "jobs");
        assert_eq!(
            labels(&available(&job, None, 1)),
            ["Rerun as new Job…", "Delete"]
        );
        assert_eq!(
            labels(&available_for(&cron, None, 1, false)),
            ["Run now…", "Suspend schedule…", "Delete"]
        );
        let named: Rules = serde_json::from_value(serde_json::json!({"resourceRules":[{"verbs":["patch"],"apiGroups":["batch"],"resources":["cronjobs"],"resourceNames":["daily"]}]})).unwrap();
        let enabled = available_for_target(&cron, Some(&named), 1, false, Some("daily"));
        assert!(
            enabled
                .iter()
                .find(|choice| matches!(choice.operation, Operation::CronJobSuspend { .. }))
                .unwrap()
                .allowed
        );
        let disabled = available_for_target(&cron, Some(&named), 1, false, Some("hourly"));
        assert!(
            !disabled
                .iter()
                .find(|choice| matches!(choice.operation, Operation::CronJobSuspend { .. }))
                .unwrap()
                .allowed
        );
    }

    #[test]
    fn node_scheduling_actions_reflect_state_and_cluster_permissions() {
        let mut node = kind("", "Node", "nodes");
        node.namespaced = false;
        assert_eq!(
            labels(&available_for(&node, None, 1, false)),
            ["Cordon…", "Drain…", "Delete"]
        );
        assert_eq!(
            labels(&available_for(&node, None, 1, true)),
            ["Uncordon…", "Drain…", "Delete"]
        );
        let read = rules(&[("get", "", "nodes")]);
        let choices = available_for_target(&node, Some(&read), 1, false, Some("node-a"));
        assert!(!choices.iter().any(|choice| choice.allowed));
        assert_eq!(
            choices[0].tooltip(),
            Some("You do not have patch on nodes in this cluster")
        );
    }
}
