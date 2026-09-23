//! Columns a CRD declares for itself.
//!
//! `additionalPrinterColumns` is the reason Beacon can list a resource nobody
//! wrote code for: the CRD says what its own table should look like, and this
//! turns that declaration into a [`ColumnSet`].
//!
//! Two details of it are not obvious and both are visible immediately if you
//! get them wrong. Columns carry a `priority`, and everything above zero is
//! `-o wide` material that kubectl hides by default. And a column of
//! `type: date` is not a timestamp on screen: the apiextensions server converts
//! those to a duration before kubectl ever sees them, which is why a CRD's own
//! `Age` column looks like every other `Age` column.

use serde_json::Value;

use crate::{ColumnDef, ColumnSet, ColumnSource, ColumnWidth, PathKind};

/// Columns whose `priority` is above this are shown only in a wide listing,
/// which Beacon does not have yet.
const NORMAL_PRIORITY: i64 = 0;

/// Builds a column set from a CRD's `additionalPrinterColumns` array.
///
/// `None` when the CRD declares nothing worth showing, in which case the caller
/// falls back to Name / Namespace / Age.
pub fn column_set(printer_columns: &Value, namespaced: bool) -> Option<ColumnSet> {
    let declared: Vec<ColumnDef> = printer_columns
        .as_array()?
        .iter()
        .filter_map(column)
        .collect();

    if declared.is_empty() {
        return None;
    }

    // Name always comes first and is never declared; the CRD's columns are
    // additional to it, which is what `additionalPrinterColumns` means.
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

    columns.extend(declared);
    Some(ColumnSet { columns })
}

fn column(declaration: &Value) -> Option<ColumnDef> {
    let name = declaration.get("name")?.as_str()?;
    let expression = declaration.get("jsonPath")?.as_str()?;

    let priority = declaration
        .get("priority")
        .and_then(Value::as_i64)
        .unwrap_or(NORMAL_PRIORITY);
    if priority > NORMAL_PRIORITY {
        return None;
    }

    let declared_type = declaration
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("string");

    let kind = if declared_type == "date" {
        PathKind::Date
    } else {
        PathKind::Value
    };

    Some(ColumnDef::new(
        name,
        width_for(declared_type),
        ColumnSource::JsonPath {
            expression: expression.to_string(),
            kind,
        },
    ))
}

/// A first guess at how much room a column needs, from the only thing the CRD
/// tells us about it. Columns are resizable, so this only has to be sane.
fn width_for(declared_type: &str) -> ColumnWidth {
    match declared_type {
        "integer" | "number" => ColumnWidth::Fixed(90.0),
        "boolean" => ColumnWidth::Fixed(80.0),
        "date" => ColumnWidth::Fixed(80.0),
        _ => ColumnWidth::Flex(1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Taken verbatim from `applications.argoproj.io`, priorities included.
    fn argo_application() -> Value {
        json!([
            { "name": "Sync Status", "type": "string", "jsonPath": ".status.sync.status" },
            { "name": "Health Status", "type": "string", "jsonPath": ".status.health.status" },
            { "name": "Revision", "type": "string", "jsonPath": ".status.sync.revision",
              "priority": 10 },
            { "name": "Project", "type": "string", "jsonPath": ".spec.project", "priority": 10 }
        ])
    }

    fn headers(columns: &ColumnSet) -> Vec<&str> {
        columns.headers()
    }

    #[test]
    fn a_crds_columns_come_after_name() {
        let columns = column_set(&argo_application(), true).expect("columns");
        assert_eq!(
            headers(&columns),
            ["Name", "Namespace", "Sync Status", "Health Status"]
        );
    }

    /// `kubectl get applications` shows four columns, not six. Revision and
    /// Project are `-o wide` material.
    #[test]
    fn wide_only_columns_are_hidden() {
        let columns = column_set(&argo_application(), false).expect("columns");
        assert_eq!(headers(&columns), ["Name", "Sync Status", "Health Status"]);
    }

    #[test]
    fn a_cluster_scoped_crd_has_no_namespace_column() {
        let columns = column_set(&argo_application(), false).expect("columns");
        assert!(!headers(&columns).contains(&"Namespace"));
    }

    /// A `date` column is a duration on screen, not a timestamp -- the
    /// apiextensions server converts it before kubectl sees it, and a CRD's own
    /// Age column has to look like everyone else's.
    #[test]
    fn a_date_column_is_rendered_as_an_age() {
        let columns = column_set(
            &json!([{ "name": "Age", "type": "date",
                      "jsonPath": ".metadata.creationTimestamp" }]),
            false,
        )
        .expect("columns");

        let age = columns.columns.last().expect("age column");
        assert!(matches!(
            age.source,
            ColumnSource::JsonPath {
                kind: PathKind::Date,
                ..
            }
        ));
    }

    /// A CRD that declares nothing gets Beacon's fallback rather than a column
    /// set that is only a Name.
    #[test]
    fn nothing_declared_means_no_column_set() {
        assert!(column_set(&json!([]), true).is_none());
        assert!(column_set(&Value::Null, true).is_none());
        assert!(column_set(&json!("nonsense"), true).is_none());
    }

    /// Every declared column is wide-only: there is nothing to add to Name, so
    /// the fallback is a better answer than a one-column table.
    #[test]
    fn a_crd_whose_columns_are_all_wide_gets_the_fallback() {
        let all_wide = json!([
            { "name": "Revision", "type": "string", "jsonPath": ".status.rev", "priority": 10 }
        ]);
        assert!(column_set(&all_wide, true).is_none());
    }

    /// A malformed entry is skipped rather than taking the whole table down
    /// with it. CRDs are written by hand and this happens.
    #[test]
    fn a_malformed_column_is_skipped() {
        let columns = column_set(
            &json!([
                { "name": "Good", "type": "string", "jsonPath": ".spec.good" },
                { "name": "No path", "type": "string" },
                { "jsonPath": ".spec.nameless" },
                "not an object"
            ]),
            false,
        )
        .expect("columns");

        assert_eq!(headers(&columns), ["Name", "Good"]);
    }

    /// `type` is required by the CRD schema but absent CRDs exist; treating it
    /// as a string shows the value rather than nothing.
    #[test]
    fn a_column_without_a_type_is_text() {
        let columns = column_set(
            &json!([{ "name": "Source", "jsonPath": ".spec.source" }]),
            false,
        )
        .expect("columns");
        assert_eq!(headers(&columns), ["Name", "Source"]);
    }
}
