//! What this user is allowed to do.
//!
//! Beacon asks before it offers. An action the cluster will refuse is shown
//! greyed out with the reason, rather than enabled and then failing with a 403
//! — which is the difference between a client that knows where you stand and
//! one that lets you find out by trying.
//!
//! The question is asked with `SelfSubjectRulesReview`, which returns every
//! rule that applies to the user in one namespace in a single request. The
//! alternative, a `SelfSubjectAccessReview` per button, would be a round trip
//! per row per verb.
//!
//! Two properties of RBAC matter here and both are easy to get wrong:
//!
//! * A rule on `pods` does not grant `pods/log`. Subresources are named
//!   separately, so "can list pods" says nothing about "can read logs".
//! * The answer can be admittedly incomplete — a webhook authorizer cannot
//!   enumerate its rules. When that happens Beacon enables the action anyway:
//!   a 403 the user can read beats a button they cannot press for a reason
//!   nobody can explain.

use std::{collections::HashMap, sync::Arc};

use k8s_openapi::api::authorization::v1::{
    ResourceRule, SelfSubjectRulesReview, SelfSubjectRulesReviewSpec,
};
use kube::{Api, api::PostParams};

use crate::Result;

/// The verbs Beacon asks about.
pub mod verbs {
    pub const GET: &str = "get";
    pub const LIST: &str = "list";
    pub const WATCH: &str = "watch";
    pub const CREATE: &str = "create";
    pub const UPDATE: &str = "update";
    pub const PATCH: &str = "patch";
    pub const DELETE: &str = "delete";
}

/// What a user may do in one namespace.
///
/// Deserializable from a `SubjectRulesReviewStatus` as the API returns it,
/// which is how it is built here and how a test builds a realistic one.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Rules {
    #[serde(rename = "resourceRules", default)]
    rules: Vec<ResourceRule>,
    /// The server said it could not enumerate everything. Treated as "allow":
    /// see the module docs.
    #[serde(default)]
    incomplete: bool,
}

impl Rules {
    /// Whether a verb is permitted on a resource.
    ///
    /// `resource` is the plural name, optionally with a subresource:
    /// `deployments`, or `deployments/scale`.
    pub fn allows(&self, verb: &str, group: &str, resource: &str) -> bool {
        if self.incomplete {
            return true;
        }
        self.rules
            .iter()
            .any(|rule| matches(rule, verb, group, resource))
    }

    /// Whether the answer is a guess. The UI says so rather than pretending.
    pub fn is_incomplete(&self) -> bool {
        self.incomplete
    }
}

fn matches(rule: &ResourceRule, verb: &str, group: &str, resource: &str) -> bool {
    contains(&rule.verbs, verb)
        && contains(rule.api_groups.as_deref().unwrap_or_default(), group)
        && resources_match(rule.resources.as_deref().unwrap_or_default(), resource)
        // A rule narrowed to specific names cannot answer a question about the
        // kind as a whole, and Beacon only asks about kinds.
        && rule.resource_names.as_ref().is_none_or(Vec::is_empty)
}

fn contains(values: &[String], wanted: &str) -> bool {
    values.iter().any(|value| value == "*" || value == wanted)
}

/// Resource matching, including the two wildcard forms RBAC allows.
///
/// `*` is every resource and every subresource of them. `*/log` is that
/// subresource on any resource. A bare `pods` is only `pods` -- it does not
/// reach `pods/log`, which is the rule people misremember.
fn resources_match(patterns: &[String], wanted: &str) -> bool {
    let wanted_subresource = wanted.split_once('/').map(|(_, sub)| sub);

    patterns
        .iter()
        .any(|pattern| match pattern.split_once('/') {
            Some(("*", subresource)) => wanted_subresource == Some(subresource),
            Some(_) | None => pattern == "*" || pattern == wanted,
        })
}

/// The rules for each namespace somebody has looked at, asked once.
#[derive(Default)]
pub struct Access {
    /// Keyed by namespace; `None` is the cluster scope.
    known: HashMap<Option<String>, Arc<Rules>>,
}

impl Access {
    pub fn get(&self, namespace: Option<&str>) -> Option<&Arc<Rules>> {
        self.known.get(&namespace.map(str::to_string))
    }

    pub fn insert(&mut self, namespace: Option<String>, rules: Arc<Rules>) {
        self.known.insert(namespace, rules);
    }
}

/// Asks the cluster what this user may do in a namespace.
///
/// A failure is not propagated as an error: a cluster that will not answer this
/// question is not a cluster where every button should be disabled. The
/// permissive fallback is the same one used for an incomplete answer.
pub async fn fetch_rules(client: &kube::Client, namespace: Option<&str>) -> Rules {
    match review(client, namespace).await {
        Ok(rules) => rules,
        Err(error) => {
            tracing::debug!(
                namespace = ?namespace,
                %error,
                "could not read permissions; assuming they are allowed"
            );
            Rules {
                rules: Vec::new(),
                incomplete: true,
            }
        }
    }
}

async fn review(client: &kube::Client, namespace: Option<&str>) -> Result<Rules> {
    let api: Api<SelfSubjectRulesReview> = Api::all(client.clone());

    let review = SelfSubjectRulesReview {
        spec: SelfSubjectRulesReviewSpec {
            // An empty namespace asks about the cluster scope.
            namespace: Some(namespace.unwrap_or_default().to_string()),
        },
        ..Default::default()
    };

    let answered = api.create(&PostParams::default(), &review).await?;
    let status = answered.status.unwrap_or_default();

    if let Some(error) = &status.evaluation_error {
        tracing::debug!(namespace = ?namespace, %error, "partial permission answer");
    }

    Ok(Rules {
        rules: status.resource_rules,
        incomplete: status.incomplete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(verbs: &[&str], groups: &[&str], resources: &[&str]) -> ResourceRule {
        ResourceRule {
            verbs: verbs.iter().map(|v| v.to_string()).collect(),
            api_groups: Some(groups.iter().map(|g| g.to_string()).collect()),
            resources: Some(resources.iter().map(|r| r.to_string()).collect()),
            resource_names: None,
        }
    }

    fn rules(rules: Vec<ResourceRule>) -> Rules {
        Rules {
            rules,
            incomplete: false,
        }
    }

    #[test]
    fn an_exact_rule_grants_exactly_what_it_names() {
        let rules = rules(vec![rule(&["get", "list"], &[""], &["pods"])]);

        assert!(rules.allows("get", "", "pods"));
        assert!(rules.allows("list", "", "pods"));
        assert!(!rules.allows("delete", "", "pods"), "verb not granted");
        assert!(!rules.allows("get", "apps", "pods"), "wrong group");
        assert!(!rules.allows("get", "", "services"), "wrong resource");
    }

    /// The rule everybody misremembers: listing pods is not reading their logs.
    #[test]
    fn a_rule_on_a_resource_does_not_reach_its_subresources() {
        let rules = rules(vec![rule(&["get"], &[""], &["pods"])]);
        assert!(rules.allows("get", "", "pods"));
        assert!(!rules.allows("get", "", "pods/log"));

        let with_logs = rules_with_logs();
        assert!(with_logs.allows("get", "", "pods/log"));
    }

    fn rules_with_logs() -> Rules {
        rules(vec![rule(&["get"], &[""], &["pods", "pods/log"])])
    }

    /// `*/scale` is how a role grants scaling across every workload kind.
    #[test]
    fn a_subresource_wildcard_matches_that_subresource_anywhere() {
        let rules = rules(vec![rule(&["update"], &["apps"], &["*/scale"])]);

        assert!(rules.allows("update", "apps", "deployments/scale"));
        assert!(rules.allows("update", "apps", "statefulsets/scale"));
        assert!(
            !rules.allows("update", "apps", "deployments"),
            "the wildcard is about the subresource, not the resource"
        );
    }

    #[test]
    fn a_full_wildcard_grants_everything_in_its_groups() {
        let rules = rules(vec![rule(&["*"], &["*"], &["*"])]);

        assert!(rules.allows("delete", "apps", "deployments"));
        assert!(rules.allows("get", "", "pods/log"));
        assert!(rules.allows("patch", "argoproj.io", "applications"));
    }

    /// The core group is the empty string, and a rule for `apps` must not
    /// answer for it.
    #[test]
    fn the_core_group_is_its_own_group() {
        let rules = rules(vec![rule(&["get"], &["apps"], &["*"])]);
        assert!(rules.allows("get", "apps", "deployments"));
        assert!(!rules.allows("get", "", "pods"));
    }

    /// A rule limited to named objects cannot answer a question about the kind,
    /// so it is ignored rather than counted as a grant.
    #[test]
    fn a_rule_narrowed_to_names_does_not_grant_the_kind() {
        let mut narrowed = rule(&["delete"], &[""], &["pods"]);
        narrowed.resource_names = Some(vec!["only-this-one".into()]);

        assert!(!rules(vec![narrowed]).allows("delete", "", "pods"));
    }

    /// A webhook authorizer cannot enumerate its rules. Disabling everything
    /// would make Beacon useless against exactly the clusters that use one.
    #[test]
    fn an_incomplete_answer_allows_everything() {
        let unknown = Rules {
            rules: Vec::new(),
            incomplete: true,
        };

        assert!(unknown.allows("delete", "apps", "deployments"));
        assert!(unknown.is_incomplete());
    }

    /// No rules and a complete answer is a real "no".
    #[test]
    fn no_rules_grants_nothing() {
        let none = rules(Vec::new());
        assert!(!none.allows("get", "", "pods"));
        assert!(!none.is_incomplete());
    }

    #[test]
    fn the_cache_separates_namespaces_from_the_cluster_scope() {
        let mut access = Access::default();
        access.insert(
            None,
            Arc::new(rules(vec![rule(&["get"], &[""], &["nodes"])])),
        );
        access.insert(
            Some("kube-system".into()),
            Arc::new(rules(vec![rule(&["get"], &[""], &["pods"])])),
        );

        assert!(access.get(None).unwrap().allows("get", "", "nodes"));
        assert!(!access.get(None).unwrap().allows("get", "", "pods"));
        assert!(
            access
                .get(Some("kube-system"))
                .unwrap()
                .allows("get", "", "pods")
        );
        assert!(access.get(Some("default")).is_none(), "not asked yet");
    }
}
