//! Reviewed, create-only transfers. Source cleanup is an explicit, separate phase.
mod manifest;
mod references;
#[cfg(test)]
mod tests;
use crate::{ClusterSession, DynamicObject, Error, Kind, ObjectRef, Result};
use kube::{
    Api,
    api::{DeleteParams, Preconditions},
};
pub use manifest::unsupported;
use serde_json::Value;
use std::{collections::HashSet, sync::Arc};

#[derive(Clone, Debug)]
pub struct Selection {
    pub kind: Kind,
    pub reference: ObjectRef,
    pub uid: Option<String>,
    pub name: String,
    /// Included configuration dependencies are copied, never removed from the source.
    pub copy_only: bool,
}
#[derive(Clone, Debug)]
pub struct Options {
    pub namespace: Option<String>,
    pub moving: bool,
    pub secret_consent: bool,
}
#[derive(Clone, Debug)]
pub struct Dependency {
    pub kind: Kind,
    pub reference: ObjectRef,
    pub uid: Option<String>,
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetState {
    Uncreated,
    /// The response was lost. A repeat POST must not adopt an unrelated object.
    Uncertain,
    Created {
        uid: String,
    },
}
#[derive(Clone, Debug)]
pub struct Item {
    selection: Selection,
    pub owner: Option<Selection>,
    source: Option<DynamicObject>,
    target_data: Option<Value>,
    target_kind: Option<Kind>,
    manifest: Option<Value>,
    pub problem: Option<String>,
    pub conflict: bool,
    pub target: TargetState,
    pub verified: bool,
    pub delete_requested: bool,
    pub deleted: bool,
}
impl Item {
    pub fn selection(&self) -> &Selection {
        &self.selection
    }
    pub fn target_kind(&self) -> Option<&Kind> {
        self.target_kind.as_ref()
    }
    /// Masks Secret data in the read-only review; the write manifest stays private.
    pub fn preview(&self) -> Option<Value> {
        let mut value = self.manifest.clone()?;
        if self.selection.kind.resource.group.is_empty()
            && self.selection.kind.resource.kind == "Secret"
        {
            for field in ["data", "stringData"] {
                if let Some(data) = value.get_mut(field).and_then(Value::as_object_mut) {
                    for value in data.values_mut() {
                        *value = Value::String("••••••••".into());
                    }
                }
            }
        }
        Some(value)
    }
}
#[derive(Clone)]
pub struct Batch {
    source: Arc<ClusterSession>,
    destination: Arc<ClusterSession>,
    options: Options,
    items: Vec<Item>,
    dependencies: Vec<Dependency>,
    pub warnings: Vec<String>,
    started: bool,
    prepared: bool,
}
fn api(
    session: &ClusterSession,
    kind: &Kind,
    namespace: Option<&str>,
    write: bool,
) -> Api<DynamicObject> {
    let client = if write {
        session.write_client()
    } else {
        session.client()
    }
    .clone();
    match namespace {
        Some(ns) => Api::namespaced_with(client, ns, &kind.resource),
        None => Api::all_with(client, &kind.resource),
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::Manifest(message.into())
}
fn api_missing(error: &kube::Error) -> bool {
    matches!(error, kube::Error::Api(status) if status.code == 404)
}
fn same_server(left: &ClusterSession, right: &ClusterSession) -> bool {
    left.server().trim_end_matches('/') == right.server().trim_end_matches('/')
}

impl Batch {
    pub fn items(&self) -> &[Item] {
        &self.items
    }
    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }
    pub fn moving(&self) -> bool {
        self.options.moving
    }
    pub fn namespace(&self) -> Option<&str> {
        self.options.namespace.as_deref()
    }
    pub fn destination(&self) -> &Arc<ClusterSession> {
        &self.destination
    }
    pub fn has_targets(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.target != TargetState::Uncreated)
    }
    pub fn ready(&self) -> bool {
        self.prepared && !self.items.is_empty()
    }
    pub fn can_cleanup(&self) -> bool {
        self.options.moving
            && self.started
            && !self.items.is_empty()
            && self
                .items
                .iter()
                .all(|item| item.verified && matches!(item.target, TargetState::Created { .. }))
    }
    pub fn complete(&self) -> bool {
        self.can_cleanup()
            && self
                .items
                .iter()
                .all(|item| item.selection.copy_only || item.deleted)
    }

    /// Fetches full source objects, pins versions, checks API compatibility,
    /// target conflicts, references and dry-run admission without writing either cluster.
    pub async fn check(
        source: Arc<ClusterSession>,
        destination: Arc<ClusterSession>,
        selections: Vec<Selection>,
        options: Options,
    ) -> Self {
        let mut batch = Self {
            source,
            destination,
            options,
            items: vec![],
            dependencies: vec![],
            warnings: vec![],
            started: false,
            prepared: false,
        };
        let mut destinations = HashSet::new();
        for selection in selections {
            let mut item = Item {
                selection,
                owner: None,
                source: None,
                target_data: None,
                target_kind: None,
                manifest: None,
                problem: None,
                conflict: false,
                target: TargetState::Uncreated,
                verified: false,
                delete_requested: false,
                deleted: false,
            };
            if let Err(error) = batch.check_item(&mut item).await {
                item.problem = Some(error.user_message());
            }
            if let Some(kind) = &item.target_kind {
                let key = (
                    kind.resource.group.clone(),
                    kind.resource.kind.clone(),
                    batch.options.namespace.clone(),
                    item.selection.name.clone(),
                );
                if !destinations.insert(key) {
                    item.problem = Some("Multiple source resources map to the same target name. Rename or skip one of them.".into());
                }
            }
            batch.items.push(item);
        }
        batch.check_references().await;
        for item in &mut batch.items {
            if item.problem.is_none()
                && let (Some(kind), Some(manifest)) = (&item.target_kind, &item.manifest)
                && let Err(error) = batch
                    .destination
                    .clone()
                    .create(kind.clone(), manifest.clone(), true)
                    .await
            {
                item.problem = Some(error.user_message());
            }
        }
        if batch.options.moving && batch.items.iter().all(|item| item.selection.copy_only) {
            for item in &mut batch.items {
                item.problem = Some(
                    "Select at least one source resource to move; dependencies are copied only."
                        .into(),
                );
            }
        }
        batch
            .items
            .sort_by_key(|item| match item.selection.kind.resource.kind.as_str() {
                "Secret" | "ConfigMap" => 0,
                "ServiceAccount" | "Role" | "ClusterRole" => 1,
                "Service" => 2,
                _ => 3,
            });
        batch.prepared = !batch.items.is_empty()
            && batch.dependencies.is_empty()
            && batch.items.iter().all(|item| item.problem.is_none());
        batch
    }
    async fn check_item(&mut self, item: &mut Item) -> Result<()> {
        let selection = &item.selection;
        let kind = &selection.kind;
        if let Some(reason) = unsupported(kind, None) {
            return Err(invalid(reason));
        }
        let current = api(
            &self.source,
            kind,
            selection.reference.namespace.as_deref(),
            false,
        )
        .get(&selection.reference.name)
        .await?;
        if selection.uid.as_ref().is_none_or(String::is_empty)
            || selection.uid != current.metadata.uid
        {
            return Err(invalid(
                "The selected source was replaced or has no UID. Refresh the resource list.",
            ));
        }
        if current
            .metadata
            .resource_version
            .as_ref()
            .is_none_or(String::is_empty)
            || current.metadata.deletion_timestamp.is_some()
        {
            return Err(invalid(
                "The source is being deleted or has no resource version.",
            ));
        }
        if let Some(owner) = current
            .metadata
            .owner_references
            .as_ref()
            .and_then(|owners| {
                owners
                    .iter()
                    .find(|owner| owner.controller == Some(true))
                    .or_else(|| owners.first())
            })
            && let Some(owner_kind) = self.source.discovery().kinds().iter().find(|kind| {
                kind.resource.api_version == owner.api_version && kind.resource.kind == owner.kind
            })
        {
            item.owner = Some(Selection {
                kind: owner_kind.clone(),
                reference: ObjectRef::new(
                    owner_kind
                        .namespaced
                        .then(|| selection.reference.namespace.clone())
                        .flatten(),
                    owner.name.clone(),
                ),
                uid: Some(owner.uid.clone()),
                name: owner.name.clone(),
                copy_only: false,
            });
        }
        if let Some(reason) = unsupported(kind, Some(&current)) {
            return Err(invalid(reason));
        }
        let target_kind = self
            .destination
            .discovery()
            .kinds()
            .iter()
            .find(|target| target.gvk() == kind.gvk() && target.namespaced == kind.namespaced)
            .cloned()
            .ok_or_else(|| {
                invalid(
                    "The target cluster does not serve this exact API version and resource scope.",
                )
            })?;
        if kind.namespaced && self.options.namespace.is_none() {
            return Err(invalid(
                "Choose a target namespace for namespaced resources; cluster-scoped resources have no namespace.",
            ));
        }
        if self
            .options
            .namespace
            .as_ref()
            .is_some_and(|ns| ns.trim().is_empty())
        {
            return Err(invalid("Choose a target namespace."));
        }
        if same_server(&self.source, &self.destination)
            && selection.reference.namespace == self.options.namespace
            && selection.reference.name == selection.name
        {
            return Err(invalid(
                "The source and target are identical. Choose another location or target name.",
            ));
        }
        let namespace = kind
            .namespaced
            .then_some(self.options.namespace.as_deref())
            .flatten();
        let manifest = manifest::prepare(kind, &current, &selection.name, namespace)?;
        if current.metadata.annotations.as_ref().is_some_and(|a| {
            a.keys()
                .any(|key| key.starts_with("meta.helm.sh/") || key.contains("argocd.argoproj.io/"))
        }) || current.metadata.labels.as_ref().is_some_and(|a| {
            a.get("app.kubernetes.io/managed-by")
                .is_some_and(|v| v == "Helm")
        }) {
            let warning = "Helm or GitOps manages this resource. The copy is independent; update the source configuration to prevent reconciliation.".to_string();
            if !self.warnings.contains(&warning) {
                self.warnings.push(warning);
            }
        }
        item.source = Some(current);
        let mut manifest = manifest;
        let marker = format!(
            "{:x}-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            std::process::id()
        );
        if !manifest["metadata"]["annotations"].is_object() {
            manifest["metadata"]["annotations"] = serde_json::json!({});
        }
        manifest["metadata"]["annotations"]["beacon.yoogoc.io/transfer-id"] = Value::String(marker);
        item.manifest = Some(manifest.clone());
        item.target_kind = Some(target_kind.clone());
        match api(&self.destination, &target_kind, namespace, false)
            .get(&selection.name)
            .await
        {
            Ok(_) => {
                item.conflict = true;
                return Err(invalid(
                    "A target resource with this name already exists. Rename or skip it; existing resources are never overwritten.",
                ));
            }
            Err(error) if api_missing(&error) => {}
            Err(error) => return Err(error.into()),
        }
        if kind.resource.kind == "Secret"
            && kind.resource.group.is_empty()
            && !self.options.secret_consent
        {
            return Err(invalid(
                "Confirm sending Secret contents to the selected destination.",
            ));
        }
        if self.options.moving && !selection.copy_only {
            let rules = self
                .source
                .clone()
                .rules(selection.reference.namespace.clone())
                .await;
            if !kind.supports("delete")
                || !rules.allows_named(
                    "delete",
                    &kind.resource.group,
                    &kind.resource.plural,
                    &selection.reference.name,
                )
            {
                return Err(invalid("Source delete permission is required for a move."));
            }
        }
        Ok(())
    }

    /// Each retry skips confirmed creations. Ambiguous POST failures are kept
    /// unresolved: GET alone cannot prove an existing object was created by us.
    pub async fn create(&mut self) -> Result<()> {
        if !self.prepared {
            return Err(invalid(
                "Resolve the transfer checks before creating target resources.",
            ));
        }
        // Validate the entire frozen source snapshot before any writes.
        for item in &self.items {
            if !matches!(item.target, TargetState::Created { .. }) {
                self.check_source(item).await?;
            }
        }
        self.started = true;
        for item in &mut self.items {
            if matches!(item.target, TargetState::Created { .. }) {
                continue;
            }
            let kind = item.target_kind.clone().expect("checked kind");
            if item.target == TargetState::Uncertain {
                match api(
                    &self.destination,
                    &kind,
                    kind.namespaced
                        .then_some(self.options.namespace.as_deref())
                        .flatten(),
                    false,
                )
                .get(&item.selection.name)
                .await
                {
                    Ok(object) => {
                        let marker = item
                            .manifest
                            .as_ref()
                            .and_then(|v| {
                                v.pointer("/metadata/annotations/beacon.yoogoc.io~1transfer-id")
                            })
                            .and_then(Value::as_str);
                        if manifest::contains_intent(
                            item.manifest.as_ref().expect("checked manifest"),
                            &object,
                        ) && object
                            .metadata
                            .annotations
                            .as_ref()
                            .and_then(|a| a.get("beacon.yoogoc.io/transfer-id"))
                            .map(String::as_str)
                            == marker
                            && marker.is_some()
                        {
                            if let Some(uid) =
                                object.metadata.uid.clone().filter(|uid| !uid.is_empty())
                            {
                                item.target_data = Some(manifest::target_snapshot(&object));
                                item.target = TargetState::Created { uid };
                                item.problem = None;
                            }
                        } else {
                            item.problem=Some("An unrelated target already exists. It was not adopted or overwritten; source cleanup is blocked.".into());
                        }
                        continue;
                    }
                    Err(error) if api_missing(&error) => {
                        item.target = TargetState::Uncreated;
                    }
                    Err(error) => {
                        item.problem = Some(Error::from(error).user_message());
                        continue;
                    }
                }
            }

            match self
                .destination
                .clone()
                .create(
                    kind,
                    item.manifest.clone().expect("checked manifest"),
                    false,
                )
                .await
            {
                Ok(object) => {
                    item.target_data = Some(manifest::target_snapshot(&object));
                    if let Some(uid) = object.metadata.uid.filter(|uid| !uid.is_empty()) {
                        item.target = TargetState::Created { uid };
                        item.problem = None;
                    } else {
                        item.target = TargetState::Uncertain;
                        item.problem = Some("The create response has no UID. Inspect the target; source cleanup is blocked.".into());
                    }
                }
                Err(error) => {
                    // A definite API rejection is safe to retry, except an
                    // AlreadyExists response must always go back through review.
                    if !matches!(&error, Error::Api(kube::Error::Api(status)) if (400..500).contains(&status.code) && status.code != 408 && status.code != 409)
                    {
                        item.target = TargetState::Uncertain;
                    }
                    item.problem = Some(error.user_message());
                }
            }
        }
        self.verify().await;
        Ok(())
    }
    async fn check_source(&self, item: &Item) -> Result<()> {
        let current = api(
            &self.source,
            &item.selection.kind,
            item.selection.reference.namespace.as_deref(),
            false,
        )
        .get(&item.selection.reference.name)
        .await?;
        let original = item
            .source
            .as_ref()
            .ok_or_else(|| invalid("The source snapshot is missing."))?;
        if current.metadata.uid != original.metadata.uid
            || current.metadata.resource_version != original.metadata.resource_version
            || current.metadata.deletion_timestamp.is_some()
        {
            return Err(invalid(format!(
                "Source {} changed after review. Keep both sides and start a fresh transfer review.",
                item.selection.reference
            )));
        }
        Ok(())
    }
    /// Fresh reads after POST, and again before cleanup. Workloads must have
    /// observed the new generation and reached their desired replica counts.
    pub async fn verify(&mut self) {
        for item in &mut self.items {
            item.verified = false;
            let TargetState::Created { uid } = &item.target else {
                continue;
            };
            let kind = item.target_kind.as_ref().expect("checked kind");
            match api(
                &self.destination,
                kind,
                kind.namespaced
                    .then_some(self.options.namespace.as_deref())
                    .flatten(),
                false,
            )
            .get(&item.selection.name)
            .await
            {
                Ok(object) if object.metadata.uid.as_ref() == Some(uid) => {
                    let data = manifest::target_snapshot(&object);
                    item.problem = if item.target_data.as_ref() != Some(&data) {
                        Some("The target configuration changed after creation. Source cleanup is blocked.".into())
                    } else {
                        manifest::waiting(kind, &object)
                    };
                    item.verified = item.problem.is_none();
                }
                Ok(_) => {
                    item.problem = Some(
                        "The target was replaced after creation. Source cleanup is blocked.".into(),
                    )
                }
                Err(error) => item.problem = Some(Error::from(error).user_message()),
            }
        }
    }
    /// Requires a separate explicit UI confirmation. Validate *all* targets
    /// and source versions before sending any source DELETE, then use atomic
    /// UID + resourceVersion preconditions on each individual request.
    pub async fn cleanup(&mut self) -> Result<()> {
        if !self.options.moving {
            return Err(invalid("A copy must never delete source resources."));
        }
        self.verify().await;
        if !self.can_cleanup() {
            return Err(invalid(
                "Every target must be created and verified before source cleanup.",
            ));
        }
        for item in &self.items {
            if !item.selection.copy_only && !item.deleted && !item.delete_requested {
                self.check_source(item).await?;
            }
        }
        for item in &mut self.items {
            if item.selection.copy_only || item.deleted {
                continue;
            }
            let kind = &item.selection.kind;
            let original = item.source.as_ref().expect("source snapshot");
            let source_api = api(
                &self.source,
                kind,
                item.selection.reference.namespace.as_deref(),
                true,
            );
            if !item.delete_requested {
                let params = DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: original.metadata.uid.clone(),
                        resource_version: original.metadata.resource_version.clone(),
                    }),
                    ..Default::default()
                };
                match source_api
                    .delete(&item.selection.reference.name, &params)
                    .await
                {
                    Ok(_) => {
                        item.delete_requested = true;
                        item.problem = None;
                    }
                    Err(error) => {
                        item.problem = Some(Error::from(error).user_message());
                        continue;
                    }
                }
            }
            match source_api.get(&item.selection.reference.name).await {
                Err(error) if api_missing(&error) => { item.deleted = true; item.problem = None; }
                Ok(object) if object.metadata.uid != original.metadata.uid => { item.deleted = true; item.problem = Some("The reviewed source was deleted; a replacement now uses its name and was not touched.".into()); }
                Ok(_) => item.problem = Some("Deletion requested; waiting for the source to disappear. Finalizers are not removed.".into()),
                Err(error) => item.problem = Some(Error::from(error).user_message()),
            }
        }
        Ok(())
    }
}
