//! One connected cluster, on screen.
//!
//! Owns the session, so dropping this view disconnects: every watch it started
//! is aborted with it. That is what makes switching contexts a matter of
//! replacing one entity with another rather than unwinding state by hand.
//!
//! The view is generic over resource kinds in the same way the layers under it
//! are. Nothing here knows what a Pod is: picking a kind in the sidebar changes
//! a `WatchKey` and a `ColumnSet`, and everything else follows.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use beacon_columns::ColumnSet;
use beacon_kube::{
    Applied, ClusterSession, Forward, Health, Kind, ObjectRef, Operation, Release, ResourceStore,
    Rules, WatchKey, resources,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::resizable::{ResizableState, resizable_panel, v_resizable};
use gpui_kit::component::sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::table::{TableEvent, TableState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
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

/// Where a cluster opens when its context does not name a namespace. The same
/// one `kubectl` falls back to.
const DEFAULT_NAMESPACE: &str = "default";

/// How often the Age column is repainted. Ages are relative, so a table that
/// nothing is changing still has to advance.
const CLOCK: Duration = Duration::from_secs(1);

/// How often usage is re-read. metrics-server itself only recomputes every
/// fifteen seconds or so, so asking faster costs requests and shows the same
/// numbers.
const METRICS_INTERVAL: Duration = Duration::from_secs(10);

/// What the main area is showing.
///
/// Helm releases and port forwards are cluster-wide lists rather than resource
/// kinds, so they cannot live in the catalog with the kinds -- but they belong
/// in the same sidebar, because that is where somebody looks for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Objects,
    Releases,
    Forwards,
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Objects => "Objects",
            Self::Releases => "Helm Releases",
            Self::Forwards => "Port Forwards",
        }
    }
}

/// What the Helm list has to show.
enum Releases {
    Unopened,
    Loading,
    Ready(Vec<Release>),
    Failed(String),
}

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

    /// The namespaces on screen. Empty means every namespace, which is the
    /// one case that is a single cluster-wide watch rather than a watch each.
    scoped_to: BTreeSet<String>,
    /// The namespaces that exist, kept live by its own watch. Whether a
    /// namespace disappeared while the user was looking at it is exactly the
    /// kind of thing a client should notice.
    namespaces: ResourceStore,
    /// Whether the namespace menu is open. Controlled rather than left to the
    /// popover, because picking one namespace has to close it.
    namespace_menu_open: bool,
    /// The names the picker offers, sorted. Recomputed from the store only
    /// when it actually changed, so a label edit on some namespace does not
    /// rebuild the menu under the user's cursor.
    namespace_names: Vec<SharedString>,

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
    mode: Mode,
    releases: Releases,

    /// Whether this view's tab is the one on screen.
    ///
    /// A hidden tab keeps its watches -- that is what makes coming back to it
    /// instant, and what a tab is for -- but stops the two timers that exist
    /// only to repaint: see [`Self::set_visible`].
    visible: bool,

    // Dropping any of these stops the work behind it.
    _health: Task<()>,
    _namespaces: Task<()>,
    /// One per watched namespace -- see [`Self::watch_scopes`].
    _objects: Vec<Task<()>>,
    _columns: Option<Task<()>>,
    _operation: Option<Task<()>>,
    _rules: Option<Task<()>>,
    _metrics: Task<()>,
    _forward: Option<Task<()>>,
    _releases: Option<Task<()>>,
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
            // kubectl falls back to `default` when the context does not name
            // a namespace, and opening on every namespace of a busy cluster is
            // thousands of rows nobody asked for.
            scoped_to: BTreeSet::from([namespace.unwrap_or_else(|| DEFAULT_NAMESPACE.to_string())]),
            namespaces: ResourceStore::new(),
            namespace_menu_open: false,
            namespace_names: Vec::new(),
            sidebar_search,
            sidebar_query: String::new(),
            row_search,
            table,
            detail: None,
            split,
            rules: None,
            prompt: None,
            outcome: None,
            mode: Mode::Objects,
            releases: Releases::Unopened,
            visible: true,
            _health: Task::ready(()),
            _namespaces: Task::ready(()),
            _objects: Vec::new(),
            _columns: None,
            _operation: None,
            _rules: None,
            _metrics: Task::ready(()),
            _forward: None,
            _releases: None,
            _clock: Task::ready(()),
            _subscriptions: Vec::new(),
            session,
        };

        this.listen(window, cx);
        this.load_rules(window, cx);
        this.watch_metrics(window, cx);
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

    /// What this view is showing, for its tab's label.
    pub fn title(&self) -> SharedString {
        match self.mode {
            Mode::Objects => match &self.kind {
                Some(kind) => SharedString::from(kind.resource.kind.clone()),
                None => SharedString::from("Objects"),
            },
            mode => SharedString::from(mode.label()),
        }
    }

    /// Tells the view whether its tab is the one on screen.
    ///
    /// The watches keep running either way. They are the expensive thing to
    /// rebuild and the reason a background tab is worth keeping at all -- and
    /// because the registry refcounts them, two tabs on the same cluster and
    /// kind share one. What stops is the pair of timers that only exist to
    /// repaint: the one-second clock behind the Age column, and the
    /// ten-second metrics poll, which is a request to the cluster that nobody
    /// is looking at the answer to.
    pub fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;

        if visible {
            self.start_clock(cx);
            self.watch_metrics(window, cx);
        } else {
            self._clock = Task::ready(());
            self._metrics = Task::ready(());
        }
        cx.notify();
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
            ports: self.selected_ports(cx),
            current_kind: self.kind.as_ref().map(|kind| kind.resource.kind.clone()),
        }
    }

    /// The ports the selected pod declares.
    fn selected_ports(&self, cx: &App) -> Vec<u16> {
        let Some(kind) = self.kind.as_ref() else {
            return Vec::new();
        };
        if kind.resource.kind != "Pod" || !kind.resource.group.is_empty() {
            return Vec::new();
        }

        let Some(selected) = self.selected(cx) else {
            return Vec::new();
        };
        self.table
            .read(cx)
            .delegate()
            .object(&selected)
            .map(|object| crate::actions::ports(&object.data))
            .unwrap_or_default()
    }

    /// Opens a port forward to the selected pod and shows the forward list.
    pub fn start_forward(&mut self, remote_port: u16, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.selected(cx) else {
            return;
        };
        let Some(namespace) = selected.namespace.clone() else {
            return;
        };

        self.outcome = Some(Outcome::Running(format!("Forwarding port {remote_port}")));
        self.mode = Mode::Forwards;
        cx.notify();

        let session = self.session.clone();
        let pod = selected.name.clone();
        // A local port of 0 asks the operating system for a free one. Picking
        // a number ourselves means a clash with whatever is already listening.
        let opening = Bridge::global(cx)
            .run(async move { session.forward(namespace, pod, remote_port, 0).await });

        self._forward = Some(cx.spawn_in(window, async move |this, cx| {
            let result = opening.await;
            let _ = this.update(cx, |view, cx| {
                view.outcome = Some(match result {
                    Ok(Ok(forward)) => Outcome::Done(format!("Forwarding {}", forward.address())),
                    Ok(Err(error)) => Outcome::Failed(error.to_string()),
                    Err(error) => Outcome::Failed(error.to_string()),
                });
                cx.notify();
            });
        }));
    }

    /// Stops one forward.
    pub fn close_forward(&mut self, id: beacon_kube::ForwardId, cx: &mut Context<Self>) {
        self.session.close_forward(id);
        cx.notify();
    }

    /// Switches the main area between the object table and the cluster-wide
    /// lists that are not resource kinds.
    pub fn show_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if mode == Mode::Releases {
            self.load_releases(window, cx);
        }
        cx.notify();
    }

    fn load_releases(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.releases = Releases::Loading;
        let session = self.session.clone();
        let namespace = self.only_namespace();
        let listing = Bridge::global(cx).run(async move { session.helm_releases(namespace).await });

        self._releases = Some(cx.spawn_in(window, async move |this, cx| {
            let result = listing.await;
            let _ = this.update(cx, |view, cx| {
                view.releases = match result {
                    Ok(Ok(releases)) => Releases::Ready(releases),
                    Ok(Err(error)) => Releases::Failed(error.to_string()),
                    Err(error) => Releases::Failed(error.to_string()),
                };
                cx.notify();
            });
        }));
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
    ///
    /// One namespace only. With several on screen the answer would have to be
    /// per object rather than per view, and until it is, this asks
    /// cluster-wide and the preflight degrades to "assume allowed" -- the same
    /// thing it has always done for "All namespaces".
    fn load_rules(&mut self, window: &Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let namespace = self.only_namespace();
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
        let scope = match namespace {
            Some(namespace) => BTreeSet::from([namespace]),
            None => BTreeSet::new(),
        };
        self.rescope(scope, window, cx);
    }

    /// Adds or removes one namespace, leaving the rest of the selection alone.
    ///
    /// Unticking the last one lands on every namespace rather than on nothing:
    /// a table that can only be empty is not a state worth being able to reach.
    pub fn toggle_namespace(
        &mut self,
        namespace: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut scope = self.scoped_to.clone();
        if !scope.remove(namespace) {
            scope.insert(namespace.to_string());
        }
        self.rescope(scope, window, cx);
    }

    /// Points the view at a set of namespaces. Empty is every namespace.
    fn rescope(&mut self, scope: BTreeSet<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.scoped_to == scope {
            return;
        }
        tracing::info!(namespaces = ?scope, "scoping");
        self.scoped_to = scope;
        self.watch_objects(window, cx);
        self.load_columns(window, cx);
        // Permissions are per namespace, so the answer for the last scope says
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
        self.mode = Mode::Objects;
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

        // Replaced wholesale: dropping the old tasks drops the old
        // subscriptions, which is what releases watches on namespaces that are
        // no longer on screen.
        self._objects = self
            .watch_scopes(&kind)
            .into_iter()
            .map(|namespace| {
                let key = WatchKey::all(kind.resource.clone()).in_namespace(namespace.clone());
                let subscription = self.session.subscribe(key);

                drain_into(
                    cx,
                    subscription,
                    move |view, batch, _window, cx| {
                        let namespace = namespace.clone();
                        view.table.update(cx, |state, cx| {
                            state.delegate_mut().apply_from(namespace.as_deref(), batch);
                            cx.notify();
                        });
                        view.refresh_detail(cx);
                    },
                    window,
                )
            })
            .collect();
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
    /// The watches one kind needs, one entry each.
    ///
    /// `None` is a cluster-wide watch, and is the only option for a
    /// cluster-scoped kind. Several namespaces are several watches rather than
    /// one cluster-wide watch filtered down, because multi-select exists
    /// largely for people whose RBAC is namespaced: a cluster-wide list would
    /// be refused outright for exactly those users.
    fn watch_scopes(&self, kind: &Kind) -> Vec<Option<String>> {
        if !kind.namespaced || self.scoped_to.is_empty() {
            return vec![None];
        }
        self.scoped_to.iter().cloned().map(Some).collect()
    }

    /// The one namespace everything is scoped to, if there is exactly one.
    ///
    /// The requests that take a single namespace -- the permission preflight,
    /// the Helm listing -- use this and fall back to cluster-wide, which is
    /// what they already did for "All namespaces".
    fn only_namespace(&self) -> Option<String> {
        match self.scoped_to.len() {
            1 => self.scoped_to.iter().next().cloned(),
            _ => None,
        }
    }

    fn columns(&self, kind: &Kind, printer_columns: Option<&serde_json::Value>) -> ColumnSet {
        // The Namespace column earns its place as soon as the rows can come
        // from more than one.
        let mixed = kind.namespaced && self.scoped_to.len() != 1;
        ColumnSet::resolve(
            &kind.resource.group,
            &kind.resource.kind,
            mixed,
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
            |view, batch, _window, cx| {
                if view.namespaces.apply_batch(batch) && view.refresh_namespace_names() {
                    cx.notify();
                }
            },
            window,
        );
    }

    /// Re-reads CPU and memory on a timer.
    ///
    /// Usage is the one thing here that is only interesting when it is
    /// current, so it is polled rather than watched -- metrics-server has no
    /// watch endpoint to use even if we wanted one.
    fn watch_metrics(&mut self, window: &Window, cx: &mut Context<Self>) {
        let session = self.session.clone();

        self._metrics = cx.spawn_in(window, async move |this, cx| {
            loop {
                let reading = match cx.update(|_, cx| {
                    let session = session.clone();
                    Bridge::global(cx).run(async move { session.metrics().await })
                }) {
                    Ok(reading) => reading,
                    Err(_) => return,
                };

                if let Ok(metrics) = reading.await {
                    let updated = this.update(cx, |view, cx| {
                        view.table.update(cx, |state, cx| {
                            if state.delegate_mut().set_metrics(metrics) {
                                cx.notify();
                            }
                        });
                    });
                    if updated.is_err() {
                        return;
                    }
                }

                cx.background_executor().timer(METRICS_INTERVAL).await;
            }
        });
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

        self._subscriptions = vec![sidebar_search, row_search, table];
    }

    /// Keeps the picker's list in step with the namespaces the cluster has.
    ///
    /// Returns whether it changed, so that a namespace merely being updated --
    /// a label edit, a status tick -- does not repaint a menu the user has
    /// open.
    fn refresh_namespace_names(&mut self) -> bool {
        let mut names: Vec<SharedString> = self
            .namespaces
            .iter()
            .map(|(key, _)| SharedString::from(key.name.clone()))
            .collect();
        names.sort();

        if names == self.namespace_names {
            return false;
        }
        self.namespace_names = names;
        true
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

        let mode = self.mode;
        let tools = SidebarMenu::new().children([Mode::Releases, Mode::Forwards].map(|item| {
            SidebarMenuItem::new(item.label())
                .active(mode == item)
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.show_mode(item, window, cx);
                }))
        }));

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
            .child(SidebarGroup::new("Cluster tools").child(tools))
            .child(SidebarGroup::new("").child(menu))
    }

    fn render_releases(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows: AnyElement = match &self.releases {
            Releases::Unopened | Releases::Loading => self.notice("Reading releases…", cx),
            Releases::Failed(error) => self.notice(error.clone(), cx),
            Releases::Ready(releases) if releases.is_empty() => {
                self.notice("No Helm releases in this scope.", cx)
            }
            Releases::Ready(releases) => div()
                .id("releases")
                .size_full()
                .overflow_y_scroll()
                .child(v_flex().w_full().children(releases.iter().map(|release| {
                    let tone = if release.status == "deployed" {
                        Tone::Healthy
                    } else if release.status.starts_with("pending") {
                        Tone::Progressing
                    } else {
                        Tone::Warning
                    };

                    h_flex()
                        .w_full()
                        .px_3()
                        .py_2()
                        .gap_3()
                        .items_center()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .text_sm()
                        .child(div().w(px(220.)).truncate().child(release.name.clone()))
                        .child(
                            div()
                                .w(px(160.))
                                .truncate()
                                .text_color(cx.theme().muted_foreground)
                                .child(release.namespace.clone()),
                        )
                        .child(
                            div()
                                .w(px(110.))
                                .text_color(cx.theme().tone(tone))
                                .child(release.status.clone()),
                        )
                        .child(
                            div()
                                .w(px(80.))
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("rev {}", release.revision)),
                        )
                        .child(div().flex_1().truncate().child(release.chart.clone()))
                        .child(
                            div()
                                .w(px(120.))
                                .truncate()
                                .text_color(cx.theme().muted_foreground)
                                .child(release.app_version.clone()),
                        )
                })))
                .into_any_element(),
        };

        rows
    }

    fn render_forwards(&self, cx: &mut Context<Self>) -> AnyElement {
        let forwards: Vec<Forward> = self.session.forwards();

        if forwards.is_empty() {
            return self.notice(
                "Nothing is being forwarded. Select a pod and run “Forward port …” from ⌘K.",
                cx,
            );
        }

        div()
            .id("forwards")
            .size_full()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .children(forwards.into_iter().map(|forward| {
                        let id = forward.id;
                        h_flex()
                            .w_full()
                            .px_3()
                            .py_2()
                            .gap_3()
                            .items_center()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .text_sm()
                            .child(
                                div()
                                    .w(px(170.))
                                    .font_family("monospace")
                                    .text_color(cx.theme().tone(Tone::Healthy))
                                    .child(forward.address()),
                            )
                            .child(div().flex_1().truncate().child(format!(
                                "{}/{}:{}",
                                forward.namespace, forward.pod, forward.remote_port
                            )))
                            .child(
                                div()
                                    .w(px(120.))
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(match forward.connections {
                                        0 => "idle".to_string(),
                                        1 => "1 connection".to_string(),
                                        many => format!("{many} connections"),
                                    }),
                            )
                            .child(
                                Button::new(SharedString::from(format!("close-forward-{id}")))
                                    .xsmall()
                                    .ghost()
                                    .label("Stop")
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.close_forward(id, cx);
                                    })),
                            )
                    })),
            )
            .into_any_element()
    }

    fn notice(&self, message: impl Into<SharedString>, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .size_full()
            .p_6()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(message.into())
            .into_any_element()
    }

    /// What the picker's button says.
    fn scope_label(&self) -> SharedString {
        match self.scoped_to.len() {
            0 => SharedString::from(ALL_NAMESPACES),
            1 => SharedString::from(self.scoped_to.iter().next().cloned().unwrap_or_default()),
            many => SharedString::from(format!("{many} namespaces")),
        }
    }

    /// The namespace picker: a checkbox per namespace, and one for "all".
    ///
    /// Not a `Select`: that component picks one of a list, and the point here
    /// is several. The popover builds its contents on every render with an
    /// `App` rather than this view's `Context`, so the handlers go back
    /// through a weak handle -- which also stops an open popover from keeping
    /// a closed tab's view alive.
    ///
    /// There is no search box in here. The list scrolls, and `#` in the
    /// palette is the fuzzy way to jump to one namespace; this is the way to
    /// tick several.
    /// The namespace picker.
    ///
    /// Two click targets per row, and the difference between them is the whole
    /// design: **the tick box adds and removes, the name picks that one and
    /// nothing else**. Multi-select is what you get for reaching for a
    /// checkbox, so the ordinary case -- one namespace, chosen by name --
    /// stays a single click that also closes the menu. Everywhere else,
    /// including `#` in the palette, is single-select as it always was.
    ///
    /// Not a `Select`: that component picks one of a list, and this has to do
    /// both. The contents are built with an `App` rather than this view's
    /// `Context`, so the handlers go back through a weak handle -- which also
    /// stops an open menu from keeping a closed tab's view alive.
    ///
    /// There is no search box. The list scrolls, and `#` in the palette is the
    /// fuzzy way to find one by name.
    fn render_namespace_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let selected = self.scoped_to.clone();
        let names = self.namespace_names.clone();

        let toggling = view.clone();
        let opening = view.clone();

        Popover::new("namespace-picker")
            .open(self.namespace_menu_open)
            .on_open_change(move |open, _, cx| {
                let open = *open;
                let _ = opening.update(cx, |view, cx| {
                    view.namespace_menu_open = open;
                    cx.notify();
                });
            })
            .trigger(
                Button::new("namespace-picker-trigger")
                    .small()
                    .outline()
                    .label(self.scope_label())
                    .tooltip("Which namespaces the table shows"),
            )
            .content(move |_, _, cx| {
                let everything = selected.is_empty();
                let muted = cx.theme().muted_foreground;

                let rows = names.iter().map(|name| {
                    let ticked = selected.contains(name.as_ref());

                    let box_view = toggling.clone();
                    let toggled = name.clone();
                    let tick = Checkbox::new(SharedString::from(format!("ns-tick-{name}")))
                        .checked(ticked)
                        .tooltip("Add or remove this namespace")
                        .on_click(move |_, window, cx| {
                            let toggled = toggled.to_string();
                            let _ = box_view.update(cx, |view, cx| {
                                view.toggle_namespace(&toggled, window, cx);
                            });
                        });

                    let name_view = toggling.clone();
                    let only = name.clone();
                    let label = div()
                        .id(SharedString::from(format!("ns-only-{name}")))
                        .flex_1()
                        .truncate()
                        .cursor_pointer()
                        .child(name.clone())
                        .on_click(move |_, window, cx| {
                            let only = only.to_string();
                            let _ = name_view.update(cx, |view, cx| {
                                view.set_namespace(Some(only), window, cx);
                                view.namespace_menu_open = false;
                                cx.notify();
                            });
                        });

                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(tick)
                        .child(label)
                });

                let all_view = toggling.clone();
                let all = div()
                    .id("ns-all")
                    .w_full()
                    .cursor_pointer()
                    .font_weight(if everything {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    })
                    .child(ALL_NAMESPACES)
                    .on_click(move |_, window, cx| {
                        let _ = all_view.update(cx, |view, cx| {
                            view.set_namespace(None, window, cx);
                            view.namespace_menu_open = false;
                            cx.notify();
                        });
                    });

                v_flex()
                    .w(px(260.))
                    // Bounded *and* allowed to shrink. A flex child's automatic
                    // minimum size is its content, which beats `max_h` -- so
                    // without `min_h_0` a long list ignores the cap, grows past
                    // the menu and is simply clipped, with no way to scroll to
                    // the rest of it.
                    .min_h_0()
                    .max_h(px(420.))
                    .gap_1()
                    .child(all)
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("Tick to add, click a name for just that one"),
                    )
                    .child(
                        div()
                            .id("namespace-list")
                            .min_h_0()
                            .flex_1()
                            .overflow_y_scroll()
                            .child(v_flex().gap_1p5().children(rows)),
                    )
            })
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (shown, total) = self.counts(cx);
        let title = match self.mode {
            Mode::Objects => self
                .kind
                .as_ref()
                .map(|kind| kind.resource.kind.clone())
                .unwrap_or_else(|| "Nothing selected".into()),
            other => other.label().to_string(),
        };

        // `12 of 340` while filtering, `340` otherwise: the fraction is only
        // information when something is being hidden. The other modes count
        // their own rows -- showing the pod count above a list of Helm
        // releases is worse than showing nothing.
        let count = match self.mode {
            Mode::Objects if shown == total => total.to_string(),
            Mode::Objects => format!("{shown} of {total}"),
            Mode::Releases => match &self.releases {
                Releases::Ready(releases) => releases.len().to_string(),
                _ => String::new(),
            },
            Mode::Forwards => self.session.forwards().len().to_string(),
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
                    .children(
                        self.kind
                            .as_ref()
                            .filter(|kind| kind.namespaced)
                            .map(|_| self.render_namespace_picker(cx)),
                    ),
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

        let table = match self.mode {
            Mode::Objects => table,
            Mode::Releases => self.render_releases(cx),
            Mode::Forwards => self.render_forwards(cx),
        };

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
