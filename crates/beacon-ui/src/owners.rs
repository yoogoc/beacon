//! Owner identities and the ReplicaSet lookup shared by Pod lists and details.
use beacon_kube::{ApiResource, ClusterSession, DynamicObject, Kind, ObjectRef};
use futures::{StreamExt as _, stream};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnerLink {
    pub api_version: String,
    pub kind: String,
    pub name: String,
    pub uid: String,
}

impl OwnerLink {
    pub fn label(&self) -> String {
        format!("{}/{}", self.kind, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ReplicaSetOwner {
    target: ObjectRef,
    uid: String,
}

impl ReplicaSetOwner {
    fn new(
        object: &DynamicObject,
        api_version: &str,
        kind: &str,
        name: &str,
        uid: &str,
    ) -> Option<Self> {
        (kind == "ReplicaSet" && api_version.starts_with("apps/")).then(|| Self {
            target: ObjectRef::new(object.metadata.namespace.clone(), name),
            uid: uid.to_owned(),
        })
    }
}

pub(crate) type ResolvedOwners = BTreeMap<ReplicaSetOwner, Vec<OwnerLink>>;

pub(crate) fn owner_links(object: &DynamicObject) -> Vec<OwnerLink> {
    object
        .metadata
        .owner_references
        .iter()
        .flatten()
        .map(|owner| OwnerLink {
            api_version: owner.api_version.clone(),
            kind: owner.kind.clone(),
            name: owner.name.clone(),
            uid: owner.uid.clone(),
        })
        .collect()
}

pub(crate) fn replica_set_targets(
    object: &DynamicObject,
) -> impl Iterator<Item = ReplicaSetOwner> + '_ {
    object
        .metadata
        .owner_references
        .iter()
        .flatten()
        .filter_map(|owner| {
            ReplicaSetOwner::new(
                object,
                &owner.api_version,
                &owner.kind,
                &owner.name,
                &owner.uid,
            )
        })
}

pub(crate) fn displayed_owners(
    object: &DynamicObject,
    resolved: &ResolvedOwners,
) -> Vec<OwnerLink> {
    owner_links(object)
        .into_iter()
        .flat_map(|owner| {
            ReplicaSetOwner::new(
                object,
                &owner.api_version,
                &owner.kind,
                &owner.name,
                &owner.uid,
            )
            .and_then(|key| resolved.get(&key))
            .filter(|parents| !parents.is_empty())
            .cloned()
            .unwrap_or_else(|| vec![owner])
        })
        .collect()
}

/// References may use an older version. Match the group and use the served API.
pub(crate) fn owner_kind<'a>(kinds: &'a [Kind], api_version: &str, name: &str) -> Option<&'a Kind> {
    let group = api_version.rsplit_once('/').map_or("", |(group, _)| group);
    kinds
        .iter()
        .find(|kind| kind.resource.group == group && kind.resource.kind == name)
}

pub(crate) fn replica_set_resource(session: &ClusterSession) -> Option<ApiResource> {
    owner_kind(session.discovery().kinds(), "apps/v1", "ReplicaSet")
        .map(|kind| kind.resource.clone())
}

pub(crate) fn replica_set_parents(uid: &str, replica_set: &DynamicObject) -> Vec<OwnerLink> {
    if replica_set.metadata.uid.as_deref() != Some(uid) {
        return Vec::new();
    }
    owner_links(replica_set)
}

pub(crate) async fn resolve_replica_sets(
    session: Arc<ClusterSession>,
    resource: ApiResource,
    targets: BTreeSet<ReplicaSetOwner>,
) -> ResolvedOwners {
    stream::iter(targets)
        .map(|key| {
            let session = session.clone();
            let resource = resource.clone();
            async move {
                let parents = session
                    .get_object(
                        resource,
                        key.target.namespace.clone(),
                        key.target.name.clone(),
                    )
                    .await
                    .map(|object| replica_set_parents(&key.uid, &object))
                    .unwrap_or_default();
                (key, parents)
            }
        })
        .buffer_unordered(8)
        .collect()
        .await
}
