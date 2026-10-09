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

use beacon_columns::{
    Cell, CellValue, ColumnDef, ColumnSet, ColumnSource, ColumnWidth, PathKind, Timestamp, Usage,
};
use beacon_kube::labels::LabelSelector;
use beacon_kube::{
    DeleteTarget, Delta, DeltaBatch, DynamicObject, Metrics, ObjectRef, ResourceStore, data,
};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use crate::actions;
use crate::cluster::ClusterView;
use crate::detail::DetailTab;
use crate::filters::Field;
use crate::pod_tools::PodToolTab;
use crate::status;
use crate::table_preferences::{ColumnLayout, ColumnSort as SavedSort, FilterPreset};
use crate::theme::{BeaconTheme as _, Tone};

/// A `Flex` column's share, converted to the starting pixel width the table
/// component wants. Columns are resizable afterwards, so this only has to be a
/// reasonable first guess at a normal window width.
const FLEX_UNIT: f32 = 140.0;

pub struct ResourceTable {
    context_view: Option<WeakEntity<ClusterView>>,
    selection_column: bool,
    columns: ColumnSet,
    column_ids: Vec<String>,
    display_columns: Vec<usize>,
    layout: ColumnLayout,
    store: ResourceStore,
    /// The store in display order. Rebuilt whenever the store or the sort
    /// changes; everything else reads it.
    rows: Vec<ObjectRef>,
    /// Checked rows and the exact UIDs they referred to when checked.
    checked: BTreeMap<ObjectRef, String>,
    sort: Sort,
    /// What the search box contains. Empty means everything.
    filter: String,
    /// Exact facets combine with each other and the fuzzy name search.
    field_filters: BTreeMap<Field, String>,
    label_filter: String,
    label_selector: LabelSelector,
    /// Reused across keystrokes: it owns scratch buffers, and allocating one
    /// per rebuild would be the expensive part of filtering.
    matcher: Matcher,
    /// What "now" means for the whole frame, so that every Age in a render
    /// agrees and so that tests can pin it.
    now: Timestamp,
    /// Whether the first list is still on its way.
    ///
    /// An empty table and a table that has not been filled yet look identical,
    /// and the difference matters: one means "this kind has nothing in it" and
    /// the other means "wait".
    loading: bool,
    /// CPU and memory, refreshed on its own timer. Empty on a cluster with no
    /// metrics-server, which the columns render as `<none>`.
    metrics: Metrics,
}

/// How the rows are ordered.
#[derive(Clone)]
enum Sort {
    /// Namespace, then name -- what `kubectl get -A` prints, and the order
    /// somebody scanning for a name expects.
    Natural,
    ByColumn {
        index: usize,
        descending: bool,
    },
}

#[derive(Clone)]
pub(crate) struct ViewFilters {
    sort: Sort,
    filter: String,
    fields: BTreeMap<Field, String>,
    labels: LabelSelector,
    layout: ColumnLayout,
}

pub(crate) struct TableLayoutChanged(pub ColumnLayout);
impl EventEmitter<TableLayoutChanged> for TableState<ResourceTable> {}

fn column_ids(columns: &ColumnSet) -> Vec<String> {
    let mut occurrences = BTreeMap::<String, usize>::new();
    columns
        .columns
        .iter()
        .map(|column| {
            let key = format!("{}:{:?}", column.header, column.source);
            let occurrence = occurrences.entry(key.clone()).or_default();
            let id = format!("{key}:{occurrence}");
            *occurrence += 1;
            id
        })
        .collect()
}

impl ResourceTable {
    pub(crate) fn view_filters(&self) -> ViewFilters {
        ViewFilters {
            sort: self.sort.clone(),
            filter: self.filter.clone(),
            fields: self.field_filters.clone(),
            labels: self.label_selector.clone(),
            layout: self.layout_snapshot(),
        }
    }

    pub(crate) fn restore_filters(&mut self, filters: ViewFilters) {
        self.sort = filters.sort;
        self.filter = filters.filter;
        self.field_filters = filters.fields;
        self.label_selector = filters.labels;
        self.label_filter = self.label_selector.to_string();
        self.apply_layout(filters.layout);
        self.reindex();
    }
    pub fn new(columns: ColumnSet) -> Self {
        let column_ids = column_ids(&columns);
        let display_columns = (0..columns.len()).collect();
        Self {
            context_view: None,
            selection_column: true,
            columns,
            column_ids,
            display_columns,
            layout: ColumnLayout::default(),
            store: ResourceStore::new(),
            rows: Vec::new(),
            checked: BTreeMap::new(),
            sort: Sort::Natural,
            filter: String::new(),
            field_filters: BTreeMap::new(),
            label_filter: String::new(),
            label_selector: LabelSelector::default(),
            matcher: crate::catalog::matcher(),
            now: Timestamp::now(),
            metrics: Metrics::default(),
            loading: false,
        }
    }

    fn ordered_columns(&self) -> Vec<usize> {
        let mut columns: Vec<_> = (0..self.columns.len()).collect();
        columns.sort_by_key(|index| {
            self.layout
                .order
                .iter()
                .position(|id| id == &self.column_ids[*index])
                .unwrap_or(self.layout.order.len() + *index)
        });
        columns
    }

    fn rebuild_display_columns(&mut self) {
        self.display_columns = self
            .ordered_columns()
            .into_iter()
            .filter(|index| {
                matches!(self.columns.columns[*index].source, ColumnSource::Name)
                    || !self.layout.hidden.contains(&self.column_ids[*index])
            })
            .collect();
        self.sort = self
            .layout
            .sort
            .as_ref()
            .and_then(|sort| {
                self.column_ids
                    .iter()
                    .position(|id| id == &sort.column)
                    .map(|index| Sort::ByColumn {
                        index,
                        descending: sort.descending,
                    })
            })
            .unwrap_or(Sort::Natural);
    }

    pub(crate) fn apply_layout(&mut self, layout: ColumnLayout) {
        self.layout = layout;
        self.rebuild_display_columns();
        self.reindex();
    }

    pub(crate) fn layout_snapshot(&self) -> ColumnLayout {
        let mut layout = self.layout.clone();
        for id in &self.column_ids {
            if !layout.order.contains(id) {
                layout.order.push(id.clone());
            }
        }
        layout.sort = match self.sort {
            Sort::Natural => None,
            Sort::ByColumn { index, descending } => {
                self.column_ids.get(index).map(|column| SavedSort {
                    column: column.clone(),
                    descending,
                })
            }
        };
        layout
    }

    pub(crate) fn column_choices(&self) -> Vec<(String, String, bool, bool)> {
        self.ordered_columns()
            .into_iter()
            .map(|index| {
                (
                    self.column_ids[index].clone(),
                    self.columns.columns[index].header.clone(),
                    self.display_columns.contains(&index),
                    matches!(self.columns.columns[index].source, ColumnSource::Name),
                )
            })
            .collect()
    }

    pub(crate) fn toggle_column(&mut self, id: &str) {
        let Some(index) = self.column_ids.iter().position(|column| column == id) else {
            return;
        };
        if matches!(self.columns.columns[index].source, ColumnSource::Name) {
            return;
        }
        self.layout = self.layout_snapshot();
        if !self.layout.hidden.remove(id) {
            self.layout.hidden.insert(id.to_owned());
        }
        self.rebuild_display_columns();
    }

    pub(crate) fn update_widths(&mut self, widths: &[Pixels]) {
        for (index, width) in self
            .display_columns
            .iter()
            .zip(widths.iter().skip(usize::from(self.selection_column)))
        {
            self.layout.widths.insert(
                self.column_ids[*index].clone(),
                f32::from(*width).clamp(56., 4000.),
            );
        }
    }

    pub(crate) fn filter_preset(&self, namespaces: BTreeSet<String>) -> FilterPreset {
        FilterPreset {
            namespaces,
            search: self.filter.clone(),
            labels: self.label_filter.clone(),
            fields: self.field_filters.clone(),
            sort: self.layout_snapshot().sort,
        }
    }

    pub(crate) fn apply_preset(&mut self, preset: &FilterPreset) -> Result<(), String> {
        let labels = LabelSelector::parse(&preset.labels).map_err(|error| error.to_string())?;
        self.filter = preset.search.clone();
        self.field_filters = preset.fields.clone();
        self.label_selector = labels;
        self.label_filter = preset.labels.clone();
        self.checked.clear();
        self.layout.sort = preset.sort.clone();
        self.rebuild_display_columns();
        self.reindex();
        Ok(())
    }

    fn data_column(&self, index: usize) -> Option<usize> {
        self.display_columns
            .get(index.checked_sub(usize::from(self.selection_column))?)
            .copied()
    }

    fn display_cell_text(&self, row: usize, column: usize) -> String {
        self.data_column(column)
            .and_then(|column| self.cell(row, column))
            .map(|(_, value)| value.display().to_string())
            .unwrap_or_default()
    }

    fn reorder_column(&mut self, from: usize, to: usize) {
        let (Some(from_index), Some(to_index)) = (self.data_column(from), self.data_column(to))
        else {
            return;
        };
        let mut layout = self.layout_snapshot();
        let id = self.column_ids[from_index].clone();
        let destination = self.column_ids[to_index].clone();
        layout.order.retain(|column| column != &id);
        if let Some(position) = layout
            .order
            .iter()
            .position(|column| column == &destination)
        {
            layout.order.insert(position + usize::from(from < to), id);
        }
        self.layout = layout;
        self.rebuild_display_columns();
    }

    /// Related-resource lists use row navigation without batch selection.
    pub(crate) fn without_selection(mut self) -> Self {
        self.selection_column = false;
        self
    }

    /// The containing cluster handles actions on the row under the pointer.
    pub fn set_context_view(&mut self, view: WeakEntity<ClusterView>) {
        self.context_view = Some(view);
    }

    /// Whether the first list is still on its way.
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Says the first list has arrived. Returns whether that was news.
    pub fn finish_loading(&mut self) -> bool {
        std::mem::replace(&mut self.loading, false)
    }

    /// Replaces the usage figures. Returns whether anything changed, so a
    /// cluster without metrics-server costs one comparison and no renders.
    pub fn set_metrics(&mut self, metrics: Metrics) -> bool {
        if metrics.is_empty() && self.metrics.is_empty() {
            return false;
        }
        self.metrics = metrics;
        true
    }

    /// What one object is using, in the shape a column reads.
    fn usage(&self, key: &ObjectRef) -> Option<Usage> {
        self.metrics
            .get(key.namespace.as_deref(), &key.name)
            .map(|usage| Usage {
                cpu_millis: usage.cpu_millis,
                memory_bytes: usage.memory_bytes,
            })
    }

    /// Applies one coalesced batch from a watch.
    pub fn apply(&mut self, batch: DeltaBatch) {
        if self.store.apply_batch(batch) {
            self.reindex();
        }
    }

    /// Applies a batch that came from the watch on one namespace.
    ///
    /// `Delta::Reset` means "replace everything", which is exactly right when
    /// one watch owns the table and exactly wrong when several do: the second
    /// namespace to finish listing would wipe the first. So a Reset is
    /// narrowed to the namespace it came from -- remove what the table holds
    /// for that namespace, then add the snapshot -- and the other namespaces
    /// are left alone.
    pub fn apply_from(&mut self, namespace: Option<&str>, batch: DeltaBatch) {
        let Some(namespace) = namespace else {
            // A cluster-wide watch is the only one feeding the table, so
            // Reset can keep meaning what it says.
            self.apply(batch);
            return;
        };

        let mut expanded = Vec::with_capacity(batch.len());
        for delta in batch {
            match delta {
                Delta::Reset(objects) => {
                    let stale: Vec<ObjectRef> = self
                        .store
                        .iter()
                        .filter(|(key, _)| key.namespace.as_deref() == Some(namespace))
                        .map(|(key, _)| key.clone())
                        .collect();
                    expanded.extend(stale.into_iter().map(Delta::Remove));
                    expanded.extend(objects.into_iter().map(Delta::Upsert));
                }
                delta => expanded.push(delta),
            }
        }

        self.apply(expanded);
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

    pub(crate) fn label_filter(&self) -> &str {
        &self.label_filter
    }

    pub(crate) fn label_selector(&self) -> &LabelSelector {
        &self.label_selector
    }

    /// Parse first: invalid drafts must never broaden the visible selection.
    pub(crate) fn set_label_filter(&mut self, query: &str) -> Result<bool, String> {
        let selector = LabelSelector::parse(query)?;
        let query = selector.to_string();
        if self.label_filter == query {
            return Ok(false);
        }
        self.label_filter = query;
        self.label_selector = selector;
        self.reindex();
        Ok(true)
    }

    /// Suggestions include labels on rows hidden by any active filter.
    pub(crate) fn label_values(&self) -> Vec<(String, String)> {
        self.store
            .iter()
            .flat_map(|(_, object)| {
                object
                    .metadata
                    .labels
                    .iter()
                    .flat_map(|labels| labels.iter())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }

    pub(crate) fn filter_values(&self, field: Field) -> Vec<String> {
        self.store
            .iter()
            .flat_map(|(_, object)| field.values(object, self.now))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn field_filter(&self, field: Field) -> Option<&str> {
        self.field_filters.get(&field).map(String::as_str)
    }

    pub(crate) fn set_field_filter(&mut self, field: Field, selected: Option<String>) -> bool {
        if self.field_filters.get(&field) == selected.as_ref() {
            return false;
        }
        if let Some(value) = selected {
            self.field_filters.insert(field, value);
        } else {
            self.field_filters.remove(&field);
        }
        self.reindex();
        true
    }

    pub(crate) fn clear_field_filters(&mut self) -> bool {
        if self.field_filters.is_empty() {
            return false;
        }
        self.field_filters.clear();
        self.reindex();
        true
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

    pub fn selected_targets(&self) -> Vec<DeleteTarget> {
        self.checked
            .iter()
            .map(|(reference, uid)| DeleteTarget {
                reference: reference.clone(),
                uid: uid.clone(),
            })
            .collect()
    }

    pub fn selected_count(&self) -> usize {
        self.checked.len()
    }

    pub fn all_visible_selected(&self) -> bool {
        !self.rows.is_empty() && self.rows.iter().all(|key| self.checked.contains_key(key))
    }

    pub fn toggle_selected(&mut self, key: &ObjectRef) {
        if self.checked.remove(key).is_some() {
            return;
        }
        if let Some(uid) = self
            .store
            .get(key)
            .and_then(|object| object.metadata.uid.clone())
        {
            self.checked.insert(key.clone(), uid);
        }
    }

    pub fn toggle_all_visible(&mut self) {
        if self.all_visible_selected() {
            self.checked.clear();
            return;
        }
        for key in &self.rows {
            if let Some(uid) = self
                .store
                .get(key)
                .and_then(|object| object.metadata.uid.clone())
            {
                self.checked.insert(key.clone(), uid);
            }
        }
    }

    pub fn clear_selected(&mut self) {
        self.checked.clear();
    }

    pub fn remove_selected(&mut self, targets: &[DeleteTarget]) {
        for target in targets {
            if self.checked.get(&target.reference) == Some(&target.uid) {
                self.checked.remove(&target.reference);
            }
        }
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
        self.column_ids = column_ids(&columns);
        self.display_columns = (0..columns.len()).collect();
        self.layout = Default::default();
        self.columns = columns;
        self.store = ResourceStore::new();
        self.rows.clear();
        self.checked.clear();
        self.sort = Sort::Natural;
        // Every reset is followed by a new subscription, so from here until
        // that watch says something the table is waiting rather than empty.
        self.loading = true;
    }

    /// Swaps the columns without disturbing the rows.
    ///
    /// A kind's own printer columns arrive from the cluster a moment after its
    /// list does. Re-listing to show them would throw away rows that are
    /// already on screen and correct.
    pub fn set_columns(&mut self, columns: ColumnSet) {
        self.layout = self.layout_snapshot();
        self.column_ids = column_ids(&columns);
        self.columns = columns;
        self.rebuild_display_columns();
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
            self.prune_selection();
            return;
        }

        let mut rows: Vec<ObjectRef> = ranked.into_iter().map(|(_, key)| key).collect();

        match self.sort {
            Sort::Natural => rows.sort(),
            Sort::ByColumn { index, descending } => {
                let Some(column) = self.columns.columns.get(index) else {
                    rows.sort();
                    self.rows = rows;
                    self.prune_selection();
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
        self.prune_selection();
    }

    fn prune_selection(&mut self) {
        if self.checked.is_empty() {
            return;
        }
        let visible: BTreeSet<_> = self.rows.iter().collect();
        self.checked.retain(|key, uid| {
            visible.contains(key)
                && self
                    .store
                    .get(key)
                    .and_then(|object| object.metadata.uid.as_ref())
                    == Some(uid)
        });
    }

    /// The keys that survive the filter, each with its match score.
    ///
    /// An empty filter scores everything zero, which costs one pass and keeps
    /// the two paths through `reindex` identical in shape.
    fn matching(&mut self) -> Vec<(u32, ObjectRef)> {
        let filters = &self.field_filters;
        let labels = &self.label_selector;
        let now = self.now;
        let accepts = |object: &DynamicObject| {
            labels.matches(object.metadata.labels.as_ref())
                && filters
                    .iter()
                    .all(|(field, value)| field.values(object, now).contains(value))
        };
        if self.filter.is_empty() {
            return self
                .store
                .iter()
                .filter(|(_, object)| accepts(object))
                .map(|(key, _)| (0, key.clone()))
                .collect();
        }

        let pattern = Pattern::parse(&self.filter, CaseMatching::Smart, Normalization::Smart);
        let mut buffer = Vec::new();
        let mut matched = Vec::new();

        for (key, object) in self.store.iter() {
            if !accepts(object) {
                continue;
            }
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

        // Absolute execution times sort by the underlying instant. Display
        // strings begin with the year, which the numeric cell sorter alone
        // would mistake for the entire value.
        if let ColumnSource::JsonPath {
            expression,
            kind: PathKind::Timestamp,
        } = &column.source
        {
            return beacon_columns::path::evaluate(expression, &object.data)
                .first()
                .and_then(|value| value.as_str())
                .filter(|value| !value.starts_with("0001-"))
                .and_then(|value| value.parse::<Timestamp>().ok())
                .map(|time| SortKey::Number(time.as_second()))
                .unwrap_or(SortKey::Missing);
        }

        SortKey::of(&column.resolve(&Cell {
            metadata: &object.metadata,
            data: &object.data,
            now: self.now,
            usage: self.usage(key),
        }))
    }

    fn cell(&self, row: usize, column: usize) -> Option<(&ColumnDef, CellValue)> {
        let column = self.columns.columns.get(column)?;
        let key = self.rows.get(row)?;
        let object = self.store.get(key)?;

        Some((
            column,
            column.resolve(&Cell {
                metadata: &object.metadata,
                data: &object.data,
                now: self.now,
                usage: self.usage(key),
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
        self.display_columns.len() + usize::from(self.selection_column)
    }

    /// Draws the component's skeleton rows instead of an empty table. The
    /// alternative is an empty grid that reads as "nothing here" for as long
    /// as the first list takes.
    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, index: usize, _: &App) -> Column {
        if self.selection_column && index == 0 {
            return Column::new("select", "")
                .width(px(44.))
                .min_width(px(44.))
                .fixed_left()
                .resizable(false)
                .movable(false)
                .selectable(false);
        }
        let Some(data_index) = self.data_column(index) else {
            return Column::new("", "");
        };
        let Some(definition) = self.columns.columns.get(data_index) else {
            return Column::new("", "");
        };

        let width = self
            .layout
            .widths
            .get(&self.column_ids[data_index])
            .copied()
            .unwrap_or(match definition.width {
                ColumnWidth::Fixed(pixels) => pixels,
                ColumnWidth::Flex(share) => share * FLEX_UNIT,
            });

        let sort = match self.sort {
            Sort::ByColumn {
                index: sorted,
                descending,
            } if sorted == data_index => {
                if descending {
                    ColumnSort::Descending
                } else {
                    ColumnSort::Ascending
                }
            }
            _ => ColumnSort::Default,
        };

        Column::new(
            self.column_ids[data_index].clone(),
            definition.header.clone(),
        )
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
        if self.selection_column && index == 0 {
            return;
        }
        let Some(index) = self.data_column(index) else {
            return;
        };
        self.sort_by(index, sort);
        cx.emit(TableLayoutChanged(self.layout_snapshot()));
        cx.notify();
    }

    fn move_column(
        &mut self,
        from: usize,
        to: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.reorder_column(from, to);
        cx.emit(TableLayoutChanged(self.layout_snapshot()));
        cx.notify();
    }

    fn render_th(
        &mut self,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if !self.selection_column || column != 0 {
            return div()
                .size_full()
                .child(self.column(column, cx).name)
                .into_any_element();
        }
        let table = cx.entity().downgrade();
        let cluster = self.context_view.clone();
        div()
            .id("select-all-cell")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                Checkbox::new("select-all-resources")
                    .checked(self.all_visible_selected())
                    .disabled(self.rows.is_empty())
                    .accessibility_label("Select all visible resources")
                    .on_click(move |_, _, cx| {
                        let _ = table.update(cx, |state, cx| {
                            state.delegate_mut().toggle_all_visible();
                            cx.notify();
                        });
                        if let Some(cluster) = &cluster {
                            let _ = cluster.update(cx, |_, cx| cx.notify());
                        }
                    }),
            )
            .into_any_element()
    }

    fn context_menu(
        &mut self,
        row: usize,
        mut menu: PopupMenu,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let (Some(target), Some(view)) = (self.key_at(row).cloned(), self.context_view.clone())
        else {
            return menu;
        };
        let Some(cluster) = view.upgrade() else {
            return menu;
        };
        let Some((kind, rules)) = cluster.read(cx).menu_context() else {
            return menu;
        };
        let Some(object) = self.object(&target) else {
            return menu;
        };
        let replicas = object
            .data
            .get("spec")
            .and_then(|spec| spec.get("replicas"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(1) as i32;
        let group = kind.resource.group.as_str();
        let name = kind.resource.kind.as_str();
        let is_pod = group.is_empty() && name == "Pod";

        menu = menu
            .label(format!("{name} · {}", target.name))
            .item(detail_item(
                "Open details",
                &view,
                &target,
                DetailTab::Overview,
            ));

        if data::is_keyed(group, name) {
            menu = menu.item(detail_item("View data", &view, &target, DetailTab::Data));
        }
        if is_pod {
            menu = menu.item(pod_tools_item(
                "View logs",
                &view,
                &target,
                PodToolTab::Logs,
            ));
            let may_exec = actions::may_exec(rules.as_deref());
            menu = menu
                .item(
                    pod_tools_item("Run command", &view, &target, PodToolTab::Exec)
                        .disabled(!may_exec),
                )
                .item(
                    pod_tools_item("Open shell", &view, &target, PodToolTab::Shell)
                        .disabled(!may_exec),
                );

            let ports = actions::ports(&object.data);
            if !ports.is_empty() && target.namespace.is_some() {
                for port in ports {
                    let forward_view = view.clone();
                    let forward_target = target.clone();
                    menu = menu.item(PopupMenuItem::new(format!("Forward port {port}")).on_click(
                        move |_, window, cx| {
                            let _ = forward_view.update(cx, |cluster, cx| {
                                cluster.start_forward_for(forward_target.clone(), port, window, cx);
                            });
                        },
                    ));
                }
            }
        }
        menu = menu.item(detail_item("View YAML", &view, &target, DetailTab::Yaml));

        let choices = actions::available(&kind, rules.as_deref(), replicas);
        if !choices.is_empty() {
            menu = menu.separator();
        }
        for choice in choices {
            let action_view = view.clone();
            let action_target = target.clone();
            let operation = choice.operation;
            menu = menu.item(
                PopupMenuItem::new(choice.label)
                    .disabled(!choice.allowed)
                    .on_click(move |_, window, cx| {
                        let _ = action_view.update(cx, |cluster, cx| {
                            cluster.start_for(action_target.clone(), operation.clone(), window, cx);
                        });
                    }),
            );
        }

        let copy_name = target.name.clone();
        menu = menu
            .separator()
            .item(PopupMenuItem::new("Copy name").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy_name.clone()));
            }));
        if let Some(index) = self
            .columns
            .columns
            .iter()
            .position(|column| column.header == "Status")
            && let Some((_, value)) = self.cell(row, index)
            && !value.is_missing()
        {
            menu = menu.item(crate::copyable_text::copy_item(
                "Copy status",
                value.display().to_string(),
            ));
        }
        if is_pod {
            menu = menu.item(crate::copyable_text::copy_item(
                "Copy container states",
                beacon_columns::containers::description(&object.data),
            ));
        }
        menu
    }

    fn render_td(
        &mut self,
        row: usize,
        column: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if self.selection_column && column == 0 {
            let Some(key) = self.key_at(row).cloned() else {
                return h_flex().into_any_element();
            };
            let enabled = self
                .store
                .get(&key)
                .and_then(|object| object.metadata.uid.as_ref())
                .is_some();
            let checked = self.checked.contains_key(&key);
            let table = cx.entity().downgrade();
            let cluster = self.context_view.clone();
            return div()
                .id(SharedString::from(format!(
                    "select-cell-{}-{}",
                    key.namespace.as_deref().unwrap_or(""),
                    key.name
                )))
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    Checkbox::new(SharedString::from(format!(
                        "select-{}-{}",
                        key.namespace.as_deref().unwrap_or(""),
                        key.name
                    )))
                    .checked(checked)
                    .disabled(!enabled)
                    .accessibility_label(format!("Select {key}"))
                    .on_click(move |_, _, cx| {
                        let _ = table.update(cx, |state, cx| {
                            state.delegate_mut().toggle_selected(&key);
                            cx.notify();
                        });
                        if let Some(cluster) = &cluster {
                            let _ = cluster.update(cx, |_, cx| cx.notify());
                        }
                    }),
                )
                .into_any_element();
        }
        let Some((definition, value)) = self
            .data_column(column)
            .and_then(|column| self.cell(row, column))
        else {
            return h_flex().into_any_element();
        };

        if matches!(definition.source, ColumnSource::Containers) {
            let Some(object) = self.key_at(row).and_then(|key| self.object(key)) else {
                return h_flex().into_any_element();
            };
            let containers = beacon_columns::containers::summarize(&object.data);
            let summary = beacon_columns::containers::description(&object.data);
            return h_flex()
                .id(("container-states", row))
                .size_full()
                .items_center()
                .gap_1()
                .flex_wrap()
                .aria_label(summary.clone())
                .children(
                    containers
                        .into_iter()
                        .enumerate()
                        .map(|(index, container)| {
                            use beacon_columns::containers::ContainerHealth::*;
                            let color = match container.health {
                                Ready => cx.theme().tone(Tone::Healthy),
                                Running => cx.theme().tone(Tone::Warning),
                                Waiting => cx.theme().resource_link(),
                                Failed => cx.theme().tone(Tone::Critical),
                                Completed if container.init => cx.theme().tone(Tone::Healthy),
                                Completed | Unknown => cx.theme().muted_foreground,
                            };
                            div()
                                .id(("container-dot", index))
                                .size(px(9.))
                                .flex_shrink_0()
                                .rounded_full()
                                .border_1()
                                .border_color(color)
                                .when(!container.init, |dot| dot.bg(color))
                        }),
                )
                .tooltip(move |window, cx| Tooltip::new(summary.clone()).build(window, cx))
                .into_any_element();
        }

        if matches!(definition.source, ColumnSource::Taints) {
            let Some(object) = self.key_at(row).and_then(|key| self.object(key)) else {
                return h_flex().into_any_element();
            };
            let taints = beacon_columns::node::taint_descriptions(&object.data);
            let summary = if taints.is_empty() {
                "No taints".to_string()
            } else {
                taints.join("\n")
            };
            return h_flex()
                .id(("node-taints", row))
                .size_full()
                .items_center()
                .aria_label(format!("{} taints: {summary}", taints.len()))
                .text_color(if taints.is_empty() {
                    cx.theme().muted_foreground
                } else {
                    cx.theme().foreground
                })
                .child(value.display().to_string())
                .tooltip(move |window, cx| {
                    let taints = taints.clone();
                    Tooltip::element(move |_, _| {
                        v_flex()
                            .max_w(px(560.))
                            .gap_1()
                            .child(format!("Taints · {}", taints.len()))
                            .when(taints.is_empty(), |this| this.child("No taints"))
                            .children(taints.iter().map(|taint| div().child(taint.clone())))
                    })
                    .build(window, cx)
                })
                .into_any_element();
        }

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
            .into_any_element()
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
        self.display_cell_text(row, column)
    }
}

fn detail_item(
    label: &'static str,
    view: &WeakEntity<ClusterView>,
    target: &ObjectRef,
    tab: DetailTab,
) -> PopupMenuItem {
    let view = view.clone();
    let target = target.clone();
    PopupMenuItem::new(label).on_click(move |_, window, cx| {
        let _ = view.update(cx, |cluster, cx| {
            cluster.open_target(&target, tab, window, cx);
        });
    })
}

fn pod_tools_item(
    label: &'static str,
    view: &WeakEntity<ClusterView>,
    target: &ObjectRef,
    tab: PodToolTab,
) -> PopupMenuItem {
    let view = view.clone();
    let target = target.clone();
    PopupMenuItem::new(label).on_click(move |_, window, cx| {
        let _ = view.update(cx, |cluster, cx| {
            cluster.open_pod_tools(&target, tab, window, cx);
        });
    })
}

#[cfg(test)]
mod tests {
    // Imported by name rather than with `use super::*`: this module globs
    // `gpui_kit`, whose test-support build exports its own `#[test]`, and a
    // glob would shadow Rust's.
    use super::{ResourceTable, SortKey};
    use crate::filters::Field;
    use beacon_columns::{CellValue, ColumnSet};
    use beacon_kube::{Delta, DynamicObject, resources};
    use gpui_kit::component::table::ColumnSort;
    use std::{collections::BTreeMap, sync::Arc};

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

    #[test]
    fn workflow_execution_columns_sort_by_instants_instead_of_years() {
        let workflow = |name, started, finished| {
            Arc::new(
                serde_json::from_value::<DynamicObject>(serde_json::json!({
                    "apiVersion":"argoproj.io/v1alpha1", "kind":"Workflow",
                    "metadata":{"name":name},
                    "status":{"startedAt":started,"finishedAt":finished}
                }))
                .unwrap(),
            )
        };
        let mut table = ResourceTable::new(ColumnSet::for_kind("argoproj.io", "Workflow", false));
        table.apply(vec![Delta::Reset(vec![
            workflow("alpha", "2026-10-06T12:00:00Z", "2026-10-06T12:05:00Z"),
            workflow("beta", "2026-10-06T19:54:46+08:00", "2026-10-06T12:10:00Z"),
            workflow("gamma", "0001-01-01T00:00:00Z", "0001-01-01T00:00:00Z"),
        ])]);
        table.sort_by(1, ColumnSort::Ascending);
        assert_eq!(
            table
                .rows
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["beta", "alpha", "gamma"]
        );
        table.sort_by(2, ColumnSort::Ascending);
        assert_eq!(
            table
                .rows
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta", "gamma"]
        );
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

    #[test]
    fn layout_keeps_sort_widths_and_copy_values_after_hiding_reordering_and_rescoping() {
        let mut table = filled(3);
        let name = table.column_ids[0].clone();
        let namespace = table.column_ids[1].clone();
        let restarts_index = table
            .columns
            .columns
            .iter()
            .position(|column| column.header == "Restarts")
            .unwrap();
        let restarts = table.column_ids[restarts_index].clone();
        table.sort_by(restarts_index, ColumnSort::Ascending);
        table.toggle_column(&namespace);
        let restarts_display = table
            .display_columns
            .iter()
            .position(|index| *index == restarts_index)
            .unwrap()
            + 1;
        table.reorder_column(restarts_display, 1);
        assert_eq!(table.display_cell_text(0, 1), "4998");
        assert_eq!(table.display_cell_text(0, 2), "pod-00002");
        table.update_widths(&[gpui_kit::px(44.), gpui_kit::px(230.), gpui_kit::px(320.)]);
        let layout = table.layout_snapshot();
        assert_eq!(layout.widths[&restarts], 230.);
        assert_eq!(layout.widths[&name], 320.);
        assert!(layout.hidden.contains(&namespace));

        let encoded = serde_json::to_string(&layout).unwrap();
        let mut restored = filled(3);
        restored.apply_layout(serde_json::from_str(&encoded).unwrap());
        assert_eq!(restored.key_at(0).unwrap().name, "pod-00002");
        assert_eq!(restored.display_cell_text(0, 1), "4998");
        restored.set_columns(ColumnSet::for_kind("", "Pod", false));
        assert_eq!(restored.display_cell_text(0, 1), "4998");
        restored.set_columns(ColumnSet::for_kind("", "Pod", true));
        assert!(!restored.display_columns.contains(&1));
        assert_eq!(restored.layout_snapshot(), layout);
        restored.toggle_column(&name);
        assert!(
            restored.display_columns.contains(&0),
            "the identifying Name column remains visible"
        );
        restored.reorder_column(0, 1);
        assert_eq!(
            restored.display_cell_text(0, 1),
            "4998",
            "the fixed selection column cannot move"
        );
    }

    #[test]
    fn named_filters_restore_search_labels_facets_and_sort_without_bulk_selection() {
        let mut table = ResourceTable::new(ColumnSet::for_kind("", "Pod", false));
        table.apply(vec![Delta::Reset(vec![
            labeled("web-one", "web", "prod"),
            labeled("web-two", "web", "prod"),
            labeled("database", "db", "prod"),
        ])]);
        table.set_filter("web");
        table.set_label_filter("app=web,env in (prod,qa)").unwrap();
        let phase = Field::PodStatus
            .values(table.store.iter().next().unwrap().1, table.now)
            .into_iter()
            .next()
            .unwrap();
        table.set_field_filter(Field::PodStatus, Some(phase));
        table.sort_by(0, ColumnSort::Descending);
        let preset = table.filter_preset(
            ["default".to_owned(), "staging".to_owned()]
                .into_iter()
                .collect(),
        );
        let preset = serde_json::from_str(&serde_json::to_string(&preset).unwrap()).unwrap();
        table.set_filter("");
        table.clear_field_filters();
        table.set_label_filter("").unwrap();
        table.sort_by(0, ColumnSort::Ascending);
        table.toggle_all_visible();
        table.apply_preset(&preset).unwrap();
        assert_eq!(table.len(), 2);
        assert_eq!(table.key_at(0).unwrap().name, "web-two");
        assert_eq!(table.selected_count(), 0);
        assert_eq!(table.filter_preset(preset.namespaces.clone()), preset);
        let mut invalid = preset.clone();
        invalid.labels = "app in (".into();
        assert!(table.apply_preset(&invalid).is_err());
        assert_eq!(table.filter_preset(preset.namespaces.clone()), preset);
    }

    fn named(namespace: &str, name: &str) -> Arc<DynamicObject> {
        let mut object = DynamicObject::new(name, &resources::pod())
            .within(namespace)
            .data(serde_json::json!({ "spec": { "containers": [{}] } }));
        object.metadata.resource_version = Some("1".into());
        object.metadata.uid = Some(format!("uid-{namespace}-{name}"));
        Arc::new(object)
    }

    #[test]
    fn checked_resources_track_uid_and_never_include_hidden_rows() {
        let mut table = ResourceTable::new(ColumnSet::for_kind("", "Pod", true));
        table.apply(vec![Delta::Reset(vec![
            named("default", "one"),
            named("default", "two"),
        ])]);
        table.toggle_all_visible();
        assert_eq!(table.selected_count(), 2);
        assert!(table.all_visible_selected());

        table.set_filter("one");
        assert_eq!(table.selected_count(), 1);
        assert_eq!(table.selected_targets()[0].reference.name, "one");
        table.set_filter("");
        assert_eq!(table.selected_count(), 1, "hidden rows stay unselected");

        let mut replacement = (*named("default", "one")).clone();
        replacement.metadata.uid = Some("replacement-uid".into());
        replacement.metadata.resource_version = Some("2".into());
        table.apply(vec![Delta::Upsert(Arc::new(replacement))]);
        assert_eq!(
            table.selected_count(),
            0,
            "a replacement is never deleted by stale selection"
        );
    }

    fn secret(name: &str, secret_type: Option<&str>) -> Arc<DynamicObject> {
        let mut data = serde_json::json!({});
        if let Some(secret_type) = secret_type {
            data["type"] = serde_json::json!(secret_type);
        }
        Arc::new(
            DynamicObject::new(name, &resources::pod())
                .within("default")
                .data(data),
        )
    }

    fn labeled(name: &str, app: &str, env: &str) -> Arc<DynamicObject> {
        let mut object = (*named("default", name)).clone();
        object.metadata.labels = Some(BTreeMap::from([
            ("app".into(), app.into()),
            ("env".into(), env.into()),
        ]));
        Arc::new(object)
    }

    #[test]
    fn split_filters_preserve_order_without_sharing_filters_or_delete_selection() {
        let objects: Vec<_> = [
            ("web-one", "web", "prod", "Failed"),
            ("web-two", "web", "qa", "Failed"),
            ("web-running", "web", "prod", "Running"),
            ("db-one", "db", "prod", "Failed"),
        ]
        .into_iter()
        .map(|(name, app, env, phase)| {
            let mut object = (*labeled(name, app, env)).clone();
            object.data["status"] = serde_json::json!({"phase": phase});
            Arc::new(object)
        })
        .collect();
        let mut source = ResourceTable::new(ColumnSet::for_kind("", "Pod", true));
        source.apply(vec![Delta::Reset(objects.clone())]);
        source.set_filter("web");
        source.set_label_filter("app=web").unwrap();
        source.set_field_filter(Field::PodStatus, Some("Failed".into()));
        source.sort_by(0, ColumnSort::Descending);
        source.toggle_all_visible();
        assert_eq!(source.selected_count(), 2);

        let mut split = ResourceTable::new(ColumnSet::for_kind("", "Pod", true));
        split.restore_filters(source.view_filters());
        split.apply(vec![Delta::Reset(objects)]);
        assert_eq!(split.len(), 2);
        assert_eq!(split.key_at(0).unwrap().name, "web-two");
        assert_eq!(split.selected_count(), 0);
        assert_eq!(split.filter(), "web");
        assert_eq!(split.label_filter(), "app=web");

        source.set_filter("one");
        assert_eq!(source.len(), 1);
        assert_eq!(split.len(), 2);
        split.set_label_filter("env=qa").unwrap();
        assert_eq!(split.len(), 1);
        assert_eq!(split.key_at(0).unwrap().name, "web-two");
        assert_eq!(source.key_at(0).unwrap().name, "web-one");
    }

    #[test]
    fn label_filters_combine_with_search_facets_sort_and_safe_selection() {
        let mut table = ResourceTable::new(ColumnSet::for_kind("", "Pod", true));
        let mut stopped = (*labeled("web-stopped", "web", "prod")).clone();
        stopped.data["status"] = serde_json::json!({"phase":"Failed"});
        table.apply(vec![Delta::Reset(vec![
            labeled("web-one", "web", "prod"),
            labeled("web-two", "web", "qa"),
            labeled("db-one", "db", "prod"),
            Arc::new(stopped),
        ])]);
        table.toggle_all_visible();
        table.sort_by(0, ColumnSort::Descending);
        assert!(table.set_label_filter("app=web,env in (prod,qa)").unwrap());
        assert_eq!(table.len(), 3);
        assert_eq!(table.selected_count(), 3);
        table.set_field_filter(Field::PodStatus, Some("Failed".into()));
        assert_eq!(table.len(), 1);
        assert_eq!(table.key_at(0).unwrap().name, "web-stopped");
        assert_eq!(
            table.label_values(),
            vec![
                ("app".into(), "db".into()),
                ("app".into(), "web".into()),
                ("env".into(), "prod".into()),
                ("env".into(), "qa".into()),
            ],
            "hidden resources still contribute suggestions"
        );
        let previous = table.label_filter().to_string();
        assert!(table.set_label_filter("app in (").is_err());
        assert_eq!(table.label_filter(), previous);
        assert_eq!(table.selected_count(), 1);
        table.set_filter("one");
        assert_eq!(table.len(), 0);
        assert_eq!(
            table.selected_count(),
            0,
            "bulk deletion cannot include hidden rows"
        );
        table.set_filter("");
        table.clear_field_filters();
        table.set_label_filter("").unwrap();
        assert_eq!(table.total(), 4);
        assert_eq!(table.len(), 4);
        assert_eq!(
            table.key_at(0).unwrap().name,
            "web-two",
            "the chosen sort survives"
        );
    }

    #[test]
    fn label_filters_follow_watch_updates_and_remain_local_to_each_table() {
        let mut table = ResourceTable::new(ColumnSet::fallback(true));
        let mut other = ResourceTable::new(ColumnSet::fallback(true));
        let objects = vec![labeled("one", "web", "prod"), labeled("two", "db", "prod")];
        table.apply(vec![Delta::Reset(objects.clone())]);
        other.apply(vec![Delta::Reset(objects)]);
        table.set_label_filter("app=web").unwrap();
        assert_eq!(table.len(), 1);
        assert_eq!(other.len(), 2);
        let mut changed = (*labeled("two", "web", "prod")).clone();
        changed.metadata.resource_version = Some("2".into());
        table.apply(vec![Delta::Upsert(Arc::new(changed))]);
        assert_eq!(
            table.len(),
            2,
            "a live label update brings the row into view"
        );
        let mut changed = (*labeled("one", "db", "prod")).clone();
        changed.metadata.resource_version = Some("2".into());
        table.apply(vec![Delta::Upsert(Arc::new(changed))]);
        assert_eq!(table.key_at(0).unwrap().name, "two");
        table.reset(ColumnSet::fallback(true));
        table.apply_from(
            Some("default"),
            vec![Delta::Reset(vec![labeled("three", "db", "prod")])],
        );
        assert_eq!(
            table.len(),
            0,
            "changing namespace or relisting retains the label filter"
        );
        assert_eq!(table.label_filter(), "app=web");
        assert!(other.label_selector().is_empty());
    }

    #[test]
    fn claim_filters_combine_modes_scope_search_and_watch_updates() {
        let claim = |name: &str, phase: &str, class: &str, modes: Vec<&str>| {
            let mut object = (*named("default", name)).clone();
            object.data = serde_json::json!({"spec":{"volumeName":"pv-one","storageClassName":class,"accessModes":modes,"volumeMode":"Filesystem"},"status":{"phase":phase}});
            Arc::new(object)
        };
        let mut table = ResourceTable::new(ColumnSet::fallback(true));
        table.apply(vec![Delta::Reset(vec![
            claim(
                "one",
                "Bound",
                "fast",
                vec!["ReadWriteOnce", "ReadOnlyMany"],
            ),
            claim("two", "Pending", "slow", vec!["ReadWriteMany"]),
        ])]);
        table.set_field_filter(Field::ClaimStatus, Some("Bound".into()));
        table.set_field_filter(Field::StorageClass, Some("fast".into()));
        table.set_field_filter(Field::AccessMode, Some("ReadOnlyMany".into()));
        assert_eq!(table.len(), 1);
        assert_eq!(
            table.filter_values(Field::ClaimStatus),
            ["Bound", "Pending"]
        );
        table.set_filter("two");
        assert_eq!(table.len(), 0);
        table.set_filter("");
        let mut changed = (*claim("one", "Lost", "fast", vec!["ReadOnlyMany"])).clone();
        changed.metadata.resource_version = Some("new".into());
        table.apply(vec![Delta::Upsert(Arc::new(changed))]);
        assert_eq!(table.len(), 0);
        table.clear_field_filters();
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn secret_type_filter_is_exact_and_combines_with_name_search() {
        let mut table = ResourceTable::new(ColumnSet::for_kind("", "Secret", true));
        table.apply(vec![Delta::Reset(vec![
            secret("registry-main", Some("kubernetes.io/dockerconfigjson")),
            secret("registry-alt", Some("example.com/dockerconfigjson")),
            secret("service-token", Some("kubernetes.io/service-account-token")),
            secret("plain", None),
        ])]);

        assert_eq!(
            table.filter_values(Field::SecretType),
            [
                "Opaque",
                "example.com/dockerconfigjson",
                "kubernetes.io/dockerconfigjson",
                "kubernetes.io/service-account-token",
            ]
        );

        assert!(table.set_field_filter(
            Field::SecretType,
            Some("kubernetes.io/dockerconfigjson".into())
        ));
        assert_eq!(table.len(), 1);
        assert_eq!(table.key_at(0).unwrap().name, "registry-main");
        assert_eq!(table.total(), 4);

        table.set_filter("token");
        assert_eq!(table.len(), 0, "the name search also applies");
        table.set_filter("registry");
        assert_eq!(table.len(), 1);
        assert_eq!(table.key_at(0).unwrap().name, "registry-main");

        table.set_field_filter(Field::SecretType, Some("Opaque".into()));
        table.set_filter("");
        assert_eq!(table.key_at(0).unwrap().name, "plain");
        table.set_field_filter(Field::SecretType, None);
        assert_eq!(table.len(), 4);
    }

    /// Two namespaces watched at once, each listing in its own time. The
    /// second one's initial Reset must not take the first one's rows with it.
    #[test]
    fn one_namespaces_reset_leaves_the_others_alone() {
        let mut table = ResourceTable::new(ColumnSet::for_kind("", "Pod", true));

        table.apply_from(
            Some("alpha"),
            vec![Delta::Reset(vec![named("alpha", "one")])],
        );
        table.apply_from(Some("beta"), vec![Delta::Reset(vec![named("beta", "two")])]);

        assert_eq!(table.total(), 2, "both namespaces are still in the table");

        // A re-list of one namespace -- a watch desync, say -- replaces that
        // namespace and nothing else.
        table.apply_from(
            Some("alpha"),
            vec![Delta::Reset(vec![named("alpha", "three")])],
        );

        let names: Vec<&str> = table.rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["three", "two"]);
    }

    /// The cluster-wide watch is the only one feeding the table, so its Reset
    /// keeps meaning "replace everything".
    #[test]
    fn a_cluster_wide_reset_replaces_the_table() {
        let mut table = ResourceTable::new(ColumnSet::for_kind("", "Pod", true));

        table.apply_from(None, vec![Delta::Reset(vec![named("alpha", "one")])]);
        table.apply_from(None, vec![Delta::Reset(vec![named("beta", "two")])]);

        let names: Vec<&str> = table.rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, vec!["two"]);
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
        let restarts = table
            .columns
            .columns
            .iter()
            .position(|column| column.header == "Restarts")
            .expect("Restarts column");
        table.sort_by(restarts, ColumnSort::Ascending);
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

        // Restarts is `5000 - index`.
        let restarts = table
            .columns
            .columns
            .iter()
            .position(|column| column.header == "Restarts")
            .expect("Restarts column");
        table.sort_by(restarts, ColumnSort::Ascending);
        assert_eq!(table.rows.first().expect("rows").name, "pod-04999");
        assert_eq!(table.rows.last().expect("rows").name, "pod-00000");

        table.sort_by(restarts, ColumnSort::Descending);
        assert_eq!(table.rows.first().expect("rows").name, "pod-00000");

        table.sort_by(restarts, ColumnSort::Default);
        assert_eq!(table.rows.first().expect("rows").name, "pod-00000");
        assert_eq!(
            table.rows.first().expect("rows").namespace.as_deref(),
            Some("ns-00")
        );
    }
}
