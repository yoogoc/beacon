//! Kubeconfig discovery: which clusters can we connect to, and which one is
//! current. Reading this is cheap and synchronous, so the UI can do it during
//! startup before any runtime exists.

use kube::config::Kubeconfig;

use crate::{ClusterId, Result};

/// One connectable context, flattened into what the UI actually displays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextEntry {
    pub id: ClusterId,
    pub cluster: String,
    pub user: Option<String>,
    /// The context's default namespace. `None` means the context did not pin
    /// one, in which case `default` applies.
    pub namespace: Option<String>,
    pub is_current: bool,
}

impl ContextEntry {
    /// The namespace a fresh connection should start in.
    pub fn initial_namespace(&self) -> &str {
        self.namespace.as_deref().unwrap_or("default")
    }
}

/// Every context in the merged kubeconfig, in file order.
///
/// Order matters: users recognise their kubeconfig by its layout, so we never
/// sort this. `KUBECONFIG` with multiple paths is merged by kube-rs the same
/// way kubectl merges it.
#[derive(Debug, Clone, Default)]
pub struct Contexts {
    entries: Vec<ContextEntry>,
}

impl Contexts {
    /// Reads `$KUBECONFIG`, falling back to `~/.kube/config`.
    pub fn load() -> Result<Self> {
        Ok(Self::from_kubeconfig(&Kubeconfig::read()?))
    }

    pub fn from_kubeconfig(kubeconfig: &Kubeconfig) -> Self {
        let current = kubeconfig.current_context.as_deref();

        let entries = kubeconfig
            .contexts
            .iter()
            .filter_map(|named| {
                // A context without a body is malformed; kubectl ignores it
                // rather than failing the whole file, and so do we.
                let context = named.context.as_ref()?;
                Some(ContextEntry {
                    id: ClusterId::new(&named.name),
                    cluster: context.cluster.clone(),
                    user: context.user.clone(),
                    namespace: context.namespace.clone(),
                    is_current: current == Some(named.name.as_str()),
                })
            })
            .collect();

        Self { entries }
    }

    pub fn entries(&self) -> &[ContextEntry] {
        &self.entries
    }

    pub fn current(&self) -> Option<&ContextEntry> {
        self.entries.iter().find(|entry| entry.is_current)
    }

    pub fn get(&self, id: &ClusterId) -> Option<&ContextEntry> {
        self.entries.iter().find(|entry| &entry.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
apiVersion: v1
kind: Config
current-context: staging
clusters:
  - name: prod-cluster
    cluster:
      server: https://prod.example.com
  - name: staging-cluster
    cluster:
      server: https://staging.example.com
users:
  - name: prod-user
  - name: staging-user
contexts:
  - name: prod
    context:
      cluster: prod-cluster
      user: prod-user
      namespace: payments
  - name: staging
    context:
      cluster: staging-cluster
      user: staging-user
  - name: broken
"#;

    fn sample() -> Contexts {
        Contexts::from_kubeconfig(&Kubeconfig::from_yaml(SAMPLE).expect("sample parses"))
    }

    #[test]
    fn keeps_file_order_and_skips_bodyless_contexts() {
        let contexts = sample();
        let names: Vec<_> = contexts
            .entries()
            .iter()
            .map(|e| e.id.to_string())
            .collect();
        assert_eq!(names, ["prod", "staging"]);
    }

    #[test]
    fn marks_only_the_current_context() {
        let contexts = sample();
        assert_eq!(contexts.current().map(|e| e.id.as_str()), Some("staging"));
        assert_eq!(
            contexts.entries().iter().filter(|e| e.is_current).count(),
            1
        );
    }

    #[test]
    fn falls_back_to_default_namespace() {
        let contexts = sample();
        let prod = contexts.get(&ClusterId::new("prod")).unwrap();
        let staging = contexts.get(&ClusterId::new("staging")).unwrap();
        assert_eq!(prod.initial_namespace(), "payments");
        assert_eq!(staging.initial_namespace(), "default");
    }

    #[test]
    fn empty_kubeconfig_is_not_an_error() {
        let contexts = Contexts::from_kubeconfig(&Kubeconfig::default());
        assert!(contexts.is_empty());
        assert!(contexts.current().is_none());
    }
}
