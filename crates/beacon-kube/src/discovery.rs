//! What a cluster can show us.
//!
//! Beacon has no compiled-in list of resource kinds. It asks the cluster, which
//! is the only way a client can list a CRD that was installed after it
//! shipped -- and on a real cluster most of what people look at is CRDs.
//!
//! Two things come back. Discovery gives the kinds and what may be done with
//! each one. A CRD additionally publishes the columns its own table should
//! have, but only inside the CRD object, which is large; those are fetched one
//! at a time, when somebody actually opens that kind.

use std::{collections::HashMap, sync::Arc};

use kube::{
    Api, Client,
    api::{ApiResource, DynamicObject, ListParams},
    core::GroupVersionKind,
    discovery::{Scope, verbs},
};
use serde_json::Value;

use crate::Result;

/// One kind the cluster serves, and what can be done with it.
#[derive(Debug, Clone)]
pub struct Kind {
    pub resource: ApiResource,
    /// Whether objects of this kind live in a namespace.
    pub namespaced: bool,
    /// The verbs the API server advertises. This is what the server *supports*,
    /// not what this user is allowed to do -- that is a separate question, and
    /// the answer to it arrives with M4.
    pub verbs: Vec<String>,
}

impl Kind {
    pub fn gvk(&self) -> GroupVersionKind {
        GroupVersionKind::gvk(
            &self.resource.group,
            &self.resource.version,
            &self.resource.kind,
        )
    }

    /// Whether Beacon can put this kind in a table at all.
    ///
    /// Everything here is built on watches, so a kind that cannot be watched
    /// cannot be shown -- and there is no point offering it.
    pub fn is_listable(&self) -> bool {
        self.supports(verbs::LIST) && self.supports(verbs::WATCH)
    }

    pub fn supports(&self, verb: &str) -> bool {
        self.verbs.iter().any(|supported| supported == verb)
    }

    /// `Pod`, or `Application (argoproj.io)` -- the group is what separates two
    /// kinds that share a name, and there are more of those than you would
    /// expect.
    pub fn display_name(&self) -> String {
        if self.resource.group.is_empty() {
            self.resource.kind.clone()
        } else {
            format!("{} ({})", self.resource.kind, self.resource.group)
        }
    }
}

/// Every kind a cluster serves, at the version the server recommends.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    kinds: Vec<Kind>,
}

impl Discovery {
    /// Asks the cluster what it serves.
    ///
    /// Only listable kinds are kept, at the server's preferred version for each
    /// group. Offering `v1beta1` alongside `v1` of the same thing is a menu
    /// nobody wants to read.
    pub async fn load(client: &Client) -> Result<Self> {
        // Aggregated discovery answers in one or two requests. Without it,
        // kube queries every group separately, which on a cluster with fifty
        // CRD groups is fifty round trips.
        let discovery = match kube::Discovery::new(client.clone()).run_aggregated().await {
            Ok(discovery) => discovery,
            Err(error) => {
                tracing::debug!(%error, "aggregated discovery unavailable, querying each group");
                kube::Discovery::new(client.clone()).run().await?
            }
        };

        let mut kinds: Vec<Kind> = discovery
            .groups()
            .flat_map(|group| group.recommended_resources())
            .map(|(resource, capabilities)| Kind {
                resource,
                namespaced: capabilities.scope == Scope::Namespaced,
                verbs: capabilities.operations,
            })
            .filter(Kind::is_listable)
            .collect();

        // Alphabetical by kind, then group: the order a list of names is read
        // in, rather than the order the API server happened to answer in.
        kinds.sort_by(|left, right| {
            (&left.resource.kind, &left.resource.group)
                .cmp(&(&right.resource.kind, &right.resource.group))
        });

        tracing::info!(kinds = kinds.len(), "discovered");
        Ok(Self { kinds })
    }

    pub fn kinds(&self) -> &[Kind] {
        &self.kinds
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    pub fn get(&self, gvk: &GroupVersionKind) -> Option<&Kind> {
        self.kinds.iter().find(|kind| &kind.gvk() == gvk)
    }

    /// The kind a given `kind` name refers to, preferring the core group.
    ///
    /// `Pod` should mean the Pod everybody means, even on a cluster where
    /// something else also calls itself one.
    pub fn find(&self, kind: &str) -> Option<&Kind> {
        self.kinds
            .iter()
            .filter(|candidate| candidate.resource.kind == kind)
            .min_by_key(|candidate| candidate.resource.group.len())
    }
}

/// The `additionalPrinterColumns` a CRD publishes, cached per cluster.
///
/// Fetched one CRD at a time rather than by listing them all: a CRD carries its
/// whole OpenAPI schema, so listing fifty of them moves megabytes to answer a
/// question about four columns.
#[derive(Default)]
pub struct PrinterColumns {
    /// `None` means asked and there are none -- a built-in kind, a CRD without
    /// columns, or a cluster that will not let us read CRDs. All three are
    /// answered the same way, and none is worth asking twice.
    known: HashMap<GroupVersionKind, Option<Arc<Value>>>,
}

impl PrinterColumns {
    pub fn get(&self, gvk: &GroupVersionKind) -> Option<Option<&Arc<Value>>> {
        self.known.get(gvk).map(Option::as_ref)
    }

    pub fn insert(&mut self, gvk: GroupVersionKind, columns: Option<Arc<Value>>) {
        self.known.insert(gvk, columns);
    }
}

/// Reads one CRD's printer columns for one version.
///
/// `Ok(None)` for a kind that is not backed by a CRD, which is the ordinary
/// answer for everything built in, and for a cluster that does not let this
/// user read CRDs. Neither is a failure worth surfacing: the kind simply gets
/// the fallback columns.
pub async fn fetch_printer_columns(client: &Client, resource: &ApiResource) -> Option<Arc<Value>> {
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &crd_resource());
    // A CRD's own name is always `<plural>.<group>`, so this is a direct read
    // rather than a search.
    let name = format!("{}.{}", resource.plural, resource.group);

    let crd = match api.get_opt(&name).await {
        Ok(Some(crd)) => crd,
        Ok(None) => return None,
        Err(error) => {
            tracing::debug!(crd = %name, %error, "could not read the CRD; using fallback columns");
            return None;
        }
    };

    let columns = crd
        .data
        .get("spec")?
        .get("versions")?
        .as_array()?
        .iter()
        .find(|version| version.get("name").and_then(Value::as_str) == Some(&resource.version))?
        .get("additionalPrinterColumns")?
        .clone();

    Some(Arc::new(columns))
}

/// Every CRD on the cluster, for the rare caller that wants them all.
///
/// Kept separate from [`fetch_printer_columns`] and deliberately not used to
/// warm the cache: see that function's note about size.
pub async fn list_crd_names(client: &Client) -> Result<Vec<String>> {
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &crd_resource());
    let crds = api.list(&ListParams::default()).await?;
    Ok(crds
        .items
        .into_iter()
        .filter_map(|crd| crd.metadata.name)
        .collect())
}

fn crd_resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("apiextensions.k8s.io", "v1", "CustomResourceDefinition"),
        "customresourcedefinitions",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(group: &str, version: &str, name: &str, verbs: &[&str]) -> Kind {
        Kind {
            resource: ApiResource::from_gvk_with_plural(
                &GroupVersionKind::gvk(group, version, name),
                &format!("{}s", name.to_lowercase()),
            ),
            namespaced: true,
            verbs: verbs.iter().map(|verb| verb.to_string()).collect(),
        }
    }

    fn discovery(kinds: Vec<Kind>) -> Discovery {
        Discovery { kinds }
    }

    /// Everything is built on watches, so a kind that cannot be watched cannot
    /// be shown -- offering it would produce an empty table and no explanation.
    #[test]
    fn a_kind_that_cannot_be_watched_is_not_listable() {
        assert!(kind("", "v1", "Pod", &["list", "watch", "get"]).is_listable());
        assert!(!kind("", "v1", "Binding", &["create"]).is_listable());
        assert!(
            !kind(
                "authorization.k8s.io",
                "v1",
                "SelfSubjectAccessReview",
                &["create"]
            )
            .is_listable()
        );
        // `list` without `watch` is the shape of an aggregated API that cannot
        // stream, and it is just as unusable here.
        assert!(!kind("metrics.k8s.io", "v1beta1", "PodMetrics", &["list", "get"]).is_listable());
    }

    /// A name alone is ambiguous on a cluster where a CRD borrowed it. The core
    /// group is what the name means.
    #[test]
    fn a_bare_kind_name_resolves_to_the_core_group() {
        let discovery = discovery(vec![
            kind("example.com", "v1", "Pod", &["list", "watch"]),
            kind("", "v1", "Pod", &["list", "watch"]),
        ]);

        assert_eq!(discovery.find("Pod").unwrap().resource.group, "");
        assert!(discovery.find("Widget").is_none());
    }

    #[test]
    fn a_kind_is_found_by_its_full_group_version_kind() {
        let discovery = discovery(vec![kind("apps", "v1", "Deployment", &["list", "watch"])]);

        let gvk = GroupVersionKind::gvk("apps", "v1", "Deployment");
        assert_eq!(discovery.get(&gvk).unwrap().resource.kind, "Deployment");
        assert!(
            discovery
                .get(&GroupVersionKind::gvk("apps", "v1beta1", "Deployment"))
                .is_none(),
            "a different version is a different kind"
        );
    }

    /// The group is what tells two same-named kinds apart, and a menu that
    /// lists `Application` twice with no way to tell which is which is worse
    /// than useless.
    #[test]
    fn a_grouped_kind_is_named_with_its_group() {
        assert_eq!(kind("", "v1", "Pod", &[]).display_name(), "Pod");
        assert_eq!(
            kind("argoproj.io", "v1alpha1", "Application", &[]).display_name(),
            "Application (argoproj.io)"
        );
    }

    /// The cache has to distinguish "no columns" from "not asked yet", or every
    /// built-in kind re-reads a CRD that does not exist on every visit.
    #[test]
    fn the_printer_column_cache_remembers_an_absence() {
        let mut cache = PrinterColumns::default();
        let gvk = GroupVersionKind::gvk("", "v1", "Pod");

        assert!(cache.get(&gvk).is_none(), "not asked yet");

        cache.insert(gvk.clone(), None);
        assert!(
            matches!(cache.get(&gvk), Some(None)),
            "asked, and there are none"
        );

        let columns = Arc::new(serde_json::json!([{ "name": "Source" }]));
        cache.insert(gvk.clone(), Some(columns));
        assert!(matches!(cache.get(&gvk), Some(Some(_))));
    }

    #[test]
    fn the_crd_resource_builds_the_right_path() {
        let resource = crd_resource();
        assert_eq!(resource.group, "apiextensions.k8s.io");
        assert_eq!(resource.version, "v1");
        assert_eq!(resource.plural, "customresourcedefinitions");
    }
}
