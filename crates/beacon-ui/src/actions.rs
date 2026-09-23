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

use beacon_kube::{Kind, Operation, Rules};

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
    let group = kind.resource.group.as_str();
    let name = kind.resource.kind.as_str();

    [
        Operation::Restart,
        Operation::Scale { replicas },
        Operation::Delete,
    ]
    .into_iter()
    .filter(|operation| operation.applies_to(group, name))
    .map(|operation| {
        let allowed = rules.is_none_or(|rules| {
            rules.allows(
                operation.verb(),
                group,
                &operation.resource(&kind.resource.plural),
            )
        });

        Choice {
            label: label(&operation),
            blocked_because: (!allowed).then(|| refusal(&operation, kind)),
            allowed,
            operation,
        }
    })
    .collect()
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

fn label(operation: &Operation) -> String {
    match operation {
        Operation::Delete => "Delete".to_string(),
        Operation::Restart => "Restart".to_string(),
        // The number is asked for, so the label ends in an ellipsis the way a
        // menu item that opens a prompt should.
        Operation::Scale { .. } => "Scale…".to_string(),
        Operation::Apply => "Apply".to_string(),
    }
}

/// The sentence shown on a disabled action.
///
/// Phrased as the RBAC question that was asked, because that is what somebody
/// has to take to whoever grants them access.
fn refusal(operation: &Operation, kind: &Kind) -> String {
    format!(
        "You do not have {} on {} in this namespace",
        operation.verb(),
        operation.resource(&kind.resource.plural)
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
}
