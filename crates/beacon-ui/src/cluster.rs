//! One connected cluster, on screen.
//!
//! Owns the session, so dropping this view disconnects: every watch it started
//! is aborted with it. That is what makes switching contexts a matter of
//! replacing one entity with another rather than unwinding state by hand.
//!
//! The view is generic over resource kinds in the same way the layers under it
//! are. Nothing here knows what a Pod is: each view watches one selected kind,
//! and the sidebar can either change it or open another view explicitly.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

use beacon_columns::ColumnSet;
use beacon_kube::{
    Applied, ClusterSession, DeleteTarget, Delta, Forward, Health, Kind, ObjectRef, Operation,
    Release, ResourceStore, Rules, WatchKey, resources,
};
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::sidebar::SidebarMenuItem;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::table::{TableEvent, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use nucleo_matcher::Matcher;

use crate::bridge::{Bridge, drain_into};
use crate::catalog::{Catalog, Entry};
use crate::create::{CreateEvent, CreateView};
use crate::detail::{DetailClosed, DetailTab, DetailView, OwnerRequested};
use crate::filters::{Field, OptionSearch};
use crate::palette::Sources;
use crate::pod_tools::{PodToolTab, PodToolsClosed, PodToolsView};
use crate::prompt::{Ask, Prompt, PromptEvent};
use crate::table::ResourceTable;
use crate::theme::{BeaconTheme as _, Tone};

mod list_settings;

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
    is_eks: bool,
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
    /// Whether a resource field picker is open.
    filter_menu_open: Option<Field>,
    /// The open picker searches its choices, independently of resource filters.
    picker_search: Entity<InputState>,
    label_menu_open: bool,
    label_input: Entity<InputState>,
    label_error: Option<String>,

    sidebar_search: Entity<InputState>,
    sidebar_query: String,
    row_search: Entity<InputState>,

    table: Entity<TableState<ResourceTable>>,
    list_settings: list_settings::ListSettings,

    /// The panel for the selected object. `None` means nothing is selected, or
    /// the user closed it.
    detail: Option<Entity<DetailView>>,
    /// An owner jump waits for the target kind's initial list before selecting.
    pending_reveal: Option<ObjectRef>,
    /// Kept across selections so that closing and reopening the panel does not
    /// reset the split the user dragged.
    split: Entity<ResizableState>,
    /// Pod streams and terminals have their own lifetime and bottom split.
    pod_tools: Option<Entity<PodToolsView>>,
    pod_tools_split: Entity<ResizableState>,
    debug_dialogs: Vec<WeakEntity<crate::debug_container::DebugView>>,

    /// What this user may do in the current namespace. `None` until the answer
    /// arrives; see [`crate::actions`].
    rules: Option<Arc<Rules>>,
    /// An operation waiting on a confirmation or a number.
    prompt: Option<Entity<Prompt>>,
    /// An editable manifest bound to this cluster and resource kind.
    creation: Option<Entity<CreateView>>,
    /// What the last write said, for the toolbar.
    outcome: Option<Outcome>,
    bulk_deleting: bool,
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
    _bulk_operation: Option<Task<()>>,
    _rules: Option<Task<()>>,
    _metrics: Task<()>,
    _forward: Option<Task<()>>,
    _releases: Option<Task<()>>,
    _clock: Task<()>,
    _subscriptions: Vec<Subscription>,
}

/// Only navigation changes need to repaint the window-level cluster tree and
/// tab labels. Table ticks and watch updates stay within the cluster view.
pub(crate) struct NavigationChanged;

impl EventEmitter<NavigationChanged> for ClusterView {}

/// A resource selection carries the current namespace scope and whether the
/// user explicitly requested another tab.
pub(crate) struct ResourceRequested {
    pub kind: Arc<Kind>,
    pub scope: BTreeSet<String>,
    pub new_tab: bool,
    pub target: Option<ObjectRef>,
}

impl EventEmitter<ResourceRequested> for ClusterView {}

impl ClusterView {
    pub(crate) fn pending_edits(&self, cx: &App) -> Vec<String> {
        let mut edits = Vec::new();
        if self
            .debug_dialogs
            .iter()
            .filter_map(WeakEntity::upgrade)
            .any(|view| view.read(cx).busy())
        {
            edits.push(format!(
                "{}: debug container operation",
                self.session.id().display_name()
            ));
        }
        if self
            .creation
            .as_ref()
            .is_some_and(|view| view.read(cx).has_pending_edits(cx))
        {
            edits.push(format!(
                "{}: new resource draft",
                self.session.id().display_name()
            ));
        }
        if let Some(detail) = &self.detail
            && detail.read(cx).has_pending_edits(cx)
        {
            let target = detail.read(cx).target();
            edits.push(format!(
                "{}: {}/{}",
                self.session.id().display_name(),
                target.namespace.as_deref().unwrap_or("cluster"),
                target.name
            ));
        }
        edits
    }
    pub(crate) fn focus_pending_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = &self.creation
            && view.read(cx).has_pending_edits(cx)
        {
            view.update(cx, |view, cx| view.focus_editor(window, cx));
        } else if let Some(detail) = &self.detail {
            detail.update(cx, |view, cx| view.focus_pending_edits(window, cx));
        }
    }
    pub(crate) fn has_active_shell(&self, cx: &App) -> bool {
        self.pod_tools
            .as_ref()
            .is_some_and(|view| view.read(cx).has_active_session(cx))
            || self
                .debug_dialogs
                .iter()
                .filter_map(WeakEntity::upgrade)
                .any(|view| view.read(cx).has_active_terminal(cx))
    }

    pub fn new(
        session: Arc<ClusterSession>,
        namespace: Option<String>,
        initial_kind: Option<Arc<Kind>>,
        initial_scope: Option<BTreeSet<String>>,
        is_eks: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let catalog = Catalog::new(session.discovery().kinds());
        let kind = initial_kind;

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
        let label_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("app=web,env in (dev,prod)"));
        let picker_search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search options")
                .clean_on_escape()
        });

        let table = cx.new(|cx| {
            TableState::new(ResourceTable::new(ColumnSet::fallback(true)), window, cx)
                .row_selectable(true)
        });
        let split = cx.new(|_| ResizableState::default());

        let mut this = Self {
            is_eks,
            health: Health::Connecting,
            catalog,
            kind: None,
            matcher: crate::catalog::matcher(),
            // kubectl falls back to `default` when the context does not name
            // a namespace, and opening on every namespace of a busy cluster is
            // thousands of rows nobody asked for.
            scoped_to: initial_scope.unwrap_or_else(|| {
                BTreeSet::from([namespace.unwrap_or_else(|| DEFAULT_NAMESPACE.to_string())])
            }),
            namespaces: ResourceStore::new(),
            namespace_menu_open: false,
            namespace_names: Vec::new(),
            filter_menu_open: None,
            picker_search,
            label_menu_open: false,
            label_input,
            label_error: None,
            sidebar_search,
            sidebar_query: String::new(),
            row_search,
            table,
            list_settings: list_settings::ListSettings::new(window, cx),
            detail: None,
            pending_reveal: None,
            split,
            pod_tools: None,
            pod_tools_split: cx.new(|_| ResizableState::default()),
            debug_dialogs: vec![],
            rules: None,
            prompt: None,
            creation: None,
            outcome: None,
            bulk_deleting: false,
            mode: Mode::Objects,
            releases: Releases::Unopened,
            visible: true,
            _health: Task::ready(()),
            _namespaces: Task::ready(()),
            _objects: Vec::new(),
            _columns: None,
            _operation: None,
            _bulk_operation: None,
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
        this.watch_health(window, cx);
        this.watch_namespaces(window, cx);

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
                None => SharedString::from("Cluster"),
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

        if visible && !self.is_idle() {
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

    /// Enough navigation to rebuild a tab after its session is replaced.
    pub(crate) fn navigation(&self) -> (Option<Arc<Kind>>, BTreeSet<String>, Mode) {
        (self.kind.clone(), self.scoped_to.clone(), self.mode)
    }

    pub(crate) fn view_filters(&self, cx: &App) -> crate::table::ViewFilters {
        self.table.read(cx).delegate().view_filters()
    }

    pub(crate) fn restore_filters(
        &mut self,
        filters: crate::table::ViewFilters,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.table.update(cx, |table, cx| {
            table.delegate_mut().restore_filters(filters);
            cx.notify();
        });
        let table = self.table.read(cx).delegate();
        let name = table.filter().to_string();
        let labels = table.label_filter().to_string();
        self.row_search
            .update(cx, |input, cx| input.set_value(name, window, cx));
        self.label_input
            .update(cx, |input, cx| input.set_value(labels, window, cx));
        cx.notify();
    }

    pub fn kind(&self) -> Option<&Kind> {
        self.kind.as_deref()
    }

    /// Snapshot used to build a row's menu without borrowing its table again.
    pub(crate) fn menu_context(&self) -> Option<(Arc<Kind>, Option<Arc<Rules>>)> {
        Some((self.kind.clone()?, self.rules.clone()))
    }

    pub(crate) fn debug_for(
        &mut self,
        target: &ObjectRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(object) = self.table.read(cx).delegate().object(target).cloned() {
            let view = crate::debug_container::open(self.session.clone(), object, window, cx);
            self.debug_dialogs.retain(|view| view.upgrade().is_some());
            self.debug_dialogs.push(view.downgrade());
        }
    }

    pub(crate) fn aggregate_for(
        &self,
        target: &ObjectRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(object) = self.table.read(cx).delegate().object(target) {
            match beacon_kube::aggregate_logs::workload_selector(object) {
                Ok(selector) => {
                    crate::aggregate_logs::open(
                        self.session.clone(),
                        vec![target.namespace.clone()],
                        selector,
                        window,
                        cx,
                    );
                }
                Err(error) => window.push_notification(error, cx),
            }
        }
    }

    fn aggregate_filtered(&self, window: &mut Window, cx: &mut Context<Self>) {
        let scopes = if self.scoped_to.is_empty() {
            vec![None]
        } else {
            self.scoped_to.iter().cloned().map(Some).collect()
        };
        crate::aggregate_logs::open(
            self.session.clone(),
            scopes,
            self.table.read(cx).delegate().label_filter().into(),
            window,
            cx,
        );
    }

    pub(crate) fn compare_for(
        &self,
        target: &ObjectRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let (Some(kind), Some(object)) = (
            self.kind.clone(),
            self.table.read(cx).delegate().object(target).cloned(),
        ) {
            crate::resource_compare::open(self.session.clone(), kind, object, window, cx);
        }
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
            cluster_aliases: Default::default(),
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
        self.start_forward_for(selected, remote_port, window, cx);
    }

    pub(crate) fn start_forward_for(
        &mut self,
        target: ObjectRef,
        remote_port: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(namespace) = target.namespace.clone() else {
            return;
        };

        self.outcome = Some(Outcome::Running(format!("Forwarding port {remote_port}")));
        self.mode = Mode::Forwards;
        cx.notify();

        let session = self.session.clone();
        let pod = target.name.clone();
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
        cx.emit(NavigationChanged);
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
        let Some(target) = self.selected(cx) else {
            return;
        };
        self.start_for(target, operation, window, cx);
    }

    /// Runs a row action against the object that was right-clicked, even if
    /// selection or sort order changes while a confirmation prompt is open.
    pub(crate) fn start_for(
        &mut self,
        target: ObjectRef,
        operation: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(kind) = self.kind.clone() else {
            return;
        };

        // Restarting is disruptive but not destructive, and it is exactly what
        // the menu item says. Deleting has no undo; scaling needs a number.
        if matches!(operation, Operation::Restart) {
            self.run_for(target, operation, window, cx);
            return;
        }

        let ask = Ask {
            operation,
            target: target.clone(),
            kind: SharedString::from(kind.resource.kind.clone()),
            bulk_targets: None,
        };
        let prompt = cx.new(|cx| Prompt::new(ask, window, cx));

        cx.subscribe_in(
            &prompt,
            window,
            move |view, _, event: &PromptEvent, window, cx| {
                view.prompt = None;
                if let PromptEvent::Confirmed(operation) = event {
                    view.run_for(target.clone(), operation.clone(), window, cx);
                }
                cx.notify();
            },
        )
        .detach();

        self.prompt = Some(prompt);
        cx.notify();
    }

    fn start_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self.kind.clone().filter(|kind| kind.supports("create")) else {
            return;
        };
        if self.mode != Mode::Objects || self.creation.is_some() || self.prompt.is_some() {
            return;
        }
        let namespace = self.only_namespace();
        let session = self.session.clone();
        let creation =
            cx.new(|cx| CreateView::new(session, kind.clone(), namespace.as_deref(), window, cx));
        cx.subscribe_in(
            &creation,
            window,
            move |view, _, event: &CreateEvent, window, cx| {
                view.creation = None;
                if let CreateEvent::Created(object) = event {
                    let target = ObjectRef::of(object);
                    view.outcome = Some(Outcome::Done(format!(
                        "Created {} {target}",
                        kind.resource.kind
                    )));
                    if view.shows_kind(&kind) {
                        if let Some(namespace) = &target.namespace
                            && !view.scoped_to.is_empty()
                            && !view.scoped_to.contains(namespace)
                        {
                            view.rescope(BTreeSet::from([namespace.clone()]), window, cx);
                        }
                        view.table.update(cx, |table, cx| {
                            table.delegate_mut().finish_loading();
                            table
                                .delegate_mut()
                                .apply(vec![Delta::Upsert(Arc::new((**object).clone()))]);
                            cx.notify();
                        });
                        view.reveal(&target, window, cx);
                    }
                }
                cx.notify();
            },
        )
        .detach();
        self.creation = Some(creation);
        cx.notify();
    }

    /// Confirms a fixed snapshot of checked objects before deleting them.
    pub fn start_delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self.kind.clone() else {
            return;
        };
        let targets = self.table.read(cx).delegate().selected_targets();
        if targets.is_empty() || !self.may_delete() || self.bulk_deleting {
            return;
        }
        let ask = Ask::bulk_delete(
            SharedString::from(kind.resource.kind.clone()),
            targets
                .iter()
                .map(|target| target.reference.clone())
                .collect(),
        );
        let prompt = cx.new(|cx| Prompt::new(ask, window, cx));
        cx.subscribe_in(
            &prompt,
            window,
            move |view, _, event: &PromptEvent, window, cx| {
                view.prompt = None;
                if matches!(event, PromptEvent::Confirmed(Operation::Delete)) {
                    view.run_delete_many(kind.clone(), targets.clone(), window, cx);
                }
                cx.notify();
            },
        )
        .detach();
        self.prompt = Some(prompt);
        cx.notify();
    }

    fn may_delete(&self) -> bool {
        self.kind.as_ref().is_some_and(|kind| {
            self.rules.as_deref().is_none_or(|rules| {
                rules.allows("delete", &kind.resource.group, &kind.resource.plural)
            })
        })
    }

    fn run_delete_many(
        &mut self,
        kind: Arc<Kind>,
        targets: Vec<DeleteTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = targets.len();
        self.bulk_deleting = true;
        self.outcome = Some(Outcome::Running(format!("Deleting {count} resources")));
        cx.notify();
        let session = self.session.clone();
        let resource = kind.resource.clone();
        let running =
            Bridge::global(cx).run(async move { session.delete_many(resource, targets).await });
        self._bulk_operation = Some(cx.spawn_in(window, async move |this, cx| {
            let result = running.await;
            let _ = this.update(cx, |view, cx| {
                view.bulk_deleting = false;
                view.outcome = Some(match result {
                    Ok(results) => {
                        let deleted: Vec<DeleteTarget> = results
                            .iter()
                            .filter(|(_, result)| result.is_ok())
                            .map(|(target, _)| target.clone())
                            .collect();
                        view.table.update(cx, |table, cx| {
                            table.delegate_mut().remove_selected(&deleted);
                            cx.notify();
                        });
                        let succeeded = results.iter().filter(|(_, result)| result.is_ok()).count();
                        let failed = results.len() - succeeded;
                        if failed == 0 {
                            Outcome::Done(format!("Deleted {succeeded} resources"))
                        } else {
                            let first = results
                                .iter()
                                .find_map(|(target, result)| {
                                    result
                                        .as_ref()
                                        .err()
                                        .map(|error| format!("{}: {error}", target.reference))
                                })
                                .unwrap_or_default();
                            Outcome::Failed(format!(
                                "Deleted {succeeded}; {failed} failed. {first}"
                            ))
                        }
                    }
                    Err(error) => Outcome::Failed(error.to_string()),
                });
                cx.notify();
            });
        }));
    }

    /// Sends one operation, and reports what came back.
    fn run_for(
        &mut self,
        target: ObjectRef,
        operation: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(kind) = self.kind.clone() else {
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
                view.refresh_detail(cx);
                cx.notify();
            });
        }));
    }

    fn request_resource(&self, kind: Arc<Kind>, new_tab: bool, cx: &mut Context<Self>) {
        cx.emit(ResourceRequested {
            kind,
            scope: self.scoped_to.clone(),
            new_tab,
            target: None,
        });
    }

    /// A connected cluster waiting for an explicit resource selection.
    pub(crate) fn is_idle(&self) -> bool {
        self.mode == Mode::Objects && self.kind.is_none()
    }

    /// Normal selection reuses the existing tab for this cluster and kind.
    pub(crate) fn select_resource(&self, kind: Arc<Kind>, cx: &mut Context<Self>) {
        self.request_resource(kind, false, cx);
    }

    /// The sidebar context menu always creates another tab.
    pub(crate) fn open_resource(&self, kind: Arc<Kind>, cx: &mut Context<Self>) {
        self.request_resource(kind, true, cx);
    }

    pub(crate) fn shows_kind(&self, kind: &Kind) -> bool {
        self.mode == Mode::Objects
            && self
                .kind
                .as_ref()
                .is_some_and(|current| current.gvk() == kind.gvk())
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
        self.pending_reveal = None;
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

        self.open_target(key, DetailTab::Overview, window, cx);
    }

    pub(crate) fn reveal_owner(
        &mut self,
        target: ObjectRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(namespace) = &target.namespace {
            self.rescope(BTreeSet::from([namespace.clone()]), window, cx);
        }
        self.clear_filter(window, cx);
        self.pending_reveal = Some(target);
        let listed = !self.table.read(cx).delegate().is_loading();
        self.reveal_pending(listed, window, cx);
    }

    fn reveal_pending(&mut self, listed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.pending_reveal.clone() else {
            return;
        };
        if self.table.read(cx).delegate().row_of(&target).is_some() {
            self.pending_reveal = None;
            self.reveal(&target, window, cx);
        } else if listed {
            self.pending_reveal = None;
            self.outcome = Some(Outcome::Failed(format!(
                "Owner {target} is no longer available"
            )));
            cx.notify();
        }
    }

    /// Opens the exact row and detail section chosen from its context menu.
    pub(crate) fn open_target(
        &mut self,
        key: &ObjectRef,
        tab: DetailTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        if let Some(detail) = self.detail.clone() {
            detail.update(cx, |detail, cx| detail.select(tab, window, cx));
        }
    }

    /// Closes the bottom panel first, unless Escape belongs to its terminal.
    ///
    /// The guard is here rather than in the key binding's context. A context
    /// predicate would be the tidier way to say it, but the cost of getting it
    /// wrong is Escape killing the panel out from under somebody in vim, and
    /// asking the panel directly is something a reader can check.
    pub fn close_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(creation) = &self.creation {
            if !creation.read(cx).is_running() {
                self.creation = None;
                cx.notify();
            }
            return;
        }
        let in_shell = self
            .pod_tools
            .as_ref()
            .is_some_and(|tools| tools.read(cx).shell_has_focus(window, cx));
        if in_shell {
            return;
        }
        self.pending_reveal = None;
        if self.pod_tools.take().is_some() || self.detail.take().is_some() {
            cx.notify();
        }
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
        self.label_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.label_error = None;
        self.label_menu_open = false;
        self.table.update(cx, |state, cx| {
            let delegate = state.delegate_mut();
            let changed = delegate.set_filter("")
                | delegate.clear_field_filters()
                | delegate.set_label_filter("").unwrap_or(false);
            if changed {
                cx.notify();
            }
        });
        self.filter_menu_open = None;
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
    pub(crate) fn show(&mut self, kind: Arc<Kind>, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::Objects
            && self
                .kind
                .as_ref()
                .is_some_and(|current| current.gvk() == kind.gvk())
        {
            return;
        }

        if self.kind.is_none() && self.visible {
            self.start_clock(cx);
            self.watch_metrics(window, cx);
        }
        tracing::info!(kind = %kind.display_name(), "showing");
        self.mode = Mode::Objects;
        self.kind = Some(kind);
        self.list_settings.close();
        self.filter_menu_open = None;
        self.label_menu_open = false;
        self.label_error = None;
        self.label_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.table.update(cx, |state, _| {
            state.delegate_mut().clear_field_filters();
            let _ = state.delegate_mut().set_label_filter("");
        });
        // The panel is about an object of the previous kind.
        self.detail = None;
        self.pod_tools = None;
        self.pending_reveal = None;
        self.watch_objects(window, cx);
        self.load_columns(window, cx);
        cx.emit(NavigationChanged);
        cx.notify();
    }

    /// Opens the detail panel on a row, reusing the existing panel when it is
    /// already showing that object.
    fn open_detail(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.pending_reveal = None;
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

        cx.subscribe(&detail, |_, _, event: &OwnerRequested, cx| {
            cx.emit(ResourceRequested {
                kind: event.kind.clone(),
                scope: event.target.namespace.iter().cloned().collect(),
                new_tab: false,
                target: Some(event.target.clone()),
            });
        })
        .detach();

        self.detail = Some(detail);
        cx.notify();
    }

    /// Hands the detail panel the object the table now holds, so that Overview
    /// tracks a changing pod rather than freezing at the moment it was opened.
    fn refresh_detail(&mut self, cx: &mut Context<Self>) {
        if let Some(detail) = self.detail.clone() {
            let key = detail.read(cx).target().clone();
            if let Some(object) = self.table.read(cx).delegate().object(&key).cloned() {
                let rules = self.rules.clone();
                detail.update(cx, |detail, cx| detail.refresh(object, rules, cx));
            }
        }
        if let Some(tools) = self.pod_tools.clone() {
            let key = tools.read(cx).target().clone();
            if let Some(object) = self.table.read(cx).delegate().object(&key).cloned() {
                let rules = self.rules.clone();
                tools.update(cx, |tools, cx| tools.refresh(object, rules, cx));
            }
        }
    }

    pub(crate) fn open_pod_tools(
        &mut self,
        key: &ObjectRef,
        tab: PodToolTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.mode != Mode::Objects
            || !self
                .kind
                .as_ref()
                .is_some_and(|kind| kind.resource.group.is_empty() && kind.resource.kind == "Pod")
        {
            return;
        }
        let Some(row) = self.table.read(cx).delegate().row_of(key) else {
            return;
        };
        let Some(object) = self.table.read(cx).delegate().object(key).cloned() else {
            return;
        };
        self.table
            .update(cx, |table, cx| table.scroll_to_row(row, cx));
        if let Some(tools) = &self.pod_tools
            && tools.read(cx).target() == key
        {
            tools.update(cx, |tools, cx| tools.select(tab, window, cx));
            cx.notify();
            return;
        }

        let session = self.session.clone();
        let rules = self.rules.clone();
        let tools = cx.new(|cx| PodToolsView::new(session, object, rules, tab, window, cx));
        cx.subscribe(&tools, |view, _, _: &PodToolsClosed, cx| {
            view.pod_tools = None;
            cx.notify();
        })
        .detach();
        self.pod_tools = Some(tools);
        cx.notify();
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
        let layout = self.resource_preferences(cx).columns;
        self.table.update(cx, |state, cx| {
            state.delegate_mut().reset(columns);
            state.delegate_mut().apply_layout(layout);
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
                    move |view, batch, window, cx| {
                        let namespace = namespace.clone();
                        let listed = batch.iter().any(|delta| matches!(delta, Delta::Reset(_)));
                        view.table.update(cx, |state, cx| {
                            // The first batch from *any* of the namespaces ends
                            // the skeleton, rather than waiting for all of
                            // them: rows that have arrived are worth more on
                            // screen than behind a placeholder.
                            state.delegate_mut().finish_loading();
                            state.delegate_mut().apply_from(namespace.as_deref(), batch);
                            cx.notify();
                        });
                        view.refresh_detail(cx);
                        view.reveal_pending(listed, window, cx);
                        // The toolbar count and Secret type choices read the
                        // same store, so they must follow each watch batch.
                        cx.notify();
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
        let mut columns = ColumnSet::resolve(
            &kind.resource.group,
            &kind.resource.kind,
            mixed,
            printer_columns,
        );
        if self.is_eks && kind.resource.group.is_empty() && kind.resource.kind == "Node" {
            beacon_columns::node::add_node_group(&mut columns);
        }
        columns
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
                        // A watch that cannot start never sends a first batch,
                        // so the skeleton would spin for as long as the app is
                        // open. Degraded health is that news arriving by
                        // another route: stop waiting, show the empty table,
                        // and let the status bar say why.
                        if matches!(current, Health::Degraded { .. }) {
                            view.table.update(cx, |state, cx| {
                                if state.delegate_mut().finish_loading() {
                                    cx.notify();
                                }
                            });
                        }
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
    fn watch_metrics(&mut self, _window: &Window, cx: &mut Context<Self>) {
        let session = self.session.clone();

        self._metrics = cx.spawn(async move |this, cx| {
            loop {
                let reading = cx.update(|cx| {
                    let session = session.clone();
                    Bridge::global(cx).run(async move { session.metrics().await })
                });

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
        let view = cx.entity().downgrade();
        self.table.update(cx, move |table, _| {
            table.delegate_mut().set_context_view(view);
        });
        let table = cx.subscribe_in(
            &self.table.clone(),
            window,
            |view, table, event: &TableEvent, window, cx| {
                // A single click is enough: in a list of pods, picking a row is
                // always a request to look at it.
                if let TableEvent::SelectRow(row) = event {
                    view.open_detail(*row, window, cx);
                } else if let TableEvent::ColumnWidthsChanged(widths) = event {
                    let layout = table.update(cx, |table, _| {
                        table.delegate_mut().update_widths(widths);
                        table.delegate().layout_snapshot()
                    });
                    view.persist_layout(layout, cx);
                }
            },
        );
        let layout = cx.subscribe(
            &self.table.clone(),
            |view, _, event: &crate::table::TableLayoutChanged, cx| {
                view.persist_layout(event.0.clone(), cx);
            },
        );

        let sidebar_search = cx.subscribe(
            &self.sidebar_search.clone(),
            |view, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    view.sidebar_query = state.read(cx).value().to_string();
                    cx.emit(NavigationChanged);
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

        let label_input = cx.subscribe_in(
            &self.label_input.clone(),
            window,
            |view, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::PressEnter { .. } => view.apply_label_filter(true, window, cx),
                    InputEvent::Change => view.label_error = None,
                    _ => return,
                }
                cx.notify();
            },
        );
        let picker_search = cx.subscribe(
            &self.picker_search.clone(),
            |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            },
        );
        self._subscriptions = vec![
            sidebar_search,
            row_search,
            label_input,
            picker_search,
            table,
            layout,
        ];
        self.listen_list_settings(window, cx);
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

    pub(crate) fn sidebar_search(&self) -> Entity<InputState> {
        self.sidebar_search.clone()
    }

    pub(crate) fn sidebar_search_active(&self) -> bool {
        !self.sidebar_query.is_empty()
    }

    /// Resource navigation for the expanded cluster in the app's sidebar.
    /// A normal selection activates the existing tab for a kind or opens one;
    /// the context menu always asks the window to open a separate tab.
    pub(crate) fn sidebar_items(&mut self, cx: &mut Context<Self>) -> Vec<SidebarMenuItem> {
        let current = self.kind.as_ref().map(|kind| kind.gvk());

        // A query replaces the sections with a flat ranked list: with a hundred
        // kinds, the answer to "where is it" should not be "in one of seven
        // collapsed groups".
        let sections: Vec<(SharedString, Vec<Entry>, bool, bool, Icon)> =
            if self.sidebar_query.is_empty() {
                self.catalog
                    .sections()
                    .iter()
                    .map(|(section, entries)| {
                        let open = section.starts_open()
                            || entries
                                .iter()
                                .any(|entry| current.as_ref() == Some(&entry.kind.gvk()));
                        (
                            section.label(),
                            entries.clone(),
                            open,
                            section.is_custom_group(),
                            crate::icons::section(section),
                        )
                    })
                    .collect()
            } else {
                let matches = self.catalog.search(&self.sidebar_query, &mut self.matcher);
                vec![(
                    SharedString::from("Matches"),
                    matches,
                    true,
                    false,
                    Icon::new(IconName::Search),
                )]
            };

        let (cluster_sections, custom_sections): (Vec<_>, Vec<_>) = sections
            .into_iter()
            .partition(|(_, _, _, custom, _)| !custom);

        let mode = self.mode;
        let tools = [Mode::Releases, Mode::Forwards].map(|item| {
            SidebarMenuItem::new(item.label())
                .icon(Icon::new(if item == Mode::Releases {
                    IconName::Inbox
                } else {
                    IconName::Network
                }))
                .active(mode == item)
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.show_mode(item, window, cx);
                }))
        });

        let menu = |sections: Vec<(SharedString, Vec<Entry>, bool, bool, Icon)>| {
            sections
                .into_iter()
                .map(|(label, entries, open, _, icon)| {
                    let controller_open = entries.iter().any(|entry| {
                        beacon_kube::argo::is_controller(
                            &entry.kind.resource.group,
                            &entry.kind.resource.kind,
                        ) && current.as_ref() == Some(&entry.kind.gvk())
                    });
                    let (controllers, entries): (Vec<_>, Vec<_>) =
                        entries.into_iter().partition(|entry| {
                            label.as_ref() == "Argo Workflows"
                                && beacon_kube::argo::is_controller(
                                    &entry.kind.resource.group,
                                    &entry.kind.resource.kind,
                                )
                        });
                    let item = |entry: Entry| {
                        let selected = current.as_ref() == Some(&entry.kind.gvk());
                        let kind = entry.kind.clone();
                        let menu_kind = kind.clone();
                        let menu_view = cx.entity().downgrade();
                        SidebarMenuItem::new(entry.label)
                            .active(selected)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.select_resource(kind.clone(), cx);
                            }))
                            .context_menu(move |menu, _, _| {
                                let view = menu_view.clone();
                                let kind = menu_kind.clone();
                                menu.item(PopupMenuItem::new("Open in new tab").on_click(
                                    move |_, _, cx| {
                                        let _ = view.update(cx, |view, cx| {
                                            view.open_resource(kind.clone(), cx);
                                        });
                                    },
                                ))
                            })
                    };
                    let mut children: Vec<_> = entries.into_iter().map(item).collect();
                    if !controllers.is_empty() {
                        children.push(
                            SidebarMenuItem::new("Controller resources")
                                .icon(crate::icons::tools())
                                .click_to_toggle(true)
                                .default_open(controller_open)
                                .children(controllers.into_iter().map(item)),
                        );
                    }
                    SidebarMenuItem::new(label)
                        .icon(icon)
                        .click_to_toggle(true)
                        .default_open(open)
                        .children(children)
                })
                .collect::<Vec<_>>()
        };

        let custom_open = custom_sections.iter().any(|(_, entries, _, _, _)| {
            entries
                .iter()
                .any(|entry| current.as_ref() == Some(&entry.kind.gvk()))
        });

        // Keep the common sections directly under the cluster, as in a tree.
        // Only extensions get an extra heading, so the distinction is visible
        // without making Workloads and Config three levels deep.
        let mut items = menu(cluster_sections);
        if !custom_sections.is_empty() {
            items.push(
                SidebarMenuItem::new("Custom resources")
                    .icon(crate::icons::custom())
                    .click_to_toggle(true)
                    .default_open(custom_open)
                    .children(menu(custom_sections)),
            );
        }
        items.push(
            SidebarMenuItem::new("Cluster tools")
                .icon(crate::icons::tools())
                .click_to_toggle(true)
                .default_open(mode != Mode::Objects)
                .children(tools),
        );
        items
    }

    fn render_releases(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows: AnyElement = match &self.releases {
            Releases::Unopened | Releases::Loading => self.waiting("Reading releases…", cx),
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

    /// A notice for something that has not finished yet. Same shape as
    /// [`Self::notice`] with a spinner, because static text cannot say whether
    /// anything is still happening.
    fn waiting(&self, message: impl Into<SharedString>, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .size_full()
            .p_6()
            .gap_2()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(cx.theme().tone(Tone::Progressing))
            .child(
                Spinner::new()
                    .small()
                    .color(cx.theme().tone(Tone::Progressing)),
            )
            .child(message.into())
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
            .child(crate::copyable_text::copyable_text(
                "cluster-notice",
                message,
            ))
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

    /// A searchable namespace picker with separate single- and multi-select targets.
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
    fn render_namespace_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        let selected = self.scoped_to.clone();
        let names = self.namespace_names.clone();
        let input = self.picker_search.clone();
        let search = OptionSearch::new(input.read(cx).value().as_ref());

        let toggling = view.clone();
        let opening = view.clone();

        Popover::new("namespace-picker")
            .open(self.namespace_menu_open)
            .track_focus(&input.read(cx).focus_handle(cx))
            .on_open_change(move |open, window, cx| {
                let open = *open;
                let _ = opening.update(cx, |view, cx| {
                    view.namespace_menu_open = open;
                    if open {
                        view.filter_menu_open = None;
                        view.label_menu_open = false;
                        view.list_settings.close();
                        view.picker_search
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }
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
                let names: Vec<_> = names
                    .iter()
                    .filter(|name| search.matches(name))
                    .cloned()
                    .collect();

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
                    .child(
                        Input::new(&input)
                            .id("namespace-search")
                            .small()
                            .cleanable(true)
                            .prefix(Icon::new(IconName::Search).small()),
                    )
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
                    .when(names.is_empty(), |this| {
                        this.child(
                            div()
                                .py_2()
                                .text_sm()
                                .text_color(muted)
                                .child("No matching namespaces"),
                        )
                    })
            })
    }

    /// Apply a validated draft locally, preserving the shared resource watch.
    fn apply_label_filter(
        &mut self,
        close_menu: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = self.label_input.read(cx).value().to_string();
        let result = self.table.update(cx, |state, cx| {
            let changed = state.delegate_mut().set_label_filter(&query)?;
            if changed {
                state.scroll_to_row(0, cx);
                cx.notify();
            }
            Ok::<_, String>(())
        });
        match result {
            Ok(()) => {
                self.label_error = None;
                if close_menu {
                    self.label_menu_open = false;
                    // The popover's input survives after its contents unmount.
                    self.row_search.read(cx).focus_handle(cx).focus(window, cx);
                }
            }
            Err(error) => {
                self.label_error = Some(format!(
                    "Invalid label selector: {error} The current filter is unchanged."
                ))
            }
        }
        cx.notify();
    }

    fn toggle_label(
        &mut self,
        key: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = beacon_kube::labels::LabelSelector::parse(&self.label_input.read(cx).value())
            .and_then(|mut selector| {
                selector.toggle_equality(key, value)?;
                Ok(selector.to_string())
            });
        match result {
            Ok(query) => {
                self.label_input
                    .update(cx, |input, cx| input.set_value(query, window, cx));
                self.apply_label_filter(false, window, cx);
            }
            Err(error) => {
                self.label_error = Some(format!(
                    "Invalid label selector: {error} The current filter is unchanged."
                ));
                cx.notify();
            }
        }
    }

    fn render_label_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let table = self.table.read(cx).delegate();
        let selector = table.label_selector().clone();
        let selected = table.label_filter().to_string();
        // Collect suggestions only while open, never on table ticks.
        let values = if self.label_menu_open {
            table.label_values()
        } else {
            Vec::new()
        };
        let input = self.label_input.clone();
        let search_input = self.picker_search.clone();
        let search = OptionSearch::new(search_input.read(cx).value().as_ref());
        let error = self.label_error.clone();
        let opening = cx.entity().downgrade();
        let choosing = opening.clone();
        Popover::new("label-filter")
            .open(self.label_menu_open)
            .on_open_change(move |open, window, cx| {
                let _ = opening.update(cx, |view, cx| {
                    view.label_menu_open = *open;
                    if *open {
                        view.filter_menu_open = None;
                        view.namespace_menu_open = false;
                        view.list_settings.close();
                        view.picker_search
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        view.label_input.read(cx).focus_handle(cx).focus(window, cx);
                    } else {
                        view.row_search.read(cx).focus_handle(cx).focus(window, cx);
                    }
                    cx.notify();
                });
            })
            .trigger(
                Button::new("label-filter-trigger")
                    .small().outline()
                    .label(if selector.is_empty() { "Labels: All".into() } else { format!("Labels: {}", selector.len()) })
                    .tooltip(if selected.is_empty() { "Filter resources by labels".into() } else { selected }),
            )
            .content(move |_, _, cx| {
                let applying = choosing.clone();
                let clearing = choosing.clone();
                let matching: Vec<_> = values.iter()
                    .filter(|(key, value)| search.matches(&format!("{key}={value}")))
                    .collect();
                let rows = matching.iter().take(200).map(|(key, value)| {
                    let view = choosing.clone();
                    let key = key.clone();
                    let value = value.clone();
                    let label = format!("{key}={value}");
                    Checkbox::new(SharedString::from(format!("label-choice-{label}")))
                        .checked(selector.has_equality(&key, &value))
                        .label(label.clone())
                        .accessibility_label(label)
                        .on_click(move |_, window, cx| {
                            let _ = view.update(cx, |view, cx| view.toggle_label(&key, &value, window, cx));
                        })
                });
                v_flex().w(px(430.)).min_h_0().max_h(px(540.)).gap_2()
                    .text_sm().text_color(cx.theme().foreground)
                    .child(div().font_weight(FontWeight::MEDIUM).child("Label selector"))
                    .child(Input::new(&input).small())
                    .child(h_flex().gap_2()
                        .child(Button::new("apply-label-filter").small().primary().label("Apply")
                            .on_click(move |_, window, cx| { let _ = applying.update(cx, |view, cx| view.apply_label_filter(true, window, cx)); }))
                        .child(Button::new("clear-label-filter").small().ghost().label("Clear labels")
                            .on_click(move |_, window, cx| { let _ = clearing.update(cx, |view, cx| {
                                view.label_input.update(cx, |input, cx| input.set_value("", window, cx));
                                view.apply_label_filter(true, window, cx);
                            }); }))
                    )
                    .child(div().text_xs().text_color(cx.theme().muted_foreground)
                        .child("Use =, ==, !=, in, notin, key or !key. Commas require all conditions. Press Enter to apply."))
                    .children(error.clone().map(|error| div().text_xs().text_color(cx.theme().tone(Tone::Critical))
                        .child(crate::copyable_text::copyable_text("label-filter-error", error))))
                    .child(Input::new(&search_input).id("label-options-search").small().cleanable(true)
                        .prefix(Icon::new(IconName::Search).small()))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground)
                        .child(if values.is_empty() { "No labels in this scope. You can still enter a selector.".to_string() }
                            else if matching.is_empty() { "No matching labels.".to_string() }
                            else if matching.len() > 200 { format!("Showing 200 of {} matching labels. Refine your search to find more.", matching.len()) }
                            else { "Select labels below to apply them together.".to_string() }))
                    .child(div().id("label-filter-values").min_h_0().flex_1().overflow_y_scroll()
                        .child(v_flex().gap_1p5().children(rows)))
            })
    }

    /// Exact facets come from all watched objects, including currently hidden rows.
    fn render_field_picker(&self, field: Field, cx: &mut Context<Self>) -> impl IntoElement {
        let table = self.table.read(cx);
        let selected = table.delegate().field_filter(field).map(str::to_owned);
        let mut types = table.delegate().filter_values(field);
        if let Some(value) = &selected
            && !types.contains(value)
        {
            types.push(value.clone());
            types.sort();
        }
        let label = selected.clone().unwrap_or_else(|| "All".to_string());
        let input = self.picker_search.clone();
        let search = OptionSearch::new(input.read(cx).value().as_ref());
        let opening = cx.entity().downgrade();
        let choosing = opening.clone();

        Popover::new(SharedString::from(format!("filter-{field:?}")))
            .open(self.filter_menu_open == Some(field))
            .track_focus(&input.read(cx).focus_handle(cx))
            .on_open_change(move |open, window, cx| {
                let _ = opening.update(cx, |view, cx| {
                    if *open {
                        view.filter_menu_open = Some(field);
                        view.namespace_menu_open = false;
                        view.label_menu_open = false;
                        view.list_settings.close();
                        view.picker_search
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    } else if view.filter_menu_open == Some(field) {
                        view.filter_menu_open = None;
                    }
                    cx.notify();
                });
            })
            .trigger(
                Button::new(SharedString::from(format!("filter-trigger-{field:?}")))
                    .small()
                    .outline()
                    .label(format!("{}: {label}", field.label()))
                    .tooltip(format!("Filter by {}", field.label().to_lowercase())),
            )
            .content(move |_, _, cx| {
                let matching: Vec<_> = types
                    .iter()
                    .filter(|value| search.matches(value))
                    .cloned()
                    .collect();
                let choices = std::iter::once(None)
                    .chain(matching.iter().cloned().map(Some))
                    .map(|value| {
                        let name = value.as_deref().unwrap_or("All");
                        let is_selected = selected == value;
                        let view = choosing.clone();
                        div()
                            .id(SharedString::from(format!("filter-{field:?}-{name}")))
                            .role(Role::Button)
                            .aria_label(name.to_string())
                            .w_full()
                            .px_2()
                            .py_1()
                            .truncate()
                            .cursor_pointer()
                            .font_weight(if is_selected {
                                FontWeight::MEDIUM
                            } else {
                                FontWeight::NORMAL
                            })
                            .child(name.to_string())
                            .on_click(move |_, _, cx| {
                                let value = value.clone();
                                let _ = view.update(cx, |view, cx| {
                                    view.table.update(cx, |state, cx| {
                                        if state.delegate_mut().set_field_filter(field, value) {
                                            state.scroll_to_row(0, cx);
                                            cx.notify();
                                        }
                                    });
                                    view.filter_menu_open = None;
                                    cx.notify();
                                });
                            })
                    });

                v_flex()
                    .w(px(300.))
                    .min_h_0()
                    .max_h(px(360.))
                    .gap_1()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(
                        Input::new(&input)
                            .id(SharedString::from(format!("filter-search-{field:?}")))
                            .small()
                            .cleanable(true)
                            .prefix(Icon::new(IconName::Search).small()),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("filter-list-{field:?}")))
                            .min_h_0()
                            .flex_1()
                            .overflow_y_scroll()
                            .child(v_flex().children(choices)),
                    )
                    .when(matching.is_empty(), |this| {
                        this.child(
                            div()
                                .px_2()
                                .py_1()
                                .text_color(cx.theme().muted_foreground)
                                .child("No matching options"),
                        )
                    })
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
            // A count of zero beside a skeleton is a number that is not true
            // yet.
            Mode::Objects if self.table.read(cx).delegate().is_loading() => String::new(),
            Mode::Objects if shown == total => total.to_string(),
            Mode::Objects => format!("{shown} of {total}"),
            Mode::Releases => match &self.releases {
                Releases::Ready(releases) => releases.len().to_string(),
                _ => String::new(),
            },
            Mode::Forwards => self.session.forwards().len().to_string(),
        };
        let selected = if self.mode == Mode::Objects {
            self.table.read(cx).delegate().selected_count()
        } else {
            0
        };
        h_flex()
            .w_full()
            .flex_wrap()
            .flex_shrink_0()
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
                    .items_center()
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
                    .flex_wrap()
                    .gap_2()
                    .items_center()
                    // What the last write said. It lives here rather than in a
                    // toast because the thing it is about is on screen.
                    .when(
                        self.mode == Mode::Objects
                            && self.kind.as_ref().is_some_and(|kind| {
                                kind.resource.group.is_empty() && kind.resource.kind == "Pod"
                            }),
                        |bar| {
                            bar.child(
                                Button::new("aggregate-pod-logs")
                                    .small()
                                    .ghost()
                                    .label("Aggregated logs…")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.aggregate_filtered(window, cx)
                                    })),
                            )
                        },
                    )
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
                            .child(crate::copyable_text::copyable_text(
                                "operation-outcome",
                                text,
                            ))
                    }))
                    .children((selected > 0).then(|| {
                        h_flex()
                            .gap_1()
                            .items_center()
                            .child(div().text_xs().child(format!("{selected} selected")))
                            .child(
                                Button::new("clear-selected")
                                    .small()
                                    .ghost()
                                    .label("Clear")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.table.update(cx, |table, cx| {
                                            table.delegate_mut().clear_selected();
                                            cx.notify();
                                        });
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("delete-selected")
                                    .small()
                                    .danger()
                                    .label("Delete selected")
                                    .disabled(!self.may_delete() || self.bulk_deleting)
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.start_delete_selected(window, cx);
                                    })),
                            )
                    }))
                    .children(
                        self.kind
                            .as_ref()
                            .filter(|_| self.mode == Mode::Objects)
                            .map(|_| self.render_column_picker(cx).into_any_element()),
                    )
                    .children(
                        self.kind
                            .as_ref()
                            .filter(|_| self.mode == Mode::Objects)
                            .map(|_| self.render_saved_filters(cx).into_any_element()),
                    )
                    .children(
                        self.kind
                            .as_ref()
                            .filter(|_| self.mode == Mode::Objects)
                            .map(|kind| {
                                Button::new("create-resource")
                                    .small()
                                    .primary()
                                    .label("Create")
                                    .disabled(!kind.supports("create"))
                                    .tooltip(format!("Create a {} from YAML", kind.resource.kind))
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.start_create(window, cx)
                                    }))
                            }),
                    )
                    .child(
                        div()
                            .w(px(220.))
                            .child(Input::new(&self.row_search).small()),
                    )
                    .children(
                        self.kind
                            .as_ref()
                            .filter(|_| self.mode == Mode::Objects)
                            .map(|_| self.render_label_picker(cx).into_any_element()),
                    )
                    .children(
                        self.kind
                            .as_ref()
                            .filter(|_| self.mode == Mode::Objects)
                            .into_iter()
                            .flat_map(|kind| Field::for_kind(kind))
                            .map(|field| self.render_field_picker(field, cx).into_any_element()),
                    )
                    .children(
                        self.kind
                            .as_ref()
                            .filter(|kind| kind.namespaced)
                            .map(|_| self.render_namespace_picker(cx)),
                    ),
            )
            .children(self.render_list_preferences_error(cx))
    }
}

impl Render for ClusterView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.is_idle() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .p_8()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(format!("Connected to {}", self.session.id().display_name())),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Select a resource from the sidebar to open its page."),
                )
                .into_any_element();
        }

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
            Some(detail) => h_resizable("detail-split")
                .with_state(&self.split)
                .child(
                    resizable_panel()
                        .size_range(px(160.)..px(10000.))
                        .child(table),
                )
                .child(
                    resizable_panel()
                        .size(px(440.))
                        .size_range(px(260.)..px(10000.))
                        .child(
                            div()
                                .size_full()
                                .border_l_1()
                                .border_color(cx.theme().border)
                                .child(detail),
                        ),
                )
                .into_any_element(),
        };

        let body = match self
            .pod_tools
            .clone()
            .filter(|_| self.mode == Mode::Objects)
        {
            None => body,
            Some(tools) => v_resizable("pod-tools-split")
                .with_state(&self.pod_tools_split)
                .child(
                    resizable_panel()
                        .size_range(px(120.)..px(10000.))
                        .child(body),
                )
                .child(
                    resizable_panel()
                        .size(px(300.))
                        .size_range(px(160.)..px(10000.))
                        .child(
                            div()
                                .size_full()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .child(tools),
                        ),
                )
                .into_any_element(),
        };

        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                v_flex()
                    .size_full()
                    .child(self.render_toolbar(cx))
                    .child(div().flex_1().overflow_hidden().child(body)),
            )
            .children(self.prompt.clone().map(|prompt| {
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(cx.theme().background.opacity(0.75))
                    .child(prompt)
            }))
            .children(self.creation.clone().map(|creation| {
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(cx.theme().background.opacity(0.75))
                    .child(creation)
            }))
            .into_any_element()
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;

    fn menu(window: &Window) -> Vec<(String, bool)> {
        gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .filter(|item| item.role() == Some(Role::MenuItem))
            .filter_map(|item| {
                item.label()
                    .map(|label| (label.to_owned(), item.disabled().unwrap_or(false)))
            })
            .collect()
    }

    #[::core::prelude::v1::test]
    fn row_actions_match_the_context_menu_and_keep_the_clicked_target() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let pod = |name| json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"default","uid":name,"resourceVersion":"1"},"spec":{"containers":[{"name":"app","image":"busybox"}]},"status":{"phase":"Running"}});
        let (fixture, session) =
            support::fixture(cx, "actions-fixture", vec![pod("alpha"), pod("beta")]);
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    ClusterView::new(
                        session,
                        Some("default".into()),
                        Some(Arc::new(support::kind("", "Pod", "pods"))),
                        None,
                        false,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        support::settle(cx, |cx| {
            view.read_with(cx, |view, cx| {
                view.counts(cx).0 == 2 && view.rules.is_some()
            })
        });
        view.update(cx, |view, cx| {
            view.table.update(cx, |state, cx| {
                let table = state.delegate_mut();
                let mut layout = table.layout_snapshot();
                layout.hidden = table
                    .column_choices()
                    .into_iter()
                    .filter(|(_, name, _, _)| !matches!(name.as_str(), "Name" | "Status"))
                    .map(|(id, _, _, _)| id)
                    .collect();
                table.apply_layout(layout);
                cx.notify();
            })
        });
        let dropdown = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert!(window.find(("resource-actions", 0usize)).visible());
                window.click(("resource-actions", 0usize), cx);
                menu(window)
            })
            .unwrap();
        assert!(
            dropdown
                .iter()
                .any(|(label, _)| label == "Debug container…")
        );
        assert!(
            dropdown
                .iter()
                .any(|(label, _)| label == "Compare resource…")
        );
        assert!(view.read_with(cx, |view, _| view.detail.is_none()));
        let context = cx
            .update_window(window, |_, window, cx| {
                window.press("escape", cx);
                window.right_click(("row", 0usize), cx);
                menu(window)
            })
            .unwrap();
        assert_eq!(dropdown, context);
        cx.update_window(window, |_, window, cx| {
            let target = gpui_kit::base::test_support::snapshots(window)
                .into_iter()
                .find(|item| {
                    item.role() == Some(Role::MenuItem) && item.label() == Some("Copy name")
                })
                .unwrap();
            window.click(target.path().last().unwrap().clone(), cx);
            assert_eq!(cx.read_from_clipboard().unwrap().text().unwrap(), "alpha");
        })
        .unwrap();
        assert!(
            !fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.starts_with("PATCH") || line.starts_with("DELETE"))
        );
    }
}
