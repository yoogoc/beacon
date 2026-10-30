//! One independent, three-step window for single and bulk resource transfers.
#[cfg(all(test, feature = "ui-tests"))]
mod tests;
mod view;
use crate::{
    bridge::Bridge,
    connections::{Changed, SharedConnections},
    scroll::OverflowScrollbars as _,
};
use beacon_kube::{
    ClusterId, ClusterSession, WatchKey,
    transfer::{Batch, Options, Selection, TargetState},
};
use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, TitleBar,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;

fn tr(value: &str) -> SharedString {
    crate::i18n::text(value)
}
fn error_text(value: &str) -> SharedString {
    let value = value
        .strip_prefix("invalid resource manifest: ")
        .unwrap_or(value);
    if let Some(target) = value.strip_prefix("Source ").and_then(|value| {
        value.strip_suffix(
            " changed after review. Keep both sides and start a fresh transfer review.",
        )
    }) {
        return crate::i18n::message("transfer.source_changed", &[("target", target.to_owned())]);
    }
    tr(value)
}
fn message(key: &str, count: usize) -> SharedString {
    crate::i18n::message(key, &[("count", count.to_string())])
}
#[derive(Clone)]
struct ClusterChoice {
    id: ClusterId,
    label: String,
}
impl SelectItem for ClusterChoice {
    type Value = ClusterId;
    fn title(&self) -> SharedString {
        self.label.clone().into()
    }
    fn value(&self) -> &ClusterId {
        &self.id
    }
    fn matches(&self, query: &str) -> bool {
        self.label.to_lowercase().contains(&query.to_lowercase())
            || self
                .id
                .as_str()
                .to_lowercase()
                .contains(&query.to_lowercase())
    }
}
struct Row {
    selection: Selection,
    name: Entity<InputState>,
    included: bool,
    _change: Subscription,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Target,
    Review,
    Result,
}
struct TransferView {
    source: Arc<ClusterSession>,
    cluster: Entity<SelectState<SearchableVec<ClusterChoice>>>,
    namespaces: Entity<SelectState<SearchableVec<String>>>,
    namespace: Entity<InputState>,
    manual_namespace: bool,
    namespaced: bool,
    rows: Vec<Row>,
    moving: bool,
    secret_consent: bool,
    cleanup_confirmed: bool,
    ended: bool,
    step: Step,
    yaml: bool,
    batch: Option<Batch>,
    busy: bool,
    loading: bool,
    revision: u64,
    error: Option<String>,
    namespace_note: Option<String>,
    _operation: Option<Task<()>>,
    _namespaces: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

pub(crate) fn open(
    source: Arc<ClusterSession>,
    selections: Vec<Selection>,
    window: &mut Window,
    cx: &mut App,
) {
    if selections.is_empty() {
        return;
    }
    let bounds = Bounds::centered(
        window.display(cx).map(|display| display.id()),
        size(px(760.), px(800.)),
        cx,
    );
    cx.defer(move |cx| {
        if let Err(error) = open_window(source, selections, bounds, cx) {
            tracing::error!(%error,"could not open resource transfer");
        }
    });
}
fn open_window(
    source: Arc<ClusterSession>,
    selections: Vec<Selection>,
    bounds: Bounds<Pixels>,
    cx: &mut App,
) -> anyhow::Result<(AnyWindowHandle, Entity<TransferView>)> {
    gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(570.), px(550.))),
            ..TitleBar::window_options()
        },
        cx,
        move |window, cx| {
            #[cfg(all(test, feature = "ui-tests"))]
            window.set_view_retention(false);
            let view = cx.new(|cx| TransferView::new(source, selections, window, cx));
            crate::i18n::window_title("Resource transfer", window, cx);
            let weak = view.downgrade();
            window.on_window_should_close(cx, move |_, cx| {
                weak.update(cx, |view, cx| {
                    if view.busy {
                        view.error =
                            Some("Wait for the running transfer operation to finish.".into());
                        cx.notify();
                        false
                    } else {
                        true
                    }
                })
                .unwrap_or(true)
            });
            window.activate_window();
            view
        },
    )
}
impl TransferView {
    fn source_location(&self) -> String {
        let namespaces = self
            .rows
            .iter()
            .filter(|row| row.included && !row.selection.copy_only)
            .filter_map(|row| row.selection.reference.namespace.as_deref())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{}{}",
            self.source.id(),
            if namespaces.is_empty() {
                String::new()
            } else {
                format!(" / {namespaces}")
            }
        )
    }
    fn use_owner(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(owner) = self
            .batch
            .as_ref()
            .and_then(|batch| batch.items().get(index))
            .and_then(|item| item.owner.clone())
        else {
            return;
        };
        if let Some(item) = self
            .batch
            .as_ref()
            .and_then(|batch| batch.items().get(index))
        {
            for row in &mut self.rows {
                if row.selection.reference == item.selection().reference
                    && row.selection.kind.gvk() == item.selection().kind.gvk()
                {
                    row.included = false;
                }
            }
        }
        self.add_row(owner, window, cx);
        self.invalidate(cx);
    }
    fn open_target(&self, index: usize, window: &Window, cx: &mut Context<Self>) {
        let Some(batch) = &self.batch else {
            return;
        };
        let Some(item) = batch.items().get(index) else {
            return;
        };
        let Some(kind) = item.target_kind().cloned() else {
            return;
        };
        let namespace = kind
            .namespaced
            .then(|| batch.namespace().map(str::to_owned))
            .flatten();
        let reference =
            beacon_kube::ObjectRef::new(namespace.clone(), item.selection().name.clone());
        let session = batch.destination().clone();
        let bounds = Bounds::centered(
            window.display(cx).map(|display| display.id()),
            size(px(1000.), px(720.)),
            cx,
        );
        cx.defer(move |cx| {
            if let Err(error) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..TitleBar::window_options()
                },
                cx,
                move |window, cx| {
                    crate::i18n::window_title("Transfer destination", window, cx);
                    cx.new(|cx| {
                        let mut view = crate::ClusterView::new(
                            session,
                            namespace,
                            Some(Arc::new(kind)),
                            None,
                            false,
                            window,
                            cx,
                        );
                        view.reveal(&reference, window, cx);
                        view
                    })
                },
            ) {
                tracing::error!(%error,"could not show transfer destination");
            }
        });
    }
    fn new(
        source: Arc<ClusterSession>,
        selections: Vec<Selection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut choices = crate::sources::store(cx)
            .read(cx)
            .catalog
            .targets
            .keys()
            .map(|id| ClusterChoice {
                id: id.clone(),
                label: context_label(id, cx),
            })
            .collect::<Vec<_>>();
        for id in cx
            .global::<SharedConnections>()
            .0
            .read(cx)
            .sessions
            .keys()
            .chain(std::iter::once(source.id()))
        {
            if !choices.iter().any(|c| &c.id == id) {
                choices.push(ClusterChoice {
                    id: id.clone(),
                    label: context_label(id, cx),
                });
            }
        }
        let labels = choices.iter().map(|c| c.label.clone()).collect::<Vec<_>>();
        for choice in &mut choices {
            if labels
                .iter()
                .filter(|label| **label == choice.label)
                .count()
                > 1
            {
                choice.label = format!("{} · {}", choice.label, choice.id);
            }
        }
        choices.sort_by(|a, b| a.label.cmp(&b.label));
        let cluster = cx.new(|cx| {
            let mut picker =
                SelectState::new(SearchableVec::new(choices), None, window, cx).searchable(true);
            picker.set_selected_value(source.id(), window, cx);
            picker
        });
        let initial = selections
            .first()
            .and_then(|s| s.reference.namespace.clone())
            .unwrap_or_default();
        let namespaced = selections.first().is_some_and(|s| s.kind.namespaced);
        let namespace = cx.new(|cx| InputState::new(window, cx).default_value(initial.clone()));
        let namespaces = cx.new(|cx| {
            SelectState::new(SearchableVec::new(vec![initial]), None, window, cx).searchable(true)
        });
        let mut subscriptions = vec![];
        subscriptions.push(
            cx.subscribe_in(&cluster, window, |view, _, event, window, cx| {
                if matches!(event, SelectEvent::Confirm(_)) && !view.busy && !view.has_targets() {
                    view.invalidate(cx);
                    view.secret_consent = false;
                    view.load_target(window, cx);
                }
            }),
        );
        subscriptions.push(
            cx.subscribe_in(&namespaces, window, |view, _, event, window, cx| {
                if let SelectEvent::Confirm(Some(ns)) = event
                    && !view.busy
                    && !view.has_targets()
                {
                    view.namespace
                        .update(cx, |input, cx| input.set_value(ns.clone(), window, cx));
                    view.invalidate(cx);
                }
            }),
        );
        subscriptions.push(cx.subscribe(&namespace, |view, _, event, cx| {
            if matches!(event, InputEvent::Change) && !view.busy && !view.has_targets() {
                view.invalidate(cx);
            }
        }));
        let connections = cx.global::<SharedConnections>().0.clone();
        subscriptions.push(cx.subscribe_in(
            &connections,
            window,
            |view, _, event: &Changed, window, cx| {
                if view.selected_id(cx).as_ref() == Some(&event.0)
                    && !view.busy
                    && !view.has_targets()
                {
                    view.load_target(window, cx);
                }
            },
        ));
        let mut view = Self {
            source,
            cluster,
            namespaces,
            namespace,
            manual_namespace: false,
            namespaced,
            rows: vec![],
            moving: false,
            secret_consent: false,
            cleanup_confirmed: false,
            ended: false,
            step: Step::Target,
            yaml: false,
            batch: None,
            busy: false,
            loading: false,
            revision: 0,
            error: None,
            namespace_note: None,
            _operation: None,
            _namespaces: None,
            _subscriptions: subscriptions,
        };
        for selection in selections {
            view.add_row(selection, window, cx);
        }
        view.load_namespaces(window, cx);
        view
    }
    fn selected_id(&self, cx: &App) -> Option<ClusterId> {
        self.cluster.read(cx).selected_value().cloned()
    }
    fn destination(&self, cx: &App) -> Option<Arc<ClusterSession>> {
        let id = self.selected_id(cx)?;
        if &id == self.source.id() {
            Some(self.source.clone())
        } else {
            cx.global::<SharedConnections>()
                .0
                .read(cx)
                .sessions
                .get(&id)
                .cloned()
        }
    }
    fn has_targets(&self) -> bool {
        self.batch.as_ref().is_some_and(Batch::has_targets)
    }
    fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        self.batch = None;
        self.step = Step::Target;
        self.error = None;
        self.cleanup_confirmed = false;
        cx.notify();
    }
    fn add_row(&mut self, selection: Selection, window: &mut Window, cx: &mut Context<Self>) {
        if self.rows.iter().any(|row| {
            row.selection.reference == selection.reference
                && row.selection.kind.gvk() == selection.kind.gvk()
        }) {
            return;
        }
        let name = cx.new(|cx| InputState::new(window, cx).default_value(selection.name.clone()));
        let change = cx.subscribe(&name, |view, _, event, cx| {
            if matches!(event, InputEvent::Change) && !view.busy && !view.has_targets() {
                view.invalidate(cx);
            }
        });
        self.rows.push(Row {
            selection,
            name,
            included: true,
            _change: change,
        });
    }
    fn load_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.destination(cx).is_some() {
            self.load_namespaces(window, cx);
            return;
        }
        let Some(id) = self.selected_id(cx) else {
            return;
        };
        let connections = cx.global::<SharedConnections>().0.clone();
        let state = connections.read(cx);
        if state.connecting(&id) {
            self.loading = true;
            cx.notify();
            return;
        }
        if let Some(error) = state.errors.get(&id) {
            self.error = Some(error.clone());
            self.loading = false;
            cx.notify();
            return;
        }
        let options = crate::settings::store(cx)
            .read(cx)
            .preferences
            .connection(&id);
        self.loading = true;
        connections.update(cx, |state, cx| {
            state.disconnected.remove(&id);
            state.connect(id, options, cx);
        });
        cx.notify();
    }
    fn retry_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.has_targets() {
            return;
        }
        if let Some(id) = self.selected_id(cx) {
            let connections = cx.global::<SharedConnections>().0.clone();
            connections.update(cx, |state, _| {
                state.errors.remove(&id);
            });
            self.error = None;
            self.load_target(window, cx);
        }
    }
    fn load_namespaces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = false;
        self.namespace_note = None;
        if !self.namespaced {
            cx.notify();
            return;
        }
        let Some(destination) = self.destination(cx) else {
            return;
        };
        let id = destination.id().clone();
        self.loading = true;
        let loading = Bridge::global(cx).run_cancellable(async move {
            destination
                .list_objects(WatchKey::all(beacon_kube::resources::namespace()))
                .await
        });
        self._namespaces = Some(cx.spawn_in(window, async move |this, cx| {
            let result = loading.result().await;
            let _ = this.update_in(cx, |view, window, cx| {
                // The previous task is cancelled when the cluster changes.
                // Editing a resource name must not discard namespace results.
                if view.selected_id(cx).as_ref() != Some(&id) {
                    return;
                }
                view.loading = false;
                match result {
                    Ok(Ok(objects)) => {
                        let mut names = objects
                            .iter()
                            .filter_map(|o| o.metadata.name.clone())
                            .collect::<Vec<_>>();
                        names.sort();
                        names.dedup();
                        let current = view.namespace.read(cx).value().to_string();
                        let selected = if view.manual_namespace || names.contains(&current) {
                            current
                        } else {
                            names.first().cloned().unwrap_or_default()
                        };
                        view.namespaces.update(cx, |state, cx| {
                            state.set_items(SearchableVec::new(names), window, cx);
                            state.set_selected_value(&selected, window, cx);
                        });
                        view.namespace
                            .update(cx, |state, cx| state.set_value(selected, window, cx));
                    }
                    other => {
                        view.manual_namespace = true;
                        view.namespace_note = Some(match other {
                            Ok(Err(error)) => error.user_message(),
                            Err(error) => error.to_string(),
                            _ => unreachable!(),
                        });
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn selections(&self, cx: &App) -> Vec<Selection> {
        self.rows
            .iter()
            .filter(|row| row.included)
            .map(|row| {
                let mut selection = row.selection.clone();
                selection.name = row.name.read(cx).value().trim().into();
                selection
            })
            .collect()
    }
    fn check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.has_targets() {
            return;
        }
        let Some(destination) = self.destination(cx) else {
            self.error = Some("Connect to the target cluster first.".into());
            cx.notify();
            return;
        };
        let selections = self.selections(cx);
        if selections.is_empty() {
            self.error = Some("Select at least one resource.".into());
            cx.notify();
            return;
        }
        let options = Options {
            namespace: self
                .namespaced
                .then(|| self.namespace.read(cx).value().trim().to_string()),
            moving: self.moving,
            secret_consent: self.secret_consent,
        };
        self.busy = true;
        self.error = None;
        self.cleanup_confirmed = false;
        let source = self.source.clone();
        let revision = self.revision;
        let checking = Bridge::global(cx).run_cancellable(async move {
            Batch::check(source, destination, selections, options).await
        });
        self._operation = Some(cx.spawn_in(window, async move |this, cx| {
            let result = checking.result().await;
            let _ = this.update_in(cx, |view, _, cx| {
                if view.revision != revision {
                    return;
                }
                view.busy = false;
                match result {
                    Ok(batch) => {
                        view.batch = Some(batch);
                        view.step = Step::Review;
                    }
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn execute(&mut self, action: Execution, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.ended {
            return;
        }
        let connections = cx.global::<SharedConnections>().0.read(cx);
        let source_changed = connections.disconnected.contains(self.source.id())
            || connections
                .sessions
                .get(self.source.id())
                .is_some_and(|session| !Arc::ptr_eq(session, &self.source));
        let target_changed = self.batch.as_ref().is_some_and(|batch| {
            self.destination(cx)
                .is_none_or(|session| !Arc::ptr_eq(&session, batch.destination()))
        });
        if source_changed || target_changed {
            self.error=Some("A transfer connection was disconnected or replaced. Keep the source and open a fresh transfer review.".into());
            cx.notify();
            return;
        }

        if action == Execution::Cleanup && !self.cleanup_confirmed {
            return;
        }
        let Some(mut batch) = self.batch.clone() else {
            return;
        };
        if action == Execution::Create && !batch.ready() {
            return;
        }
        self.busy = true;
        self.error = None;
        self.step = Step::Result;
        // Writes outlive transient frame tasks, but this window refuses normal
        // closing until the operation returns and its per-resource result is shown.
        let working = Bridge::global(cx).run(async move {
            let result = match action {
                Execution::Create => batch.create().await,
                Execution::Verify => {
                    batch.verify().await;
                    Ok(())
                }
                Execution::Cleanup => batch.cleanup().await,
            };
            (batch, result)
        });
        self._operation = Some(cx.spawn_in(window, async move |this, cx| {
            let result = working.await;
            let _ = this.update_in(cx, |view, _, cx| {
                view.busy = false;
                match result {
                    Ok((batch, result)) => {
                        view.batch = Some(batch);
                        if let Err(error) = result {
                            view.error = Some(error.user_message());
                        }
                    }
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn rename(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.has_targets() {
            return;
        }
        if let Some(row) = self.rows.get(index) {
            let name = format!("{}-copy", row.selection.reference.name);
            row.name
                .update(cx, |input, cx| input.set_value(name, window, cx));
        }
        self.invalidate(cx);
    }
    fn add_dependency(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.has_targets() {
            return;
        }
        let Some(dependency) = self
            .batch
            .as_ref()
            .and_then(|batch| batch.dependencies().get(index))
            .cloned()
        else {
            return;
        };
        let needs_namespace = dependency.kind.namespaced && !self.namespaced;
        if needs_namespace {
            self.namespaced = true;
            self.namespace.update(cx, |input, cx| {
                input.set_value(
                    dependency.reference.namespace.clone().unwrap_or_default(),
                    window,
                    cx,
                );
            });
        }
        self.add_row(
            Selection {
                kind: dependency.kind,
                reference: dependency.reference.clone(),
                uid: dependency.uid,
                name: dependency.reference.name,
                copy_only: true,
            },
            window,
            cx,
        );
        self.invalidate(cx);
        if needs_namespace {
            // A cluster-scoped binding can introduce a ServiceAccount dependency.
            // Let the user choose its destination namespace before reviewing it.
            self.load_namespaces(window, cx);
        } else {
            self.check(window, cx);
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Execution {
    Create,
    Verify,
    Cleanup,
}
fn context_label(id: &ClusterId, cx: &App) -> String {
    let settings = crate::settings::store(cx);
    let name = settings
        .read(cx)
        .preferences
        .clusters
        .get(id.as_str())
        .filter(|p| !p.alias.trim().is_empty())
        .map(|p| p.alias.clone())
        .unwrap_or_else(|| id.display_name().to_owned());
    let source = crate::sources::store(cx).read(cx).labels.get(id).cloned();
    format!(
        "{}{}",
        name,
        source.map_or(String::new(), |source| format!(" · {source}"))
    )
}
