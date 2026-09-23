//! Column definitions for the generic resource table.
//!
//! Beacon never writes a Rust type per Kubernetes resource. A list view is
//! driven entirely by a [`ColumnSet`], resolved in this order:
//!
//! 1. a built-in table for well-known resources (kubectl's own columns),
//! 2. the CRD's `additionalPrinterColumns`, evaluated as a field path at
//!    runtime,
//! 3. a fallback of Name / Namespace / Age.
//!
//! Only steps 1 and 3 need code. Step 2 is data, which is why an unknown CRD
//! lists correctly on the day it is installed.

pub mod age;
pub mod path;
pub mod pod;

pub use age::{format_age, format_duration};
/// Re-exported so that consumers do not have to pick a `jiff` version to match
/// the one `k8s-openapi` models timestamps with.
pub use k8s_openapi::jiff::Timestamp;
pub use pod::PodSummary;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use serde_json::Value;

/// One object, as a column needs to see it.
///
/// Split the way a `DynamicObject` is split: typed metadata, and the rest of
/// the object as JSON. `now` is passed in rather than read, so that a whole
/// frame's cells agree about what time it is and so that ages are testable.
pub struct Cell<'a> {
    pub metadata: &'a ObjectMeta,
    /// Everything that is not `apiVersion`, `kind` or `metadata` -- in practice
    /// `spec` and `status`.
    pub data: &'a Value,
    pub now: Timestamp,
}

/// A value ready to be painted into a table cell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum CellValue {
    Text(String),
    /// The field was absent. Rendered as kubectl's `<none>`, in muted colour.
    #[default]
    Missing,
}

impl CellValue {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    /// What the cell displays, including the placeholder for missing values.
    pub fn display(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Missing => "<none>",
        }
    }

    pub fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
}

impl From<Option<String>> for CellValue {
    fn from(value: Option<String>) -> Self {
        value.map(Self::Text).unwrap_or(Self::Missing)
    }
}

/// Renders a JSON value the way kubectl prints it in a column.
impl From<&Value> for CellValue {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => Self::Missing,
            Value::String(text) if text.is_empty() => Self::Missing,
            Value::String(text) => Self::Text(text.clone()),
            Value::Bool(value) => Self::Text(value.to_string()),
            Value::Number(value) => Self::Text(value.to_string()),
            // Arrays and objects have no column representation; kubectl prints
            // their JSON, which at least shows what is there.
            other => Self::Text(other.to_string()),
        }
    }
}

/// How a column claims horizontal space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColumnWidth {
    /// Fixed pixels -- for columns whose content has a known shape (Age, Ready).
    Fixed(f32),
    /// Takes a share of the leftover space, proportional to the weight.
    Flex(f32),
}

/// Where a cell's content comes from.
#[derive(Clone)]
pub enum ColumnSource {
    Name,
    Namespace,
    /// Derived from `metadata.creationTimestamp`.
    Age,
    /// A field path from a CRD's `additionalPrinterColumns`, evaluated against
    /// the object at render time. See [`path`] for the supported subset.
    JsonPath(String),
    /// A built-in computation that needs more than one field -- a Pod's
    /// `Ready` count, a Deployment's `Up-to-date`, and so on.
    ///
    /// These read the object on demand, and only for the rows on screen.
    Computed(fn(&Cell<'_>) -> CellValue),
}

impl std::fmt::Debug for ColumnSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Name => f.write_str("Name"),
            Self::Namespace => f.write_str("Namespace"),
            Self::Age => f.write_str("Age"),
            Self::JsonPath(path) => write!(f, "JsonPath({path:?})"),
            Self::Computed(_) => f.write_str("Computed(..)"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub header: String,
    pub width: ColumnWidth,
    pub source: ColumnSource,
}

impl ColumnDef {
    pub fn new(header: impl Into<String>, width: ColumnWidth, source: ColumnSource) -> Self {
        Self {
            header: header.into(),
            width,
            source,
        }
    }

    /// Computes this column's value for one object.
    pub fn resolve(&self, cell: &Cell<'_>) -> CellValue {
        match &self.source {
            ColumnSource::Name => cell.metadata.name.clone().into(),
            ColumnSource::Namespace => cell.metadata.namespace.clone().into(),
            ColumnSource::Age => cell
                .metadata
                .creation_timestamp
                .as_ref()
                .map(|created| CellValue::text(format_age(created, cell.now)))
                .unwrap_or_default(),
            ColumnSource::JsonPath(expression) => self.resolve_path(expression, cell),
            ColumnSource::Computed(compute) => compute(cell),
        }
    }

    fn resolve_path(&self, expression: &str, cell: &Cell<'_>) -> CellValue {
        // `data` is the object without its metadata, so a path rooted at
        // `metadata` has to be answered from the typed struct instead. That is
        // rare enough to be worth a serialization when it happens and nothing
        // at all when it does not.
        if path::root(expression) == Some("metadata") {
            let metadata = serde_json::to_value(cell.metadata).unwrap_or(Value::Null);
            return path::evaluate(expression, &Value::from_iter([("metadata", metadata)]))
                .map(CellValue::from)
                .unwrap_or_default();
        }

        path::evaluate(expression, cell.data)
            .map(CellValue::from)
            .unwrap_or_default()
    }
}

/// The columns for one resource kind.
#[derive(Debug, Clone)]
pub struct ColumnSet {
    pub columns: Vec<ColumnDef>,
}

impl ColumnSet {
    /// The columns for a kind: the built-in table if there is one, otherwise
    /// the fallback. CRD printer columns slot in between these two.
    pub fn for_kind(group: &str, kind: &str, namespaced: bool) -> Self {
        Self::builtin(group, kind, namespaced).unwrap_or_else(|| Self::fallback(namespaced))
    }

    /// kubectl's own columns for a resource Beacon knows by name.
    ///
    /// Keyed on group as well as kind: `Pod` in a third-party API group is
    /// somebody else's resource that happens to share the name.
    pub fn builtin(group: &str, kind: &str, namespaced: bool) -> Option<Self> {
        match (group, kind) {
            ("", "Pod") => Some(Self::pod(namespaced)),
            _ => None,
        }
    }

    /// `kubectl get pods`.
    fn pod(namespaced: bool) -> Self {
        let mut columns = vec![ColumnDef::new(
            "Name",
            ColumnWidth::Flex(3.0),
            ColumnSource::Name,
        )];

        if namespaced {
            columns.push(ColumnDef::new(
                "Namespace",
                ColumnWidth::Flex(1.5),
                ColumnSource::Namespace,
            ));
        }

        columns.extend([
            ColumnDef::new(
                "Ready",
                ColumnWidth::Fixed(68.0),
                ColumnSource::Computed(|cell| {
                    CellValue::text(pod::summarize(cell.metadata, cell.data, cell.now).ready)
                }),
            ),
            ColumnDef::new(
                "Status",
                ColumnWidth::Fixed(170.0),
                ColumnSource::Computed(|cell| {
                    CellValue::text(pod::summarize(cell.metadata, cell.data, cell.now).status)
                }),
            ),
            ColumnDef::new(
                "Restarts",
                ColumnWidth::Fixed(120.0),
                ColumnSource::Computed(|cell| {
                    CellValue::text(pod::summarize(cell.metadata, cell.data, cell.now).restarts)
                }),
            ),
            ColumnDef::new("Age", ColumnWidth::Fixed(72.0), ColumnSource::Age),
        ]);

        Self { columns }
    }

    /// The columns used when nothing better is known about a resource.
    pub fn fallback(namespaced: bool) -> Self {
        let mut columns = vec![ColumnDef::new(
            "Name",
            ColumnWidth::Flex(2.0),
            ColumnSource::Name,
        )];

        if namespaced {
            columns.push(ColumnDef::new(
                "Namespace",
                ColumnWidth::Flex(1.0),
                ColumnSource::Namespace,
            ));
        }

        columns.push(ColumnDef::new(
            "Age",
            ColumnWidth::Fixed(72.0),
            ColumnSource::Age,
        ));

        Self { columns }
    }

    pub fn headers(&self) -> Vec<&str> {
        self.columns.iter().map(|c| c.header.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    use serde_json::json;

    fn now() -> Timestamp {
        "2026-09-20T12:00:00Z".parse().expect("fixed clock")
    }

    fn metadata() -> ObjectMeta {
        ObjectMeta {
            name: Some("api-7f9".into()),
            namespace: Some("payments".into()),
            creation_timestamp: Some(Time("2026-09-17T12:00:00Z".parse().expect("fixed"))),
            ..Default::default()
        }
    }

    fn resolve(source: ColumnSource, data: &Value) -> CellValue {
        let metadata = metadata();
        ColumnDef::new("c", ColumnWidth::Fixed(10.0), source).resolve(&Cell {
            metadata: &metadata,
            data,
            now: now(),
        })
    }

    #[test]
    fn missing_values_render_as_none() {
        assert_eq!(CellValue::Missing.display(), "<none>");
        assert_eq!(CellValue::from(None).display(), "<none>");
        assert_eq!(
            CellValue::from(Some("kube-system".into())).display(),
            "kube-system"
        );
    }

    #[test]
    fn metadata_columns_come_from_metadata() {
        let data = json!({});
        assert_eq!(resolve(ColumnSource::Name, &data).display(), "api-7f9");
        assert_eq!(
            resolve(ColumnSource::Namespace, &data).display(),
            "payments"
        );
        assert_eq!(resolve(ColumnSource::Age, &data).display(), "3d");
    }

    #[test]
    fn a_field_path_reads_the_object() {
        let data = json!({ "status": { "phase": "Running" }, "spec": { "replicas": 3 } });
        assert_eq!(
            resolve(ColumnSource::JsonPath(".status.phase".into()), &data).display(),
            "Running"
        );
        assert_eq!(
            resolve(ColumnSource::JsonPath(".spec.replicas".into()), &data).display(),
            "3"
        );
    }

    /// `data` holds no metadata, so a path rooted there has to be answered from
    /// the typed struct -- CRDs do write `.metadata.labels.*` columns.
    #[test]
    fn a_field_path_can_still_reach_metadata() {
        let data = json!({});
        assert_eq!(
            resolve(ColumnSource::JsonPath(".metadata.name".into()), &data).display(),
            "api-7f9"
        );
    }

    #[test]
    fn a_path_that_resolves_to_nothing_renders_as_none() {
        let data = json!({ "status": {} });
        assert!(resolve(ColumnSource::JsonPath(".status.phase".into()), &data).is_missing());
        assert!(resolve(ColumnSource::JsonPath(".spec.replicas".into()), &data).is_missing());
    }

    /// An empty string is a present-but-blank field. kubectl shows `<none>`
    /// rather than a cell that looks like a rendering failure.
    #[test]
    fn an_empty_string_is_a_missing_value() {
        let data = json!({ "status": { "phase": "" } });
        assert!(resolve(ColumnSource::JsonPath(".status.phase".into()), &data).is_missing());
    }

    #[test]
    fn cluster_scoped_fallback_has_no_namespace_column() {
        assert_eq!(ColumnSet::fallback(false).headers(), ["Name", "Age"]);
    }

    #[test]
    fn namespaced_fallback_has_a_namespace_column() {
        assert_eq!(
            ColumnSet::fallback(true).headers(),
            ["Name", "Namespace", "Age"]
        );
    }

    #[test]
    fn pods_get_kubectls_columns() {
        assert_eq!(
            ColumnSet::for_kind("", "Pod", true).headers(),
            ["Name", "Namespace", "Ready", "Status", "Restarts", "Age"]
        );
        assert_eq!(
            ColumnSet::for_kind("", "Pod", false).headers(),
            ["Name", "Ready", "Status", "Restarts", "Age"]
        );
    }

    /// A CRD that happens to be called Pod is not a Pod.
    #[test]
    fn the_builtin_table_is_keyed_on_the_group_too() {
        assert!(ColumnSet::builtin("example.com", "Pod", true).is_none());
        assert_eq!(
            ColumnSet::for_kind("example.com", "Pod", true).headers(),
            ["Name", "Namespace", "Age"]
        );
    }

    /// The computed Pod columns have to agree with `pod::summarize`, since the
    /// table renders through these and the tests over there cover the logic.
    #[test]
    fn the_pod_columns_compute_what_kubectl_prints() {
        let data = json!({
            "spec": { "containers": [{}, {}] },
            "status": {
                "phase": "Running",
                "conditions": [{ "type": "Ready", "status": "True" }],
                "containerStatuses": [
                    { "ready": true, "restartCount": 0, "state": { "running": {} } },
                    { "ready": true, "restartCount": 0, "state": { "running": {} } }
                ]
            }
        });

        let metadata = metadata();
        let cell = Cell {
            metadata: &metadata,
            data: &data,
            now: now(),
        };

        let values: Vec<_> = ColumnSet::for_kind("", "Pod", true)
            .columns
            .iter()
            .map(|column| column.resolve(&cell).display().to_string())
            .collect();

        assert_eq!(values, ["api-7f9", "payments", "2/2", "Running", "0", "3d"]);
    }
}
