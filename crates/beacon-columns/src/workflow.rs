//! Execution times supplement Argo's own CRD columns, including when CRD
//! discovery is unavailable to the user's Kubernetes permissions.
use crate::{ColumnDef, ColumnSet, ColumnSource, ColumnWidth, PathKind};

pub(crate) fn add_execution_times(columns: &mut ColumnSet) {
    // Replace any published copies so type: date does not render these as ages.
    columns.columns.retain(|column| {
        !column.header.eq_ignore_ascii_case("started")
            && !column.header.eq_ignore_ascii_case("finished")
            && !matches!(
                &column.source,
                ColumnSource::JsonPath { expression, .. }
                    if matches!(expression.trim().trim_matches(['{', '}']).trim(),
                        ".status.startedAt" | ".status.finishedAt")
            )
    });
    let index = columns
        .columns
        .iter()
        .position(|column| column.header.eq_ignore_ascii_case("age"))
        .unwrap_or(columns.len());
    columns.columns.splice(
        index..index,
        [
            ("STARTED", ".status.startedAt"),
            ("FINISHED", ".status.finishedAt"),
        ]
        .map(|(header, expression)| {
            ColumnDef::new(
                header,
                ColumnWidth::Fixed(210.0),
                ColumnSource::JsonPath {
                    expression: expression.into(),
                    kind: PathKind::Timestamp,
                },
            )
        }),
    );
}

#[cfg(test)]
mod tests {
    use crate::{Cell, ColumnSet};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use serde_json::json;

    #[test]
    fn workflow_times_survive_missing_crd_columns_and_preserve_published_columns() {
        assert_eq!(
            ColumnSet::for_kind("argoproj.io", "Workflow", true).headers(),
            ["Name", "Namespace", "STARTED", "FINISHED", "Age"]
        );
        let published = json!([
            {"name":"Status","type":"string","jsonPath":".status.phase"},
            {"name":"Started","type":"date","jsonPath":".status.startedAt"},
            {"name":"Completed","type":"date","jsonPath":".status.finishedAt"},
            {"name":"Age","type":"date","jsonPath":".metadata.creationTimestamp"}
        ]);
        assert_eq!(
            ColumnSet::resolve("argoproj.io", "Workflow", false, Some(&published)).headers(),
            ["Name", "Status", "STARTED", "FINISHED", "Age"]
        );
        assert_eq!(
            ColumnSet::for_kind("example.com", "Workflow", false).headers(),
            ["Name", "Age"]
        );
    }

    #[test]
    fn execution_times_normalize_offsets_and_hide_unset_times() {
        let columns = ColumnSet::for_kind("argoproj.io", "Workflow", false);
        let metadata = ObjectMeta::default();
        for (status, started, finished) in [
            (
                json!({"startedAt":"2026-10-06T19:54:46+08:00","finishedAt":"2026-10-06T12:00:00Z"}),
                "2026-10-06 11:54:46 UTC",
                "2026-10-06 12:00:00 UTC",
            ),
            (
                json!({"startedAt":"2026-10-06T11:54:46.123Z","finishedAt":"0001-01-01T00:00:00Z"}),
                "2026-10-06 11:54:46 UTC",
                "<none>",
            ),
            (json!({}), "<none>", "<none>"),
            (
                json!({"startedAt":"","finishedAt":null}),
                "<none>",
                "<none>",
            ),
        ] {
            let data = json!({"status":status});
            let cell = Cell {
                metadata: &metadata,
                data: &data,
                now: "2026-10-06T12:00:00Z".parse().unwrap(),
                usage: None,
            };
            assert_eq!(columns.columns[1].resolve(&cell).display(), started);
            assert_eq!(columns.columns[2].resolve(&cell).display(), finished);
        }
    }
}
