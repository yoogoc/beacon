//! On-demand watches: what a view asks for, and how the events reach it.
//!
//! Beacon watches only what someone is looking at. A view subscribes to a
//! [`WatchKey`]; the first subscriber starts a watcher task, later ones attach
//! to the running one and are handed its current contents immediately. When the
//! last subscriber goes away the watch lingers briefly before shutting down,
//! because clicking from Pods to Deployments and back is a normal thing to do
//! and re-listing a large namespace for it is not.
//!
//! Everything a subscriber receives is a [`DeltaBatch`] coalesced over one
//! frame. That coalescing happens here, on the network side, and it is why a
//! 5,000-pod initial list does not stall the frame loop.

use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use futures::{Stream, StreamExt as _, channel::mpsc};
use kube::{
    Api,
    api::{ApiResource, DynamicObject},
    runtime::watcher::{self, Event},
};

use crate::{
    session::HealthState,
    store::{Delta, DeltaBatch, ObjectRef, ResourceStore, slim},
};

/// How long deltas accumulate before being sent. One frame at 60Hz: long enough
/// to collapse a burst, short enough that nobody perceives the delay.
const BATCH_WINDOW: Duration = Duration::from_millis(16);

/// Flush early once a batch reaches this size, so a sustained flood does not
/// grow an unbounded buffer between ticks.
const BATCH_LIMIT: usize = 512;

/// How long a watch with no subscribers stays alive.
///
/// Long enough to cover a person clicking through a few resource kinds and
/// back; short enough that a cluster left open does not keep watching what
/// nobody is reading.
const LINGER: Duration = Duration::from_secs(30);

/// Objects fetched per page during the initial list. Bounds peak memory on a
/// cluster where one kind has tens of thousands of objects.
const PAGE_SIZE: u32 = 500;

/// What to watch: one kind, one scope, one filter.
///
/// This is the registry key, so two views asking for the same thing share a
/// single watch and a single copy of the objects.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WatchKey {
    pub resource: ApiResource,
    /// `None` means every namespace, and is the only option for a
    /// cluster-scoped kind.
    pub namespace: Option<String>,
    /// A label selector, as `kubectl -l` spells it.
    pub labels: Option<String>,
    /// A field selector, e.g. `involvedObject.uid=...` for one object's events.
    pub fields: Option<String>,
}

impl WatchKey {
    /// Every object of a kind, across all namespaces.
    pub fn all(resource: ApiResource) -> Self {
        Self {
            resource,
            namespace: None,
            labels: None,
            fields: None,
        }
    }

    /// Every object of a kind within one namespace.
    pub fn namespaced(resource: ApiResource, namespace: impl Into<String>) -> Self {
        Self {
            namespace: Some(namespace.into()),
            ..Self::all(resource)
        }
    }

    /// Scopes a key to a namespace, or to the whole cluster with `None` -- the
    /// shape the namespace picker produces.
    pub fn in_namespace(mut self, namespace: Option<String>) -> Self {
        self.namespace = namespace;
        self
    }

    pub fn with_labels(mut self, selector: impl Into<String>) -> Self {
        self.labels = Some(selector.into());
        self
    }

    pub fn with_fields(mut self, selector: impl Into<String>) -> Self {
        self.fields = Some(selector.into());
        self
    }

    /// What a log line says about this watch: `Pod`, or `Pod in kube-system`.
    pub fn describe(&self) -> String {
        match &self.namespace {
            Some(namespace) => format!("{} in {namespace}", self.resource.kind),
            None => self.resource.kind.clone(),
        }
    }

    fn api(&self, client: kube::Client) -> Api<DynamicObject> {
        match &self.namespace {
            Some(namespace) => Api::namespaced_with(client, namespace, &self.resource),
            None => Api::all_with(client, &self.resource),
        }
    }

    fn watcher_config(&self) -> watcher::Config {
        let mut config = watcher::Config::default().page_size(PAGE_SIZE);
        if let Some(labels) = &self.labels {
            config = config.labels(labels);
        }
        if let Some(fields) = &self.fields {
            config = config.fields(fields);
        }
        // Deliberately not `streaming_lists()`. It needs the WatchList feature
        // gate on the API server, and a cluster without it fails the list
        // outright rather than degrading, so turning it on wants a capability
        // probe first. See docs/DESIGN.md §4.3.
        config
    }
}

/// A view's end of a watch.
///
/// Yields [`DeltaBatch`]es until the watch stops. A subscriber that joins a
/// watch which has already listed is handed its contents immediately, as a
/// [`Delta::Reset`]; one that joins a watch still listing waits for the real
/// list. Either way the first `Reset` to arrive means "this is everything",
/// which is what lets a view tell an empty namespace from one it has not
/// heard about yet.
///
/// Dropping it releases the subscription; when it was the last one the watch
/// lingers for [`LINGER`] and then shuts down.
pub struct Subscription {
    key: WatchKey,
    id: u64,
    receiver: mpsc::UnboundedReceiver<DeltaBatch>,
    registry: Arc<Registry>,
}

impl Stream for Subscription {
    type Item = DeltaBatch;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().receiver).poll_next(cx)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        Registry::release(&self.registry, &self.key, self.id);
    }
}

/// One running watch and everyone listening to it.
struct WatchHandle {
    /// The authoritative contents, so a late subscriber is caught up without
    /// waiting for the next event.
    store: Arc<Mutex<ResourceStore>>,
    /// Whether the initial list has completed. Until it has, an empty store
    /// means "not yet", not "nothing there".
    listed: Arc<AtomicBool>,
    subscribers: Subscribers,
    /// Bumped on every new subscriber, so a pending shutdown can tell that
    /// somebody re-subscribed while it was sleeping.
    generation: u64,
    task: tokio::task::AbortHandle,
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

type Subscribers = Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<DeltaBatch>>>>;

/// Every watch running against one cluster.
///
/// Held inside the cluster's session; dropping the session drops this, which
/// aborts every watch task it started.
pub(crate) struct Registry {
    client: kube::Client,
    runtime: tokio::runtime::Handle,
    health: Arc<HealthState>,
    watches: Mutex<HashMap<WatchKey, WatchHandle>>,
    next_subscriber_id: AtomicU64,
}

impl Registry {
    pub(crate) fn new(
        client: kube::Client,
        runtime: tokio::runtime::Handle,
        health: Arc<HealthState>,
    ) -> Self {
        Self {
            client,
            runtime,
            health,
            watches: Mutex::new(HashMap::new()),
            next_subscriber_id: AtomicU64::new(0),
        }
    }

    /// Attaches to a watch, starting it if nobody is watching yet.
    ///
    /// Never blocks and never awaits: it is called from the UI thread, and the
    /// caller gets a stream already primed with whatever the watch knows now.
    pub(crate) fn subscribe(self: &Arc<Self>, key: WatchKey) -> Subscription {
        let id = self.next_subscriber_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::unbounded();

        {
            let mut watches = self.lock_watches();
            let handle = match watches.get_mut(&key) {
                Some(handle) => {
                    handle.generation += 1;
                    handle
                }
                None => {
                    tracing::debug!(watch = %key.describe(), "starting watch");
                    let handle = self.start(&key);
                    watches.entry(key.clone()).or_insert(handle)
                }
            };

            // Catch the new subscriber up before it sees a single event, so it
            // never renders an empty table over a populated watch. A watch that
            // has not listed yet has nothing to catch up with, and sending an
            // empty Reset would tell the subscriber the opposite.
            //
            // The subscriber list is locked across the snapshot and the insert,
            // and `flush` holds the same lock across its send and its write to
            // the store. Without that, a flush landing in between would be
            // applied to the store after the snapshot was taken and sent to a
            // list this subscriber was not in yet -- lost, in both directions
            // at once. The lock order is subscribers, then store, in both.
            let mut subscribers = lock(&handle.subscribers);
            if handle.listed.load(Ordering::Acquire) {
                let snapshot = lock(&handle.store).snapshot();
                let _ = sender.unbounded_send(vec![Delta::Reset(snapshot)]);
            }
            subscribers.insert(id, sender);
        }

        Subscription {
            key,
            id,
            receiver,
            registry: self.clone(),
        }
    }

    /// How many watches are running. The status bar shows it; the tests assert
    /// on it.
    pub(crate) fn active(&self) -> usize {
        self.lock_watches().len()
    }

    fn start(&self, key: &WatchKey) -> WatchHandle {
        let store = Arc::new(Mutex::new(ResourceStore::new()));
        let subscribers: Subscribers = Arc::new(Mutex::new(HashMap::new()));
        let listed = Arc::new(AtomicBool::new(false));

        let task = self.runtime.spawn(run(
            key.clone(),
            key.api(self.client.clone()),
            store.clone(),
            subscribers.clone(),
            listed.clone(),
            self.health.clone(),
        ));

        WatchHandle {
            store,
            listed,
            subscribers,
            generation: 0,
            task: task.abort_handle(),
        }
    }

    /// Drops one subscriber, and schedules the watch's shutdown if it was the
    /// last one.
    fn release(self: &Arc<Self>, key: &WatchKey, id: u64) {
        let generation = {
            let mut watches = self.lock_watches();
            let Some(handle) = watches.get_mut(key) else {
                return;
            };

            let mut subscribers = lock(&handle.subscribers);
            subscribers.remove(&id);
            if !subscribers.is_empty() {
                return;
            }
            handle.generation
        };

        tracing::debug!(watch = %key.describe(), linger = ?LINGER, "watch is idle");

        let key = key.clone();
        let registry: Weak<Self> = Arc::downgrade(self);
        self.runtime.spawn(async move {
            tokio::time::sleep(LINGER).await;

            let Some(registry) = registry.upgrade() else {
                return;
            };
            let mut watches = registry.lock_watches();

            let still_idle = watches.get(&key).is_some_and(|handle| {
                handle.generation == generation && lock(&handle.subscribers).is_empty()
            });

            if still_idle {
                tracing::debug!(watch = %key.describe(), "stopping idle watch");
                // `WatchHandle`'s Drop aborts the task.
                watches.remove(&key);
            }
        });
    }

    fn lock_watches(&self) -> MutexGuard<'_, HashMap<WatchKey, WatchHandle>> {
        self.watches.lock().expect("watch registry lock")
    }
}

fn lock<T>(value: &Arc<Mutex<T>>) -> MutexGuard<'_, T> {
    value.lock().expect("watch lock")
}

/// Turns a watcher's event stream into coalesced batches.
///
/// Kept separate from the task loop because this is where the subtle part
/// lives: an `Init`/`InitApply`/`InitDone` sequence has to become a single
/// [`Delta::Reset`], not a stream of upserts, or the table shows objects that
/// the relist already removed.
#[derive(Default)]
struct Coalescer {
    pending: DeltaBatch,
    /// `Some` while a relist is in progress.
    relisting: Option<Vec<Arc<DynamicObject>>>,
}

impl Coalescer {
    /// Takes one event. Returns whether the batch should be flushed right away
    /// rather than at the next tick.
    fn ingest(&mut self, event: Event<DynamicObject>) -> bool {
        match event {
            Event::Init => {
                // Anything buffered describes the state this relist is about to
                // replace wholesale.
                self.pending.clear();
                self.relisting = Some(Vec::new());
                false
            }
            Event::InitApply(object) => {
                self.relisting
                    .get_or_insert_with(Vec::new)
                    .push(Arc::new(slim(object)));
                false
            }
            Event::InitDone => {
                let objects = self.relisting.take().unwrap_or_default();
                self.pending.push(Delta::Reset(objects));
                // The first screen should not wait for a tick.
                true
            }
            Event::Apply(object) => {
                self.pending.push(Delta::Upsert(Arc::new(slim(object))));
                self.pending.len() >= BATCH_LIMIT
            }
            Event::Delete(object) => {
                self.pending.push(Delta::Remove(ObjectRef::of(&object)));
                self.pending.len() >= BATCH_LIMIT
            }
        }
    }

    fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    fn take(&mut self) -> DeltaBatch {
        std::mem::take(&mut self.pending)
    }
}

/// The watcher task: one per [`WatchKey`], for as long as somebody wants it.
async fn run(
    key: WatchKey,
    api: Api<DynamicObject>,
    store: Arc<Mutex<ResourceStore>>,
    subscribers: Subscribers,
    listed: Arc<AtomicBool>,
    health: Arc<HealthState>,
) {
    let mut reporter = HealthState::reporter(&health, key.describe());
    let stream = watcher::watcher(api, key.watcher_config());
    futures::pin_mut!(stream);

    let mut coalescer = Coalescer::default();
    let mut ticker = tokio::time::interval(BATCH_WINDOW);
    // The default behaviour is to catch up on missed ticks in a burst, which
    // would flush an empty batch several times over after any stall.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let flush_now = tokio::select! {
            event = stream.next() => match event {
                Some(Ok(event)) => {
                    reporter.healthy();
                    coalescer.ingest(event)
                }
                Some(Err(error)) => {
                    // `watcher` restarts itself on the next poll; this is a
                    // report, not a reason to stop.
                    reporter.failed(&error);
                    false
                }
                None => break,
            },
            _ = ticker.tick(), if !coalescer.is_empty() => true,
        };

        if flush_now {
            let batch = coalescer.take();
            // Ordering::Release pairs with the Acquire in `subscribe`: a
            // subscriber that sees this flag must also see the store it
            // describes.
            if batch.iter().any(|delta| matches!(delta, Delta::Reset(_))) {
                listed.store(true, Ordering::Release);
            }
            flush(&store, &subscribers, batch);
        }
    }

    tracing::debug!(watch = %key.describe(), "watch stream ended");
}

fn flush(store: &Arc<Mutex<ResourceStore>>, subscribers: &Subscribers, batch: DeltaBatch) {
    if batch.is_empty() {
        return;
    }

    // Held across both: see `Registry::subscribe`. A subscriber whose receiver
    // is gone is dropped here; the subscriber list is what actually ends the
    // watch, so this is also how an abandoned one stops counting.
    let mut subscribers = lock(subscribers);
    subscribers.retain(|_, sender| sender.unbounded_send(batch.clone()).is_ok());
    // Sent before applying, so the batch is moved into the store rather than
    // cloned one more time.
    lock(store).apply_batch(batch);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources;

    fn pod(name: &str) -> DynamicObject {
        DynamicObject::new(name, &resources::pod()).within("default")
    }

    fn names(batch: &DeltaBatch) -> Vec<String> {
        batch
            .iter()
            .map(|delta| match delta {
                Delta::Reset(objects) => format!(
                    "reset[{}]",
                    objects
                        .iter()
                        .filter_map(|o| o.metadata.name.clone())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
                Delta::Upsert(object) => {
                    format!(
                        "upsert:{}",
                        object.metadata.name.clone().unwrap_or_default()
                    )
                }
                Delta::Remove(key) => format!("remove:{}", key.name),
            })
            .collect()
    }

    #[test]
    fn a_relist_becomes_one_reset() {
        let mut coalescer = Coalescer::default();

        assert!(!coalescer.ingest(Event::Init));
        assert!(!coalescer.ingest(Event::InitApply(pod("api"))));
        assert!(!coalescer.ingest(Event::InitApply(pod("web"))));
        assert!(coalescer.is_empty(), "nothing is emitted mid-relist");

        assert!(coalescer.ingest(Event::InitDone), "flushes immediately");
        assert_eq!(names(&coalescer.take()), ["reset[api,web]"]);
    }

    /// A watch that desyncs relists from scratch. Deltas buffered against the
    /// old contents would be applied on top of the fresh list and resurrect
    /// objects that are gone.
    #[test]
    fn a_relist_discards_what_was_buffered_before_it() {
        let mut coalescer = Coalescer::default();
        coalescer.ingest(Event::Apply(pod("stale")));

        coalescer.ingest(Event::Init);
        coalescer.ingest(Event::InitApply(pod("api")));
        coalescer.ingest(Event::InitDone);

        assert_eq!(names(&coalescer.take()), ["reset[api]"]);
    }

    #[test]
    fn steady_state_events_accumulate_until_a_tick() {
        let mut coalescer = Coalescer::default();

        assert!(!coalescer.ingest(Event::Apply(pod("api"))));
        assert!(!coalescer.ingest(Event::Delete(pod("web"))));

        assert_eq!(names(&coalescer.take()), ["upsert:api", "remove:web"]);
        assert!(coalescer.is_empty());
    }

    /// A flood must not grow the buffer without bound between ticks.
    #[test]
    fn a_full_batch_asks_to_be_flushed() {
        let mut coalescer = Coalescer::default();

        for index in 0..BATCH_LIMIT - 1 {
            assert!(!coalescer.ingest(Event::Apply(pod(&format!("pod-{index}")))));
        }
        assert!(coalescer.ingest(Event::Apply(pod("last"))));
    }

    /// An empty relist is how "the namespace has nothing in it" arrives, and it
    /// has to clear the table rather than leave it as it was.
    #[test]
    fn an_empty_relist_still_resets() {
        let mut coalescer = Coalescer::default();
        coalescer.ingest(Event::Apply(pod("stale")));

        coalescer.ingest(Event::Init);
        assert!(coalescer.ingest(Event::InitDone));

        assert_eq!(names(&coalescer.take()), ["reset[]"]);
    }

    #[test]
    fn watch_keys_describe_their_scope() {
        assert_eq!(WatchKey::all(resources::pod()).describe(), "Pod");
        assert_eq!(
            WatchKey::namespaced(resources::pod(), "kube-system").describe(),
            "Pod in kube-system"
        );
    }

    /// The registry keys on the whole tuple: the same kind in two namespaces is
    /// two watches, and the same kind with two selectors is two watches.
    #[test]
    fn watch_keys_distinguish_scope_and_selector() {
        let all = WatchKey::all(resources::pod());
        assert_ne!(all, WatchKey::namespaced(resources::pod(), "default"));
        assert_ne!(all, all.clone().with_labels("app=api"));
        assert_ne!(all, all.clone().with_fields("spec.nodeName=node-1"));
        assert_eq!(all, WatchKey::all(resources::pod()));
    }
}
