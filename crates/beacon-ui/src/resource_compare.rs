//! Read-only comparison with searchable targets and a field-first review.
mod comparison;
mod view;
use crate::scroll::OverflowScrollbars as _;
use crate::{
    bridge::Bridge, connections::SharedConnections, copyable_text::copyable_text,
    selection::checkbox,
};
use beacon_kube::{ClusterSession, DynamicObject, Kind, ObjectRef, WatchKey};
use comparison::Change;
#[cfg(test)]
use comparison::normalized;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffMode, DiffState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, TitleBar,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Copy, Default)]
struct Options {
    status: bool,
    metadata: bool,
    reveal: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Changes,
    Yaml,
}
#[derive(Clone)]
struct ClusterChoice {
    label: String,
    id: String,
}
impl SelectItem for ClusterChoice {
    type Value = String;
    fn title(&self) -> SharedString {
        self.label.clone().into()
    }
    fn value(&self) -> &String {
        &self.id
    }
    fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.label.to_lowercase().contains(&query) || self.id.to_lowercase().contains(&query)
    }
}
type Picker = Entity<SelectState<SearchableVec<String>>>;

fn cluster_name(session: &ClusterSession, cx: &App) -> String {
    let name = crate::settings::store(cx)
        .read(cx)
        .preferences
        .clusters
        .get(session.id().as_str())
        .filter(|settings| !settings.alias.trim().is_empty())
        .map_or_else(
            || session.id().display_name().to_string(),
            |settings| settings.alias.clone(),
        );
    crate::sources::store(cx)
        .read(cx)
        .labels
        .get(session.id())
        .map_or(name.clone(), |source| format!("{name} · {source}"))
}
fn picker(choices: Vec<String>, selected: &str, window: &mut Window, cx: &mut App) -> Picker {
    cx.new(|cx| {
        let mut state =
            SelectState::new(SearchableVec::new(choices), None, window, cx).searchable(true);
        state.set_selected_value(&selected.to_owned(), window, cx);
        state
    })
}
fn label(text: &str, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(crate::i18n::text(text))
        .into_any_element()
}

pub(crate) struct CompareView {
    focus: FocusHandle,
    title: String,
    left: Arc<ClusterSession>,
    kind: Arc<Kind>,
    object: Arc<DynamicObject>,
    sessions: Vec<Arc<ClusterSession>>,
    cluster: Entity<SelectState<SearchableVec<ClusterChoice>>>,
    namespace_picker: Picker,
    resource_picker: Picker,
    namespace: Entity<InputState>,
    name: Entity<InputState>,
    search: Entity<InputState>,
    manual: bool,
    catalog_loading: bool,
    catalog_note: Option<String>,
    target_names: Vec<String>,
    options: Options,
    page: Page,
    mode: DiffMode,
    collapse: bool,
    wrap: bool,
    snapshots: Option<(Value, Value)>,
    changes: Arc<Vec<Change>>,
    visible: Arc<Vec<usize>>,
    expanded: Option<String>,
    change_list: ListState,
    added: usize,
    removed: usize,
    diff: Option<Entity<DiffState>>,
    loading: bool,
    preparing: bool,
    message: Option<String>,
    revision: u64,
    prepare_revision: u64,
    _task: Option<Task<()>>,
    _catalog: Option<Task<()>>,
    _namespaces: Option<Task<()>>,
    _prepare: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

pub(crate) fn open(
    left: Arc<ClusterSession>,
    kind: Arc<Kind>,
    object: Arc<DynamicObject>,
    window: &mut Window,
    cx: &mut App,
) {
    open_target(left, kind, object, None, window, cx);
}
pub(crate) fn open_target(
    left: Arc<ClusterSession>,
    kind: Arc<Kind>,
    object: Arc<DynamicObject>,
    target: Option<ObjectRef>,
    window: &mut Window,
    cx: &mut App,
) {
    let bounds = Bounds::centered(
        window.display(cx).map(|display| display.id()),
        size(px(1120.), px(760.)),
        cx,
    );
    // Open after the originating resource action has finished dispatching.
    cx.defer(move |cx| {
        if let Err(error) = comparison_window(left, kind, object, target, bounds, cx) {
            tracing::error!(%error, "could not open resource comparison window");
        }
    });
}
fn comparison_window(
    left: Arc<ClusterSession>,
    kind: Arc<Kind>,
    object: Arc<DynamicObject>,
    target: Option<ObjectRef>,
    bounds: Bounds<Pixels>,
    cx: &mut App,
) -> anyhow::Result<(AnyWindowHandle, Entity<CompareView>)> {
    gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(640.), px(540.))),
            ..TitleBar::window_options()
        },
        cx,
        move |window, cx| {
            // Mirror Beacon's default non-retained rendering in the test platform.
            #[cfg(all(test, feature = "ui-tests"))]
            window.set_view_retention(false);
            let view = comparison_view(left, kind, object, target, window, cx);
            crate::i18n::window_title(view.read(cx).title.clone(), window, cx);
            window.activate_window();
            view
        },
    )
}
fn comparison_view(
    left: Arc<ClusterSession>,
    kind: Arc<Kind>,
    object: Arc<DynamicObject>,
    target: Option<ObjectRef>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<CompareView> {
    let mut sessions: Vec<_> = cx
        .global::<SharedConnections>()
        .0
        .read(cx)
        .sessions
        .values()
        .cloned()
        .collect();
    sessions.sort_by(|a, b| a.id().cmp(b.id()));
    if !sessions.iter().any(|session| session.id() == left.id()) {
        sessions.push(left.clone());
    }
    let default = if target.is_some() {
        &left
    } else {
        sessions
            .iter()
            .find(|session| session.id() != left.id())
            .unwrap_or(&left)
    };
    let default_id = default.id().to_string();
    let namespace = target
        .as_ref()
        .and_then(|target| target.namespace.clone())
        .or_else(|| object.metadata.namespace.clone())
        .unwrap_or_default();
    let name = target.map(|target| target.name).unwrap_or_else(|| {
        if default.id() != left.id() {
            object.metadata.name.clone().unwrap_or_default()
        } else {
            String::new()
        }
    });
    let mut choices: Vec<_> = sessions
        .iter()
        .map(|session| ClusterChoice {
            label: cluster_name(session, cx),
            id: session.id().to_string(),
        })
        .collect();
    let labels: Vec<_> = choices.iter().map(|choice| choice.label.clone()).collect();
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
    let title = format!(
        "Beacon — Compare {} · {}",
        kind.resource.kind,
        object.metadata.name.as_deref().unwrap_or("")
    );
    cx.new(|cx: &mut Context<CompareView>| {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let cluster = cx.new(|cx| {
            let mut state =
                SelectState::new(SearchableVec::new(choices), None, window, cx).searchable(true);
            state.set_selected_value(&default_id, window, cx);
            state
        });
        let mut view = CompareView {
            focus,
            title,
            left,
            kind,
            object,
            sessions,
            namespace_picker: picker(vec![namespace.clone()], &namespace, window, cx),
            resource_picker: picker(vec![], "", window, cx),
            namespace: cx.new(|cx| {
                crate::i18n::input("Target namespace", window, cx).default_value(namespace)
            }),
            name: cx.new(|cx| {
                crate::i18n::input("Target resource name", window, cx).default_value(name)
            }),
            search: cx.new(|cx| crate::i18n::input("Filter changed fields or values", window, cx)),
            cluster,
            manual: false,
            catalog_loading: false,
            catalog_note: None,
            target_names: vec![],
            options: Options::default(),
            page: Page::Changes,
            mode: DiffMode::Split,
            collapse: true,
            wrap: true,
            snapshots: None,
            changes: Arc::default(),
            visible: Arc::default(),
            expanded: None,
            change_list: ListState::new(0, ListAlignment::Top, px(300.)),
            added: 0,
            removed: 0,
            diff: None,
            loading: false,
            preparing: false,
            message: None,
            revision: 0,
            prepare_revision: 0,
            _task: None,
            _catalog: None,
            _namespaces: None,
            _prepare: None,
            _subscriptions: vec![],
        };
        view._subscriptions.push(cx.subscribe_in(
            &view.cluster,
            window,
            |view, _, event: &SelectEvent<SearchableVec<ClusterChoice>>, window, cx| {
                if matches!(event, SelectEvent::Confirm(_)) {
                    view.manual = false;
                    view.invalidate();
                    view._namespaces = None;
                    let preferred = if view
                        .right_in(cx)
                        .is_some_and(|right| right.id() != view.left.id())
                    {
                        view.object.metadata.name.clone().unwrap_or_default()
                    } else {
                        String::new()
                    };
                    view.name
                        .update(cx, |input, cx| input.set_value(preferred, window, cx));
                    view.load_namespaces(window, cx);
                    view.load_targets(window, cx);
                }
            },
        ));
        view._subscriptions.push(cx.subscribe_in(
            &view.namespace_picker,
            window,
            |view, _, event: &SelectEvent<SearchableVec<String>>, window, cx| {
                if let SelectEvent::Confirm(Some(namespace)) = event {
                    view.namespace.update(cx, |input, cx| {
                        input.set_value(namespace.clone(), window, cx)
                    });
                    let preferred = view.object.metadata.name.clone().unwrap_or_default();
                    view.name
                        .update(cx, |input, cx| input.set_value(preferred, window, cx));
                    view.load_targets(window, cx);
                }
            },
        ));
        view._subscriptions.push(cx.subscribe_in(
            &view.resource_picker,
            window,
            |view, _, event: &SelectEvent<SearchableVec<String>>, window, cx| {
                if let SelectEvent::Confirm(Some(name)) = event {
                    view.name
                        .update(cx, |input, cx| input.set_value(name.clone(), window, cx));
                    view.catalog_note = None;
                    view.load(window, cx);
                }
            },
        ));
        for input in [view.namespace.clone(), view.name.clone()] {
            view._subscriptions
                .push(cx.subscribe(&input, |view, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        view.invalidate();
                        cx.notify();
                    }
                }));
        }
        view._subscriptions.push(
            cx.subscribe(&view.search, |view, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    view.filter(cx);
                    cx.notify();
                }
            }),
        );
        view.load_namespaces(window, cx);
        view.load_targets(window, cx);
        view
    })
}

impl Focusable for CompareView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl CompareView {
    fn right_in(&self, cx: &App) -> Option<Arc<ClusterSession>> {
        let id = self.cluster.read(cx).selected_value()?;
        self.sessions
            .iter()
            .find(|session| session.id().as_str() == id)
            .cloned()
    }
    fn right_kind(&self, right: &ClusterSession) -> Option<Kind> {
        right
            .discovery()
            .kinds()
            .iter()
            .filter(|kind| {
                kind.resource.group == self.kind.resource.group
                    && kind.resource.kind == self.kind.resource.kind
            })
            .min_by_key(|kind| kind.resource.version != self.kind.resource.version)
            .cloned()
    }
    fn invalidate(&mut self) {
        self.revision += 1;
        self.prepare_revision += 1;
        self._task = None;
        self._prepare = None;
        self.loading = false;
        self.preparing = false;
        self.snapshots = None;
        self.diff = None;
        self.changes = Arc::default();
        self.visible = Arc::default();
        self.change_list.reset(0);
        self.message = None;
    }
    fn load_namespaces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.kind.namespaced {
            return;
        }
        let Some(right) = self.right_in(cx) else {
            return;
        };
        let id = right.id().to_string();
        let listing = Bridge::global(cx).run_cancellable(async move {
            right
                .list_objects(WatchKey::all(beacon_kube::resources::namespace()))
                .await
        });
        self._namespaces = Some(cx.spawn_in(window, async move |this, cx| {
            let result = listing.result().await;
            let _ = this.update_in(cx, |view, window, cx| {
                if view
                    .right_in(cx)
                    .is_none_or(|right| right.id().as_str() != id)
                {
                    return;
                }
                let selected = view.namespace.read(cx).value().to_string();
                let mut names = vec![selected.clone()];
                if let Ok(Ok(objects)) = result {
                    names.extend(
                        objects
                            .iter()
                            .filter_map(|object| object.metadata.name.clone()),
                    );
                }
                names.retain(|name| !name.is_empty());
                names.sort();
                names.dedup();
                view.namespace_picker.update(cx, |picker, cx| {
                    picker.set_items(SearchableVec::new(names), window, cx);
                    picker.set_selected_value(&selected, window, cx);
                });
                cx.notify();
            });
        }));
    }
    fn load_targets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.invalidate();
        self._catalog = None;
        self.catalog_note = None;
        self.target_names.clear();
        self.resource_picker.update(cx, |picker, cx| {
            picker.set_items(SearchableVec::new(vec![]), window, cx)
        });
        let Some(right) = self.right_in(cx) else {
            return;
        };
        let Some(kind) = self.right_kind(&right) else {
            self.catalog_loading = false;
            self.message = Some(
                crate::i18n::message(
                    "messages.is_not_available_in_this_cluster_choose_another_connected_cluster",
                    &[("value0", (self.kind.resource.kind).to_string())],
                )
                .to_string(),
            );
            cx.notify();
            return;
        };
        let namespace = kind
            .namespaced
            .then(|| self.namespace.read(cx).value().trim().to_string());
        if namespace.as_ref().is_some_and(String::is_empty) {
            self.catalog_loading = false;
            self.catalog_note = Some(
                "Choose a namespace to browse its resources, or enter the target manually.".into(),
            );
            cx.notify();
            return;
        }
        let revision = self.revision;
        let same_cluster = right.id() == self.left.id();
        let source = ObjectRef::of(&self.object);
        let listing = Bridge::global(cx).run_cancellable(async move {
            right
                .list_objects(WatchKey::all(kind.resource).in_namespace(namespace))
                .await
                .map_err(|error| error.user_message())
        });
        self.catalog_loading = true;
        self._catalog = Some(cx.spawn_in(window, async move |this, cx| {
            let result = listing.result().await;
            let _ = this.update_in(cx, |view, window, cx| {
                if revision != view.revision {
                    return;
                }
                view.catalog_loading = false;
                match result {
                    Ok(Ok(objects)) => {
                        let mut names: Vec<_> = objects.iter()
                            .filter(|object| !same_cluster || ObjectRef::of(object) != source)
                            .filter_map(|object| object.metadata.name.clone()).collect();
                        names.sort();
                        names.dedup();
                        let preferred = view.name.read(cx).value().to_string();
                        let selected = if names.contains(&preferred) { preferred } else { String::new() };
                        view.target_names = names.clone();
                        view.resource_picker.update(cx, |picker, cx| {
                            picker.set_items(SearchableVec::new(names), window, cx);
                            picker.set_selected_value(&selected, window, cx);
                        });
                        view.name.update(cx, |input, cx| input.set_value(selected.clone(), window, cx));
                        if selected.is_empty() {
                            view.catalog_note = Some(if view.target_names.is_empty() {
                                crate::i18n::message("messages.no_other_resources_in_this_namespace_try_another_namespace_or_cluster", &[("value0", (view.kind.resource.kind).to_string())]).to_string()
                            } else {
                                "No same-name match. Search the resource selector and choose a target.".into()
                            });
                        } else {
                            view.load(window, cx);
                        }
                    }
                    result => {
                        let error = match result {
                            Ok(Err(error)) => error,
                            Err(error) => error.to_string(),
                            _ => unreachable!(),
                        };
                        view.catalog_note = Some(crate::i18n::message("messages.could_not_list_resources_enter_a_namespace_and_name_to_compare_directly_error", &[("error", (error).to_string())]).to_string());
                        view.manual = true;
                        if !view.name.read(cx).value().trim().is_empty() {
                            view.load(window, cx);
                        }
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.invalidate();
        self._catalog = None;
        self.catalog_loading = false;
        let Some(right) = self.right_in(cx) else {
            return;
        };
        let Some(kind) = self.right_kind(&right) else {
            self.message =
                Some("This resource type is not available in the selected cluster.".into());
            cx.notify();
            return;
        };
        let name = self.name.read(cx).value().trim().to_owned();
        let namespace = self.namespace.read(cx).value().trim().to_owned();
        if name.is_empty() || kind.namespaced && namespace.is_empty() {
            self.message =
                Some("Choose a target resource, or enter its namespace and name.".into());
            cx.notify();
            return;
        }
        self.loading = true;
        let revision = self.revision;
        let left = self.left.clone();
        let resource = self.kind.resource.clone();
        let source = ObjectRef::of(&self.object);
        let fetching = Bridge::global(cx).run_cancellable(async move {
            let (a, b) = futures::join!(
                left.get_object(resource, source.namespace, source.name),
                right.get_object(kind.resource, kind.namespaced.then_some(namespace), name)
            );
            let a = a.map_err(|error| {
                crate::i18n::message(
                    "messages.could_not_read_the_source",
                    &[("value0", (error.user_message()).to_string())],
                )
                .to_string()
            })?;
            let b = b.map_err(|error| {
                crate::i18n::message(
                    "messages.could_not_read_the_target",
                    &[("value0", (error.user_message()).to_string())],
                )
                .to_string()
            })?;
            Ok::<_, String>((
                serde_json::to_value(a).map_err(|error| error.to_string())?,
                serde_json::to_value(b).map_err(|error| error.to_string())?,
            ))
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = fetching.result().await;
            let _ = this.update(cx, |view, cx| {
                if revision != view.revision {
                    return;
                }
                view.loading = false;
                match result {
                    Ok(Ok(snapshots)) => {
                        view.snapshots = Some(snapshots);
                        view.rebuild(cx);
                    }
                    Ok(Err(error)) => view.message = Some(error),
                    Err(error) => view.message = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let Some((a, b)) = self.snapshots.clone() else {
            cx.notify();
            return;
        };
        self.prepare_revision += 1;
        let revision = self.prepare_revision;
        let options = self.options;
        self.preparing = true;
        let preparing = cx
            .background_executor()
            .spawn(async move { comparison::prepare(a, b, options) });
        self._prepare = Some(cx.spawn(async move |this, cx| {
            let prepared = preparing.await;
            let _ = this.update(cx, |view, cx| {
                if revision != view.prepare_revision {
                    return;
                }
                view.preparing = false;
                match prepared {
                    Ok(prepared) => {
                        view.changes = Arc::new(prepared.changes);
                        view.added = prepared.added;
                        view.removed = prepared.removed;
                        if let Some(diff) = &view.diff {
                            diff.update(cx, |state, cx| state.set_files(prepared.files, cx));
                        } else {
                            view.diff = Some(cx.new(|cx| {
                                DiffState::new(prepared.files, cx)
                                    .with_mode(view.mode)
                                    .with_context_lines(if view.collapse { Some(3) } else { None })
                            }));
                        }
                        view.expanded = None;
                        view.message = None;
                        view.filter(cx);
                    }
                    Err(error) => view.message = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn filter(&mut self, cx: &App) {
        let query = self.search.read(cx).value();
        self.visible = Arc::new(
            self.changes
                .iter()
                .enumerate()
                .filter(|(_, change)| change.matches(&query))
                .map(|(i, _)| i)
                .collect(),
        );
        self.change_list.reset(self.visible.len());
    }
    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.invalidate();
        self._catalog = None;
        self._namespaces = None;
        self.catalog_loading = false;
        self.catalog_note =
            Some("Comparison cancelled. Choose a target or click Compare to try again.".into());
        cx.notify();
    }
    fn target_form(&self, cx: &Context<Self>) -> AnyElement {
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .items_center()
                    .child(label("2 · Target resource", cx))
                    .child(div().flex_1())
                    .child(
                        Button::new("compare-manual")
                            .small()
                            .ghost()
                            .selected(self.manual)
                            .label(crate::i18n::text(if self.manual {
                                "Browse resources"
                            } else {
                                "Enter manually"
                            }))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.manual = !view.manual;
                                if view.manual {
                                    if view.catalog_loading || view.loading || view.preparing {
                                        view.invalidate();
                                    }
                                    view._catalog = None;
                                    view.catalog_loading = false;
                                } else {
                                    view.load_targets(window, cx);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex().gap_1().child(label("Cluster", cx)).child(
                    Select::new(&self.cluster)
                        .small()
                        .w_full()
                        .placeholder(crate::i18n::text("Choose a connected cluster"))
                        .search_placeholder(crate::i18n::text("Search connected clusters"))
                        .accessibility_label(crate::i18n::text("Target cluster")),
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .when(self.kind.namespaced, |row| {
                        row.child(
                            v_flex()
                                .gap_1()
                                .flex_1()
                                .min_w_0()
                                .child(label("Namespace", cx))
                                .child(if self.manual {
                                    Input::new(&self.namespace).small().into_any_element()
                                } else {
                                    Select::new(&self.namespace_picker)
                                        .small()
                                        .w_full()
                                        .placeholder(crate::i18n::text("Namespace"))
                                        .search_placeholder(crate::i18n::text("Search namespaces"))
                                        .accessibility_label(crate::i18n::text("Target namespace"))
                                        .into_any_element()
                                }),
                        )
                    })
                    .child(
                        v_flex()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(label("Resource name", cx))
                            .child(if self.manual {
                                Input::new(&self.name).small().into_any_element()
                            } else {
                                Select::new(&self.resource_picker)
                                    .small()
                                    .w_full()
                                    .disabled(self.catalog_loading)
                                    .placeholder(if self.catalog_loading {
                                        "Loading resources…"
                                    } else {
                                        "Choose a resource"
                                    })
                                    .search_placeholder(crate::i18n::text("Search resource names"))
                                    .accessibility_label(crate::i18n::text("Target resource"))
                                    .into_any_element()
                            }),
                    ),
            )
            .into_any_element()
    }
    fn changes_view(&self, cx: &Context<Self>) -> AnyElement {
        let rows = self.changes.clone();
        let visible = self.visible.clone();
        let expanded = self.expanded.clone();
        let weak = cx.entity().downgrade();
        div()
            .id("comparison-fields")
            .test_support()
            .relative()
            .flex_1()
            .min_size_0()
            .overflow_hidden()
            .child(
                list(self.change_list.clone(), move |index, _, cx| {
                    let change = &rows[visible[index]];
                    let open = expanded.as_ref() == Some(&change.path);
                    let key = change.path.clone();
                    let weak = weak.clone();
                    v_flex()
                        .id(("comparison-field", index))
                        .test_support()
                        .w_full()
                        .p_3()
                        .gap_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    Button::new(("expand-comparison-field", index))
                                        .small()
                                        .ghost()
                                        .icon(if open {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .accessibility_label(format!("Expand {}", change.path))
                                        .on_click(move |_, _, cx| {
                                            let _ = weak.update(cx, |view, cx| {
                                                view.expanded =
                                                    if open { None } else { Some(key.clone()) };
                                                view.change_list.remeasure();
                                                cx.notify();
                                            });
                                        }),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .child(copyable_text(
                                            format!("comparison-path-{index}"),
                                            change.path.clone(),
                                        )),
                                )
                                .child(label(change.label(), cx)),
                        )
                        .child(
                            h_flex().gap_3().items_stretch().children(
                                [(&change.before, "source"), (&change.after, "target")]
                                    .into_iter()
                                    .map(|(value, side)| {
                                        let text = Change::text(value);
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_1()
                                            .child(label(
                                                if side == "source" { "Source" } else { "Target" },
                                                cx,
                                            ))
                                            .child(
                                                div()
                                                    .id(format!(
                                                        "comparison-value-scroll-{side}-{index}"
                                                    ))
                                                    .p_2()
                                                    .rounded_md()
                                                    .bg(if side == "source" {
                                                        cx.theme().danger.opacity(0.06)
                                                    } else {
                                                        cx.theme().success.opacity(0.06)
                                                    })
                                                    .text_sm()
                                                    .font_family(
                                                        cx.theme().mono_font_family.clone(),
                                                    )
                                                    .overflow_y_scrollbar()
                                                    .when(open, |value| value.max_h(px(240.)))
                                                    .when(!open, |value| {
                                                        value.max_h(px(44.)).overflow_hidden()
                                                    })
                                                    .child(copyable_text(
                                                        format!("comparison-value-{side}-{index}"),
                                                        text,
                                                    )),
                                            )
                                    }),
                            ),
                        )
                        .into_any_element()
                })
                .size_full()
                .pr_3(),
            )
            .child(Scrollbar::vertical(&self.change_list))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{Options, normalized};
    use serde_json::json;
    #[test]
    fn runtime_fields_are_ignored_but_desired_configuration_and_secret_differences_remain() {
        let a = json!({"kind":"Deployment","metadata":{"name":"web","namespace":"dev","uid":"one","resourceVersion":"1","creationTimestamp":"yesterday"},"spec":{"template":{"spec":{"containers":[{"image":"web:v1"}]}}},"status":{"readyReplicas":1}});
        let mut b = a.clone();
        b["metadata"]["namespace"] = json!("prod");
        b["metadata"]["uid"] = json!("two");
        b["status"]["readyReplicas"] = json!(3);
        assert_eq!(
            normalized(a.clone(), Options::default()),
            normalized(b.clone(), Options::default())
        );
        b["spec"]["template"]["spec"]["containers"][0]["image"] = json!("web:v2");
        assert_ne!(
            normalized(a, Options::default()),
            normalized(b, Options::default())
        );
        let secret = json!({"kind":"Secret","data":{"password":"c2VjcmV0"}});
        let masked = normalized(secret.clone(), Options::default());
        assert!(!masked.to_string().contains("c2VjcmV0"));
        assert_eq!(
            normalized(
                secret.clone(),
                Options {
                    reveal: true,
                    ..Options::default()
                }
            ),
            secret
        );
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::connections::{Connections, SharedConnections};
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    #[::core::prelude::v1::test]
    fn same_name_match_compares_automatically_and_never_writes() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let object = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"default","uid":"one","resourceVersion":"1"},"spec":{"template":{"spec":{"containers":[{"name":"app","image":"web:v1"}]}}}});
        let mut other = object.clone();
        other["metadata"]["uid"] = json!("two");
        other["spec"]["template"]["spec"]["containers"][0]["image"] = json!("web:v2");
        let (left_api, left) = support::fixture(cx, "compare-left", vec![object.clone()]);
        let (right_api, right) = support::fixture(cx, "compare-right", vec![other]);
        cx.update(|cx| {
            let connections = cx.new(|_| Connections::default());
            connections.update(cx, |state, _| {
                state.sessions.insert(left.id().clone(), left.clone());
                state.sessions.insert(right.id().clone(), right);
            });
            cx.set_global(SharedConnections(connections));
        });
        let (window, view) = cx.update(|cx| {
            let bounds = Bounds::centered(None, size(px(1120.), px(760.)), cx);
            comparison_window(
                left,
                Arc::new(support::kind("apps", "Deployment", "deployments")),
                Arc::new(serde_json::from_value(object).unwrap()),
                None,
                bounds,
                cx,
            )
            .unwrap()
        });
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| {
                view.snapshots.is_some() && view.diff.is_some()
            })
        });
        view.read_with(cx, |view, _| {
            let (a, b) = view.snapshots.as_ref().unwrap();
            assert_ne!(
                a.pointer("/spec/template/spec/containers/0/image"),
                b.pointer("/spec/template/spec/containers/0/image")
            );
            assert!(view.page == Page::Changes);
            assert_eq!(view.changes.len(), 1);
            assert_eq!(
                view.changes[0].path,
                "spec.template.spec.containers[0].image"
            );
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("compare-yaml", cx);
            window.click("compare-unified", cx);
            window.click("compare-status", cx);
            window.render_frame(cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| !view.preparing));
        view.read_with(cx, |view, cx| {
            assert!(view.page == Page::Yaml);
            assert_eq!(view.mode, DiffMode::Unified);
            assert_eq!(
                view.diff.as_ref().unwrap().read(cx).mode(),
                DiffMode::Unified
            );
        });
        assert!(
            left_api
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(request, _)| request.starts_with("GET"))
        );
        assert!(
            right_api
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(request, _)| request.starts_with("GET"))
        );
    }
    fn deployment(name: &str, namespace: &str, image: &str) -> Value {
        json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":name,"namespace":namespace},"spec":{"replicas":2,"template":{"spec":{"containers":[{"name":"app","image":image}]}}}})
    }
    fn fixture(
        source: Value,
        targets: Vec<Value>,
        target: Option<ObjectRef>,
    ) -> (
        TestAppContext,
        AnyWindowHandle,
        Entity<CompareView>,
        beacon_kube::test_support::Fixture,
        beacon_kube::test_support::Fixture,
        tempfile::TempDir,
    ) {
        let mut cx = support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(&cx, directory.path());
        let (left_api, left) = support::fixture(&mut cx, "compare-dev", vec![source.clone()]);
        let (right_api, right) = support::fixture(&mut cx, "compare-prod", targets);
        cx.update(|cx| {
            let connections = cx.new(|_| Connections::default());
            connections.update(cx, |state, _| {
                state.sessions.insert(left.id().clone(), left.clone());
                state.sessions.insert(right.id().clone(), right);
            });
            cx.set_global(SharedConnections(connections));
        });
        let name = source["kind"].as_str().unwrap();
        let kind = match name {
            "Namespace" => support::kind("", name, "namespaces"),
            "Secret" => support::kind("", name, "secrets"),
            _ => support::kind("apps", name, "deployments"),
        };
        let (window, view) = cx.update(|cx| {
            let bounds = Bounds::centered(None, size(px(1120.), px(760.)), cx);
            comparison_window(
                left,
                Arc::new(kind),
                Arc::new(serde_json::from_value(source).unwrap()),
                target,
                bounds,
                cx,
            )
            .unwrap()
        });
        (cx, window, view, left_api, right_api, directory)
    }
    fn ready(cx: &mut TestAppContext, view: &Entity<CompareView>) {
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| view.diff.is_some() && !view.preparing)
        });
    }

    #[::core::prelude::v1::test]
    fn no_same_name_match_waits_for_explicit_selection() {
        let (mut cx, window, view, _left_api, right_api, _directory) = fixture(
            deployment("web", "default", "web:v1"),
            vec![deployment("worker", "default", "worker:v2")],
            None,
        );
        support::settle(&mut cx, |cx| {
            view.read_with(cx, |view, _| !view.catalog_loading)
        });
        view.read_with(&cx, |view, cx| {
            assert!(view.diff.is_none());
            assert!(view.name.read(cx).value().is_empty());
            assert!(
                view.catalog_note
                    .as_deref()
                    .unwrap()
                    .contains("No same-name match")
            );
        });
        assert!(
            !right_api
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(request, _)| request.contains("/deployments/worker"))
        );
        cx.update_window(window, |_, window, cx| {
            let picker = view.read(cx).resource_picker.clone();
            picker.update(cx, |picker, cx| {
                picker.set_selected_value(&"worker".into(), window, cx);
                cx.emit(SelectEvent::Confirm(Some("worker".into())));
            });
        })
        .unwrap();
        ready(&mut cx, &view);
        view.read_with(&cx, |view, _| {
            assert_eq!(
                view.snapshots.as_ref().unwrap().1["metadata"]["name"],
                "worker"
            );
            assert!(view.catalog_note.is_none());
        });
    }

    #[::core::prelude::v1::test]
    fn changing_namespace_replaces_results_and_search_filters_actual_fields() {
        let (mut cx, window, view, _left_api, _right_api, _directory) = fixture(
            deployment("web", "default", "web:v1"),
            vec![
                deployment("web", "default", "web:v2"),
                deployment("web", "staging", "web:v3"),
            ],
            None,
        );
        ready(&mut cx, &view);
        cx.update_window(window, |_, window, cx| {
            let picker = view.read(cx).namespace_picker.clone();
            picker.update(cx, |picker, cx| {
                picker.set_selected_value(&"staging".into(), window, cx);
                cx.emit(SelectEvent::Confirm(Some("staging".into())));
            });
        })
        .unwrap();
        cx.run_until_parked();
        ready(&mut cx, &view);
        view.read_with(&cx, |view, _| {
            assert_eq!(
                view.snapshots.as_ref().unwrap().1["metadata"]["namespace"],
                "staging"
            );
            assert_eq!(view.changes.len(), 1);
            assert_eq!(Change::text(&view.changes[0].after), "web:v3");
        });
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.search
                    .update(cx, |input, cx| input.set_value("image", window, cx));
                view.filter(cx);
                assert_eq!(view.visible.len(), 1);
                view.search
                    .update(cx, |input, cx| input.set_value("replicas", window, cx));
                view.filter(cx);
                assert!(view.visible.is_empty());
                cx.notify();
            });
        })
        .unwrap();
    }

    #[::core::prelude::v1::test]
    fn explicit_pair_uses_same_cluster_and_excludes_source_from_targets() {
        let (mut cx, window, view, left_api, _right_api, _directory) = fixture(
            deployment("web", "default", "web:v1"),
            vec![],
            Some(ObjectRef {
                name: "worker".into(),
                namespace: Some("default".into()),
            }),
        );
        left_api.put(deployment("worker", "default", "worker:v2"));
        // Repeat the catalog read after adding the second selected resource.
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| view.load_targets(window, cx))
        })
        .unwrap();
        ready(&mut cx, &view);
        view.read_with(&cx, |view, cx| {
            assert_eq!(view.right_in(cx).unwrap().id(), view.left.id());
            assert_eq!(view.target_names, vec!["worker"]);
            assert_eq!(
                view.snapshots.as_ref().unwrap().1["metadata"]["name"],
                "worker"
            );
        });
    }

    #[::core::prelude::v1::test]
    fn cluster_scoped_resources_do_not_require_a_namespace() {
        let a = json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"default","labels":{"team":"dev"}}});
        let mut b = a.clone();
        b["metadata"]["labels"]["team"] = json!("prod");
        let (mut cx, _window, view, _left_api, right_api, _directory) = fixture(a, vec![b], None);
        ready(&mut cx, &view);
        view.read_with(&cx, |view, _| {
            assert_eq!(view.changes[0].path, "metadata.labels.team")
        });
        assert!(
            right_api
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(request, _)| request.contains("/api/v1/namespaces/default"))
        );
        assert!(
            !right_api
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(request, _)| request.contains("/namespaces//"))
        );
    }

    #[::core::prelude::v1::test]
    fn cancelled_loading_stays_empty_and_can_be_retried() {
        let (mut cx, window, view, _left_api, _right_api, _directory) = fixture(
            deployment("web", "default", "web:v1"),
            vec![deployment("web", "default", "web:v2")],
            None,
        );
        view.update(&mut cx, |view, cx| view.cancel(cx));
        cx.run_until_parked();
        view.read_with(&cx, |view, _| {
            assert!(!view.catalog_loading && !view.loading && !view.preparing);
            assert!(view.diff.is_none() && view.snapshots.is_none());
            assert!(view.message.is_none());
            assert!(view.catalog_note.as_deref().unwrap().contains("cancelled"));
        });
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| view.load_targets(window, cx))
        })
        .unwrap();
        ready(&mut cx, &view);
    }

    #[::core::prelude::v1::test]
    fn comparison_controls_fit_narrow_windows_and_both_themes() {
        let (mut cx, window, view, _left_api, _right_api, _directory) = fixture(
            deployment("web", "default", "web:v1"),
            vec![deployment("web", "default", "web:v2")],
            None,
        );
        ready(&mut cx, &view);
        for appearance in [
            crate::settings::Appearance::Light,
            crate::settings::Appearance::Dark,
        ] {
            cx.update(|cx| {
                crate::settings::store(cx)
                    .update(cx, |store, _| store.preferences.appearance = appearance);
                crate::settings::apply(None, cx);
            });
            for width in [640., 880., 1280.] {
                for page in [Page::Changes, Page::Yaml] {
                    cx.update_window(window, |_, window, cx| {
                        window.resize(size(px(width), px(900.)));
                        window.bounds_changed(cx);
                        view.update(cx, |view, cx| {
                            view.page = page;
                            cx.notify();
                        });
                        window.render_frame(cx);
                    })
                    .unwrap();
                    cx.run_until_parked();
                    cx.update_window(window, |_, window, cx| {
                        window.render_frame(cx);
                        let panel = window.find("resource-comparison").bounds();
                        assert!(
                            panel.right() <= px(width),
                            "comparison window exceeds {width}: {panel:?}"
                        );
                        for id in [
                            "compare-manual",
                            "compare-status",
                            "compare-metadata",
                            "compare-resources",
                            "compare-yaml",
                        ] {
                            let control = window.find(id).bounds();
                            assert!(
                                control.right() <= panel.right(),
                                "{id} exceeds panel at {width}: {control:?}"
                            );
                        }
                        let result = window
                            .find(if page == Page::Changes {
                                "comparison-fields"
                            } else {
                                "comparison-yaml"
                            })
                            .bounds();
                        assert!(result.size.height > px(200.));
                        assert!(result.bottom() <= panel.bottom());
                    })
                    .unwrap();
                }
            }
        }
    }
    #[::core::prelude::v1::test]
    fn comparison_windows_open_independently_and_close_without_affecting_the_origin() {
        use gpui_kit::component::WindowExt as _;
        let (mut cx, first, view, left_api, _right_api, _directory) = fixture(
            deployment("web", "default", "web:v1"),
            vec![deployment("web", "default", "web:v2")],
            None,
        );
        ready(&mut cx, &view);
        left_api.put(deployment("worker", "default", "worker:v2"));
        let origin = support::window(&mut cx);
        cx.update_window(origin, |_, window, cx| {
            let view = view.read(cx);
            open_target(
                view.left.clone(),
                view.kind.clone(),
                view.object.clone(),
                Some(ObjectRef {
                    name: "worker".into(),
                    namespace: Some("default".into()),
                }),
                window,
                cx,
            );
            assert!(!window.has_active_dialog(cx));
        })
        .unwrap();
        cx.run_until_parked();
        let second = cx
            .windows()
            .into_iter()
            .find(|window| *window != first && *window != origin)
            .unwrap();
        cx.update_window(second, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("resource-comparison-window").visible());
            assert!(!window.has_active_dialog(cx));
        })
        .unwrap();
        assert_eq!(
            VisualTestContext::from_window(second, &cx)
                .window_title()
                .as_deref(),
            Some("Beacon — Compare Deployment · web")
        );
        let search_focus = view.read_with(&cx, |view, cx| view.search.read(cx).focus_handle(cx));
        let weak = view.downgrade();
        drop(view);
        let shortcut = if cfg!(target_os = "macos") {
            "cmd-w"
        } else {
            "ctrl-w"
        };
        cx.update_window(first, |_, window, cx| {
            window.render_frame(cx);
            // The close action must also bubble from a focused text input.
            search_focus.focus(window, cx);
            window.render_frame(cx);
            window.press(shortcut, cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(!cx.windows().contains(&first));
        assert!(weak.upgrade().is_none());
        assert!(cx.windows().contains(&origin));
        assert!(cx.windows().contains(&second));
        assert!(!cx.has_pending_prompt());
        cx.update_window(second, |_, window, cx| {
            window.render_frame(cx);
            window.press(shortcut, cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(!cx.windows().contains(&second));
        assert_eq!(cx.windows(), vec![origin]);
    }
}
