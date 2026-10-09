//! Resolve related resources from API references, never from name prefixes or IP guesses.
use crate::{DynamicObject, ObjectRef, ResourceStore};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relationship {
    Deployment,
    Controller {
        group: &'static str,
        kind: &'static str,
    },
    CronJob,
    Service,
    EndpointSlice,
    Claim,
}

impl Relationship {
    pub fn for_kind(group: &str, kind: &str) -> Option<Self> {
        Some(match (group, kind) {
            ("apps", "Deployment") => Self::Deployment,
            ("apps", "ReplicaSet") => Self::Controller {
                group: "apps",
                kind: "ReplicaSet",
            },
            ("apps", "StatefulSet") => Self::Controller {
                group: "apps",
                kind: "StatefulSet",
            },
            ("apps", "DaemonSet") => Self::Controller {
                group: "apps",
                kind: "DaemonSet",
            },
            ("batch", "Job") => Self::Controller {
                group: "batch",
                kind: "Job",
            },
            ("", "ReplicationController") => Self::Controller {
                group: "",
                kind: "ReplicationController",
            },
            ("batch", "CronJob") => Self::CronJob,
            ("", "Service") => Self::Service,
            ("discovery.k8s.io", "EndpointSlice") => Self::EndpointSlice,
            ("", "PersistentVolumeClaim") => Self::Claim,
            _ => return None,
        })
    }

    /// The first entry is the intermediate resource, if there is one.
    pub fn dependencies(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Deployment => &[("apps", "ReplicaSet"), ("", "Pod")],
            Self::CronJob => &[("batch", "Job"), ("", "Pod")],
            Self::Service => &[("discovery.k8s.io", "EndpointSlice"), ("", "Pod")],
            _ => &[("", "Pod")],
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Deployment => "ReplicaSets and the Pods they control.",
            Self::CronJob => "Jobs and the Pods they control.",
            Self::Controller { .. } => "Pods controlled by this workload.",
            Self::Service => "EndpointSlices and the Pods referenced by their endpoints.",
            Self::EndpointSlice => "Pods referenced by the endpoints in this slice.",
            Self::Claim => "Pods that reference this claim in their volumes.",
        }
    }

    /// Outputs have the same order as `dependencies`. Source stores may be
    /// incomplete while loading; the caller reports those states separately.
    pub fn resolve(
        self,
        source: &DynamicObject,
        stores: &[ResourceStore],
    ) -> Vec<Vec<Arc<DynamicObject>>> {
        let empty = ResourceStore::new();
        let first = stores.first().unwrap_or(&empty);
        let pods = stores.last().unwrap_or(&empty);
        let same_namespace =
            |object: &DynamicObject| object.metadata.namespace == source.metadata.namespace;
        let mut groups = match self {
            Self::Deployment | Self::CronJob => {
                let (group, kind, child_group, child_kind) = if self == Self::Deployment {
                    ("apps", "Deployment", "apps", "ReplicaSet")
                } else {
                    ("batch", "CronJob", "batch", "Job")
                };
                let children: Vec<_> = first
                    .iter()
                    .filter(|(_, child)| {
                        same_namespace(child)
                            && controlled_by(child, source.metadata.uid.as_deref(), group, kind)
                    })
                    .map(|(_, child)| child.clone())
                    .collect();
                let owners: BTreeSet<_> = children
                    .iter()
                    .filter_map(|child| child.metadata.uid.as_deref())
                    .collect();
                let pods = pods
                    .iter()
                    .filter(|(_, pod)| {
                        same_namespace(pod)
                            && owners
                                .iter()
                                .any(|uid| controlled_by(pod, Some(uid), child_group, child_kind))
                    })
                    .map(|(_, pod)| pod.clone())
                    .collect();
                vec![children, pods]
            }
            Self::Controller { group, kind } => vec![
                pods.iter()
                    .filter(|(_, pod)| {
                        same_namespace(pod)
                            && controlled_by(pod, source.metadata.uid.as_deref(), group, kind)
                    })
                    .map(|(_, pod)| pod.clone())
                    .collect(),
            ],
            Self::Claim => vec![
                pods.iter()
                    .filter(|(_, pod)| {
                        same_namespace(pod)
                            && source.metadata.name.is_some()
                            && pod
                                .data
                                .pointer("/spec/volumes")
                                .and_then(|volumes| volumes.as_array())
                                .is_some_and(|volumes| {
                                    volumes.iter().any(|volume| {
                                        volume
                                            .pointer("/persistentVolumeClaim/claimName")
                                            .and_then(|name| name.as_str())
                                            == source.metadata.name.as_deref()
                                    })
                                })
                    })
                    .map(|(_, pod)| pod.clone())
                    .collect(),
            ],
            Self::Service => {
                let slices: Vec<_> = first
                    .iter()
                    .filter(|(_, slice)| {
                        same_namespace(slice)
                            && source.metadata.name.is_some()
                            && slice
                                .metadata
                                .labels
                                .as_ref()
                                .and_then(|labels| labels.get("kubernetes.io/service-name"))
                                .map(String::as_str)
                                == source.metadata.name.as_deref()
                            && !slice
                                .metadata
                                .owner_references
                                .as_deref()
                                .unwrap_or_default()
                                .iter()
                                .any(|owner| {
                                    owner.kind == "Service"
                                        && api_group(&owner.api_version).is_empty()
                                        && Some(owner.uid.as_str())
                                            != source.metadata.uid.as_deref()
                                })
                    })
                    .map(|(_, slice)| slice.clone())
                    .collect();
                let targets = endpoint_pods(slices.iter().map(Arc::as_ref), pods);
                vec![slices, targets]
            }
            Self::EndpointSlice => vec![endpoint_pods(std::iter::once(source), pods)],
        };
        for objects in &mut groups {
            objects.sort_by_key(|object| ObjectRef::of(object));
        }
        groups
    }
}

fn api_group(version: &str) -> &str {
    version.split_once('/').map_or("", |(group, _)| group)
}

fn controlled_by(object: &DynamicObject, uid: Option<&str>, group: &str, kind: &str) -> bool {
    let Some(uid) = uid.filter(|uid| !uid.is_empty()) else {
        return false;
    };
    object
        .metadata
        .owner_references
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|owner| {
            owner.uid == uid
                && owner.kind == kind
                && api_group(&owner.api_version) == group
                && owner.controller == Some(true)
        })
}

fn endpoint_pods<'a>(
    slices: impl Iterator<Item = &'a DynamicObject>,
    pods: &ResourceStore,
) -> Vec<Arc<DynamicObject>> {
    let mut found = BTreeSet::new();
    for slice in slices {
        for endpoint in slice
            .data
            .get("endpoints")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
        {
            let Some(reference) = endpoint.get("targetRef") else {
                continue;
            };
            if reference.get("kind").and_then(|value| value.as_str()) != Some("Pod")
                || !api_group(
                    reference
                        .get("apiVersion")
                        .and_then(|value| value.as_str())
                        .unwrap_or("v1"),
                )
                .is_empty()
            {
                continue;
            }
            let Some(name) = reference.get("name").and_then(|value| value.as_str()) else {
                continue;
            };
            let namespace = reference
                .get("namespace")
                .and_then(|value| value.as_str())
                .or(slice.metadata.namespace.as_deref());
            let key = ObjectRef {
                name: name.to_owned(),
                namespace: namespace.map(str::to_owned),
            };
            if let Some(pod) = pods.get(&key) {
                let uid = reference
                    .get("uid")
                    .and_then(|value| value.as_str())
                    .filter(|uid| !uid.is_empty());
                if uid.is_none() || uid == pod.metadata.uid.as_deref() {
                    found.insert(key);
                }
            }
        }
    }
    found
        .into_iter()
        .filter_map(|key| pods.get(&key).cloned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Delta;
    use serde_json::json;
    fn object(
        kind: &str,
        name: &str,
        uid: &str,
        namespace: &str,
        data: serde_json::Value,
    ) -> Arc<DynamicObject> {
        let mut value = json!({"apiVersion":"v1", "kind":kind, "metadata":{"name":name,"uid":uid,"namespace":namespace}});
        value
            .as_object_mut()
            .unwrap()
            .extend(data.as_object().unwrap().clone());
        Arc::new(serde_json::from_value(value).unwrap())
    }
    fn owned(
        kind: &str,
        name: &str,
        uid: &str,
        parent_kind: &str,
        parent_uid: &str,
        api: &str,
    ) -> Arc<DynamicObject> {
        object(
            kind,
            name,
            uid,
            "default",
            json!({"metadata":{"name":name,"uid":uid,"namespace":"default","ownerReferences":[{"kind":parent_kind,"name":"parent","uid":parent_uid,"apiVersion":api,"controller":true}]}}),
        )
    }
    fn store(objects: Vec<Arc<DynamicObject>>) -> ResourceStore {
        let mut store = ResourceStore::new();
        store.apply(Delta::Reset(objects));
        store
    }
    #[test]
    fn deployment_uses_controller_uids_through_replica_sets_and_live_deletions() {
        let deployment = object("Deployment", "parent", "dep", "default", json!({}));
        let replica = owned(
            "ReplicaSet",
            "parent-rs",
            "rs",
            "Deployment",
            "dep",
            "apps/v1",
        );
        let wrong = owned(
            "ReplicaSet",
            "parent-old",
            "old",
            "Deployment",
            "old-dep",
            "apps/v1",
        );
        let pod = owned("Pod", "pod", "pod", "ReplicaSet", "rs", "apps/v1");
        let stale = owned("Pod", "pod-old", "old-pod", "ReplicaSet", "old", "apps/v1");
        let mut stores = vec![store(vec![replica, wrong]), store(vec![pod, stale])];
        let groups = Relationship::Deployment.resolve(&deployment, &stores);
        assert_eq!(groups[0].len(), 1);
        assert_eq!(groups[1].len(), 1);
        assert_eq!(groups[1][0].metadata.name.as_deref(), Some("pod"));
        stores[0].apply(Delta::Reset(vec![]));
        assert!(
            Relationship::Deployment
                .resolve(&deployment, &stores)
                .iter()
                .all(Vec::is_empty)
        );
    }
    #[test]
    fn services_deduplicate_pod_references_and_reject_reused_uids_and_external_ips() {
        let service = object("Service", "web", "svc", "default", json!({}));
        let slice = object(
            "EndpointSlice",
            "slice",
            "slice",
            "default",
            json!({
            "metadata":{"name":"slice","uid":"slice","namespace":"default","labels":{"kubernetes.io/service-name":"web"}},
            "endpoints":[
                {"targetRef":{"kind":"Pod","name":"app","uid":"app","namespace":"default"}},
                {"targetRef":{"kind":"Pod","name":"app","uid":"app","namespace":"default"}},
                {"targetRef":{"kind":"Pod","name":"replacement","uid":"old"}},
                {"addresses":["10.0.0.1"]}
            ]}),
        );
        let stale_slice = object(
            "EndpointSlice",
            "old-slice",
            "old-slice",
            "default",
            json!({
            "metadata":{"name":"old-slice","namespace":"default","labels":{"kubernetes.io/service-name":"web"},
                "ownerReferences":[{"kind":"Service","apiVersion":"v1","name":"web","uid":"old-svc"}]}}),
        );
        let pods = store(vec![
            object("Pod", "app", "app", "default", json!({})),
            object("Pod", "replacement", "new", "default", json!({})),
        ]);
        let groups =
            Relationship::Service.resolve(&service, &[store(vec![slice, stale_slice]), pods]);
        assert_eq!(groups[0].len(), 1);
        assert_eq!(groups[1].len(), 1);
        assert_eq!(groups[1][0].metadata.uid.as_deref(), Some("app"));
    }
    #[test]
    fn pvc_matches_volume_references_in_its_namespace_only() {
        let claim = object(
            "PersistentVolumeClaim",
            "data",
            "claim",
            "default",
            json!({}),
        );
        let volumes = json!({"spec":{"volumes":[{"name":"data","persistentVolumeClaim":{"claimName":"data"}}]}});
        let pods = store(vec![
            object("Pod", "consumer", "one", "default", volumes.clone()),
            object("Pod", "other", "two", "other", volumes),
        ]);
        let groups = Relationship::Claim.resolve(&claim, &[pods]);
        assert_eq!(groups[0].len(), 1);
        assert_eq!(groups[0][0].metadata.name.as_deref(), Some("consumer"));
    }

    #[test]
    fn cron_jobs_and_direct_controllers_reject_noncontrollers_and_wrong_namespaces() {
        let cron = object("CronJob", "schedule", "cron", "default", json!({}));
        let job = owned("Job", "job", "job", "CronJob", "cron", "batch/v1");
        let pod = owned("Pod", "pod", "pod", "Job", "job", "batch/v1");
        let mut foreign = (*pod).clone();
        foreign.metadata.namespace = Some("other".into());
        let mut observer = (*pod).clone();
        observer.metadata.name = Some("observer".into());
        observer.metadata.owner_references.as_mut().unwrap()[0].controller = Some(false);
        let mut wrong_group = (*pod).clone();
        wrong_group.metadata.name = Some("wrong-group".into());
        wrong_group.metadata.owner_references.as_mut().unwrap()[0].api_version =
            "example.com/v1".into();
        let pods = store(vec![
            pod,
            Arc::new(foreign),
            Arc::new(observer),
            Arc::new(wrong_group),
        ]);
        let relationship = Relationship::for_kind("batch", "Job").unwrap();
        assert_eq!(
            relationship.resolve(&job, std::slice::from_ref(&pods))[0].len(),
            1
        );
        let groups = Relationship::CronJob.resolve(&cron, &[store(vec![job]), pods]);
        assert_eq!(groups[0].len(), 1);
        assert_eq!(groups[1].len(), 1);
        assert!(Relationship::for_kind("example.com", "Deployment").is_none());
    }
}
