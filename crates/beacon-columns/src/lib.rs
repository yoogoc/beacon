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
pub mod builtin;
pub mod event;
pub mod path;
pub mod pod;
pub mod printer;

pub use age::{format_age, format_duration};
pub use event::EventSummary;
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

impl From<Option<&str>> for CellValue {
    fn from(value: Option<&str>) -> Self {
        value.map(Self::text).unwrap_or(Self::Missing)
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

impl CellValue {
    /// Renders what a field path selected.
    ///
    /// A path is multi-valued -- `.status.addresses[*].value` is every address
    /// -- and kubectl prints all of them, comma separated.
    pub fn from_nodes(nodes: &[&Value]) -> Self {
        match nodes {
            [] => Self::Missing,
            [single] => Self::from(*single),
            many => {
                let joined: Vec<_> = many
                    .iter()
                    .map(|node| Self::from(*node).display().to_string())
                    .collect();
                Self::Text(joined.join(","))
            }
        }
    }

    /// Renders a timestamp the way kubectl renders a `type: date` column: as
    /// how long ago it was, not as the timestamp itself.
    pub fn from_date(nodes: &[&Value], now: Timestamp) -> Self {
        let Some(Value::String(text)) = nodes.first().copied() else {
            return Self::from_nodes(nodes);
        };
        match text.parse::<Timestamp>() {
            Ok(timestamp) => Self::text(format_duration(now.duration_since(timestamp).as_secs())),
            // Not a timestamp after all. The CRD said it was one, but showing
            // what is actually there beats showing nothing.
            Err(_) => Self::text(text.clone()),
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
    JsonPath {
        expression: String,
        kind: PathKind,
    },
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
            Self::JsonPath { expression, kind } => {
                write!(f, "JsonPath({expression:?}, {kind:?})")
            }
            Self::Computed(_) => f.write_str("Computed(..)"),
        }
    }
}

/// How a field path's value is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    /// Whatever is there, as text.
    Value,
    /// A CRD's `type: date` column. kubectl prints these as how long ago the
    /// timestamp was, which is why a CRD's own `Age` column looks like every
    /// other Age column.
    Date,
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
            ColumnSource::JsonPath { expression, kind } => {
                Self::resolve_path(expression, *kind, cell)
            }
            ColumnSource::Computed(compute) => compute(cell),
        }
    }

    fn resolve_path(expression: &str, kind: PathKind, cell: &Cell<'_>) -> CellValue {
        // `data` is the object without its metadata, so a path rooted at
        // `metadata` has to be answered from the typed struct instead. That is
        // rare enough to be worth a serialization when it happens -- and CRDs
        // do write `.metadata.creationTimestamp` for their Age column -- and
        // nothing at all when it does not.
        let rebuilt;
        let root = match path::root(expression).as_deref() {
            Some("metadata") => {
                rebuilt = serde_json::json!({ "metadata": cell.metadata });
                &rebuilt
            }
            _ => cell.data,
        };

        let nodes = path::evaluate(expression, root);
        match kind {
            PathKind::Value => CellValue::from_nodes(&nodes),
            PathKind::Date => CellValue::from_date(&nodes, cell.now),
        }
    }
}

/// The columns for one resource kind.
#[derive(Debug, Clone)]
pub struct ColumnSet {
    pub columns: Vec<ColumnDef>,
}

impl ColumnSet {
    /// The columns for a kind, resolved in the order the design calls for: the
    /// built-in table kubectl has compiled in, then the CRD's own
    /// `additionalPrinterColumns`, then a fallback of Name / Namespace / Age.
    ///
    /// Only the first and last are code. The middle one is data the cluster
    /// published, which is why a CRD installed this morning lists correctly
    /// this afternoon.
    ///
    /// Beacon diverges from kubectl in one place here. A CRD that declares no
    /// columns at all gets `Age` rather than kubectl's `Created At`, which
    /// prints a raw timestamp; in a window next to fifteen other resource
    /// kinds, an age that matches all of them is worth more than the exact
    /// string.
    pub fn resolve(
        group: &str,
        kind: &str,
        namespaced: bool,
        printer_columns: Option<&Value>,
    ) -> Self {
        builtin::column_set(group, kind, namespaced)
            .or_else(|| printer::column_set(printer_columns?, namespaced))
            .unwrap_or_else(|| Self::fallback(namespaced))
    }

    /// [`ColumnSet::resolve`] for a kind that published no printer columns.
    pub fn for_kind(group: &str, kind: &str, namespaced: bool) -> Self {
        Self::resolve(group, kind, namespaced, None)
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

    fn json_path(expression: &str) -> ColumnSource {
        ColumnSource::JsonPath {
            expression: expression.to_string(),
            kind: PathKind::Value,
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
        assert_eq!(CellValue::from(None::<String>).display(), "<none>");
        assert_eq!(CellValue::from(None::<&str>).display(), "<none>");
        assert_eq!(
            CellValue::from(Some("kube-system")).display(),
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
            resolve(json_path(".status.phase"), &data).display(),
            "Running"
        );
        assert_eq!(resolve(json_path(".spec.replicas"), &data).display(), "3");
    }

    /// `data` holds no metadata, so a path rooted there has to be answered from
    /// the typed struct -- CRDs do write `.metadata.labels.*` columns.
    #[test]
    fn a_field_path_can_still_reach_metadata() {
        let data = json!({});
        assert_eq!(
            resolve(json_path(".metadata.name"), &data).display(),
            "api-7f9"
        );
    }

    #[test]
    fn a_path_that_resolves_to_nothing_renders_as_none() {
        let data = json!({ "status": {} });
        assert!(resolve(json_path(".status.phase"), &data).is_missing());
        assert!(resolve(json_path(".spec.replicas"), &data).is_missing());
    }

    /// An empty string is a present-but-blank field. kubectl shows `<none>`
    /// rather than a cell that looks like a rendering failure.
    #[test]
    fn an_empty_string_is_a_missing_value() {
        let data = json!({ "status": { "phase": "" } });
        assert!(resolve(json_path(".status.phase"), &data).is_missing());
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

    /// The resolution order, in one test: a kind we know beats the CRD's own
    /// columns, a CRD's columns beat the fallback, and the fallback is what is
    /// left.
    #[test]
    fn columns_resolve_builtin_then_crd_then_fallback() {
        let declared = json!([
            { "name": "Source", "type": "string", "jsonPath": ".spec.source" }
        ]);

        assert_eq!(
            ColumnSet::resolve("", "Pod", true, Some(&declared)).headers(),
            ["Name", "Namespace", "Ready", "Status", "Restarts", "Age"],
            "a built-in table wins over whatever else is published"
        );

        assert_eq!(
            ColumnSet::resolve("k3s.cattle.io", "Addon", true, Some(&declared)).headers(),
            ["Name", "Namespace", "Source"],
            "a CRD's own columns beat the fallback"
        );

        assert_eq!(
            ColumnSet::resolve("hub.traefik.io", "ApiAccess", true, None).headers(),
            ["Name", "Namespace", "Age"],
            "and the fallback is what is left"
        );
    }

    /// A CRD that happens to be called Pod is not a Pod.
    #[test]
    fn the_builtin_table_is_keyed_on_the_group_too() {
        assert_eq!(
            ColumnSet::for_kind("example.com", "Pod", true).headers(),
            ["Name", "Namespace", "Age"]
        );
    }

    /// The Pod columns have to agree with `pod::summarize` end to end, since
    /// the table renders through these.
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
