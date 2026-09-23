//! CPU and memory, as metrics-server reports them.
//!
//! `metrics.k8s.io` is an aggregated API with no types in `k8s-openapi`, so
//! the handful of fields worth reading are declared here. It is also optional:
//! plenty of clusters do not run metrics-server, and asking one of those must
//! produce "no metrics" rather than an error banner over a working table.
//!
//! The interesting part is the numbers. Kubernetes writes quantities as
//! strings — `1500n`, `128974848`, `123Mi`, `129e6` — and getting the suffixes
//! wrong is the difference between 123 megabytes and 129 megabytes, or between
//! a core and a nanocore.

use std::collections::HashMap;

use kube::{
    Api,
    api::{ApiResource, DynamicObject, ListParams},
    core::GroupVersionKind,
};
use serde_json::Value;

use crate::Result;

/// What one container, pod or node is using.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Thousandths of a core, the unit Kubernetes itself asks for.
    pub cpu_millis: i64,
    pub memory_bytes: i64,
}

impl Usage {
    /// `250m`, the way a resource request is written.
    pub fn cpu(&self) -> String {
        format!("{}m", self.cpu_millis)
    }

    /// `1.4Gi`, rounded the way a person reads it rather than exactly.
    pub fn memory(&self) -> String {
        const UNITS: [(&str, f64); 4] = [
            ("Gi", 1024.0 * 1024.0 * 1024.0),
            ("Mi", 1024.0 * 1024.0),
            ("Ki", 1024.0),
            ("", 1.0),
        ];

        let bytes = self.memory_bytes as f64;
        for (unit, size) in UNITS {
            if bytes >= size {
                let value = bytes / size;
                return if value >= 100.0 || unit.is_empty() {
                    format!("{}{unit}", value.round() as i64)
                } else {
                    format!("{value:.1}{unit}")
                };
            }
        }
        "0".to_string()
    }
}

/// Usage for everything metrics-server knows about, keyed the way a table
/// looks it up.
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    /// Keyed by `namespace/name` for pods, and by bare name for nodes.
    usage: HashMap<String, Usage>,
}

impl Metrics {
    pub fn get(&self, namespace: Option<&str>, name: &str) -> Option<Usage> {
        let key = match namespace {
            Some(namespace) => format!("{namespace}/{name}"),
            None => name.to_string(),
        };
        self.usage.get(&key).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.usage.is_empty()
    }

    pub fn len(&self) -> usize {
        self.usage.len()
    }
}

fn resource(kind: &str, plural: &str) -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("metrics.k8s.io", "v1beta1", kind),
        plural,
    )
}

/// Reads pod and node usage.
///
/// Never fails loudly: a cluster without metrics-server returns empty, which
/// is what the columns show.
pub async fn fetch(client: &kube::Client) -> Metrics {
    let mut usage = HashMap::new();

    match list(client, resource("PodMetrics", "pods")).await {
        Ok(items) => {
            for item in items {
                let Some(name) = item.metadata.name.as_deref() else {
                    continue;
                };
                let namespace = item.metadata.namespace.as_deref().unwrap_or_default();
                usage.insert(format!("{namespace}/{name}"), pod_usage(&item.data));
            }
        }
        Err(error) => {
            tracing::debug!(%error, "no pod metrics");
        }
    }

    match list(client, resource("NodeMetrics", "nodes")).await {
        Ok(items) => {
            for item in items {
                let Some(name) = item.metadata.name.clone() else {
                    continue;
                };
                usage.insert(name, read_usage(item.data.get("usage")));
            }
        }
        Err(error) => {
            tracing::debug!(%error, "no node metrics");
        }
    }

    Metrics { usage }
}

async fn list(client: &kube::Client, resource: ApiResource) -> Result<Vec<DynamicObject>> {
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &resource);
    Ok(api.list(&ListParams::default()).await?.items)
}

/// A pod's usage is the sum of its containers'; metrics-server does not
/// report a pod total.
///
/// Summed in base units and rounded once at the end. Rounding each container
/// first loses a little every time -- `3500u` is 3.5 millicores, and in binary
/// it is a hair under, so per-container rounding turns it into 3.
fn pod_usage(data: &Value) -> Usage {
    let Some(containers) = data.get("containers").and_then(Value::as_array) else {
        return Usage::default();
    };

    let mut cpu_cores = 0.0;
    let mut memory_bytes = 0.0;
    for container in containers {
        let (cores, bytes) = raw_usage(container.get("usage"));
        cpu_cores += cores;
        memory_bytes += bytes;
    }

    round(cpu_cores, memory_bytes)
}

fn read_usage(usage: Option<&Value>) -> Usage {
    let (cores, bytes) = raw_usage(usage);
    round(cores, bytes)
}

/// Cores and bytes, unrounded.
fn raw_usage(usage: Option<&Value>) -> (f64, f64) {
    let Some(usage) = usage else {
        return (0.0, 0.0);
    };

    let read = |field: &str| {
        usage
            .get(field)
            .and_then(Value::as_str)
            .and_then(parse_quantity)
            .unwrap_or(0.0)
    };

    (read("cpu"), read("memory"))
}

fn round(cpu_cores: f64, memory_bytes: f64) -> Usage {
    Usage {
        cpu_millis: (cpu_cores * 1000.0).round() as i64,
        memory_bytes: memory_bytes.round() as i64,
    }
}

/// Reads a Kubernetes quantity into its base unit: cores for CPU, bytes for
/// memory.
///
/// Three notations are in use and all three turn up in real answers. Decimal
/// SI suffixes (`n`, `u`, `m`, `k`, `M`, `G`), binary ones (`Ki`, `Mi`, `Gi`),
/// and scientific notation (`129e6`). `m` is milli and `M` is mega, which is
/// the mistake that turns 100 millicores into 100 megacores.
pub fn parse_quantity(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let split = value
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(value.len());
    let (number, suffix) = value.split_at(split);

    // Scientific notation: the exponent is part of the number, not a suffix.
    if matches!(suffix.as_bytes().first(), Some(b'e' | b'E')) {
        return value.parse().ok();
    }

    let number: f64 = number.parse().ok()?;
    let multiplier = match suffix {
        "" => 1.0,
        "n" => 1e-9,
        "u" => 1e-6,
        "m" => 1e-3,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "E" => 1e18,
        "Ki" => 1024.0,
        "Mi" => 1024f64.powi(2),
        "Gi" => 1024f64.powi(3),
        "Ti" => 1024f64.powi(4),
        "Pi" => 1024f64.powi(5),
        "Ei" => 1024f64.powi(6),
        _ => return None,
    };

    Some(number * multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn about(value: Option<f64>, expected: f64) {
        let value = value.expect("a quantity");
        assert!(
            (value - expected).abs() < expected.abs() * 1e-9 + 1e-9,
            "{value} is not {expected}"
        );
    }

    /// `m` is milli and `M` is mega. Confusing them turns a tenth of a core
    /// into a hundred million of them.
    #[test]
    fn case_decides_the_magnitude() {
        about(parse_quantity("100m"), 0.1);
        about(parse_quantity("100M"), 1e8);
    }

    #[test]
    fn decimal_suffixes() {
        about(parse_quantity("1"), 1.0);
        about(parse_quantity("1500n"), 1.5e-6);
        about(parse_quantity("250u"), 250e-6);
        about(parse_quantity("2k"), 2000.0);
        about(parse_quantity("3G"), 3e9);
    }

    #[test]
    fn binary_suffixes() {
        about(parse_quantity("123Ki"), 123.0 * 1024.0);
        about(parse_quantity("1Gi"), 1024.0 * 1024.0 * 1024.0);
    }

    /// `129e6` is how some controllers write a memory limit, and the exponent
    /// is part of the number rather than a suffix.
    #[test]
    fn scientific_notation() {
        about(parse_quantity("129e6"), 129e6);
        about(parse_quantity("1.5e3"), 1500.0);
    }

    #[test]
    fn nonsense_is_not_a_quantity() {
        assert!(parse_quantity("").is_none());
        assert!(parse_quantity("lots").is_none());
        assert!(parse_quantity("12Qi").is_none());
    }

    /// metrics-server reports per container; the pod total is ours to compute.
    #[test]
    fn a_pods_usage_is_the_sum_of_its_containers() {
        let data = json!({
            "containers": [
                { "name": "app", "usage": { "cpu": "12m", "memory": "100Mi" } },
                { "name": "sidecar", "usage": { "cpu": "3500u", "memory": "24Mi" } }
            ]
        });

        let usage = pod_usage(&data);
        assert_eq!(usage.cpu_millis, 16, "12m + 3.5m, rounded once");
        assert_eq!(usage.memory_bytes, 124 * 1024 * 1024);
    }

    #[test]
    fn a_pod_with_no_metrics_reads_as_zero() {
        assert_eq!(pod_usage(&json!({})), Usage::default());
        assert_eq!(read_usage(None), Usage::default());
    }

    /// The column is read at a glance, so it is rounded the way somebody would
    /// say it out loud.
    #[test]
    fn usage_reads_the_way_a_person_says_it() {
        let usage = Usage {
            cpu_millis: 250,
            memory_bytes: (1.4 * 1024.0 * 1024.0 * 1024.0) as i64,
        };
        assert_eq!(usage.cpu(), "250m");
        assert_eq!(usage.memory(), "1.4Gi");

        let big = Usage {
            cpu_millis: 4000,
            memory_bytes: 512 * 1024 * 1024,
        };
        assert_eq!(big.cpu(), "4000m");
        assert_eq!(big.memory(), "512Mi");

        assert_eq!(Usage::default().memory(), "0");
    }

    #[test]
    fn metrics_are_looked_up_the_way_a_table_reads_them() {
        let metrics = Metrics {
            usage: HashMap::from([
                (
                    "argo/api".to_string(),
                    Usage {
                        cpu_millis: 5,
                        memory_bytes: 10,
                    },
                ),
                (
                    "k3s".to_string(),
                    Usage {
                        cpu_millis: 900,
                        memory_bytes: 20,
                    },
                ),
            ]),
        };

        assert_eq!(metrics.get(Some("argo"), "api").unwrap().cpu_millis, 5);
        assert_eq!(metrics.get(None, "k3s").unwrap().cpu_millis, 900);
        assert!(metrics.get(Some("other"), "api").is_none());
        assert_eq!(metrics.len(), 2);
    }
}
