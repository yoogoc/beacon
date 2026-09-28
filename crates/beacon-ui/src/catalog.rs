//! The resource kinds a cluster serves, arranged for a person to read.
//!
//! Discovery returns a flat list -- ninety-eight kinds on a small development
//! cluster, several hundred on a real one -- in no order anybody would choose.
//! This groups them the way people already think about Kubernetes, and sorts
//! each group so the kinds that are looked at daily come before the ones that
//! exist for controllers.
//!
//! The grouping is a table rather than a rule, because there isn't a rule: a
//! `Lease` is a coordination primitive, an `Endpoint` is networking, and
//! nothing in the API says so. Everything the table does not name is a custom
//! resource, which is the honest default -- on most clusters that is the
//! majority, and it is where the interesting things live.
//!
//! Custom resources are then split again, by API group. One "Custom
//! Resources" heading over two hundred kinds is a list, not a grouping; the
//! group is the one piece of structure the cluster actually gives us, and it
//! is the one people already use -- everything under `argoproj.io` arrived
//! together, is documented together and is reasoned about together.

use std::sync::Arc;

use beacon_kube::Kind;
use gpui_kit::SharedString;
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

/// The sections of the sidebar, in the order they appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Workloads,
    Config,
    Network,
    Storage,
    AccessControl,
    Cluster,
    /// Everything the table below does not name *and* that has no API group
    /// to be filed under -- which leaves the few core kinds nobody thought to
    /// curate. A custom resource never lands here; it has a group.
    Other,
}

impl Category {
    pub const ALL: [Self; 7] = [
        Self::Workloads,
        Self::Config,
        Self::Network,
        Self::Storage,
        Self::AccessControl,
        Self::Cluster,
        Self::Other,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::Workloads => "Workloads",
            Self::Config => "Config",
            Self::Network => "Network",
            Self::Storage => "Storage",
            Self::AccessControl => "Access Control",
            Self::Cluster => "Cluster",
            Self::Other => "Other",
        }
    }

    /// Whether the section starts expanded. Only the one people open first.
    pub fn starts_open(&self) -> bool {
        matches!(self, Self::Workloads)
    }
}

/// Where a kind belongs, and how high up its section it sits.
///
/// The rank is what stops `Pod` appearing below `ReplicationController`
/// alphabetically. Unranked kinds sort by name after the ranked ones.
fn placement(kind: &Kind) -> (Category, u8) {
    use Category::*;

    let group = kind.resource.group.as_str();
    let name = kind.resource.kind.as_str();

    match (group, name) {
        ("", "Pod") => (Workloads, 0),
        ("apps", "Deployment") => (Workloads, 1),
        ("apps", "StatefulSet") => (Workloads, 2),
        ("apps", "DaemonSet") => (Workloads, 3),
        ("apps", "ReplicaSet") => (Workloads, 4),
        ("apps", "ControllerRevision") => (Workloads, 6),
        ("batch", "Job") => (Workloads, 5),
        ("batch", "CronJob") => (Workloads, 5),
        ("", "ReplicationController") => (Workloads, 6),

        ("", "ConfigMap") => (Config, 0),
        ("", "Secret") => (Config, 1),
        ("", "ResourceQuota" | "LimitRange") => (Config, 2),
        ("autoscaling", "HorizontalPodAutoscaler") => (Config, 2),
        ("policy", "PodDisruptionBudget") => (Config, 2),
        ("scheduling.k8s.io", "PriorityClass") => (Config, 3),
        ("node.k8s.io", "RuntimeClass") => (Config, 3),
        ("coordination.k8s.io", "Lease") => (Config, 4),
        (
            "admissionregistration.k8s.io",
            "MutatingWebhookConfiguration" | "ValidatingWebhookConfiguration",
        ) => (Config, 4),

        ("", "Service") => (Network, 0),
        ("networking.k8s.io", "Ingress") => (Network, 1),
        ("networking.k8s.io", "NetworkPolicy") => (Network, 2),
        ("networking.k8s.io", "IngressClass") => (Network, 3),
        ("", "Endpoints") => (Network, 4),
        ("discovery.k8s.io", "EndpointSlice") => (Network, 4),

        ("", "PersistentVolumeClaim") => (Storage, 0),
        ("", "PersistentVolume") => (Storage, 1),
        ("storage.k8s.io", "StorageClass") => (Storage, 2),
        ("storage.k8s.io", _) => (Storage, 3),

        ("", "ServiceAccount") => (AccessControl, 0),
        ("rbac.authorization.k8s.io", "Role") => (AccessControl, 1),
        ("rbac.authorization.k8s.io", "RoleBinding") => (AccessControl, 2),
        ("rbac.authorization.k8s.io", "ClusterRole") => (AccessControl, 3),
        ("rbac.authorization.k8s.io", "ClusterRoleBinding") => (AccessControl, 4),
        ("certificates.k8s.io", _) => (AccessControl, 5),

        ("", "Node") => (Cluster, 0),
        ("", "Namespace") => (Cluster, 1),
        ("", "Event") => (Cluster, 2),
        ("events.k8s.io", "Event") => (Cluster, 3),
        ("apiextensions.k8s.io", "CustomResourceDefinition") => (Cluster, 4),
        ("apiregistration.k8s.io", "APIService") => (Cluster, 5),
        ("", "ComponentStatus") => (Cluster, 6),

        // Uncommon, but core, and not worth a line of its own above.
        ("", "PodTemplate") => (Workloads, 7),

        _ => (Other, 0),
    }
}

/// A heading in the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Section {
    /// One of the curated groupings above.
    Builtin(Category),
    /// One API group's custom resources, headed by the group itself.
    Group(SharedString),
}

impl Section {
    pub fn label(&self) -> SharedString {
        match self {
            Self::Builtin(category) => SharedString::from(category.label()),
            Self::Group(group) => group.clone(),
        }
    }

    /// Whether the section starts expanded. Only the one people open first --
    /// and never a custom group, of which there can be dozens.
    pub fn starts_open(&self) -> bool {
        matches!(self, Self::Builtin(category) if category.starts_open())
    }

    /// Built-ins in the order of [`Category::ALL`], then groups by name.
    fn order(&self) -> (u8, usize, SharedString) {
        match self {
            Self::Builtin(category) => (
                0,
                Category::ALL
                    .iter()
                    .position(|candidate| candidate == category)
                    .unwrap_or(usize::MAX),
                SharedString::default(),
            ),
            Self::Group(group) => (1, 0, group.clone()),
        }
    }
}

/// One entry in the sidebar.
#[derive(Clone)]
pub struct Entry {
    pub kind: Arc<Kind>,
    /// What the sidebar shows. Under a group heading that is the bare kind:
    /// the heading has already said `argoproj.io`, and saying it again on
    /// every row is what made the old flat list unreadable.
    pub label: SharedString,
    /// The kind with its group, always. This is what search matches on and
    /// what search shows, because a flat list of results has no heading to
    /// lean on -- and two CRDs sharing a name is common.
    pub qualified: SharedString,
    rank: u8,
}

/// The discovered kinds, grouped and ordered.
#[derive(Default)]
pub struct Catalog {
    sections: Vec<(Section, Vec<Entry>)>,
}

impl Catalog {
    pub fn new(kinds: &[Kind]) -> Self {
        let mut sections: Vec<(Section, Vec<Entry>)> = Vec::new();

        for kind in kinds {
            let (category, rank) = placement(kind);
            let qualified = SharedString::from(kind.display_name());

            // Anything the table does not name is filed under its own API
            // group, and drops the group from its label because the heading
            // now carries it. That catches the CRDs, which is the point, and
            // also the built-in groups nobody curated -- `flowcontrol` and
            // friends -- which is a better home than a bucket labelled
            // "other". Only a core-group kind, which has no group to be filed
            // under, falls through to that bucket.
            let (section, label) = match (category, kind.resource.group.as_str()) {
                (Category::Other, "") => (Section::Builtin(Category::Other), qualified.clone()),
                (Category::Other, group) => (
                    Section::Group(SharedString::from(group.to_string())),
                    SharedString::from(kind.resource.kind.clone()),
                ),
                (category, _) => (Section::Builtin(category), qualified.clone()),
            };

            let entry = Entry {
                label,
                qualified,
                kind: Arc::new(kind.clone()),
                rank,
            };

            match sections
                .iter_mut()
                .find(|(candidate, _)| *candidate == section)
            {
                Some((_, entries)) => entries.push(entry),
                None => sections.push((section, vec![entry])),
            }
        }

        for (_, entries) in &mut sections {
            entries
                .sort_by(|left, right| (left.rank, &left.label).cmp(&(right.rank, &right.label)));
        }
        sections.sort_by_key(|(section, _)| section.order());

        Self { sections }
    }

    pub fn sections(&self) -> &[(Section, Vec<Entry>)] {
        &self.sections
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sections.iter().map(|(_, entries)| entries.len()).sum()
    }

    /// The first kind to show when a cluster connects.
    ///
    /// Pods, unless this cluster somehow does not serve them.
    pub fn default_kind(&self) -> Option<Arc<Kind>> {
        self.sections
            .iter()
            .flat_map(|(_, entries)| entries)
            .find(|entry| entry.kind.resource.kind == "Pod" && entry.kind.resource.group.is_empty())
            .or_else(|| self.sections.first()?.1.first())
            .map(|entry| entry.kind.clone())
    }

    /// The entries matching a query, best first.
    ///
    /// Fuzzy rather than substring: `dep` should find Deployment, and `rolebind`
    /// should find RoleBinding without the exact casing.
    pub fn search(&self, query: &str, matcher: &mut Matcher) -> Vec<Entry> {
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut buffer = Vec::new();

        let mut scored: Vec<(u32, &Entry)> = self
            .sections
            .iter()
            .flat_map(|(_, entries)| entries)
            .filter_map(|entry| {
                // Matched on the qualified name so that typing a group finds
                // everything in it, which is the other half of grouping them.
                let haystack = Utf32Str::new(&entry.qualified, &mut buffer);
                Some((pattern.score(haystack, matcher)?, entry))
            })
            .collect();

        // Score descending, then by name, so that equal scores do not reorder
        // themselves between keystrokes.
        scored.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.qualified.cmp(&right.qualified))
        });

        scored
            .into_iter()
            .map(|(_, entry)| Entry {
                // A flat list has no heading above it, so results carry the
                // group even when the sidebar entry does not.
                label: entry.qualified.clone(),
                ..entry.clone()
            })
            .collect()
    }
}

/// A matcher configured the way Beacon searches: one per view, reused across
/// keystrokes because it owns reusable scratch buffers.
pub fn matcher() -> Matcher {
    Matcher::new(Config::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::{Catalog, Category, Section, matcher, placement};
    use beacon_kube::{ApiResource, GroupVersionKind, Kind};
    use gpui_kit::SharedString;

    fn kind(group: &str, name: &str) -> Kind {
        Kind {
            resource: ApiResource::from_gvk_with_plural(
                &GroupVersionKind::gvk(group, "v1", name),
                &format!("{}s", name.to_lowercase()),
            ),
            namespaced: true,
            verbs: vec!["list".into(), "watch".into()],
        }
    }

    fn catalog() -> Catalog {
        Catalog::new(&[
            kind("apps", "Deployment"),
            kind("", "Pod"),
            kind("", "ConfigMap"),
            kind("argoproj.io", "Application"),
            kind("", "Node"),
            kind("rbac.authorization.k8s.io", "Role"),
            kind("k3s.cattle.io", "Addon"),
        ])
    }

    fn section(catalog: &Catalog, category: Category) -> Vec<String> {
        entries(catalog, &Section::Builtin(category))
    }

    fn group(catalog: &Catalog, name: &str) -> Vec<String> {
        entries(
            catalog,
            &Section::Group(SharedString::from(name.to_string())),
        )
    }

    fn entries(catalog: &Catalog, section: &Section) -> Vec<String> {
        catalog
            .sections()
            .iter()
            .find(|(candidate, _)| candidate == section)
            .map(|(_, entries)| entries.iter().map(|e| e.label.to_string()).collect())
            .unwrap_or_default()
    }

    fn headings(catalog: &Catalog) -> Vec<String> {
        catalog
            .sections()
            .iter()
            .map(|(section, _)| section.label().to_string())
            .collect()
    }

    /// Alphabetical order would put Deployment above Pod. Nobody opens this
    /// sidebar looking for Deployment first.
    #[test]
    fn workloads_lead_with_pods() {
        let catalog = catalog();
        assert_eq!(
            section(&catalog, Category::Workloads),
            ["Pod", "Deployment (apps)"]
        );
    }

    /// Anything the table does not name is a custom resource, and each API
    /// group gets its own heading rather than all of them sharing one.
    #[test]
    fn custom_resources_are_filed_under_their_api_group() {
        let catalog = catalog();
        assert_eq!(group(&catalog, "argoproj.io"), ["Application"]);
        assert_eq!(group(&catalog, "k3s.cattle.io"), ["Addon"]);
        // The old single heading is gone, not merely empty.
        assert!(!headings(&catalog).contains(&"Custom Resources".to_string()));
        assert!(!headings(&catalog).contains(&"Other".to_string()));
    }

    /// The heading already says the group, so the row does not repeat it.
    #[test]
    fn a_grouped_entry_drops_the_group_from_its_label() {
        let catalog = catalog();
        let entry = catalog
            .sections()
            .iter()
            .find(|(section, _)| section.label() == "argoproj.io")
            .and_then(|(_, entries)| entries.first())
            .expect("the group");
        assert_eq!(entry.label, "Application");
        assert_eq!(entry.qualified, "Application (argoproj.io)");
    }

    /// Curated sections first, in their own order; groups after, by name.
    #[test]
    fn groups_come_after_the_curated_sections() {
        assert_eq!(
            headings(&catalog()),
            [
                "Workloads",
                "Config",
                "Access Control",
                "Cluster",
                "argoproj.io",
                "k3s.cattle.io",
            ]
        );
    }

    /// Typing a group is how you get everything that came with it.
    #[test]
    fn search_finds_a_kind_by_its_group() {
        let found = catalog().search("argoproj", &mut matcher());
        assert_eq!(
            found
                .iter()
                .map(|e| e.label.to_string())
                .collect::<Vec<_>>(),
            ["Application (argoproj.io)"],
            "a result has no heading above it, so it carries the group"
        );
    }

    #[test]
    fn known_kinds_land_in_their_section() {
        assert_eq!(placement(&kind("", "Service")).0, Category::Network);
        assert_eq!(placement(&kind("", "Secret")).0, Category::Config);
        assert_eq!(
            placement(&kind("", "PersistentVolumeClaim")).0,
            Category::Storage
        );
        assert_eq!(
            placement(&kind("rbac.authorization.k8s.io", "ClusterRole")).0,
            Category::AccessControl
        );
        assert_eq!(placement(&kind("", "Namespace")).0, Category::Cluster);
    }

    /// A CRD that borrowed a built-in name is still uncurated -- the group is
    /// what the table is keyed on -- and so ends up under its own group.
    #[test]
    fn a_borrowed_name_is_still_a_custom_resource() {
        assert_eq!(
            placement(&kind("example.com", "Service")).0,
            Category::Other
        );

        let catalog = Catalog::new(&[kind("example.com", "Service")]);
        assert_eq!(group(&catalog, "example.com"), ["Service"]);
    }

    /// The bucket is for core kinds the table forgot, and nothing else. It
    /// said "Custom Resources" until the groups moved out and left one
    /// `PodTemplate` sitting under a heading that was no longer true.
    #[test]
    fn only_core_kinds_fall_through_to_other() {
        let catalog = Catalog::new(&[kind("", "Binding"), kind("argoproj.io", "Application")]);
        assert_eq!(section(&catalog, Category::Other), ["Binding"]);
        assert_eq!(group(&catalog, "argoproj.io"), ["Application"]);
    }

    #[test]
    fn empty_sections_are_not_shown() {
        let catalog = Catalog::new(&[kind("", "Pod")]);
        assert_eq!(catalog.sections().len(), 1);
        assert_eq!(catalog.len(), 1);
    }

    /// Only the first section opens itself. A cluster with thirty CRD groups
    /// must not greet you with thirty open ones.
    #[test]
    fn no_custom_group_starts_open() {
        for (section, _) in catalog().sections() {
            match section {
                Section::Group(_) => assert!(!section.starts_open(), "{}", section.label()),
                Section::Builtin(_) => {}
            }
        }
    }

    /// Pods are where a cluster opens, and the fallback only matters on a
    /// cluster that does not serve them.
    #[test]
    fn a_cluster_opens_on_pods() {
        let catalog = catalog();
        let default = catalog.default_kind().expect("a default");
        assert_eq!(default.resource.kind, "Pod");

        let without_pods = Catalog::new(&[kind("argoproj.io", "Application")]);
        assert_eq!(
            without_pods
                .default_kind()
                .expect("a default")
                .resource
                .kind,
            "Application"
        );

        assert!(Catalog::new(&[]).default_kind().is_none());
    }

    /// Fuzzy, not substring: nobody types the group, or the capitals.
    #[test]
    fn search_finds_a_kind_from_an_abbreviation() {
        let catalog = catalog();
        let mut matcher = matcher();

        let found = catalog.search("dep", &mut matcher);
        assert_eq!(
            found.first().map(|entry| entry.label.to_string()),
            Some("Deployment (apps)".to_string())
        );

        let found = catalog.search("cfgmap", &mut matcher);
        assert_eq!(
            found.first().map(|entry| entry.label.to_string()),
            Some("ConfigMap".to_string())
        );
    }

    #[test]
    fn search_matching_nothing_returns_nothing() {
        assert!(catalog().search("zzzz", &mut matcher()).is_empty());
    }
}
