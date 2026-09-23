//! The resource table.
//!
//! One table renders any kind: it holds a [`ColumnSet`] and a
//! [`ResourceStore`], and knows nothing about Pods. What a column contains is
//! the column's problem; what this does is keep an ordered index of the store
//! and paint the rows that are on screen.
//!
//! The index is the point. The store is a map, the table needs an order, and
//! rebuilding that order is the only work here that scales with cluster size.
//! It happens once per incoming batch -- which the watch already limits to one
//! per frame -- and never per row.

use beacon_columns::{Cell, CellValue, ColumnDef, ColumnSet, ColumnSource, ColumnWidth, Timestamp};
use beacon_kube::{DeltaBatch, ObjectRef, ResourceStore};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::*;

use crate::status;
use crate::theme::BeaconTheme as _;

/// A `Flex` column's share, converted to the starting pixel width the table
/// component wants. Columns are resizable afterwards, so this only has to be a
/// reasonable first guess at a normal window width.
const FLEX_UNIT: f32 = 140.0;

pub struct ResourceTable {
    columns: ColumnSet,
    store: ResourceStore,
    /// The store in display order. Rebuilt whenever the store or the sort
    /// changes; everything else reads it.
    rows: Vec<ObjectRef>,
    sort: Sort,
    /// What "now" means for the whole frame, so that every Age in a render
    /// agrees and so that tests can pin it.
    now: Timestamp,
}

/// How the rows are ordered.
enum Sort {
    /// Namespace, then name -- what `kubectl get -A` prints, and the order
    /// somebody scanning for a name expects.
    Natural,
    ByColumn {
        index: usize,
        descending: bool,
    },
}

impl ResourceTable {
    pub fn new(columns: ColumnSet) -> Self {
        Self {
            columns,
            store: ResourceStore::new(),
            rows: Vec::new(),
            sort: Sort::Natural,
            now: Timestamp::now(),
        }
    }

    /// Applies one coalesced batch from a watch.
    pub fn apply(&mut self, batch: DeltaBatch) {
        if self.store.apply_batch(batch) {
            self.reindex();
        }
    }

    /// Re-reads the clock. Ages are relative, so a table nobody is changing
    /// still has to be repainted for them to advance.
    pub fn tick(&mut self) {
        self.now = Timestamp::now();
    }

    /// Points the table at a different list: new columns, nothing in the
    /// store, and back to the natural order.
    ///
    /// Keeping the old rows visible while the new watch lists would show one
    /// namespace's pods under another namespace's heading.
    pub fn reset(&mut self, columns: ColumnSet) {
        self.columns = columns;
        self.store = ResourceStore::new();
        self.rows.clear();
        self.sort = Sort::Natural;
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Reorders by a column, or back to the natural order with
    /// [`ColumnSort::Default`].
    pub fn sort_by(&mut self, index: usize, sort: ColumnSort) {
        self.sort = match sort {
            ColumnSort::Ascending => Sort::ByColumn {
                index,
                descending: false,
            },
            ColumnSort::Descending => Sort::ByColumn {
                index,
                descending: true,
            },
            ColumnSort::Default => Sort::Natural,
        };
        self.reindex();
    }

    /// Rebuilds the display order.
    ///
    /// Sorting by a column means resolving that column for every object, which
    /// for a computed column is real work. It is bounded to once per batch, and
    /// the default order costs nothing because it sorts the keys themselves.
    fn reindex(&mut self) {
        let mut rows: Vec<ObjectRef> = self.store.iter().map(|(key, _)| key.clone()).collect();

        match self.sort {
            Sort::Natural => rows.sort(),
            Sort::ByColumn { index, descending } => {
                let Some(column) = self.columns.columns.get(index) else {
                    rows.sort();
                    self.rows = rows;
                    return;
                };

                let mut keyed: Vec<(SortKey, ObjectRef)> = rows
                    .into_iter()
                    .map(|key| (self.sort_key(column, &key), key))
                    .collect();

                // Ties broken by the natural order, so that a sort on a column
                // where many rows are equal -- every pod Running -- still
                // produces a stable, readable list rather than hash order.
                keyed.sort_by(|(left_key, left), (right_key, right)| {
                    left_key.cmp(right_key).then_with(|| left.cmp(right))
                });
                if descending {
                    keyed.reverse();
                }

                rows = keyed.into_iter().map(|(_, key)| key).collect();
            }
        }

        self.rows = rows;
    }

    fn sort_key(&self, column: &ColumnDef, key: &ObjectRef) -> SortKey {
        let Some(object) = self.store.get(key) else {
            return SortKey::Missing;
        };

        // Age sorts by the timestamp behind it. Sorting "3d" against "38d" as
        // text puts them next to each other and in the wrong order.
        if matches!(column.source, ColumnSource::Age) {
            return match &object.metadata.creation_timestamp {
                Some(created) => SortKey::Number(created.0.as_second()),
                None => SortKey::Missing,
            };
        }

        SortKey::of(&column.resolve(&Cell {
            metadata: &object.metadata,
            data: &object.data,
            now: self.now,
        }))
    }

    fn cell(&self, row: usize, column: usize) -> Option<(&ColumnDef, CellValue)> {
        let column = self.columns.columns.get(column)?;
        let object = self.store.get(self.rows.get(row)?)?;

        Some((
            column,
            column.resolve(&Cell {
                metadata: &object.metadata,
                data: &object.data,
                now: self.now,
            }),
        ))
    }
}

/// What a cell sorts by.
///
/// Cells are strings, but plenty of them are numbers wearing a suffix --
/// `5 (8d ago)`, `2/3`. Sorting those as text puts `10` before `5`, which reads
/// as a broken sort rather than as a subtle one.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SortKey {
    /// Whole numbers only -- a leading run of digits, or a timestamp in
    /// seconds. Nothing a cell contains needs a fraction, and integers keep
    /// this totally ordered.
    Number(i64),
    Text(String),
    /// Absent values sort after everything else, so an ascending sort leads
    /// with the rows that have something to show.
    Missing,
}

impl SortKey {
    fn of(value: &CellValue) -> Self {
        match value {
            CellValue::Missing => Self::Missing,
            CellValue::Text(text) => {
                // A number long enough to overflow is not a number anybody is
                // sorting by; let it fall through to a text comparison.
                let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
                match digits.parse::<i64>() {
                    Ok(number) => Self::Number(number),
                    Err(_) => Self::Text(text.to_lowercase()),
                }
            }
        }
    }
}

impl TableDelegate for ResourceTable {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, index: usize, _: &App) -> Column {
        let Some(definition) = self.columns.columns.get(index) else {
            return Column::new("", "");
        };

        let width = match definition.width {
            ColumnWidth::Fixed(pixels) => pixels,
            ColumnWidth::Flex(share) => share * FLEX_UNIT,
        };

        let sort = match self.sort {
            Sort::ByColumn {
                index: sorted,
                descending,
            } if sorted == index => {
                if descending {
                    ColumnSort::Descending
                } else {
                    ColumnSort::Ascending
                }
            }
            _ => ColumnSort::Default,
        };

        Column::new(definition.header.to_lowercase(), definition.header.clone())
            .width(px(width))
            .min_width(px(56.))
            .sort(sort)
    }

    fn perform_sort(
        &mut self,
        index: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.sort_by(index, sort);
        cx.notify();
    }

    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some((definition, value)) = self.cell(row, column) else {
            return h_flex();
        };

        let color = if value.is_missing() {
            cx.theme().muted_foreground
        } else if definition.header == "Status" {
            cx.theme().tone(status::tone(value.display()))
        } else if definition.header == "Name" {
            cx.theme().foreground
        } else {
            cx.theme().muted_foreground
        };

        h_flex()
            .size_full()
            .items_center()
            .text_color(color)
            .child(value.display().to_string())
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .items_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Nothing here")
    }

    /// The table's own keyboard navigation and copy support read cells through
    /// this, so it has to produce the same text the row shows.
    fn cell_text(&self, row: usize, column: usize, _: &App) -> String {
        self.cell(row, column)
            .map(|(_, value)| value.display().to_string())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    // Imported by name rather than with `use super::*`: this module globs
    // `gpui_kit`, whose test-support build exports its own `#[test]`, and a
    // glob would shadow Rust's.
    use super::{ResourceTable, SortKey};
    use beacon_columns::{CellValue, ColumnSet};
    use beacon_kube::{Delta, DynamicObject, resources};
    use gpui_kit::component::table::ColumnSort;
    use std::sync::Arc;

    fn key(value: &str) -> SortKey {
        SortKey::of(&CellValue::text(value))
    }

    /// `10` after `5`, not before it.
    #[test]
    fn numbers_in_a_cell_sort_as_numbers() {
        assert!(key("5") < key("10"));
        assert!(key("5 (8d ago)") < key("10 (2m ago)"));
        assert!(key("0/2") < key("2/2"));
    }

    #[test]
    fn text_sorts_case_insensitively() {
        assert!(key("apache") < key("Zookeeper"));
        assert_eq!(key("Redis"), key("redis"));
    }

    /// A cell with nothing in it should not push its way to the top of a sort.
    #[test]
    fn missing_values_sort_last() {
        assert!(key("anything") < SortKey::Missing);
        assert!(key("0") < SortKey::Missing);
    }

    /// One pod per namespace-and-index, with a restart count that is
    /// deliberately out of step with the name so that sorting by it has to
    /// actually move rows.
    fn pod(index: usize) -> Arc<DynamicObject> {
        let namespace = format!("ns-{:02}", index % 20);
        let mut object = DynamicObject::new(&format!("pod-{index:05}"), &resources::pod())
            .within(&namespace)
            .data(serde_json::json!({
                "spec": { "containers": [{}] },
                "status": {
                    "phase": "Running",
                    "containerStatuses": [{
                        "ready": true,
                        "restartCount": 5000 - index,
                        "state": { "running": {} }
                    }]
                }
            }));
        object.metadata.resource_version = Some("1".into());
        Arc::new(object)
    }

    fn filled(count: usize) -> ResourceTable {
        let mut table = ResourceTable::new(ColumnSet::for_kind("", "Pod", true));
        table.apply(vec![Delta::Reset((0..count).map(pod).collect())]);
        table
    }

    /// The natural order is what `kubectl get -A` prints: namespace, then name.
    #[test]
    fn the_default_order_is_namespace_then_name() {
        let table = filled(40);
        let first = table.rows.first().expect("rows");
        let second = &table.rows[1];

        assert_eq!(first.namespace.as_deref(), Some("ns-00"));
        assert_eq!(first.name, "pod-00000");
        assert_eq!(second.name, "pod-00020", "same namespace, next name");
    }

    /// Sorting by a computed column resolves that column for every object, so
    /// this is the case that scales worst. M1 targets a 5,000-object list; the
    /// order has to be right at that size and the rebuild has to stay a single
    /// pass rather than resolving cells inside the comparator.
    #[test]
    fn a_large_list_sorts_by_a_computed_column() {
        let mut table = filled(5_000);
        assert_eq!(table.len(), 5_000);

        // Column 4 is Restarts, which is `5000 - index`.
        table.sort_by(4, ColumnSort::Ascending);
        assert_eq!(table.rows.first().expect("rows").name, "pod-04999");
        assert_eq!(table.rows.last().expect("rows").name, "pod-00000");

        table.sort_by(4, ColumnSort::Descending);
        assert_eq!(table.rows.first().expect("rows").name, "pod-00000");

        table.sort_by(4, ColumnSort::Default);
        assert_eq!(table.rows.first().expect("rows").name, "pod-00000");
        assert_eq!(
            table.rows.first().expect("rows").namespace.as_deref(),
            Some("ns-00")
        );
    }
}
