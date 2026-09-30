//! Kubeconfig discovery: which clusters can we connect to, and which one is
//! current. Reading this is cheap and synchronous, so the UI can do it during
//! startup before any runtime exists.

use kube::config::Kubeconfig;

use crate::{ClusterId, Result};

/// The readable identity inside an EKS cluster ARN. It never replaces a context ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EksCluster<'a> {
    pub name: &'a str,
    pub region: &'a str,
    pub account_id: &'a str,
}

impl<'a> EksCluster<'a> {
    pub fn parse(value: &'a str) -> Option<Self> {
        let mut parts = value.splitn(6, ':');
        if parts.next()? != "arn" || parts.next()?.is_empty() || parts.next()? != "eks" {
            return None;
        }
        let region = parts.next()?;
        let account_id = parts.next()?;
        let name = parts.next()?.strip_prefix("cluster/")?;
        if region.is_empty()
            || account_id.is_empty()
            || name.is_empty()
            || name.contains(['/', ':'])
            || value.chars().any(char::is_whitespace)
        {
            return None;
        }
        Some(Self {
            name,
            region,
            account_id,
        })
    }
}

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
    /// Recognises EKS even when the user gave its context a custom alias.
    pub fn eks(&self) -> Option<EksCluster<'_>> {
        EksCluster::parse(&self.cluster).or_else(|| EksCluster::parse(self.id.as_str()))
    }

    /// Custom aliases stay as written; ARN context names become cluster names.
    pub fn display_name(&self) -> &str {
        self.id.display_name()
    }

    /// The actual EKS name, or the context name for other clusters.
    pub fn cluster_name(&self) -> &str {
        self.eks()
            .map_or_else(|| self.display_name(), |eks| eks.name)
    }

    /// Region and account distinguish EKS clusters with the same short name.
    pub fn label(&self) -> String {
        match self.eks() {
            Some(eks) => format!(
                "{} · EKS · {} · {}",
                self.display_name(),
                eks.region,
                eks.account_id
            ),
            None => self.display_name().to_string(),
        }
    }

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
    fn recognises_eks_arns_across_aws_partitions() {
        for (partition, region) in [
            ("aws", "us-east-1"),
            ("aws-cn", "cn-north-1"),
            ("aws-us-gov", "us-gov-west-1"),
        ] {
            let arn = format!("arn:{partition}:eks:{region}:123456789012:cluster/web-dev-cluster");
            let eks = EksCluster::parse(&arn).unwrap();
            assert_eq!(eks.name, "web-dev-cluster");
            assert_eq!(eks.region, region);
            assert_eq!(eks.account_id, "123456789012");
            let id = ClusterId::new(&arn);
            assert_eq!(id.display_name(), "web-dev-cluster");
            assert_eq!(id.as_str(), arn);
            assert_eq!(id.to_string(), arn);
        }
    }

    #[test]
    fn ordinary_names_and_other_arns_are_not_shortened() {
        for name in [
            "default",
            "aliyun-dev",
            "arn:aws:ec2:us-east-1:123456789012:cluster/test",
            "arn:aws:eks:us-east-1:123456789012:nodegroup/test/group/id",
            "arn:aws:eks:us-east-1:123456789012:cluster/",
            "arn:aws:eks::123456789012:cluster/test",
            "arn:aws:eks:us-east-1::cluster/test",
            "arn:aws:eks:us-east-1:123456789012:cluster/test/extra",
            "arn:aws:eks:us-east-1:123456789012:cluster/test:extra",
            "arn:aws:eks:us-east-1:123456789012:cluster/test name",
        ] {
            assert!(EksCluster::parse(name).is_none(), "{name}");
            assert_eq!(ClusterId::new(name).display_name(), name);
        }
    }

    #[test]
    fn recognises_aliased_eks_without_replacing_the_context() {
        let entry = ContextEntry {
            id: ClusterId::new("dev"),
            cluster: "arn:aws:eks:us-east-1:123456789012:cluster/web-dev-cluster".into(),
            user: None,
            namespace: None,
            is_current: false,
        };
        assert_eq!(entry.display_name(), "dev");
        assert_eq!(entry.cluster_name(), "web-dev-cluster");
        assert_eq!(entry.label(), "dev · EKS · us-east-1 · 123456789012");
        assert_eq!(entry.id.as_str(), "dev");
    }

    #[test]
    fn same_eks_name_in_different_accounts_keeps_distinct_identities() {
        let entries: Vec<_> = ["123456789012", "987654321012"]
            .into_iter()
            .map(|account| {
                let arn = format!("arn:aws:eks:us-east-1:{account}:cluster/dev");
                ContextEntry {
                    id: ClusterId::new(&arn),
                    cluster: arn,
                    user: None,
                    namespace: None,
                    is_current: false,
                }
            })
            .collect();
        assert_eq!(entries[0].display_name(), entries[1].display_name());
        assert_ne!(entries[0].id, entries[1].id);
        assert_ne!(entries[0].label(), entries[1].label());
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
