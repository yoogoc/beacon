//! The object store and the deltas that feed it.
//!
//! One store holds the objects of a single [`WatchKey`](crate::watch::WatchKey):
//! one kind, one namespace scope, one selector. A watcher owns the
//! authoritative copy; every subscriber builds its own from the same delta
//! stream, so a view can read it synchronously without touching a lock the
//! network side holds.

use std::{collections::HashMap, fmt, sync::Arc};

use kube::api::DynamicObject;

/// Identifies one object within a single resource kind.
///
/// Namespace and name, not UID. A pod deleted and recreated under the same name
/// is the same row to the person reading the table, and it is the identity
/// kubectl prints, so the two views agree about what a row is.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectRef {
    /// `None` for cluster-scoped objects.
    pub namespace: Option<String>,
    pub name: String,
}

impl ObjectRef {
    pub fn new(namespace: Option<String>, name: impl Into<String>) -> Self {
        Self {
            namespace,
            name: name.into(),
        }
    }

    /// The reference to an object as the API server returned it.
    ///
    /// The API server never returns a nameless object; an empty name here means
    /// the object was malformed, and an empty-named row is a visible symptom
    /// rather than a silent drop.
    pub fn of(object: &DynamicObject) -> Self {
        Self {
            namespace: object.metadata.namespace.clone(),
            name: object.metadata.name.clone().unwrap_or_default(),
        }
    }
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.namespace {
            Some(namespace) => write!(f, "{namespace}/{}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

/// One change to a store.
#[derive(Debug, Clone)]
pub enum Delta {
    /// Replace everything. Sent for the initial list, after a watch desync, and
    /// to a subscriber that joins an already-running watch.
    Reset(Vec<Arc<DynamicObject>>),
    Upsert(Arc<DynamicObject>),
    Remove(ObjectRef),
}

/// What crosses the channel: deltas coalesced over one frame.
///
/// Never a single delta. The first list of a large namespace is thousands of
/// events, and a channel message per event would put a render between each
/// pair of them.
pub type DeltaBatch = Vec<Delta>;

/// Everything currently known about one watched kind.
#[derive(Debug, Default)]
pub struct ResourceStore {
    objects: HashMap<ObjectRef, Arc<DynamicObject>>,
    revision: u64,
}

impl ResourceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one delta. Returns whether anything actually changed.
    pub fn apply(&mut self, delta: Delta) -> bool {
        let changed = match delta {
            Delta::Reset(objects) => {
                self.objects = objects
                    .into_iter()
                    .map(|object| (ObjectRef::of(&object), object))
                    .collect();
                true
            }
            Delta::Upsert(object) => {
                let key = ObjectRef::of(&object);
                // An unchanged object still arrives on every relist. Comparing
                // resourceVersion is cheaper than the re-sort and re-render a
                // spurious change would trigger upstream.
                match self.objects.get(&key) {
                    Some(existing) if same_version(existing, &object) => false,
                    _ => {
                        self.objects.insert(key, object);
                        true
                    }
                }
            }
            Delta::Remove(key) => self.objects.remove(&key).is_some(),
        };

        if changed {
            self.revision += 1;
        }
        changed
    }

    /// Applies a whole batch. Returns whether anything changed, so a caller can
    /// skip a render for a batch that turned out to be all no-ops.
    pub fn apply_batch(&mut self, batch: impl IntoIterator<Item = Delta>) -> bool {
        // Written out rather than folded or `any`-ed: every delta has to be
        // applied, so nothing here may short-circuit.
        let mut changed = false;
        for delta in batch {
            changed |= self.apply(delta);
        }
        changed
    }

    /// Everything in the store, for handing to a subscriber as a [`Delta::Reset`].
    ///
    /// Unordered -- the caller sorts, because the order a table wants depends on
    /// the column it is sorted by.
    pub fn snapshot(&self) -> Vec<Arc<DynamicObject>> {
        self.objects.values().cloned().collect()
    }

    pub fn get(&self, key: &ObjectRef) -> Option<&Arc<DynamicObject>> {
        self.objects.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ObjectRef, &Arc<DynamicObject>)> {
        self.objects.iter()
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Bumped on every effective change. A view that caches a derived index
    /// (a filtered, sorted `Vec<ObjectRef>`) rebuilds it when this moves.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

fn same_version(a: &DynamicObject, b: &DynamicObject) -> bool {
    match (&a.metadata.resource_version, &b.metadata.resource_version) {
        (Some(a), Some(b)) => a == b,
        // Without a resourceVersion there is nothing to compare, so treat it as
        // changed rather than risk holding a stale object.
        _ => false,
    }
}

/// Drops the parts of an object that cost memory and that nothing in a list
/// view reads.
///
/// `managedFields` alone is routinely larger than the rest of a Pod, and
/// `last-applied-configuration` is a second full copy of the manifest. Together
/// they are most of the memory a large cluster costs us. The detail pane fetches
/// the object again when it needs the real YAML.
pub fn slim(mut object: DynamicObject) -> DynamicObject {
    object.metadata.managed_fields = None;

    if let Some(annotations) = object.metadata.annotations.as_mut() {
        annotations.remove("kubectl.kubernetes.io/last-applied-configuration");
        if annotations.is_empty() {
            object.metadata.annotations = None;
        }
    }

    object
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ManagedFieldsEntry;
    use std::collections::BTreeMap;

    fn object(namespace: &str, name: &str, version: &str) -> Arc<DynamicObject> {
        let mut object = DynamicObject::new(name, &crate::resources::pod()).within(namespace);
        object.metadata.resource_version = Some(version.to_string());
        Arc::new(object)
    }

    fn names(store: &ResourceStore) -> Vec<String> {
        let mut names: Vec<_> = store.iter().map(|(key, _)| key.to_string()).collect();
        names.sort();
        names
    }

    #[test]
    fn upsert_then_remove() {
        let mut store = ResourceStore::new();
        assert!(store.apply(Delta::Upsert(object("default", "api", "1"))));
        assert!(store.apply(Delta::Upsert(object("default", "web", "1"))));
        assert_eq!(names(&store), ["default/api", "default/web"]);

        assert!(store.apply(Delta::Remove(ObjectRef::new(Some("default".into()), "api"))));
        assert_eq!(names(&store), ["default/web"]);
    }

    #[test]
    fn reset_replaces_everything() {
        let mut store = ResourceStore::new();
        store.apply(Delta::Upsert(object("default", "gone", "1")));
        store.apply(Delta::Reset(vec![object("kube-system", "coredns", "1")]));
        assert_eq!(names(&store), ["kube-system/coredns"]);
    }

    /// A relist re-sends every object unchanged. Reporting those as changes
    /// would make the table re-sort and re-render for nothing.
    #[test]
    fn an_unchanged_object_is_not_a_change() {
        let mut store = ResourceStore::new();
        assert!(store.apply(Delta::Upsert(object("default", "api", "7"))));
        let revision = store.revision();

        assert!(!store.apply(Delta::Upsert(object("default", "api", "7"))));
        assert_eq!(store.revision(), revision);

        assert!(store.apply(Delta::Upsert(object("default", "api", "8"))));
        assert_eq!(store.revision(), revision + 1);
    }

    #[test]
    fn removing_an_absent_object_is_not_a_change() {
        let mut store = ResourceStore::new();
        assert!(!store.apply(Delta::Remove(ObjectRef::new(None, "nothing"))));
        assert_eq!(store.revision(), 0);
    }

    /// One no-op inside a batch must not hide the changes around it, and every
    /// delta in the batch still has to land.
    #[test]
    fn a_batch_reports_change_if_any_delta_changed() {
        let mut store = ResourceStore::new();
        store.apply(Delta::Upsert(object("default", "api", "1")));

        let changed = store.apply_batch([
            Delta::Upsert(object("default", "api", "1")),
            Delta::Upsert(object("default", "web", "1")),
        ]);

        assert!(changed);
        assert_eq!(names(&store), ["default/api", "default/web"]);
        assert!(!store.apply_batch([Delta::Upsert(object("default", "api", "1"))]));
    }

    #[test]
    fn slim_drops_managed_fields_and_last_applied() {
        let mut object = DynamicObject::new("api", &crate::resources::pod());
        object.metadata.managed_fields = Some(vec![ManagedFieldsEntry::default()]);
        object.metadata.annotations = Some(BTreeMap::from([
            (
                "kubectl.kubernetes.io/last-applied-configuration".to_string(),
                "{}".to_string(),
            ),
            ("team".to_string(), "payments".to_string()),
        ]));

        let slimmed = slim(object);
        assert!(slimmed.metadata.managed_fields.is_none());
        let annotations = slimmed.metadata.annotations.expect("team survives");
        assert_eq!(annotations.len(), 1);
        assert!(annotations.contains_key("team"));
    }

    /// An object whose only annotation was the one we strip should not be left
    /// holding an empty map.
    #[test]
    fn slim_drops_an_emptied_annotation_map() {
        let mut object = DynamicObject::new("api", &crate::resources::pod());
        object.metadata.annotations = Some(BTreeMap::from([(
            "kubectl.kubernetes.io/last-applied-configuration".to_string(),
            "{}".to_string(),
        )]));

        assert!(slim(object).metadata.annotations.is_none());
    }
}
