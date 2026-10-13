//! Reviewed Node scheduling changes and policy/v1 evictions. Drain never falls
//! back to deleting Pods, so the API server continues to enforce PDBs.
//!
//! https://kubernetes.io/docs/tasks/administer-cluster/safely-drain-node/
//! https://kubernetes.io/docs/concepts/scheduling-eviction/api-eviction/
use crate::{
    ApiResource, ClusterSession, DynamicObject, Error, GroupVersionKind, ObjectRef, Result,
    WatchKey, labels::LabelSelector, resources,
};
use futures::{StreamExt as _, channel::mpsc::UnboundedSender, stream};
use kube::{
    Api,
    api::{DeleteParams, Patch, PatchParams, PostParams, Preconditions, Request},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Budget {
    pub target: ObjectRef,
    pub disruptions: Option<i64>,
    pub blocked: bool,
}

#[derive(Clone, Debug)]
pub struct PodTarget {
    pub target: ObjectRef,
    pub uid: String,
    pub owner: String,
    pub skipped: Option<String>,
    pub unmanaged: bool,
    pub empty_dir: Vec<String>,
    pub storage: Vec<String>,
    pub budgets: Vec<Budget>,
    pub budgets_known: bool,
    pub allowed: bool,
}

#[derive(Clone)]
pub struct Plan {
    pub node: String,
    pub uid: String,
    pub version: String,
    pub was_cordoned: bool,
    pub pods: Vec<PodTarget>,
    pub warnings: Vec<String>,
}

#[derive(Clone)]
pub struct Options {
    pub allow_empty_dir: bool,
    pub force_unmanaged: bool,
    pub timeout: Duration,
    pub grace_seconds: Option<u32>,
    pub retry_interval: Duration,
    pub poll_interval: Duration,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            allow_empty_dir: false,
            force_unmanaged: false,
            timeout: Duration::from_secs(300),
            grace_seconds: None,
            retry_interval: Duration::from_secs(5),
            poll_interval: Duration::from_secs(1),
        }
    }
}

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PodState {
    Pending,
    Skipped(String),
    Evicting,
    Blocked(String),
    Terminating,
    Evicted,
    Failed(String),
    Cancelled,
}
impl PodState {
    pub fn label(&self) -> String {
        match self {
            Self::Pending => "Ready to evict".into(),
            Self::Skipped(reason) => format!("Skipped · {reason}"),
            Self::Evicting => "Requesting eviction…".into(),
            Self::Blocked(reason) => format!("Waiting · {reason}"),
            Self::Terminating => "Waiting for termination…".into(),
            Self::Evicted => "Evicted".into(),
            Self::Failed(reason) => format!("Failed · {reason}"),
            Self::Cancelled => "Cancelled; submitted evictions cannot be undone".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Report {
    pub states: Vec<PodState>,
    pub remaining: Vec<ObjectRef>,
}
impl Report {
    pub fn summary(&self) -> String {
        let count = |predicate: fn(&PodState) -> bool| {
            self.states.iter().filter(|state| predicate(state)).count()
        };
        format!(
            "{} evicted · {} skipped · {} failed · {} cancelled · {} remaining. Node remains cordoned.",
            count(|state| matches!(state, PodState::Evicted)),
            count(|state| matches!(state, PodState::Skipped(_))),
            count(|state| matches!(state, PodState::Failed(_))),
            count(|state| matches!(state, PodState::Cancelled)),
            self.remaining.len()
        )
    }
    pub fn complete(&self) -> bool {
        self.remaining.is_empty()
            && !self
                .states
                .iter()
                .any(|state| matches!(state, PodState::Failed(_) | PodState::Cancelled))
    }
}

#[derive(Clone, Debug)]
pub enum Progress {
    Cordoned,
    Pod { index: usize, state: PodState },
}

impl Plan {
    pub fn validate(&self, options: &Options) -> Result<()> {
        if options.timeout.is_zero() {
            return Err(Error::Manifest("provide a positive drain timeout".into()));
        }
        for pod in self.pods.iter().filter(|pod| pod.skipped.is_none()) {
            if pod.uid.is_empty() {
                return Err(Error::Manifest(format!(
                    "{} has no UID; refresh the preview",
                    pod.target
                )));
            }
            if !pod.allowed {
                return Err(Error::Manifest(format!(
                    "you do not have create on pods/eviction for {}",
                    pod.target
                )));
            }
            if !pod.empty_dir.is_empty() && !options.allow_empty_dir {
                return Err(Error::Manifest(
                    "acknowledge deletion of emptyDir data before draining".into(),
                ));
            }
            if pod.unmanaged && !options.force_unmanaged {
                return Err(Error::Manifest(
                    "acknowledge Pods without a controller before draining".into(),
                ));
            }
        }
        Ok(())
    }
}

fn resource(group: &str, kind: &str) -> ApiResource {
    ApiResource::from_gvk(&GroupVersionKind::gvk(group, "v1", kind))
}

async fn pods(session: Arc<ClusterSession>, node: &str) -> Result<Vec<Arc<DynamicObject>>> {
    Ok(session
        .list_objects(WatchKey::all(resources::pod()).with_fields(format!("spec.nodeName={node}")))
        .await?
        .into_iter()
        .filter(|pod| pod.data.pointer("/spec/nodeName").and_then(Value::as_str) == Some(node))
        .collect())
}

fn classify(pod: &DynamicObject) -> PodTarget {
    let owner = pod
        .metadata
        .owner_references
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|owner| owner.controller == Some(true));
    let mirror = pod
        .metadata
        .annotations
        .as_ref()
        .is_some_and(|annotations| annotations.contains_key("kubernetes.io/config.mirror"));
    let daemon = owner
        .is_some_and(|owner| owner.kind == "DaemonSet" && owner.api_version.starts_with("apps/"));
    let mut empty_dir = vec![];
    let mut storage = vec![];
    for volume in pod
        .data
        .pointer("/spec/volumes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = volume
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("volume");
        if volume.get("emptyDir").is_some() {
            empty_dir.push(name.into());
            storage.push(format!("emptyDir · {name} · data deleted on eviction"));
        }
        if let Some(path) = volume.pointer("/hostPath/path").and_then(Value::as_str) {
            storage.push(format!(
                "hostPath · {name} · {path} · retained on this Node"
            ));
        }
    }
    PodTarget {
        target: ObjectRef::of(pod),
        uid: pod.metadata.uid.clone().unwrap_or_default(),
        owner: owner.map_or_else(
            || "No controller".into(),
            |owner| format!("{}/{}", owner.kind, owner.name),
        ),
        skipped: if mirror {
            Some("mirror/static Pod".into())
        } else if daemon {
            Some("DaemonSet Pod".into())
        } else {
            None
        },
        unmanaged: owner.is_none(),
        empty_dir,
        storage,
        budgets: vec![],
        budgets_known: true,
        allowed: true,
    }
}

/// policy/v1's empty selector matches every Pod in its namespace; an absent
/// selector matches none. Other selectors share the resource list semantics.
pub fn pdb_matches(pdb: &DynamicObject, pod: &DynamicObject) -> bool {
    if pdb.metadata.namespace != pod.metadata.namespace {
        return false;
    }
    let Some(selector) = pdb
        .data
        .pointer("/spec/selector")
        .filter(|selector| selector.is_object())
    else {
        return false;
    };
    let empty = selector
        .get("matchLabels")
        .and_then(Value::as_object)
        .is_none_or(|labels| labels.is_empty())
        && selector
            .get("matchExpressions")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty);
    if empty {
        return true;
    }
    let object = DynamicObject {
        metadata: Default::default(),
        types: None,
        data: json!({"spec":{"selector":selector}}),
    };
    crate::aggregate_logs::workload_selector(&object)
        .ok()
        .and_then(|selector| LabelSelector::parse(&selector).ok())
        .is_some_and(|selector| selector.matches(pod.metadata.labels.as_ref()))
}

pub fn pdb_blocks(pdb: &DynamicObject, pod: &DynamicObject) -> bool {
    if pod.metadata.deletion_timestamp.is_some() {
        return false;
    }
    if matches!(
        pod.data.pointer("/status/phase").and_then(Value::as_str),
        Some("Pending" | "Succeeded" | "Failed")
    ) {
        return false;
    }
    let ready = pod
        .data
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition["type"] == "Ready" && condition["status"] == "True")
        });
    let always = pdb
        .data
        .pointer("/spec/unhealthyPodEvictionPolicy")
        .and_then(Value::as_str)
        == Some("AlwaysAllow");
    let healthy_budget = pdb
        .data
        .pointer("/status/currentHealthy")
        .and_then(Value::as_i64)
        .zip(
            pdb.data
                .pointer("/status/desiredHealthy")
                .and_then(Value::as_i64),
        )
        .is_some_and(|(current, desired)| current >= desired && desired > 0);
    let enforce = ready || (!always && !healthy_budget);
    let stale = pdb.metadata.generation.is_some_and(|generation| {
        pdb.data
            .pointer("/status/observedGeneration")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            < generation
    });
    enforce
        && (stale
            || pdb
                .data
                .pointer("/status/disruptionsAllowed")
                .and_then(Value::as_i64)
                .is_none_or(|allowed| allowed <= 0))
}

pub async fn prepare(
    session: Arc<ClusterSession>,
    node: String,
    expected_uid: Option<String>,
    drain: bool,
) -> Result<Plan> {
    let current = session
        .clone()
        .get_object(resource("", "Node"), None, node.clone())
        .await?;
    if expected_uid
        .as_ref()
        .is_some_and(|uid| current.metadata.uid.as_ref() != Some(uid))
    {
        return Err(Error::Manifest(
            "the Node was replaced; refresh before maintenance".into(),
        ));
    }
    let rules = session.clone().rules(None).await;
    if !rules.allows_named("patch", "", "nodes", &node) {
        return Err(Error::Manifest(
            "you do not have patch on nodes in this cluster".into(),
        ));
    }
    let mut plan = Plan {
        node,
        uid: current
            .metadata
            .uid
            .clone()
            .ok_or_else(|| Error::Manifest("the Node has no UID".into()))?,
        version: current
            .metadata
            .resource_version
            .clone()
            .ok_or_else(|| Error::Manifest("the Node has no resourceVersion".into()))?,
        was_cordoned: current
            .data
            .pointer("/spec/unschedulable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        pods: vec![],
        warnings: vec![],
    };
    if !drain {
        return Ok(plan);
    }
    let objects = pods(session.clone(), &plan.node).await?;
    let namespaces: BTreeSet<_> = objects
        .iter()
        .filter_map(|pod| pod.metadata.namespace.clone())
        .collect();
    let mut budgets = vec![];
    let mut unknown_budgets = BTreeSet::new();
    let mut permissions = BTreeMap::new();
    for namespace in namespaces {
        permissions.insert(
            namespace.clone(),
            session.clone().rules(Some(namespace.clone())).await,
        );
        match session
            .clone()
            .list_objects(
                WatchKey::all(resource("policy", "PodDisruptionBudget"))
                    .in_namespace(Some(namespace.clone())),
            )
            .await
        {
            Ok(listed) => budgets.extend(listed),
            Err(error) => {
                unknown_budgets.insert(namespace.clone());
                plan.warnings.push(format!(
                    "PDBs in {namespace} could not be read: {}. Eviction still enforces budgets.",
                    error.user_message()
                ));
            }
        }
    }
    let mut claims = BTreeMap::new();
    let mut volumes = BTreeMap::new();
    for pod in objects {
        let mut target = classify(&pod);
        target.budgets_known = pod
            .metadata
            .namespace
            .as_ref()
            .is_some_and(|namespace| !unknown_budgets.contains(namespace));
        target.allowed = pod
            .metadata
            .namespace
            .as_ref()
            .and_then(|namespace| permissions.get(namespace))
            .is_some_and(|rules| {
                rules.allows_named("create", "", "pods/eviction", &target.target.name)
            });
        target.budgets = budgets
            .iter()
            .filter(|pdb| pdb_matches(pdb, &pod))
            .map(|pdb| Budget {
                target: ObjectRef::of(pdb),
                disruptions: pdb
                    .data
                    .pointer("/status/disruptionsAllowed")
                    .and_then(Value::as_i64),
                blocked: pdb_blocks(pdb, &pod),
            })
            .collect();
        for volume in pod
            .data
            .pointer("/spec/volumes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(claim) = volume
                .pointer("/persistentVolumeClaim/claimName")
                .and_then(Value::as_str)
            {
                let namespace = target.target.namespace.clone().unwrap_or_default();
                let key = (namespace.clone(), claim.to_owned());
                if !claims.contains_key(&key) {
                    claims.insert(
                        key.clone(),
                        session
                            .clone()
                            .get_object(
                                resource("", "PersistentVolumeClaim"),
                                Some(namespace),
                                claim.into(),
                            )
                            .await
                            .map_err(|error| error.user_message()),
                    );
                }
                match &claims[&key] {
                    Ok(pvc) => {
                        if let Some(name) =
                            pvc.data.pointer("/spec/volumeName").and_then(Value::as_str)
                        {
                            if !volumes.contains_key(name) {
                                volumes.insert(
                                    name.to_owned(),
                                    session
                                        .clone()
                                        .get_object(
                                            resource("", "PersistentVolume"),
                                            None,
                                            name.into(),
                                        )
                                        .await
                                        .map_err(|error| error.user_message()),
                                );
                            }
                            match &volumes[name] {
                                Ok(pv) => {
                                    if let Some(path) = pv
                                        .data
                                        .pointer("/spec/local/path")
                                        .and_then(Value::as_str)
                                        .or_else(|| {
                                            pv.data
                                                .pointer("/spec/hostPath/path")
                                                .and_then(Value::as_str)
                                        })
                                    {
                                        target.storage.push(format!(
                                            "Local PV · {name} · {path} · data stays on this Node"
                                        ));
                                    }
                                }
                                Err(error) => target.storage.push(format!(
                                    "PVC {claim} / PV {name}: locality unknown · {error}"
                                )),
                            }
                        }
                    }
                    Err(error) => target
                        .storage
                        .push(format!("PVC {claim}: locality unknown · {error}")),
                }
            }
        }
        plan.pods.push(target);
    }
    plan.pods.sort_by(|a, b| a.target.cmp(&b.target));
    Ok(plan)
}

async fn current_node(session: Arc<ClusterSession>, plan: &Plan) -> Result<DynamicObject> {
    let node = session
        .clone()
        .get_object(resource("", "Node"), None, plan.node.clone())
        .await?;
    if node.metadata.uid.as_deref() != Some(&plan.uid)
        || node.metadata.resource_version.as_deref() != Some(&plan.version)
    {
        return Err(Error::Manifest(
            "the Node changed after review; refresh the maintenance preview".into(),
        ));
    }
    let rules = session.rules(None).await;
    if !rules.allows_named("patch", "", "nodes", &plan.node) {
        return Err(Error::Manifest(
            "you do not have patch on nodes in this cluster".into(),
        ));
    }
    Ok(node)
}

pub async fn cordon(
    session: Arc<ClusterSession>,
    plan: &Plan,
    unschedulable: bool,
) -> Result<DynamicObject> {
    current_node(session.clone(), plan).await?;
    let api: Api<DynamicObject> =
        Api::all_with(session.write_client().clone(), &resource("", "Node"));
    Ok(api.patch(&plan.node,&PatchParams::default(),&Patch::Merge(json!({"metadata":{"uid":plan.uid,"resourceVersion":plan.version},"spec":{"unschedulable":unschedulable}}))).await?)
}

async fn eviction(session: &ClusterSession, target: &PodTarget, options: &Options) -> Result<()> {
    let namespace = target
        .target
        .namespace
        .as_deref()
        .ok_or_else(|| Error::Manifest("the Pod has no namespace".into()))?;
    let params = DeleteParams {
        preconditions: Some(Preconditions {
            uid: Some(target.uid.clone()),
            ..Default::default()
        }),
        grace_period_seconds: options.grace_seconds,
        ..Default::default()
    };
    let body = json!({"apiVersion":"policy/v1","kind":"Eviction","metadata":{"name":target.target.name,"namespace":namespace},"deleteOptions":params});
    let request = Request::new(format!(
        "/api/v1/namespaces/{namespace}/pods/{}/eviction",
        target.target.name
    ))
    .create(
        &PostParams::default(),
        serde_json::to_vec(&body).map_err(|error| Error::Manifest(error.to_string()))?,
    )
    .map_err(|error| Error::Manifest(error.to_string()))?;
    session
        .write_client()
        .request::<kube::core::Status>(request)
        .await?;
    Ok(())
}

async fn pause(duration: Duration, cancellation: &Cancellation) {
    let until = tokio::time::Instant::now() + duration;
    while !cancellation.cancelled() && tokio::time::Instant::now() < until {
        tokio::time::sleep(
            Duration::from_millis(50)
                .min(until.saturating_duration_since(tokio::time::Instant::now())),
        )
        .await;
    }
}

async fn within_deadline<T>(
    deadline: tokio::time::Instant,
    work: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::time::timeout_at(deadline, work).await.map_err(|_| {
        Error::Manifest("drain timeout; a submitted request may still finish on the server".into())
    })?
}

async fn evict_one(
    session: Arc<ClusterSession>,
    index: usize,
    target: PodTarget,
    options: Options,
    cancellation: Cancellation,
    deadline: tokio::time::Instant,
    progress: UnboundedSender<Progress>,
) -> (usize, PodState) {
    let send = |state: PodState| {
        let _ = progress.unbounded_send(Progress::Pod { index, state });
    };
    if let Some(reason) = target.skipped {
        return (index, PodState::Skipped(reason));
    }
    let namespace = target.target.namespace.clone();
    let name = target.target.name.clone();
    loop {
        if cancellation.cancelled() {
            return (index, PodState::Cancelled);
        }
        if tokio::time::Instant::now() >= deadline {
            return (
                index,
                PodState::Failed("drain timed out before eviction".into()),
            );
        }
        match within_deadline(
            deadline,
            session
                .clone()
                .get_object(resources::pod(), namespace.clone(), name.clone()),
        )
        .await
        {
            Ok(current) if current.metadata.uid.as_deref() != Some(&target.uid) => {
                return (
                    index,
                    PodState::Failed("Pod was replaced; the new Pod was not evicted".into()),
                );
            }
            Err(Error::Api(kube::Error::Api(status))) if status.code == 404 => {
                return (index, PodState::Evicted);
            }
            Err(error) => return (index, PodState::Failed(error.user_message())),
            _ => {}
        }
        send(PodState::Evicting);
        match within_deadline(deadline, eviction(&session, &target, &options)).await {
            Ok(()) => break,
            Err(Error::Api(kube::Error::Api(status))) if status.code == 404 => {
                return (index, PodState::Evicted);
            }
            Err(Error::Api(kube::Error::Api(status))) if status.code == 429 => {
                send(PodState::Blocked(status.message));
                pause(
                    options
                        .retry_interval
                        .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
                    &cancellation,
                )
                .await;
            }
            Err(error) => return (index, PodState::Failed(error.user_message())),
        }
    }
    send(PodState::Terminating);
    loop {
        if cancellation.cancelled() {
            return (index, PodState::Cancelled);
        }
        if tokio::time::Instant::now() >= deadline {
            return (
                index,
                PodState::Failed("timed out waiting for Pod termination".into()),
            );
        }
        match within_deadline(
            deadline,
            session
                .clone()
                .get_object(resources::pod(), namespace.clone(), name.clone()),
        )
        .await
        {
            Err(Error::Api(kube::Error::Api(status))) if status.code == 404 => {
                return (index, PodState::Evicted);
            }
            Ok(current) if current.metadata.uid.as_deref() != Some(&target.uid) => {
                return (index, PodState::Evicted);
            }
            Err(error) => return (index, PodState::Failed(error.user_message())),
            _ => {
                pause(
                    options
                        .poll_interval
                        .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
                    &cancellation,
                )
                .await
            }
        }
    }
}

/// Re-check the reviewed Pod identities, cordon once, then evict up to four
/// Pods concurrently. Failed and PDB-blocked requests never turn into DELETEs.
pub async fn drain(
    session: Arc<ClusterSession>,
    plan: Plan,
    options: Options,
    cancellation: Cancellation,
    progress: UnboundedSender<Progress>,
) -> Result<Report> {
    plan.validate(&options)?;
    current_node(session.clone(), &plan).await?;
    let reviewed: BTreeSet<_> = plan.pods.iter().map(|pod| pod.uid.as_str()).collect();
    let before = pods(session.clone(), &plan.node).await?;
    if before
        .iter()
        .any(|pod| !reviewed.contains(pod.metadata.uid.as_deref().unwrap_or("")))
    {
        return Err(Error::Manifest(
            "new Pods appeared after review; refresh the drain preview".into(),
        ));
    }
    if cancellation.cancelled() {
        return Err(Error::Manifest("drain cancelled before cordoning".into()));
    }
    cordon(session.clone(), &plan, true).await?;
    let _ = progress.unbounded_send(Progress::Cordoned);
    let after = pods(session.clone(), &plan.node).await?;
    if after
        .iter()
        .any(|pod| !reviewed.contains(pod.metadata.uid.as_deref().unwrap_or("")))
    {
        return Err(Error::Manifest(
            "Node is cordoned, but new Pods appeared; refresh the drain preview before evicting"
                .into(),
        ));
    }
    let deadline = tokio::time::Instant::now() + options.timeout;
    let results: Vec<_> = stream::iter(plan.pods.iter().cloned().enumerate())
        .map(|(index, target)| {
            let (session, options, cancellation, progress) = (
                session.clone(),
                options.clone(),
                cancellation.clone(),
                progress.clone(),
            );
            async move {
                let result = evict_one(
                    session,
                    index,
                    target,
                    options,
                    cancellation,
                    deadline,
                    progress.clone(),
                )
                .await;
                let _ = progress.unbounded_send(Progress::Pod {
                    index: result.0,
                    state: result.1.clone(),
                });
                result
            }
        })
        .buffer_unordered(4)
        .collect()
        .await;
    let mut states = vec![PodState::Pending; plan.pods.len()];
    for (index, state) in results {
        states[index] = state;
    }
    let remaining = pods(session, &plan.node)
        .await?
        .into_iter()
        .filter(|pod| classify(pod).skipped.is_none())
        .map(|pod| ObjectRef::of(&pod))
        .collect();
    Ok(Report { states, remaining })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClusterId, Kind, test_support::Fixture};
    fn kind(group: &str, name: &str, plural: &str) -> Kind {
        Kind {
            resource: ApiResource::from_gvk_with_plural(
                &GroupVersionKind::gvk(group, "v1", name),
                plural,
            ),
            namespaced: !matches!(name, "Node" | "PersistentVolume"),
            verbs: vec!["get".into(), "list".into(), "watch".into(), "patch".into()],
        }
    }
    fn connect(fixture: &Fixture) -> Arc<ClusterSession> {
        Arc::new(ClusterSession::for_testing(
            ClusterId::new("node-maintenance-fixture"),
            fixture.url.clone(),
            vec![
                kind("", "Node", "nodes"),
                kind("", "Pod", "pods"),
                kind("policy", "PodDisruptionBudget", "poddisruptionbudgets"),
            ],
        ))
    }
    fn node() -> Value {
        json!({"apiVersion":"v1","kind":"Node","metadata":{"name":"node-a","uid":"node-uid","resourceVersion":"1"},"spec":{"unschedulable":false},"status":{"capacity":{"cpu":"8"}}})
    }
    fn pod(name: &str) -> Value {
        json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"default","uid":format!("uid-{name}"),"resourceVersion":"1","labels":{"app":"worker"},"ownerReferences":[{"apiVersion":"apps/v1","kind":"ReplicaSet","name":"worker","uid":"rs-uid","controller":true}]},"spec":{"nodeName":"node-a","containers":[{"name":"app","image":"busybox"}]},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}]}})
    }
    fn budget(allowed: i64) -> Value {
        json!({"apiVersion":"policy/v1","kind":"PodDisruptionBudget","metadata":{"name":"worker-budget","namespace":"default","uid":"pdb-uid","resourceVersion":"1","generation":1},"spec":{"minAvailable":1,"selector":{"matchLabels":{"app":"worker"}}},"status":{"disruptionsAllowed":allowed,"currentHealthy":1,"desiredHealthy":1,"observedGeneration":1}})
    }
    fn quick() -> Options {
        Options {
            timeout: Duration::from_secs(2),
            retry_interval: Duration::from_millis(10),
            poll_interval: Duration::from_millis(5),
            ..Default::default()
        }
    }

    #[test]
    fn budget_matching_and_unhealthy_policies_follow_policy_v1() {
        let mut pdb: DynamicObject = serde_json::from_value(budget(0)).unwrap();
        let mut pod: DynamicObject = serde_json::from_value(pod("worker")).unwrap();
        assert!(pdb_matches(&pdb, &pod));
        assert!(pdb_blocks(&pdb, &pod));
        pdb.metadata.namespace = Some("other".into());
        assert!(!pdb_matches(&pdb, &pod));
        pdb.metadata.namespace = Some("default".into());
        pdb.data["spec"]["selector"] = json!({});
        assert!(pdb_matches(&pdb, &pod));
        pdb.data["spec"]["selector"] = Value::Null;
        assert!(!pdb_matches(&pdb, &pod));
        pod.data["status"]["conditions"][0]["status"] = json!("False");
        assert!(
            !pdb_blocks(&pdb, &pod),
            "unready Pod with enough healthy replicas is evictable"
        );
        pdb.data["status"]["currentHealthy"] = json!(0);
        assert!(pdb_blocks(&pdb, &pod));
        pdb.data["spec"]["unhealthyPodEvictionPolicy"] = json!("AlwaysAllow");
        assert!(!pdb_blocks(&pdb, &pod));
        pod.data["status"]["conditions"][0]["status"] = json!("True");
        assert!(pdb_blocks(&pdb, &pod));
        pod.data["status"]["phase"] = json!("Succeeded");
        assert!(!pdb_blocks(&pdb, &pod));
    }

    #[tokio::test]
    async fn scheduling_changes_preserve_pods_and_reject_stale_review() {
        let fixture = Fixture::start(vec![node(), pod("worker")]).await;
        let session = connect(&fixture);
        let plan = prepare(
            session.clone(),
            "node-a".into(),
            Some("node-uid".into()),
            false,
        )
        .await
        .unwrap();
        cordon(session.clone(), &plan, true).await.unwrap();
        assert_eq!(
            fixture.object("Node", "node-a")["spec"]["unschedulable"],
            true
        );
        assert_eq!(fixture.object("Pod", "worker"), pod("worker"));
        assert!(
            cordon(session.clone(), &plan, false)
                .await
                .unwrap_err()
                .user_message()
                .contains("changed after review")
        );
        let fresh = prepare(session.clone(), "node-a".into(), None, false)
            .await
            .unwrap();
        cordon(session, &fresh, false).await.unwrap();
        assert_eq!(
            fixture.object("Node", "node-a")["spec"]["unschedulable"],
            false
        );
        assert_eq!(
            fixture.object("Node", "node-a")["status"]["capacity"]["cpu"],
            "8"
        );
    }

    #[tokio::test]
    async fn preview_exposes_storage_and_skips_daemon_and_mirror_pods() {
        let mut worker = pod("worker");
        worker["spec"]["volumes"] = json!([{"name":"scratch","emptyDir":{}},{"name":"host","hostPath":{"path":"/var/cache"}},{"name":"disk","persistentVolumeClaim":{"claimName":"data"}}]);
        let mut daemon = pod("daemon");
        daemon["metadata"]["ownerReferences"][0]["kind"] = json!("DaemonSet");
        let mut mirror = pod("mirror");
        mirror["metadata"]["annotations"] = json!({"kubernetes.io/config.mirror":"static"});
        let mut other = pod("elsewhere");
        other["spec"]["nodeName"] = json!("node-b");
        let pvc = json!({"apiVersion":"v1","kind":"PersistentVolumeClaim","metadata":{"name":"data","namespace":"default","uid":"claim-uid","resourceVersion":"1"},"spec":{"volumeName":"local-disk"}});
        let pv = json!({"apiVersion":"v1","kind":"PersistentVolume","metadata":{"name":"local-disk","uid":"pv-uid","resourceVersion":"1"},"spec":{"local":{"path":"/data"}}});
        let fixture = Fixture::start(vec![
            node(),
            worker,
            daemon.clone(),
            mirror.clone(),
            other.clone(),
            pvc,
            pv,
            budget(0),
        ])
        .await;
        let session = connect(&fixture);
        let plan = prepare(session.clone(), "node-a".into(), None, true)
            .await
            .unwrap();
        assert_eq!(plan.pods.len(), 3);
        let worker = plan
            .pods
            .iter()
            .find(|pod| pod.target.name == "worker")
            .unwrap();
        assert_eq!(worker.empty_dir, ["scratch"]);
        assert!(
            worker
                .storage
                .iter()
                .any(|value| value.contains("hostPath"))
        );
        assert!(
            worker
                .storage
                .iter()
                .any(|value| value.contains("Local PV"))
        );
        assert!(worker.budgets[0].blocked);
        assert!(
            plan.validate(&quick())
                .unwrap_err()
                .user_message()
                .contains("emptyDir")
        );
        fixture.put(budget(1));
        let (sender, _) = futures::channel::mpsc::unbounded();
        let report = drain(
            session,
            plan,
            Options {
                allow_empty_dir: true,
                ..quick()
            },
            Default::default(),
            sender,
        )
        .await
        .unwrap();
        assert!(report.complete());
        assert_eq!(
            report
                .states
                .iter()
                .filter(|state| matches!(state, PodState::Skipped(_)))
                .count(),
            2
        );
        assert_eq!(fixture.object("Pod", "daemon"), daemon);
        assert_eq!(fixture.object("Pod", "mirror"), mirror);
        assert_eq!(fixture.object("Pod", "elsewhere"), other);
        let requests = fixture.requests.lock().unwrap();
        let eviction = requests
            .iter()
            .find(|(line, _)| line.contains("/pods/worker/eviction"))
            .unwrap();
        assert_eq!(eviction.1["apiVersion"], "policy/v1");
        assert_eq!(
            eviction.1.pointer("/deleteOptions/preconditions/uid"),
            Some(&json!("uid-worker"))
        );
        assert!(eviction.1.get("delete_options").is_none());
        assert!(!requests.iter().any(|(line, _)| line.starts_with("DELETE")));
    }

    #[tokio::test]
    async fn pdb_blocking_retries_eviction_and_cancellation_keeps_node_cordoned() {
        let fixture = Fixture::start(vec![node(), pod("worker"), budget(0)]).await;
        let session = connect(&fixture);
        let plan = prepare(session.clone(), "node-a".into(), None, true)
            .await
            .unwrap();
        let (sender, mut receiver) = futures::channel::mpsc::unbounded();
        let task = tokio::spawn(drain(session, plan, quick(), Default::default(), sender));
        let blocked = tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(event) = receiver.next().await {
                if matches!(
                    event,
                    Progress::Pod {
                        state: PodState::Blocked(_),
                        ..
                    }
                ) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap();
        assert!(blocked, "drain ended before PDB blocking: {:?}", task.await);
        fixture.put(budget(1));
        let report = task.await.unwrap().unwrap();
        assert!(report.complete());
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(line, _)| line.contains("/pods/worker/eviction"))
                .count()
                >= 2
        );
        let other = Fixture::start(vec![node(), pod("worker"), budget(0)]).await;
        let session = connect(&other);
        let plan = prepare(session.clone(), "node-a".into(), None, true)
            .await
            .unwrap();
        let (sender, mut receiver) = futures::channel::mpsc::unbounded();
        let cancellation = Cancellation::default();
        let task = tokio::spawn(drain(session, plan, quick(), cancellation.clone(), sender));
        let blocked = tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(event) = receiver.next().await {
                if matches!(
                    event,
                    Progress::Pod {
                        state: PodState::Blocked(_),
                        ..
                    }
                ) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap();
        assert!(blocked, "drain ended before PDB blocking: {:?}", task.await);
        cancellation.cancel();
        let report = task.await.unwrap().unwrap();
        assert!(!report.complete());
        assert_eq!(report.states, [PodState::Cancelled]);
        assert_eq!(other.object("Pod", "worker"), pod("worker"));
        assert_eq!(
            other.object("Node", "node-a")["spec"]["unschedulable"],
            true
        );
    }

    #[tokio::test]
    async fn partial_refusals_timeout_and_new_pods_cannot_be_mistaken_for_success() {
        let fixture = Fixture::start(vec![node(), pod("good"), pod("fails")]).await;
        fixture.deny("pods/fails/eviction");
        let session = connect(&fixture);
        let plan = prepare(session.clone(), "node-a".into(), None, true)
            .await
            .unwrap();
        let (sender, _) = futures::channel::mpsc::unbounded();
        let report = drain(session, plan, quick(), Default::default(), sender)
            .await
            .unwrap();
        assert!(!report.complete());
        assert_eq!(report.remaining.len(), 1);
        assert_eq!(report.remaining[0].name, "fails");
        assert!(
            report
                .states
                .iter()
                .any(|state| matches!(state, PodState::Evicted))
        );
        assert!(report.states.iter().any(
            |state| matches!(state,PodState::Failed(message) if message.contains("HTTP 403"))
        ));
        let blocked = Fixture::start(vec![node(), pod("worker"), budget(0)]).await;
        let session = connect(&blocked);
        let plan = prepare(session.clone(), "node-a".into(), None, true)
            .await
            .unwrap();
        let (sender, _) = futures::channel::mpsc::unbounded();
        let started = tokio::time::Instant::now();
        let report = drain(
            session,
            plan,
            Options {
                timeout: Duration::from_millis(50),
                ..Default::default()
            },
            Default::default(),
            sender,
        )
        .await
        .unwrap();
        assert!(!report.complete());
        assert!(started.elapsed() < Duration::from_secs(1));
        let changed = Fixture::start(vec![node(), pod("worker")]).await;
        let session = connect(&changed);
        let plan = prepare(session.clone(), "node-a".into(), None, true)
            .await
            .unwrap();
        let mut replacement = pod("worker");
        replacement["metadata"]["uid"] = json!("new-uid");
        changed.put(replacement);
        let (sender, _) = futures::channel::mpsc::unbounded();
        assert!(
            drain(session, plan, quick(), Default::default(), sender)
                .await
                .unwrap_err()
                .user_message()
                .contains("new Pods")
        );
        assert!(
            !changed
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.starts_with("PATCH"))
        );
    }

    #[tokio::test]
    async fn node_and_namespaced_eviction_permissions_are_separate() {
        let denied = Fixture::start(vec![node(), pod("worker")]).await;
        denied.rules(json!([{"verbs":["create"],"apiGroups":[""],"resources":["pods/eviction"]}]));
        assert!(
            prepare(connect(&denied), "node-a".into(), None, true)
                .await
                .err()
                .unwrap()
                .user_message()
                .contains("patch on nodes")
        );
        let fixture = Fixture::start(vec![node(), pod("worker")]).await;
        fixture.rules(json!([{"verbs":["patch"],"apiGroups":[""],"resources":["nodes"],"resourceNames":["node-a"]}]));
        let session = connect(&fixture);
        let plan = prepare(session.clone(), "node-a".into(), None, true)
            .await
            .unwrap();
        assert!(!plan.pods[0].allowed);
        let (sender, _) = futures::channel::mpsc::unbounded();
        assert!(
            drain(session, plan, quick(), Default::default(), sender)
                .await
                .unwrap_err()
                .user_message()
                .contains("create on pods/eviction")
        );
        assert!(
            !fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.starts_with("PATCH"))
        );
    }

    #[tokio::test]
    async fn unavailable_budget_data_is_unknown_and_unmanaged_pods_need_acknowledgement() {
        let mut standalone = pod("standalone");
        standalone["metadata"]
            .as_object_mut()
            .unwrap()
            .remove("ownerReferences");
        let fixture = Fixture::start(vec![node(), standalone, budget(0)]).await;
        fixture.deny("poddisruptionbudgets");
        let plan = prepare(connect(&fixture), "node-a".into(), None, true)
            .await
            .unwrap();
        assert!(!plan.pods[0].budgets_known);
        assert_eq!(plan.warnings.len(), 1);
        assert!(
            plan.validate(&quick())
                .unwrap_err()
                .user_message()
                .contains("without a controller")
        );
        assert!(
            plan.validate(&Options {
                force_unmanaged: true,
                ..quick()
            })
            .is_ok()
        );
    }
}
