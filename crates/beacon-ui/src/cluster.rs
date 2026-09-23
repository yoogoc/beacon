//! One connected cluster, on screen.
//!
//! Owns the session, so dropping this view disconnects: every watch it started
//! is aborted with it. That is what makes switching contexts a matter of
//! replacing one entity with another rather than unwinding state by hand.
//!
//! The view is generic over resource kinds in the same way the layers under it
//! are. Nothing here knows what a Pod is: picking a kind in the sidebar changes
//! a `WatchKey` and a `ColumnSet`, and everything else follows.

use std::{sync::Arc, time::Duration};

use beacon_columns::ColumnSet;
use beacon_kube::{
    Applied, ClusterSession, Health, Kind, ObjectRef, Operation, ResourceStore, Rules, WatchKey,
    resources,
};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::resizable::{ResizableState, resizable_panel, v_resizable};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::table::{TableEvent, TableState};
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use nucleo_matcher::Matcher;

use crate::bridge::{Bridge, drain_into};
use crate::catalog::{Catalog, Entry};
use crate::detail::{DetailClosed, DetailView};
use crate::palette::Sources;
use crate::prompt::{Ask, Prompt, PromptEvent};
use crate::table::ResourceTable;
use crate::theme::{BeaconTheme as _, Tone};

/// The namespace picker's entry for "do not scope at all". A namespace cannot
/// contain a space, so this can never collide with a real one.
const ALL_NAMESPACES: &str = "All namespaces";

/// How often the Age column is repainted. Ages are relative, so a table that
/// nothing is changing still has to advance.
const CLOCK: Duration = Duration::from_secs(1);

/// What the last write operation said, for the toolbar.
enum Outcome {
    Running(String),
    Done(String),
    Failed(String),
}

pub struct ClusterView {
    session: Arc<ClusterSession>,
    health: Health,

    /// The kinds this cluster serves, grouped for the sidebar.
    catalog: Catalog,
    /// The kind currently on screen.
    kind: Option<Arc<Kind>>,
    matcher: Matcher,

    /// `None` means every namespace.
    namespace: Option<String>,
    /// The namespaces that exist, kept live by its own watch. Whether a
    /// namespace disappeared while the user was looking at it is exactly the
    /// kind of thing a client should notice.
    namespaces: ResourceStore,
    /// The picker's current items. Kept so that a namespace merely changing --
    /// a label edit, a status update -- does not rebuild the menu, which would
    /// wipe whatever the user had typed into its search box.
    namespace_names: Vec<SharedString>,
    namespace_picker: Entity<SelectState<SearchableVec<SharedString>>>,

    sidebar_search: Entity<InputState>,
    sidebar_query: String,
    row_search: Entity<InputState>,

    table: Entity<TableState<ResourceTable>>,

    /// The panel for the selected object. `None` means nothing is selected, or
    /// the user closed it.
    detail: Option<Entity<DetailView>>,
    /// Kept across selections so that closing and reopening the panel does not
    /// reset the split the user dragged.
    split: Entity<ResizableState>,

    /// What this user may do in the current namespace. `None` until the answer
    /// arrives; see [`crate::actions`].
    rules: Option<Arc<Rules>>,
    /// An operation waiting on a confirmation or a number.
    prompt: Option<Entity<Prompt>>,
    /// What the last write said, for the toolbar.
    outcome: Option<Outcome>,

    // Dropping any of these stops the work behind it.
    _health: Task<()>,
    _namespaces: Task<()>,
    _objects: Task<()>,
    _columns: Option<Task<()>>,
    _operation: Option<Task<()>>,
    _rules: Option<Task<()>>,
    _clock: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ClusterView {
    pub fn new(
        session: Arc<ClusterSession>,
        namespace: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let catalog = Catalog::new(session.discovery().kinds());
        let kind = catalog.default_kind();

        let namespace_picker = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(vec![SharedString::from(ALL_NAMESPACES)]),
                Some(IndexPath::default()),
                window,
                cx,
            )
            .searchable(true)
        });

        let sidebar_search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Filter resources")
                .clean_on_escape()
        });
        let row_search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search")
                .clean_on_escape()
        });

        let table = cx.new(|cx| {
            TableState::new(ResourceTable::new(ColumnSet::fallback(true)), window, cx)
                .row_selectable(true)
        });
        let split = cx.new(|_| ResizableState::default());

        let mut this = Self {
            health: Health::Connecting,
            catalog,
            kind: None,
            matcher: crate::catalog::matcher(),
            namespace,
            namespaces: ResourceStore::new(),
            namespace_names: vec![SharedString::from(ALL_NAMESPACES)],
            namespace_picker,
            sidebar_search,
            sidebar_query: String::new(),
            row_search,
            table,
            detail: None,
            split,
            rules: None,
            prompt: None,
            outcome: None,
            _health: Task::ready(()),
            _namespaces: Task::ready(()),
            _objects: Task::ready(()),
            _columns: None,
            _operation: None,
            _rules: None,
            _clock: Task::ready(()),
            _subscriptions: Vec::new(),
            session,
        };

        this.listen(window, cx);
        this.load_rules(window, cx);
        this.watch_health(window, cx);
        this.watch_namespaces(window, cx);
        this.start_clock(cx);

        if let Some(kind) = kind {
            this.show(kind, window, cx);
        }

        this
    }

    pub fn session(&self) -> &ClusterSession {
        &self.session
    }

    pub fn health(&self) -> &Health {
        &self.health
    }

    /// What the table is showing, and out of how many.
    pub fn counts(&self, cx: &App) -> (usize, usize) {
        let table = self.table.read(cx).delegate();
        (table.len(), table.total())
    }

    pub fn kind(&self) -> Option<&Kind> {
        self.kind.as_deref()
    }

    /// Everything the command palette can offer about this cluster.
    ///
    /// A snapshot: the palette is open for seconds, and a list that shifted
    /// under the highlighted row would confirm the wrong thing.
    pub fn sources(&self, cx: &App) -> Sources {
        Sources {
            kinds: self
                .catalog
                .sections()
                .iter()
                .flat_map(|(_, entries)| entries)
                .map(|entry| entry.kind.clone())
                .collect(),
            // Sorted here rather than relied upon: the store is a map, and a
            // menu in hash order looks like a bug.
            namespaces: {
                let mut names: Vec<String> = self
                    .namespaces
                    .iter()
                    .map(|(key, _)| key.name.clone())
                    .collect();
                names.sort();
                names
            },
            clusters: Vec::new(),
            objects: self.table.read(cx).delegate().keys(),
            operations: self.operations(cx),
            current_kind: self.kind.as_ref().map(|kind| kind.resource.kind.clone()),
        }
    }

    /// What can be done to the selected object right now.
    ///
    /// Empty when nothing is selected: an action with no target is a menu item
    /// that cannot mean anything.
    pub fn operations(&self, cx: &App) -> Vec<crate::actions::Choice> {
        let (Some(kind), Some(selected)) = (self.kind.clone(), self.selected(cx)) else {
            return Vec::new();
        };

        let replicas = self
            .table
            .read(cx)
            .delegate()
            .object(&selected)
            .and_then(|object| object.data.get("spec")?.get("replicas")?.as_i64())
            .unwrap_or(1) as i32;

        crate::actions::available(&kind, self.rules.as_deref(), replicas)
    }

    /// Starts an operation, asking first when it needs asking.
    pub fn start(&mut self, operation: Operation, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(kind), Some(target)) = (self.kind.clone(), self.selected(cx)) else {
            return;
        };

        // Restarting is disruptive but not destructive, and it is exactly what
        // the menu item says. Deleting has no undo; scaling needs a number.
        if matches!(operation, Operation::Restart) {
            self.run(operation, window, cx);
            return;
        }

        let ask = Ask {
            operation,
            target,
            kind: SharedString::from(kind.resource.kind.clone()),
        };
        let prompt = cx.new(|cx| Prompt::new(ask, window, cx));

        cx.subscribe_in(
            &prompt,
            window,
            |view, _, event: &PromptEvent, window, cx| {
                view.prompt = None;
                if let PromptEvent::Confirmed(operation) = event {
                    view.run(operation.clone(), window, cx);
                }
                cx.notify();
            },
        )
        .detach();

        self.prompt = Some(prompt);
        cx.notify();
    }

    /// Sends one operation, and reports what came back.
    fn run(&mut self, operation: Operation, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(kind), Some(target)) = (self.kind.clone(), self.selected(cx)) else {
            return;
        };

        let described = operation.describe();
        self.outcome = Some(Outcome::Running(described.clone()));
        cx.notify();

        let session = self.session.clone();
        let resource = kind.resource.clone();
        let running = Bridge::global(cx).run(async move {
            session
                .run(
                    operation,
                    resource,
                    target.namespace.clone(),
                    target.name.clone(),
                    None,
                    false,
                )
                .await
        });

        self._operation = Some(cx.spawn_in(window, async move |this, cx| {
            let result = running.await;
            let _ = this.update(cx, |view, cx| {
                view.outcome = Some(match result {
                    Ok(Ok(Applied::Ok(_))) => Outcome::Done(described),
                    Ok(Ok(Applied::Conflict(conflict))) => Outcome::Failed(conflict.summary()),
                    Ok(Err(error)) => Outcome::Failed(error.to_string()),
                    Err(error) => Outcome::Failed(error.to_string()),
                });
                cx.notify();
            });
        }));
    }

    /// Asks what this user may do in the namespace now on screen.
    fn load_rules(&mut self, window: &Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let namespace = self.namespace.clone();
        self.rules = None;

        let asking = Bridge::global(cx).run(async move { session.rules(namespace).await });

        self._rules = Some(cx.spawn_in(window, async move |this, cx| {
            let Ok(rules) = asking.await else { return };
            let _ = this.update(cx, |view, cx| {
                view.rules = Some(rules);
                cx.notify();
            });
        }));
    }

    /// Switches to a kind, as the sidebar or the palette asks.
    pub fn show_kind(&mut self, kind: Arc<Kind>, window: &mut Window, cx: &mut Context<Self>) {
        self.show(kind, window, cx);
    }

    /// Scopes to a namespace, or to all of them with `None`.
    pub fn set_namespace(
        &mut self,
        namespace: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.namespace == namespace {
            return;
        }
        tracing::info!(namespace = ?namespace, "scoping to namespace");
        self.namespace = namespace;
        self.refresh_namespace_picker(window, cx);
        self.watch_objects(window, cx);
        self.load_columns(window, cx);
        // Permissions are per namespace, so the answer for the last one says
        // nothing about this one.
        self.load_rules(window, cx);
        cx.notify();
    }

    /// Reveals one object: clears whatever is hiding it, selects its row and
    /// scrolls to it, and opens the detail panel on it.
    pub fn reveal(&mut self, key: &ObjectRef, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_filter(window, cx);

        let row = self.table.read(cx).delegate().row_of(key);
        let Some(row) = row else {
            // The palette searched the whole store, so this means the object
            // disappeared between opening the palette and confirming.
            tracing::debug!(object = %key, "no longer in the list");
            return;
        };

        self.table.update(cx, |state, cx| {
            state.set_selected_row(row, cx);
            state.scroll_to_row(row, cx);
        });
        self.open_detail(row, window, cx);
    }

    /// Shows or hides the detail panel without changing the selection.
    pub fn toggle_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.detail.is_some() {
            self.detail = None;
        } else if let Some(row) = self.table.read(cx).selected_row() {
            self.open_detail(row, window, cx);
        }
        cx.notify();
    }

    /// Empties the search box.
    pub fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.row_search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.table.update(cx, |state, cx| {
            if state.delegate_mut().set_filter("") {
                cx.notify();
            }
        });
        cx.notify();
    }

    /// The selected object, for the palette's copy command.
    pub fn selected(&self, cx: &App) -> Option<ObjectRef> {
        let table = self.table.read(cx);
        table.delegate().key_at(table.selected_row()?).cloned()
    }

    /// Switches the table to another kind.
    ///
    /// The list starts immediately with whatever columns are known without
    /// asking the cluster; a kind that publishes its own gets them a moment
    /// later, without re-listing.
    fn show(&mut self, kind: Arc<Kind>, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .kind
            .as_ref()
            .is_some_and(|current| current.gvk() == kind.gvk())
        {
            return;
        }

        tracing::info!(kind = %kind.display_name(), "showing");
        self.kind = Some(kind);
        // The panel is about an object of the previous kind.
        self.detail = None;
        self.watch_objects(window, cx);
        self.load_columns(window, cx);
        cx.notify();
    }

    /// Opens the detail panel on a row, reusing the existing panel when it is
    /// already showing that object.
    fn open_detail(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self.kind.clone() else {
            return;
        };

        let table = self.table.read(cx);
        let Some(key) = table.delegate().key_at(row).cloned() else {
            return;
        };
        let Some(object) = table.delegate().object(&key).cloned() else {
            return;
        };

        if let Some(detail) = &self.detail
            && detail.read(cx).target() == &key
        {
            return;
        }

        let session = self.session.clone();
        let rules = self.rules.clone();
        let detail = cx.new(|cx| DetailView::new(session, kind, object, rules, window, cx));
        cx.subscribe(&detail, |view, _, _: &DetailClosed, cx| {
            view.detail = None;
            cx.notify();
        })
        .detach();

        self.detail = Some(detail);
        cx.notify();
    }

    /// Hands the detail panel the object the table now holds, so that Overview
    /// tracks a changing pod rather than freezing at the moment it was opened.
    fn refresh_detail(&mut self, cx: &mut Context<Self>) {
        let Some(detail) = self.detail.clone() else {
            return;
        };
        let key = detail.read(cx).target().clone();
        let Some(object) = self.table.read(cx).delegate().object(&key).cloned() else {
            // The object was deleted. The panel keeps showing what it had,
            // which is more useful than a panel that empties itself the
            // instant something disappears.
            return;
        };
        let rules = self.rules.clone();
        detail.update(cx, |detail, cx| {
            detail.refresh(object, rules, cx);
        });
    }

    /// (Re)starts the watch for the current kind and namespace.
    ///
    /// Dropping the previous task drops its subscription, which is what
    /// releases the old watch -- there is no separate unsubscribe to forget.
    fn watch_objects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self.kind.clone() else {
            return;
        };

        let columns = self.columns(&kind, None);
        self.table.update(cx, |state, cx| {
            state.delegate_mut().reset(columns);
            // The table lays its columns out once and caches the result, so a
            // different set of them is not visible until it is told to look
            // again. Without this the header keeps the previous kind's shape
            // and the extra columns simply do not appear.
            state.refresh(cx);
            // Without this, scrolling halfway down one list and switching to a
            // shorter one lands on a blank stretch of table.
            state.scroll_to_row(0, cx);
            cx.notify();
        });

        let key = WatchKey::all(kind.resource.clone()).in_namespace(self.scope(&kind));
        let subscription = self.session.subscribe(key);

        self._objects = drain_into(
            cx,
            subscription,
            |view, batch, _window, cx| {
                view.table.update(cx, |state, cx| {
                    state.delegate_mut().apply(batch);
                    cx.notify();
                });
                view.refresh_detail(cx);
            },
            window,
        );
    }

    /// Asks the cluster for the kind's own printer columns, and applies them if
    /// it has any.
    fn load_columns(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self.kind.clone() else {
            return;
        };

        let wanted = kind.gvk();
        let session = self.session.clone();
        let resource = kind.resource.clone();
        let fetching =
            Bridge::global(cx).run(async move { session.printer_columns(resource).await });

        self._columns = Some(cx.spawn_in(window, async move |this, cx| {
            let Ok(Some(printer_columns)) = fetching.await else {
                return;
            };

            let _ = this.update(cx, |view, cx| {
                // The user may have moved on while this was in flight.
                let Some(kind) = view.kind.clone().filter(|kind| kind.gvk() == wanted) else {
                    return;
                };

                let columns = view.columns(&kind, Some(&printer_columns));
                view.table.update(cx, |state, cx| {
                    state.delegate_mut().set_columns(columns);
                    state.refresh(cx);
                    cx.notify();
                });
            });
        }));
    }

    /// The namespace to watch in: none at all for a cluster-scoped kind, which
    /// is also what stops its table growing a Namespace column it can never
    /// fill.
    fn scope(&self, kind: &Kind) -> Option<String> {
        self.namespace.clone().filter(|_| kind.namespaced)
    }

    fn columns(&self, kind: &Kind, printer_columns: Option<&serde_json::Value>) -> ColumnSet {
        ColumnSet::resolve(
            &kind.resource.group,
            &kind.resource.kind,
            kind.namespaced && self.scope(kind).is_none(),
            printer_columns,
        )
    }

    /// Follows the session's health. `tokio::sync::watch` is a plain channel
    /// with no I/O of its own, so it can be polled on this thread.
    fn watch_health(&mut self, window: &Window, cx: &mut Context<Self>) {
        let mut health = self.session.health();
        self._health = cx.spawn_in(window, async move |this, cx| {
            loop {
                let current = health.borrow_and_update().clone();
                let updated = this.update(cx, |view, cx| {
                    if view.health != current {
                        view.health = current;
                        cx.notify();
                    }
                });
                if updated.is_err() || health.changed().await.is_err() {
                    break;
                }
            }
        });
    }

    fn watch_namespaces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let subscription = self
            .session
            .subscribe(WatchKey::all(resources::namespace()));

        self._namespaces = drain_into(
            cx,
            subscription,
            |view, batch, window, cx| {
                if view.namespaces.apply_batch(batch) {
                    view.refresh_namespace_picker(window, cx);
                    cx.notify();
                }
            },
            window,
        );
    }

    fn start_clock(&mut self, cx: &mut Context<Self>) {
        self._clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CLOCK).await;
                let updated = this.update(cx, |view, cx| {
                    view.table.update(cx, |state, cx| {
                        state.delegate_mut().tick();
                        cx.notify();
                    });
                });
                if updated.is_err() {
                    break;
                }
            }
        });
    }

    fn listen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let namespace_picker = cx.subscribe_in(
            &self.namespace_picker.clone(),
            window,
            |view, _, event: &SelectEvent<SearchableVec<SharedString>>, window, cx| {
                let SelectEvent::Confirm(selected) = event;
                let namespace = match selected.as_deref() {
                    None | Some(ALL_NAMESPACES) => None,
                    Some(namespace) => Some(namespace.to_string()),
                };
                view.set_namespace(namespace, window, cx);
            },
        );

        let table = cx.subscribe_in(
            &self.table.clone(),
            window,
            |view, _, event: &TableEvent, window, cx| {
                // A single click is enough: in a list of pods, picking a row is
                // always a request to look at it.
                if let TableEvent::SelectRow(row) = event {
                    view.open_detail(*row, window, cx);
                }
            },
        );

        let sidebar_search = cx.subscribe(
            &self.sidebar_search.clone(),
            |view, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    view.sidebar_query = state.read(cx).value().to_string();
                    cx.notify();
                }
            },
        );

        let row_search = cx.subscribe(
            &self.row_search.clone(),
            |view, state, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let query = state.read(cx).value().to_string();
                view.table.update(cx, |table, cx| {
                    if table.delegate_mut().set_filter(&query) {
                        table.scroll_to_row(0, cx);
                        cx.notify();
                    }
                });
                cx.notify();
            },
        );

        self._subscriptions = vec![namespace_picker, sidebar_search, row_search, table];
    }

    fn refresh_namespace_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut names: Vec<SharedString> = self
            .namespaces
            .iter()
            .map(|(key, _)| SharedString::from(key.name.clone()))
            .collect();
        names.sort();
        names.insert(0, SharedString::from(ALL_NAMESPACES));

        if names == self.namespace_names {
            return;
        }
        self.namespace_names = names.clone();

        let selected = self
            .namespace
            .clone()
            .map(SharedString::from)
            .unwrap_or_else(|| SharedString::from(ALL_NAMESPACES));

        self.namespace_picker.update(cx, |picker, cx| {
            picker.set_items(SearchableVec::new(names), window, cx);
            picker.set_selected_value(&selected, window, cx);
        });
    }

    // MARK: rendering

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.kind.as_ref().map(|kind| kind.gvk());

        // A query replaces the sections with a flat ranked list: with a hundred
        // kinds, the answer to "where is it" should not be "in one of seven
        // collapsed groups".
        let sections: Vec<(SharedString, Vec<Entry>)> = if self.sidebar_query.is_empty() {
            self.catalog
                .sections()
                .iter()
                .map(|(category, entries)| (SharedString::from(category.label()), entries.clone()))
                .collect()
        } else {
            let matches = self.catalog.search(&self.sidebar_query, &mut self.matcher);
            vec![(SharedString::from("Matches"), matches)]
        };

        let open_by_default: Vec<bool> = if !self.sidebar_query.is_empty() {
            vec![true]
        } else {
            self.catalog
                .sections()
                .iter()
                .map(|(category, entries)| {
                    // A section also opens when it holds what is on screen, so
                    // that the selection is never hidden inside a closed group.
                    category.starts_open()
                        || entries
                            .iter()
                            .any(|entry| current.as_ref() == Some(&entry.kind.gvk()))
                })
                .collect()
        };

        let menu = SidebarMenu::new().children(sections.into_iter().zip(open_by_default).map(
            |((label, entries), open)| {
                SidebarMenuItem::new(label)
                    .click_to_toggle(true)
                    .default_open(open)
                    .children(entries.into_iter().map(|entry| {
                        let selected = current.as_ref() == Some(&entry.kind.gvk());
                        let kind = entry.kind.clone();
                        SidebarMenuItem::new(entry.label)
                            .active(selected)
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.show(kind.clone(), window, cx);
                            }))
                    }))
            },
        ));

        Sidebar::new("resources")
            .collapsible(false)
            .w(px(232.))
            .header(
                div()
                    .w_full()
                    .px_2()
                    .py_1()
                    .child(Input::new(&self.sidebar_search).small()),
            )
            .child(SidebarGroup::new("").child(menu))
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (shown, total) = self.counts(cx);
        let title = self
            .kind
            .as_ref()
            .map(|kind| kind.resource.kind.clone())
            .unwrap_or_else(|| "Nothing selected".into());

        // `12 of 340` while filtering, `340` otherwise: the fraction is only
        // information when something is being hidden.
        let count = if shown == total {
            total.to_string()
        } else {
            format!("{shown} of {total}")
        };

        h_flex()
            .w_full()
            .px_3()
            .py_1p5()
            .gap_3()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_sm()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(count),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    // What the last write said. It lives here rather than in a
                    // toast because the thing it is about is on screen.
                    .children(self.outcome.as_ref().map(|outcome| {
                        let (tone, text) = match outcome {
                            Outcome::Running(what) => (Tone::Progressing, format!("{what}…")),
                            Outcome::Done(what) => (Tone::Healthy, format!("{what} — done")),
                            Outcome::Failed(why) => (Tone::Critical, why.clone()),
                        };
                        div()
                            .max_w(px(360.))
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().tone(tone))
                            .child(text)
                    }))
                    .child(
                        div()
                            .w(px(220.))
                            .child(Input::new(&self.row_search).small()),
                    )
                    .children(self.kind.as_ref().filter(|kind| kind.namespaced).map(|_| {
                        Select::new(&self.namespace_picker)
                            .small()
                            .menu_width(px(280.))
                            .menu_max_h(px(420.))
                            .search_placeholder("Filter namespaces")
                            .accessibility_label("Namespace")
                    })),
            )
    }
}

impl Render for ClusterView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // `TableState` renders itself; it is the virtualised table, not a
        // delegate that something else draws.
        let table = div()
            .size_full()
            .overflow_hidden()
            .child(self.table.clone())
            .into_any_element();

        let body = match self.detail.clone() {
            None => table,
            Some(detail) => v_resizable("detail-split")
                .with_state(&self.split)
                .child(
                    resizable_panel()
                        .size(px(440.))
                        .size_range(px(120.)..px(2000.))
                        .child(table),
                )
                .child(
                    resizable_panel()
                        .size(px(300.))
                        .size_range(px(120.)..px(2000.))
                        .child(
                            div()
                                .size_full()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .child(detail),
                        ),
                )
                .into_any_element(),
        };

        h_flex()
            .size_full()
            .items_start()
            .child(self.render_sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .h_full()
                    .overflow_hidden()
                    .border_l_1()
                    .border_color(cx.theme().border)
                    .child(self.render_toolbar(cx))
                    .child(div().flex_1().overflow_hidden().child(body)),
            )
    }
}
