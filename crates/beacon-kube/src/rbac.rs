//! Permission queries and RBAC provenance. The API server, not a local evaluator, answers Can I.
use crate::{
    ApiResource, ClusterSession, DynamicObject, GroupVersionKind, ObjectRef, Result, WatchKey,
};
use k8s_openapi::api::{
    authentication::v1::SelfSubjectReview,
    authorization::v1::{
        ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec,
        SubjectAccessReview, SubjectAccessReviewSpec, SubjectAccessReviewStatus,
    },
};
use kube::{Api, api::PostParams};
use serde_json::Value;
use std::sync::Arc;

pub const GROUP: &str = "rbac.authorization.k8s.io";
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Current,
    ServiceAccount { namespace: String, name: String },
    User { name: String, groups: Vec<String> },
    Group(String),
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub user: Option<String>,
    pub groups: Vec<String>,
}
impl Subject {
    pub fn identity(&self) -> Option<Identity> {
        match self {
            Self::Current => None,
            Self::ServiceAccount { namespace, name } => Some(Identity {
                user: Some(format!("system:serviceaccount:{namespace}:{name}")),
                groups: vec![
                    "system:authenticated".into(),
                    "system:serviceaccounts".into(),
                    format!("system:serviceaccounts:{namespace}"),
                ],
            }),
            Self::User { name, groups } => Some(Identity {
                user: Some(name.clone()),
                groups: groups.clone(),
            }),
            Self::Group(group) => Some(Identity {
                user: None,
                groups: vec![group.clone()],
            }),
        }
    }
}
pub async fn current_identity(session: &ClusterSession) -> Result<Identity> {
    let api: Api<SelfSubjectReview> = Api::all(session.client().clone());
    let review = api
        .create(&PostParams::default(), &SelfSubjectReview::default())
        .await?;
    let user = review
        .status
        .and_then(|status| status.user_info)
        .unwrap_or_default();
    let username = user.username.ok_or_else(|| {
        crate::Error::Manifest("The identity review returned no username.".into())
    })?;
    Ok(Identity {
        user: Some(username),
        groups: user.groups.unwrap_or_default(),
    })
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    pub verb: String,
    pub group: String,
    pub resource: String,
    pub subresource: String,
    pub namespace: Option<String>,
    pub name: Option<String>,
}
impl Query {
    fn attributes(&self) -> ResourceAttributes {
        ResourceAttributes {
            verb: Some(self.verb.clone()),
            group: Some(self.group.clone()),
            resource: Some(self.resource.clone()),
            subresource: (!self.subresource.is_empty()).then(|| self.subresource.clone()),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            ..Default::default()
        }
    }
    pub fn describe(&self) -> String {
        format!(
            "{} {}{}{}{}",
            self.verb,
            self.resource,
            if self.subresource.is_empty() {
                String::new()
            } else {
                format!("/{}", self.subresource)
            },
            if self.group.is_empty() {
                String::new()
            } else {
                format!(".{}", self.group)
            },
            self.namespace.as_ref().map_or_else(
                || " across all namespaces / cluster scope".into(),
                |ns| format!(" in namespace {ns}")
            )
        )
    }
}
pub async fn can_i(
    session: &ClusterSession,
    subject: &Subject,
    query: &Query,
) -> Result<SubjectAccessReviewStatus> {
    if query.verb.trim().is_empty() || query.resource.trim().is_empty() {
        return Err(crate::Error::Manifest(
            "Choose a verb and resource for the permission query.".into(),
        ));
    }
    let attributes = Some(query.attributes());
    match subject {
        Subject::Current => {
            let api: Api<SelfSubjectAccessReview> = Api::all(session.client().clone());
            let review = api
                .create(
                    &PostParams::default(),
                    &SelfSubjectAccessReview {
                        spec: SelfSubjectAccessReviewSpec {
                            resource_attributes: attributes,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .await?;
            review.status.ok_or_else(|| {
                crate::Error::Manifest("The permission review returned no answer.".into())
            })
        }
        _ => {
            let identity = subject.identity().expect("named subjects have an identity");
            let api: Api<SubjectAccessReview> = Api::all(session.client().clone());
            let review = api
                .create(
                    &PostParams::default(),
                    &SubjectAccessReview {
                        spec: SubjectAccessReviewSpec {
                            user: identity.user,
                            groups: Some(identity.groups),
                            resource_attributes: attributes,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .await?;
            review.status.ok_or_else(|| {
                crate::Error::Manifest("The permission review returned no answer.".into())
            })
        }
    }
}

#[derive(Debug, Clone)]
pub struct Grant {
    pub binding_kind: String,
    pub binding: ObjectRef,
    pub role_kind: String,
    pub role: ObjectRef,
    pub rules: Vec<Value>,
    pub matched_subjects: Vec<String>,
    pub missing: bool,
}
#[derive(Default)]
pub struct Catalog {
    pub objects: Vec<(String, Arc<DynamicObject>)>,
    pub errors: Vec<String>,
}
pub async fn catalog(session: Arc<ClusterSession>, namespace: Option<String>) -> Catalog {
    let mut catalog = Catalog::default();
    for (kind, plural, namespaced) in [
        ("Role", "roles", true),
        ("RoleBinding", "rolebindings", true),
        ("ClusterRole", "clusterroles", false),
        ("ClusterRoleBinding", "clusterrolebindings", false),
    ] {
        let resource =
            ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk(GROUP, "v1", kind), plural);
        let key =
            WatchKey::all(resource).in_namespace(if namespaced { namespace.clone() } else { None });
        match session.clone().list_objects(key).await {
            Ok(objects) => catalog
                .objects
                .extend(objects.into_iter().map(|object| (kind.to_string(), object))),
            Err(error) => catalog
                .errors
                .push(format!("{kind}: {}", error.user_message())),
        }
    }
    catalog
}
pub fn grants(identity: &Identity, objects: &[(String, Arc<DynamicObject>)]) -> Vec<Grant> {
    let mut grants = Vec::new();
    for (kind, binding) in objects
        .iter()
        .filter(|(kind, _)| matches!(kind.as_str(), "RoleBinding" | "ClusterRoleBinding"))
    {
        let subjects = binding
            .data
            .get("subjects")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let matched_subjects: Vec<_> = subjects
            .iter()
            .filter_map(|subject| {
                let name = subject["name"].as_str()?;
                match subject["kind"].as_str()? {
                    "User" if identity.user.as_deref() == Some(name) => {
                        Some(format!("User {name}"))
                    }
                    "Group" if identity.groups.iter().any(|group| group == name) => {
                        Some(format!("Group {name}"))
                    }
                    "ServiceAccount" => {
                        let ns = subject["namespace"]
                            .as_str()
                            .or(binding.metadata.namespace.as_deref())?;
                        (identity.user.as_deref()
                            == Some(format!("system:serviceaccount:{ns}:{name}").as_str()))
                        .then(|| format!("ServiceAccount {ns}/{name}"))
                    }
                    _ => None,
                }
            })
            .collect();
        if matched_subjects.is_empty() {
            continue;
        }
        let role_ref = &binding.data["roleRef"];
        let role_kind = role_ref["kind"].as_str().unwrap_or_default();
        let role_name = role_ref["name"].as_str().unwrap_or_default();
        let role_namespace = if role_kind == "Role" {
            binding.metadata.namespace.clone()
        } else {
            None
        };
        let valid = role_ref["apiGroup"].as_str() == Some(GROUP)
            && matches!(role_kind, "Role" | "ClusterRole")
            && (kind != "ClusterRoleBinding" || role_kind == "ClusterRole");
        let role = valid
            .then(|| {
                objects.iter().find(|(kind, object)| {
                    kind == role_kind
                        && object.metadata.name.as_deref() == Some(role_name)
                        && object.metadata.namespace == role_namespace
                })
            })
            .flatten();
        let rules = role
            .and_then(|(_, role)| role.data.get("rules").and_then(Value::as_array))
            .cloned()
            .unwrap_or_default();
        grants.push(Grant {
            binding_kind: kind.clone(),
            binding: ObjectRef::new(
                binding.metadata.namespace.clone(),
                binding.metadata.name.clone().unwrap_or_default(),
            ),
            role_kind: role_kind.into(),
            role: ObjectRef::new(role_namespace, role_name),
            rules,
            matched_subjects,
            missing: role.is_none(),
        });
    }
    grants.sort_by(|a, b| {
        a.binding_kind
            .cmp(&b.binding_kind)
            .then(a.binding.cmp(&b.binding))
    });
    grants
}
/// Extracts the actual denied attributes from standard Kubernetes authorization errors.
pub fn denial(message: &str) -> Option<Query> {
    let after = message.split_once(" cannot ")?.1;
    let verb = after.split_whitespace().next()?.to_string();
    let quoted = |prefix: &str| {
        message
            .split_once(prefix)
            .and_then(|(_, after)| after.split('"').next())
            .map(str::to_owned)
    };
    let resource = quoted("resource \"")?;
    let (resource, subresource) = resource.split_once('/').map_or(
        (resource.clone(), String::new()),
        |(resource, subresource)| (resource.into(), subresource.into()),
    );
    Some(Query {
        verb,
        resource,
        subresource,
        group: quoted("in API group \"").unwrap_or_default(),
        namespace: quoted("in the namespace \""),
        name: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn object(
        kind: &str,
        namespace: Option<&str>,
        name: &str,
        data: Value,
    ) -> (String, Arc<DynamicObject>) {
        let mut object = data;
        object["apiVersion"] = format!("{GROUP}/v1").into();
        object["kind"] = kind.into();
        object["metadata"] = json!({"name":name,"namespace":namespace});
        (
            kind.into(),
            Arc::new(serde_json::from_value(object).unwrap()),
        )
    }
    #[test]
    fn finds_exact_user_group_and_serviceaccount_grants_with_role_scope() {
        let identity = Subject::ServiceAccount {
            namespace: "apps".into(),
            name: "runner".into(),
        }
        .identity()
        .unwrap();
        let objects = vec![
            object(
                "Role",
                Some("apps"),
                "read",
                json!({"rules":[{"verbs":["get"],"apiGroups":[""],"resources":["pods/log"],"resourceNames":["job"]}]}),
            ),
            object(
                "Role",
                Some("other"),
                "read",
                json!({"rules":[{"verbs":["delete"]}]}),
            ),
            object(
                "RoleBinding",
                Some("apps"),
                "local",
                json!({"subjects":[{"kind":"ServiceAccount","name":"runner"}],"roleRef":{"kind":"Role","name":"read","apiGroup":GROUP}}),
            ),
            object(
                "ClusterRole",
                None,
                "global",
                json!({"rules":[{"verbs":["list"],"apiGroups":[""],"resources":["nodes"]}]}),
            ),
            object(
                "ClusterRoleBinding",
                None,
                "all-sa",
                json!({"subjects":[{"kind":"Group","name":"system:serviceaccounts"}],"roleRef":{"kind":"ClusterRole","name":"global","apiGroup":GROUP}}),
            ),
        ];
        let grants = grants(&identity, &objects);
        assert_eq!(grants.len(), 2);
        assert_eq!(
            grants
                .iter()
                .find(|grant| grant.role_kind == "Role")
                .unwrap()
                .rules[0]["verbs"],
            json!(["get"])
        );
        assert!(grants.iter().all(|grant| !grant.missing));
        assert!(
            super::grants(
                &Subject::User {
                    name: "runner".into(),
                    groups: vec![]
                }
                .identity()
                .unwrap(),
                &objects
            )
            .is_empty()
        );
    }
    #[test]
    fn reports_missing_role_and_does_not_treat_cluster_binding_as_namespaced() {
        let identity = Identity {
            user: Some("alice".into()),
            groups: vec!["team".into()],
        };
        let objects = vec![
            object(
                "ClusterRoleBinding",
                None,
                "invalid",
                json!({"subjects":[{"kind":"User","name":"alice"}],"roleRef":{"kind":"Role","name":"read","apiGroup":GROUP}}),
            ),
            object(
                "RoleBinding",
                Some("team"),
                "missing",
                json!({"subjects":[{"kind":"Group","name":"team"}],"roleRef":{"kind":"ClusterRole","name":"missing","apiGroup":GROUP}}),
            ),
        ];
        let grants = grants(&identity, &objects);
        assert_eq!(grants.len(), 2);
        assert!(grants.iter().all(|grant| grant.missing));
        assert!(grants.iter().all(|grant| grant.rules.is_empty()));
    }
    #[test]
    fn denied_log_access_keeps_subresource_verb_group_and_namespace() {
        let query=denial("pods \"app\" is forbidden: User \"alice\" cannot get resource \"pods/log\" in API group \"\" in the namespace \"team\" (HTTP 403)").unwrap();
        assert_eq!(query.verb, "get");
        assert_eq!(query.resource, "pods");
        assert_eq!(query.subresource, "log");
        assert_eq!(query.namespace.as_deref(), Some("team"));
        assert!(query.describe().contains("pods/log"));
        assert!(denial("failed to connect: timeout").is_none());
    }
}
