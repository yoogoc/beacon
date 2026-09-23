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
use beacon_kube::{DeltaBatch, DynamicObject, ObjectRef, ResourceStore};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::{ActiveTheme as _, h_flex};
use gpui_kit::*;
use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use std::sync::Arc;

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
    /// What the search box contains. Empty means everything.
    filter: String,
    /// Reused across keystrokes: it owns scratch buffers, and allocating one
    /// per rebuild would be the expensive part of filtering.
    matcher: Matcher,
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
            filter: String::new(),
            matcher: crate::catalog::matcher(),
            now: Timestamp::now(),
        }
    }

    /// Applies one coalesced batch from a watch.
    pub fn apply(&mut self, batch: DeltaBatch) {
        if self.store.apply_batch(batch) {
            self.reindex();
        }
    }

    /// Narrows the table to the rows matching a query.
    ///
    /// Fuzzy, against `namespace/name`: typing part of a namespace narrows to
    /// it, and typing part of a name finds it without knowing where it lives.
    /// Returns whether anything changed, so that a repeated keystroke that
    /// resolves to the same query costs nothing.
    pub fn set_filter(&mut self, query: &str) -> bool {
        if self.filter == query {
            return false;
        }
        self.filter = query.to_string();
        self.reindex();
        true
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// How many objects the watch holds, before filtering. The table shows
    /// `len()`; this is what it is a fraction of.
    pub fn total(&self) -> usize {
        self.store.len()
    }

    /// The object a row is showing.
    pub fn key_at(&self, row: usize) -> Option<&ObjectRef> {
        self.rows.get(row)
    }

    /// Where a key sits in the current order, if it is on screen at all.
    ///
    /// `None` for an object the filter is hiding, which is why the palette
    /// clears the filter before revealing a row.
    pub fn row_of(&self, key: &ObjectRef) -> Option<usize> {
        self.rows.iter().position(|candidate| candidate == key)
    }

    pub fn object(&self, key: &ObjectRef) -> Option<&Arc<DynamicObject>> {
        self.store.get(key)
    }

    /// Every object the watch holds, unfiltered -- what the palette searches.
    pub fn keys(&self) -> Vec<ObjectRef> {
        let mut keys: Vec<ObjectRef> = self.store.iter().map(|(key, _)| key.clone()).collect();
        keys.sort();
        keys
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

    /// Swaps the columns without disturbing the rows.
    ///
    /// A kind's own printer columns arrive from the cluster a moment after its
    /// list does. Re-listing to show them would throw away rows that are
    /// already on screen and correct.
    pub fn set_columns(&mut self, columns: ColumnSet) {
        self.columns = columns;
        // A sort by column index means nothing against a different set.
        self.sort = Sort::Natural;
        self.reindex();
    }

    /// How many rows are on screen, after filtering.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
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
        let mut ranked = self.matching();
        let by_score = !self.filter.is_empty() && matches!(self.sort, Sort::Natural);

        if by_score {
            // With a fuzzy filter and no column chosen, the best match belongs
            // at the top -- that is what the filter is for.
            ranked.sort_by(|(left_score, left), (right_score, right)| {
                right_score.cmp(left_score).then_with(|| left.cmp(right))
            });
            self.rows = ranked.into_iter().map(|(_, key)| key).collect();
            return;
        }

        let mut rows: Vec<ObjectRef> = ranked.into_iter().map(|(_, key)| key).collect();

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

    /// The keys that survive the filter, each with its match score.
    ///
    /// An empty filter scores everything zero, which costs one pass and keeps
    /// the two paths through `reindex` identical in shape.
    fn matching(&mut self) -> Vec<(u32, ObjectRef)> {
        if self.filter.is_empty() {
            return self.store.iter().map(|(key, _)| (0, key.clone())).collect();
        }

        let pattern = Pattern::parse(&self.filter, CaseMatching::Smart, Normalization::Smart);
        let mut buffer = Vec::new();
        let mut matched = Vec::new();

        for (key, _) in self.store.iter() {
            let haystack = key.to_string();
            if let Some(score) =
                pattern.score(Utf32Str::new(&haystack, &mut buffer), &mut self.matcher)
            {
                matched.push((score, key.clone()));
            }
        }
        matched
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

    /// Filtering runs on every keystroke, over every object in the store. At
    /// M1's target size that is five thousand fuzzy matches between one frame
    /// and the next, so it has to stay a single pass that allocates nothing
    /// per row beyond the key it keeps.
    #[test]
    fn a_large_list_filters_on_every_keystroke() {
        let mut table = filled(5_000);

        // Typing out a name, one character at a time, the way the search box
        // delivers it.
        for query in ["p", "po", "pod", "pod-0", "pod-04", "pod-049", "pod-04990"] {
            assert!(table.set_filter(query), "{query:?} is a new query");
        }

        // Matching is fuzzy, so the survivors are not only the literal
        // substring matches -- but the best match is what leads, and that is
        // what makes the filter usable.
        assert_eq!(table.rows.first().expect("rows").name, "pod-04990");
        assert!(table.len() < 5_000, "the filter narrowed something");
        assert_eq!(table.total(), 5_000, "the store is untouched by filtering");

        assert!(
            !table.set_filter("pod-04990"),
            "the same query changes nothing"
        );

        table.set_filter("");
        assert_eq!(table.len(), 5_000);
    }

    /// A filter narrows the rows; it does not overrule a column the user chose
    /// to sort by.
    #[test]
    fn a_chosen_sort_survives_a_filter() {
        let mut table = filled(200);
        table.sort_by(4, ColumnSort::Ascending);
        table.set_filter("pod-001");

        assert!(table.len() < 200, "the filter narrowed something");

        // Restarts is `5000 - index`, so ascending by it means descending by
        // the index in the name. The filter decides which rows; the column
        // still decides their order.
        let indices: Vec<u32> = table
            .rows
            .iter()
            .map(|key| key.name.trim_start_matches("pod-").parse().expect("index"))
            .collect();
        assert!(
            indices.windows(2).all(|pair| pair[0] > pair[1]),
            "not ordered by the chosen column: {indices:?}"
        );
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
