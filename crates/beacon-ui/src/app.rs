//! The root view: the tabs, the chrome around them, and the connections.
//!
//! Everything cluster-shaped lives in [`ClusterView`]. What is here is the part
//! that exists before, between and around connections -- which contexts there
//! are, which tabs are open on them, and what went wrong if one failed.
//!
//! A tab is a view into one cluster. Several tabs may point at the same
//! cluster, and each has its own kind, namespace, filter and detail panel, so
//! "Pods here and Deployments there" is two tabs rather than two windows.
//! What a tab does *not* own is the connection: [`ClusterSession`] is keyed by
//! cluster in [`BeaconApp::sessions`] and shared, so a second tab on a cluster
//! costs a view and nothing else.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use beacon_kube::{
    ClusterId, ClusterSession, Kind,
    config::{ContextEntry, Contexts},
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab as TabItem, TabBar};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app_logs;
use crate::bridge::Bridge;
use crate::cluster::{ClusterView, Mode, NavigationChanged, ResourceRequested};
use crate::palette::{self, Choice, Palette, PaletteEvent};
use crate::theme::{BeaconTheme as _, Tone, toggle_mode};

gpui_kit::actions!(
    beacon,
    [
        TogglePalette,
        OpenAppLogs,
        OpenShortcuts,
        Quit,
        NewTab,
        CloseTab,
        NextTab,
        PreviousTab,
        CloseDetail
    ]
);

/// The sidebar carries the cluster names, leaving room for more resource tabs.
const TAB_WIDTH: Pixels = px(190.);

/// Installs the keys everything else is reachable from.
///
/// Bound without a context, so they work wherever focus happens to be -- the
/// palette is no use if it only opens when nothing is selected, and neither is
/// a tab shortcut that stops working once you click into the table.
pub fn init(log_directory: PathBuf, cx: &mut App) {
    app_logs::init(log_directory, cx);
    crate::shortcuts::init(cx);
    cx.on_action(|_: &OpenShortcuts, cx| crate::shortcuts::open(cx));
    cx.on_action(|_: &OpenAppLogs, cx| app_logs::open(cx));
    cx.on_action(|_: &Quit, cx| cx.quit());

    #[cfg(target_os = "macos")]
    cx.set_menus([
        Menu::new("Beacon").items([
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Beacon", Quit),
        ]),
        Menu::new("File").items([MenuItem::action("Close", CloseTab)]),
        Menu::new("View").items([MenuItem::action("App logs", OpenAppLogs)]),
        Menu::new("Help").items([MenuItem::action("Keyboard shortcuts", OpenShortcuts)]),
    ]);
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    cx.bind_keys([
        KeyBinding::new("f1", OpenShortcuts, None),
        KeyBinding::new(&format!("{modifier}-q"), Quit, None),
        KeyBinding::new(&format!("{modifier}-k"), TogglePalette, None),
        KeyBinding::new(&format!("{modifier}-shift-l"), OpenAppLogs, None),
        KeyBinding::new(&format!("{modifier}-t"), NewTab, None),
        KeyBinding::new(&format!("{modifier}-w"), CloseTab, None),
        // Not `cmd-shift-[` and friends: ctrl-tab is the one pair that means
        // the same thing on all three platforms.
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        // The one key here that is *not* global. Escape belongs to whatever
        // has focus -- a shell, a search box -- and only reaches the cluster
        // view when nothing nearer wanted it.
        // Escape closes the detail panel. Bound without a context like the
        // rest of these; which key events it should ignore is decided in the
        // handler, where it can be read.
        KeyBinding::new("escape", CloseDetail, None),
    ]);
}

/// One tab: a cluster, and a view into it once it has connected.
struct Tab {
    /// Stable for the life of the tab. The bar identifies tabs by position, so
    /// a close button needs something that does not move when the tab to its
    /// left goes away.
    id: u64,
    cluster: ClusterId,
    /// What the kubeconfig said this context's namespace is, passed to the
    /// view when it is built.
    namespace: Option<String>,
    /// A resource selection starts this tab on its chosen kind.
    initial_kind: Option<Arc<Kind>>,
    /// Carry the source tab's namespace selection into a resource tab.
    initial_scope: Option<BTreeSet<String>>,
    initial_mode: Mode,
    state: TabState,
    /// Dropped with the tab, which abandons a connection nobody is waiting on.
    _connect: Option<Task<()>>,
    /// Keep the app's sidebar and tab label in sync with this view's selection.
    _navigation: Option<Subscription>,
    _resource_requests: Option<Subscription>,
}

enum TabState {
    Disconnected,
    Connecting,
    Connected(Entity<ClusterView>),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivityMenu {
    Watches,
    Clusters,
}

pub struct BeaconApp {
    /// The window's baseline focus.
    ///
    /// Without this nothing in Beacon is ever focused: `TableState` holds a
    /// focus handle but never tracks it, so clicking a row focuses nothing,
    /// and GPUI dispatches a keystroke down the path from the *focused* node.
    /// With no focused node that path is the window root alone, every
    /// `on_action` in this file and in ClusterView sits outside it, and not
    /// one keyboard shortcut fires -- ⌘K included, which is why the palette
    /// could only ever be opened from instrumentation.
    focus: FocusHandle,
    /// The tab bar's scroll, kept for one thing: `max_offset` is how the bar
    /// says its tabs no longer fit. See [`Self::render_tabs`].
    tab_scroll: ScrollHandle,
    sidebar_collapsed: bool,
    sidebar_split: Entity<ResizableState>,
    contexts: Result<Contexts, String>,

    /// Every cluster connected in this session, shared by every tab on it.
    ///
    /// A session is a client, a discovery cache, a permission cache and any
    /// port forwards -- all cheap to hold and slow to rebuild. They outlive
    /// the tabs that opened them, which is what makes reopening one instant.
    /// The *watches* belong to the views, so closing a tab stops what it was
    /// watching.
    sessions: HashMap<ClusterId, Arc<ClusterSession>>,
    /// Manual disconnection persists until the user explicitly reconnects.
    disconnected: HashSet<ClusterId>,
    /// Results from an earlier connection must never resurrect a stopped session.
    connection_epochs: HashMap<ClusterId, u64>,

    tabs: Vec<Tab>,
    /// Index into `tabs`. Meaningless, and never read, while `tabs` is empty.
    active: usize,
    next_tab: u64,

    palette: Entity<Palette>,
    palette_open: bool,
    activity_open: Option<ActivityMenu>,
    activity_refresh: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl BeaconApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let contexts = Contexts::load().map_err(|err| err.to_string());

        match &contexts {
            Ok(contexts) => tracing::info!(
                count = contexts.entries().len(),
                current = contexts.current().map(|c| c.id.as_str()),
                "loaded kubeconfig"
            ),
            Err(err) => tracing::warn!(%err, "no usable kubeconfig"),
        }

        let palette = cx.new(|cx| Palette::new(window, cx));
        let palette_events = cx.subscribe_in(
            &palette,
            window,
            |view, _, event: &PaletteEvent, window, cx| match event {
                PaletteEvent::Chose(choice) => {
                    view.palette_open = false;
                    view.choose(choice.clone(), window, cx);
                }
                PaletteEvent::Dismissed => {
                    view.palette_open = false;
                    cx.notify();
                }
            },
        );

        let this = Self {
            focus: cx.focus_handle(),
            tab_scroll: ScrollHandle::new(),
            sidebar_collapsed: false,
            sidebar_split: cx.new(|_| ResizableState::default()),
            contexts,
            sessions: HashMap::new(),
            disconnected: HashSet::new(),
            connection_epochs: HashMap::new(),
            tabs: Vec::new(),
            active: 0,
            next_tab: 0,
            palette,
            palette_open: false,
            activity_open: None,
            activity_refresh: None,
            _subscriptions: vec![palette_events],
        };

        // Something has to hold focus before any binding can resolve. The
        // root is the honest place for it: keys that mean the same thing
        // wherever you are should work wherever you are.
        this.focus.focus(window, cx);

        this
    }

    fn namespace_for(&self, id: &ClusterId) -> Option<String> {
        self.contexts
            .as_ref()
            .ok()
            .and_then(|contexts| contexts.get(id))
            .and_then(|entry| entry.namespace.clone())
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    /// The view in tab `index`, if it has connected.
    fn view(&self, index: usize) -> Option<Entity<ClusterView>> {
        match self.tabs.get(index).map(|tab| &tab.state) {
            Some(TabState::Connected(view)) => Some(view.clone()),
            _ => None,
        }
    }

    fn cluster(&self) -> Option<Entity<ClusterView>> {
        self.view(self.active)
    }

    /// Opens a new tab on `cluster` and makes it the one on screen.
    fn open(&mut self, cluster: ClusterId, window: &mut Window, cx: &mut Context<Self>) {
        self.open_kind(cluster, None, None, window, cx);
    }

    /// Opens a separate tab for a resource selection.
    fn open_kind(
        &mut self,
        cluster: ClusterId,
        initial_kind: Option<Arc<Kind>>,
        initial_scope: Option<BTreeSet<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let namespace = self.namespace_for(&cluster);
        let id = self.next_tab;
        self.next_tab += 1;

        self.tabs.push(Tab {
            id,
            cluster,
            namespace,
            initial_kind,
            initial_scope,
            initial_mode: Mode::Objects,
            state: TabState::Connecting,
            _connect: None,
            _navigation: None,
            _resource_requests: None,
        });

        let index = self.tabs.len() - 1;
        self.activate(index, window, cx);
        self.connect(index, window, cx);
    }

    /// Goes to `cluster`: its tab if one is open, a new tab if not.
    ///
    /// The resource context menu and ⌘T open new tabs; selecting a cluster reuses one.
    fn go_to(&mut self, cluster: ClusterId, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .tabs
            .get(self.active)
            .is_some_and(|tab| tab.cluster == cluster)
        {
            return;
        }
        match self.tabs.iter().position(|tab| tab.cluster == cluster) {
            Some(index) => self.activate(index, window, cx),
            None => self.open(cluster, window, cx),
        }
    }

    /// One ordinary tab per cluster and resource kind. Explicit context-menu
    /// opens may create duplicates; ordinary selection prefers the active one,
    /// then the most recently opened matching tab.
    fn go_to_kind(
        &mut self,
        cluster: ClusterId,
        kind: Arc<Kind>,
        scope: BTreeSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let matches = |tab: &Tab| {
            if tab.cluster != cluster {
                return false;
            }
            match &tab.state {
                TabState::Connected(view) => view.read(cx).shows_kind(&kind),
                TabState::Connecting | TabState::Disconnected => tab
                    .initial_kind
                    .as_ref()
                    .is_some_and(|initial| initial.gvk() == kind.gvk()),
                TabState::Failed(_) => false,
            }
        };
        let index = self
            .tabs
            .get(self.active)
            .filter(|tab| matches(tab))
            .map(|_| self.active)
            .or_else(|| self.tabs.iter().rposition(matches));

        match index {
            Some(index) => self.activate(index, window, cx),
            None => {
                // The connected cluster starts without a resource page. Its
                // first ordinary selection fills that blank tab.
                if self
                    .tabs
                    .get(self.active)
                    .is_some_and(|tab| tab.cluster == cluster)
                    && let Some(view) = self.cluster().filter(|view| view.read(cx).is_idle())
                {
                    view.update(cx, |view, cx| view.show(kind, window, cx));
                } else {
                    self.open_kind(cluster, Some(kind), Some(scope), window, cx);
                }
            }
        }
    }

    /// Stop every tab on this cluster while preserving its navigation.
    fn disconnect(&mut self, cluster: &ClusterId, window: &mut Window, cx: &mut Context<Self>) {
        self.disconnected.insert(cluster.clone());
        *self.connection_epochs.entry(cluster.clone()).or_default() += 1;
        self.activity_open = None;
        self.activity_refresh = None;
        self.palette_open = false;
        for tab in self.tabs.iter_mut().filter(|tab| &tab.cluster == cluster) {
            if let TabState::Connected(view) = &tab.state {
                let (kind, scope, mode) = view.read(cx).navigation();
                tab.initial_kind = kind;
                tab.initial_scope = Some(scope);
                tab.initial_mode = mode;
            }
            tab._connect = None;
            tab._navigation = None;
            tab._resource_requests = None;
            tab.state = TabState::Disconnected;
        }
        if let Some(session) = self.sessions.remove(cluster) {
            session.disconnect();
        }
        if self
            .tabs
            .get(self.active)
            .is_some_and(|tab| &tab.cluster == cluster)
        {
            self.focus.focus(window, cx);
        }
        tracing::info!(context = %cluster, "manually disconnected cluster");
        cx.notify();
    }

    /// Replace the client once, then rebuild all tabs on that session.
    fn reconnect(&mut self, cluster: ClusterId, window: &mut Window, cx: &mut Context<Self>) {
        self.disconnect(&cluster, window, cx);
        self.disconnected.remove(&cluster);
        let index = self
            .tabs
            .get(self.active)
            .filter(|tab| tab.cluster == cluster)
            .map(|_| self.active)
            .or_else(|| self.tabs.iter().position(|tab| tab.cluster == cluster));
        let Some(index) = index else {
            self.open(cluster, window, cx);
            return;
        };
        for tab in self.tabs.iter_mut().filter(|tab| tab.cluster == cluster) {
            tab.state = TabState::Connecting;
        }
        self.activate(index, window, cx);
        self.connect(index, window, cx);
    }

    /// Gives tab `index` a view, connecting first if this cluster is new.
    fn connect(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let cluster = tab.cluster.clone();
        let namespace = tab.namespace.clone();
        let tab_id = tab.id;
        if self.disconnected.contains(&cluster) {
            self.tabs[index].state = TabState::Disconnected;
            cx.notify();
            return;
        }
        let epoch = *self.connection_epochs.entry(cluster.clone()).or_default();

        // Already connected: opening another tab on this cluster is building a
        // view over a session that still has its client, its discovery and its
        // forwards.
        if let Some(session) = self.sessions.get(&cluster).cloned() {
            tracing::info!(context = %cluster, "reusing the session");
            self.show(index, session, namespace, window, cx);
            return;
        }

        // Resource tabs opened while discovery is still running share that
        // request; its result will fill every waiting tab on this cluster.
        if self.tabs.iter().enumerate().any(|(other, tab)| {
            other != index
                && tab.cluster == cluster
                && matches!(tab.state, TabState::Connecting)
                && tab._connect.is_some()
        }) {
            self.tabs[index].state = TabState::Connecting;
            cx.notify();
            return;
        }

        tracing::info!(context = %cluster, namespace = ?namespace, "connecting");
        self.tabs[index].state = TabState::Connecting;
        cx.notify();

        let connecting = {
            let cluster = cluster.clone();
            Bridge::global(cx)
                .run_cancellable(async move { ClusterSession::connect(cluster).await })
        };

        let task = cx.spawn_in(window, async move |this, cx| {
            let result = connecting.result().await;

            let _ = this.update_in(cx, |view, window, cx| {
                // The tab may have been closed, or pointed somewhere else,
                // while this was in flight. Either way it is not ours to fill.
                let Some(index) = view.index_of(tab_id) else {
                    return;
                };
                if view.tabs[index].cluster != cluster
                    || view.connection_epochs.get(&cluster) != Some(&epoch)
                    || view.disconnected.contains(&cluster)
                {
                    return;
                }

                match result {
                    Ok(Ok(session)) => {
                        // Another tab may have finished connecting to the same
                        // cluster while this one was in flight. One session per
                        // cluster is what the rest of this relies on, so the
                        // duplicate is dropped here rather than kept.
                        let session = view
                            .sessions
                            .entry(cluster.clone())
                            .or_insert_with(|| Arc::new(session))
                            .clone();
                        let waiting: Vec<_> = view
                            .tabs
                            .iter()
                            .enumerate()
                            .filter(|(_, tab)| {
                                tab.cluster == cluster && matches!(tab.state, TabState::Connecting)
                            })
                            .map(|(index, _)| index)
                            .collect();
                        for index in waiting {
                            let namespace = view.tabs[index].namespace.clone();
                            view.show(index, session.clone(), namespace, window, cx);
                        }
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(context = %cluster, %error, "could not connect");
                        for tab in view.tabs.iter_mut().filter(|tab| {
                            tab.cluster == cluster && matches!(tab.state, TabState::Connecting)
                        }) {
                            tab.state = TabState::Failed(error.to_string());
                        }
                        cx.notify();
                    }
                    Err(error) => {
                        tracing::error!(context = %cluster, %error, "the connect task failed");
                        for tab in view.tabs.iter_mut().filter(|tab| {
                            tab.cluster == cluster && matches!(tab.state, TabState::Connecting)
                        }) {
                            tab.state = TabState::Failed(error.to_string());
                        }
                        cx.notify();
                    }
                }
            });
        });

        self.tabs[index]._connect = Some(task);
    }

    /// Puts a freshly built view into tab `index`.
    fn show(
        &mut self,
        index: usize,
        session: Arc<ClusterSession>,
        namespace: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial_kind = self.tabs[index].initial_kind.take();
        let initial_scope = self.tabs[index].initial_scope.take();
        let initial_mode = self.tabs[index].initial_mode;
        let view = cx.new(|cx| {
            ClusterView::new(session, namespace, initial_kind, initial_scope, window, cx)
        });

        if initial_mode != Mode::Objects {
            view.update(cx, |view, cx| view.show_mode(initial_mode, window, cx));
        }

        // A tab that connected in the background must not start its timers: a
        // view is visible until told otherwise, and nothing else would tell it.
        if self.active != index {
            view.update(cx, |view, cx| view.set_visible(false, window, cx));
        }

        let navigation = cx.subscribe(&view, |_, _, _: &NavigationChanged, cx| cx.notify());
        let cluster = self.tabs[index].cluster.clone();
        let resource_requests = cx.subscribe_in(
            &view,
            window,
            move |app, _, event: &ResourceRequested, window, cx| {
                if event.new_tab {
                    app.open_kind(
                        cluster.clone(),
                        Some(event.kind.clone()),
                        Some(event.scope.clone()),
                        window,
                        cx,
                    );
                } else {
                    app.go_to_kind(
                        cluster.clone(),
                        event.kind.clone(),
                        event.scope.clone(),
                        window,
                        cx,
                    );
                }
                if let Some(target) = &event.target
                    && let Some(view) = app.cluster()
                {
                    view.update(cx, |view, cx| view.reveal_owner(target.clone(), window, cx));
                }
            },
        );
        self.tabs[index].state = TabState::Connected(view);
        self.tabs[index]._navigation = Some(navigation);
        self.tabs[index]._resource_requests = Some(resource_requests);
        cx.notify();
    }

    /// Brings tab `index` to the front.
    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        if self.active != index
            && let Some(previous) = self.view(self.active)
        {
            previous.update(cx, |view, cx| view.set_visible(false, window, cx));
        }

        self.active = index;

        if let Some(view) = self.view(index) {
            view.update(cx, |view, cx| view.set_visible(true, window, cx));
        }

        cx.notify();
    }

    /// Closes tab `index`, and with it every watch its view was running.
    ///
    /// The session stays in `sessions`: reconnecting is the slow part, and
    /// nothing is being watched through it once the view is gone.
    fn close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }

        let tab = self.tabs.remove(index);
        tracing::info!(context = %tab.cluster, "closed a tab");
        let pending_cluster =
            matches!(tab.state, TabState::Connecting).then(|| tab.cluster.clone());
        drop(tab);

        if self.tabs.is_empty() {
            self.active = 0;
            cx.notify();
            return;
        }

        // Closing a tab to the left of the active one shifts it; closing the
        // active one lands on its right-hand neighbour, or the new last tab.
        self.active = if self.active > index {
            self.active - 1
        } else {
            self.active.min(self.tabs.len() - 1)
        };

        // Whatever is in front now may have been a background tab a moment
        // ago, and `activate` would see the index it already holds.
        if let Some(view) = self.view(self.active) {
            view.update(cx, |view, cx| view.set_visible(true, window, cx));
        }

        // A reconnect has one pending request shared by all waiting tabs.
        // If its owner was closed, let a remaining tab finish the connection.
        if let Some(cluster) = pending_cluster {
            let waiting: Vec<_> = self
                .tabs
                .iter()
                .enumerate()
                .filter(|(_, tab)| {
                    tab.cluster == cluster && matches!(tab.state, TabState::Connecting)
                })
                .map(|(index, tab)| (index, tab._connect.is_some()))
                .collect();
            if !waiting.iter().any(|(_, pending)| *pending)
                && let Some((index, _)) = waiting.first()
            {
                self.connect(*index, window, cx);
            }
        }
        cx.notify();
    }

    /// Another tab on the cluster in front, so it can be narrowed to something
    /// else.
    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cluster = self.tabs.get(self.active).map(|tab| tab.cluster.clone());

        match cluster {
            Some(cluster) => self.open(cluster, window, cx),
            // Nothing to open a tab on. Saying so is the palette's job, and it
            // is also where a cluster would be picked from.
            None => self.toggle_palette(window, cx),
        }
    }

    fn step(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() < 2 {
            return;
        }
        let last = self.tabs.len() - 1;
        let next = match (forward, self.active) {
            (true, index) if index == last => 0,
            (true, index) => index + 1,
            (false, 0) => last,
            (false, index) => index - 1,
        };
        self.activate(next, window, cx);
    }

    /// Opens the palette, or closes it if it is already open.
    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_open {
            self.palette_open = false;
            cx.notify();
            return;
        }

        let mut sources = self
            .cluster()
            .map(|cluster| cluster.read(cx).sources(cx))
            .unwrap_or_default();

        // Clusters come from the kubeconfig, not from the connected sessions --
        // going to one Beacon is not connected to is the point.
        sources.clusters = self
            .contexts
            .as_ref()
            .map(|contexts| contexts.entries().to_vec())
            .unwrap_or_default();

        self.palette.update(cx, |palette, cx| {
            palette.open(sources, window, cx);
        });
        self.palette_open = true;
        cx.notify();
    }

    /// Carries out what the palette was asked for.
    fn choose(&mut self, choice: Choice, window: &mut Window, cx: &mut Context<Self>) {
        // The choices about tabs and about the app are the ones that do not
        // need a connected cluster, and the ones that can change which cluster
        // is in front.
        match choice {
            Choice::Cluster(id) => {
                self.go_to(id, window, cx);
                return;
            }
            Choice::Action(palette::Action::NewTab) => {
                self.new_tab(window, cx);
                return;
            }
            Choice::Action(palette::Action::CloseTab) => {
                self.close(self.active, window, cx);
                return;
            }
            Choice::Action(palette::Action::OpenShortcuts) => {
                crate::shortcuts::open(cx);
                cx.notify();
                return;
            }
            Choice::Action(palette::Action::OpenAppLogs) => {
                app_logs::open(cx);
                cx.notify();
                return;
            }
            Choice::Action(palette::Action::ToggleTheme) => {
                toggle_mode(window, cx);
                cx.notify();
                return;
            }
            _ => {}
        }

        let Some(cluster) = self.cluster() else {
            cx.notify();
            return;
        };

        cluster.update(cx, |cluster, cx| match choice {
            Choice::Kind(kind) => cluster.select_resource(kind, cx),
            Choice::Namespace(namespace) => cluster.set_namespace(namespace, window, cx),
            Choice::Object(object) => cluster.reveal(&object, window, cx),
            Choice::Operation(operation) => cluster.start(operation, window, cx),
            Choice::Forward { remote_port } => cluster.start_forward(remote_port, window, cx),
            Choice::Action(palette::Action::ToggleDetails) => cluster.toggle_details(window, cx),
            Choice::Action(palette::Action::ClearFilter) => cluster.clear_filter(window, cx),
            Choice::Action(palette::Action::CopyName) => {
                if let Some(selected) = cluster.selected(cx) {
                    cx.write_to_clipboard(ClipboardItem::new_string(selected.name.clone()));
                    tracing::info!(object = %selected, "copied name");
                }
            }
            // Handled above, before the cluster was required.
            Choice::Cluster(_)
            | Choice::Action(
                palette::Action::ToggleTheme
                | palette::Action::OpenAppLogs
                | palette::Action::OpenShortcuts
                | palette::Action::NewTab
                | palette::Action::CloseTab,
            ) => {}
        });

        cx.notify();
    }

    /// The palette, over a scrim that dismisses it.
    fn render_palette(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .inset_0()
            .bg(gpui_kit::black().opacity(0.35))
            .flex()
            .flex_col()
            .items_center()
            .pt(px(120.))
            .id("palette-scrim")
            .on_click(cx.listener(|view, _, _, cx| {
                view.palette_open = false;
                cx.notify();
            }))
            .child(
                // The palette itself must not take the scrim's dismiss click.
                div().id("palette").occlude().child(self.palette.clone()),
            )
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_dark = cx.theme().is_dark();

        TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .px_2()
                .gap_3()
                .child(
                    h_flex()
                        .gap_3()
                        .child(
                            Button::new("toggle-sidebar")
                                .ghost()
                                .small()
                                .icon(if self.sidebar_collapsed {
                                    IconName::PanelLeftOpen
                                } else {
                                    IconName::PanelLeftClose
                                })
                                .tooltip(if self.sidebar_collapsed {
                                    "Expand sidebar"
                                } else {
                                    "Collapse sidebar"
                                })
                                .on_click(cx.listener(|app, _, _, cx| {
                                    app.sidebar_collapsed = !app.sidebar_collapsed;
                                    cx.notify();
                                })),
                        )
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Beacon"))
                        .when(!cfg!(target_os = "macos"), |bar| {
                            bar.child(
                                Button::new("view-menu")
                                    .ghost()
                                    .small()
                                    .label("View")
                                    .dropdown_menu(|menu, _, _| {
                                        menu.menu("App logs", Box::new(OpenAppLogs))
                                            .menu("Keyboard shortcuts", Box::new(OpenShortcuts))
                                    }),
                            )
                        }),
                )
                .child(
                    Button::new("toggle-theme")
                        .ghost()
                        .small()
                        .label(if is_dark { "Light" } else { "Dark" })
                        .on_click(|_, window, cx| toggle_mode(window, cx)),
                ),
        )
    }

    fn context_entry(&self, id: &ClusterId) -> Option<&ContextEntry> {
        self.contexts.as_ref().ok()?.get(id)
    }

    fn cluster_display_name<'a>(&'a self, id: &'a ClusterId) -> &'a str {
        self.context_entry(id)
            .map_or_else(|| id.display_name(), ContextEntry::display_name)
    }

    fn cluster_description(&self, id: &ClusterId) -> String {
        match self.context_entry(id) {
            Some(entry) => format!(
                "{}\nContext: {id}\nCluster: {}",
                entry.label(),
                entry.cluster
            ),
            None => format!("Context: {id}"),
        }
    }

    /// Stable cluster colours tie each tab to its row in the sidebar.
    fn cluster_color(&self, id: &ClusterId, cx: &App) -> Hsla {
        let index = self
            .contexts
            .as_ref()
            .ok()
            .and_then(|contexts| contexts.entries().iter().position(|entry| &entry.id == id))
            .unwrap_or(0);
        match index % 4 {
            0 => cx.theme().danger,
            1 => cx.theme().success,
            2 => cx.theme().warning,
            _ => cx.theme().primary,
        }
    }

    /// The cluster tree belongs to the window, not to a tab. The expanded
    /// cluster borrows only its resource navigation from the active view.
    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.tabs.get(self.active).map(|tab| tab.cluster.clone());
        let (search, mut resources, searching) = self
            .cluster()
            .map(|cluster| {
                cluster.update(cx, |view, cx| {
                    (
                        Some(view.sidebar_search()),
                        view.sidebar_items(cx),
                        view.sidebar_search_active(),
                    )
                })
            })
            .unwrap_or((None, Vec::new(), false));
        let ids: Vec<ClusterId> = self
            .contexts
            .as_ref()
            .map(|contexts| {
                contexts
                    .entries()
                    .iter()
                    .map(|entry| entry.id.clone())
                    .collect()
            })
            .unwrap_or_default();

        let rows = ids.into_iter().map(|id| {
            let selected = active.as_ref() == Some(&id);
            let color = self.cluster_color(&id, cx);
            let connected = self.sessions.contains_key(&id);
            let target = id.clone();
            let menu_target = id.clone();
            let menu_view = cx.entity().downgrade();
            let can_disconnect = connected
                || self
                    .tabs
                    .iter()
                    .any(|tab| tab.cluster == id && matches!(tab.state, TabState::Connecting));
            let is_eks = self
                .context_entry(&id)
                .is_some_and(|entry| entry.eks().is_some());
            let description = self.cluster_description(&id);
            let tooltip_id = SharedString::from(format!("cluster-info-{id}"));
            SidebarMenuItem::new(self.cluster_display_name(&id).to_string())
                .icon(crate::icons::kubernetes().text_color(color))
                .active(selected)
                .click_to_open(true)
                .default_open(selected)
                .children(if selected {
                    std::mem::take(&mut resources)
                } else {
                    Vec::new()
                })
                .suffix(move |_, cx| {
                    let description = description.clone();
                    h_flex()
                        .id(tooltip_id.clone())
                        .gap_2()
                        .text_color(cx.theme().muted_foreground)
                        .when(is_eks, |row| row.child(div().text_xs().child("EKS")))
                        .child(if connected {
                            div()
                                .size(px(7.))
                                .rounded_full()
                                .bg(cx.theme().tone(Tone::Healthy))
                                .into_any_element()
                        } else {
                            div().child("›").into_any_element()
                        })
                        .tooltip(move |window, cx| {
                            Tooltip::new(description.clone()).build(window, cx)
                        })
                })
                .context_menu(move |menu, _, _| {
                    let target = menu_target.clone();
                    let view = menu_view.clone();
                    let reconnect_target = menu_target.clone();
                    let reconnect_view = menu_view.clone();
                    menu.item(
                        PopupMenuItem::new("Disconnect")
                            .disabled(!can_disconnect)
                            .on_click(move |_, window, cx| {
                                let _ =
                                    view.update(cx, |app, cx| app.disconnect(&target, window, cx));
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Reconnect").on_click(move |_, window, cx| {
                            let _ = reconnect_view.update(cx, |app, cx| {
                                app.reconnect(reconnect_target.clone(), window, cx)
                            });
                        }),
                    )
                })
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.go_to(target.clone(), window, cx);
                }))
        });

        // Search replaces the section list. Give it separate keyed expansion
        // state, or its "Matches" row could inherit a different category's
        // open state and pass that state back when the query is cleared.
        Sidebar::new(if searching {
            "cluster-tree-search"
        } else {
            "cluster-tree"
        })
        .collapsible(false)
        .w_full()
        .when_some(search, |sidebar, search| {
            sidebar.header(
                div()
                    .w_full()
                    .px_2()
                    .py_1()
                    .child(Input::new(&search).small()),
            )
        })
        .child(SidebarGroup::new("Clusters").child(SidebarMenu::new().children(rows)))
    }

    fn render_workspace(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = v_flex()
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .child(self.render_tabs(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.render_body(cx)),
            );

        if self.sidebar_collapsed {
            return content.into_any_element();
        }

        h_resizable("sidebar-split")
            .with_state(&self.sidebar_split)
            .child(
                resizable_panel()
                    .size(px(248.))
                    .size_range(px(168.)..px(480.))
                    .child(self.render_sidebar(cx)),
            )
            .child(
                resizable_panel()
                    .size_range(px(420.)..px(10000.))
                    .child(content),
            )
            .into_any_element()
    }

    /// The tab bar, always present so that `+` is always somewhere to click.
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.tabs.iter().enumerate().map(|(index, tab)| {
            let id = tab.id;
            let color = self.cluster_color(&tab.cluster, cx);
            let what = match &tab.state {
                TabState::Connected(view) => view.read(cx).title(),
                TabState::Disconnected => {
                    if tab.initial_mode == Mode::Objects {
                        tab.initial_kind
                            .as_ref()
                            .map(|kind| SharedString::from(kind.resource.kind.clone()))
                            .unwrap_or_else(|| SharedString::from("Cluster"))
                    } else {
                        SharedString::from(tab.initial_mode.label())
                    }
                }
                TabState::Connecting => SharedString::from("connecting"),
                TabState::Failed(_) => SharedString::from("unreachable"),
            };
            let accessible_name = format!("{what} · {}", self.cluster_display_name(&tab.cluster));

            TabItem::new()
                .prefix(
                    div()
                        .pl_2()
                        .child(div().size(px(7.)).rounded_full().bg(color)),
                )
                .label(what)
                .aria_label(accessible_name)
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.activate(index, window, cx);
                }))
                .suffix(
                    Button::new(SharedString::from(format!("close-tab-{id}")))
                        .xsmall()
                        .ghost()
                        .label("×")
                        .on_click(cx.listener(move |view, _, window, cx| {
                            // By id, not by index: the bar this button was
                            // built for may be a frame out of date.
                            if let Some(index) = view.index_of(id) {
                                view.close(index, window, cx);
                            }
                        })),
                )
        });

        let new_tab = div().pl_1().child(
            Button::new("new-tab")
                .xsmall()
                .ghost()
                .label("+")
                .tooltip("Open another tab on this cluster")
                .on_click(cx.listener(|view, _, window, cx| view.new_tab(window, cx))),
        );

        // Where + goes depends on whether the tabs still fit.
        //
        // `last_empty_space` is inside the bar's scroll area, just after the
        // last tab, which is where it belongs while there is room. `suffix`
        // is outside it, pinned to the right edge. Once the tabs overflow,
        // anything inside the scroll area can be scrolled out of reach, and a
        // button you cannot find is worse than one that is not where you
        // expected -- so at that point + moves to the edge and stays put.
        //
        // The bar reports the overflow itself, through the scroll handle's
        // `max_offset`, measured on the frame just drawn. That makes the
        // switch one frame late, which nobody can see, and costs no
        // measurement of our own. The few pixels of slack are hysteresis:
        // moving + out of the scroll area makes the content narrower, and
        // without them the two states could trade places forever.
        let overflowing = self.tab_scroll.max_offset().x > px(8.);

        TabBar::new("cluster-tabs")
            .track_scroll(&self.tab_scroll)
            .small()
            .max_width(TAB_WIDTH)
            .selected_index(self.active)
            .children(tabs)
            // Every tab by name, for when there are more of them than fit.
            // It also satisfies the component's rule that `last_empty_space`
            // is only drawn when there is a suffix or a menu.
            .menu(true)
            .map(|bar| match overflowing {
                true => bar.suffix(new_tab),
                false => bar.last_empty_space(new_tab),
            })
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(view) = self.cluster() {
            return view.into_any_element();
        }

        let state = self
            .tabs
            .get(self.active)
            .map(|tab| (&tab.cluster, &tab.state));

        let (tone, headline, detail) = match (state, &self.contexts) {
            (Some((id, TabState::Disconnected)), _) => (
                Tone::Unknown,
                format!("Disconnected from {}", self.cluster_display_name(id)),
                "Right-click the cluster in the sidebar and choose Reconnect.".to_string(),
            ),
            (Some((id, TabState::Connecting)), _) => (
                Tone::Progressing,
                format!("Connecting to {}", self.cluster_display_name(id)),
                "Authenticating and reaching the API server.".to_string(),
            ),
            (Some((id, TabState::Failed(diagnosis))), _) => (
                Tone::Critical,
                format!("Could not connect to {}", self.cluster_display_name(id)),
                diagnosis.clone(),
            ),
            (_, Err(error)) => (
                Tone::Critical,
                "Could not read kubeconfig".to_string(),
                error.clone(),
            ),
            (_, Ok(contexts)) if contexts.is_empty() => (
                Tone::Warning,
                "Kubeconfig has no contexts".to_string(),
                "Add a cluster with `kubectl config set-context`, then reopen Beacon.".to_string(),
            ),
            _ => (
                Tone::Unknown,
                if self.sessions.is_empty() { "No cluster selected" } else { "No tab open" }.to_string(),
                "Select a cluster in the sidebar, or use the cluster picker in the command palette.".to_string(),
            ),
        };

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_8()
            .child(
                h_flex()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .rounded_md()
                    .bg(cx.theme().tone_surface(tone))
                    .text_color(cx.theme().tone(tone))
                    .text_sm()
                    // Connecting can take a few seconds against a cloud API
                    // server, and a static line cannot say whether it is still
                    // trying or has quietly given up.
                    .when(tone == Tone::Progressing, |this| {
                        this.child(Spinner::new().small().color(cx.theme().tone(tone)))
                    })
                    .child(headline),
            )
            .child(
                div()
                    .max_w(px(640.))
                    .text_sm()
                    .text_center()
                    .text_color(cx.theme().muted_foreground)
                    .child(detail),
            )
            .into_any_element()
    }

    fn set_activity_open(
        &mut self,
        menu: ActivityMenu,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !open && self.activity_open != Some(menu) {
            return;
        }
        self.activity_open = open.then_some(menu);
        self.activity_refresh = None;
        if open {
            // Only refresh the inspector while it is visible. This reads the
            // existing registry; it never lists resources or adds watches.
            self.activity_refresh = Some(cx.spawn_in(window, async |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(1))
                        .await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
        cx.notify();
    }

    fn render_activity_menu(
        &self,
        menu: ActivityMenu,
        count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let opening = cx.entity().downgrade();
        let view = opening.clone();
        let (id, label, tooltip) = match menu {
            ActivityMenu::Watches => (
                "status-watches",
                "watches",
                "View watches in the current cluster",
            ),
            ActivityMenu::Clusters => ("status-clusters", "clusters", "View connected clusters"),
        };
        Popover::new(id)
            .anchor(Anchor::BottomRight)
            .open(self.activity_open == Some(menu))
            .on_open_change(move |open, window, cx| {
                let _ = opening.update(cx, |view, cx| {
                    view.set_activity_open(menu, *open, window, cx)
                });
            })
            .trigger(
                Button::new((id, 0usize))
                    .ghost()
                    .xsmall()
                    .h(px(20.))
                    .label(format!("{count} {label}"))
                    .tooltip(tooltip),
            )
            .content(move |_, _, cx| {
                let Some(app) = view.upgrade() else {
                    return div().into_any_element();
                };
                let app = app.read(cx);
                match menu {
                    ActivityMenu::Watches => match app.cluster() {
                        Some(cluster) => crate::activity::watches(cluster.read(cx).session(), cx),
                        None => crate::activity::panel(
                            "Watches (0)".into(),
                            "No connected cluster in the active tab.",
                            cx,
                        )
                        .into_any_element(),
                    },
                    ActivityMenu::Clusters => app.render_clusters(view.clone(), cx),
                }
            })
    }

    fn render_clusters(&self, view: WeakEntity<Self>, cx: &App) -> AnyElement {
        let mut sessions: Vec<_> = self.sessions.values().collect();
        sessions.sort_by_key(|session| session.id());
        let active = self.tabs.get(self.active).map(|tab| &tab.cluster);
        let rows = sessions.iter().map(|session| {
            let id = session.id().clone();
            let current = active == Some(&id);
            let tabs = self.tabs.iter().filter(|tab| tab.cluster == id).count();
            let health = session.health().borrow().clone();
            let target = id.clone();
            let view = view.clone();
            v_flex()
                .id(SharedString::from(format!("activity-cluster-{id}")))
                .gap_1()
                .p_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|row| row.bg(cx.theme().muted))
                .when(current, |row| row.bg(cx.theme().muted))
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_shrink_0()
                                .size(px(7.))
                                .rounded_full()
                                .bg(cx.theme().tone(crate::activity::health_tone(&health))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_weight(FontWeight::MEDIUM)
                                .child(self.cluster_display_name(&id).to_string()),
                        )
                        .when(current, |row| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(cx.theme().primary)
                                    .child("Current"),
                            )
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "Kubernetes {} · {}",
                            session.version(),
                            session.server()
                        )),
                )
                .when_some(
                    self.context_entry(&id).and_then(ContextEntry::eks),
                    |row, eks| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("EKS · {} · {}", eks.region, eks.account_id)),
                        )
                    },
                )
                .child(div().text_xs().child(format!(
                    "{} · {tabs} tabs · {} watches · {} forwarding",
                    health.label(),
                    session.active_watches(),
                    session.forwards().len(),
                )))
                .when_some(health.reason(), |row, reason| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().tone(Tone::Warning))
                            .child(reason.to_string()),
                    )
                })
                .on_click(move |_, window, cx| {
                    let _ = view.update(cx, |app, cx| {
                        app.activity_open = None;
                        app.activity_refresh = None;
                        app.go_to(target.clone(), window, cx);
                    });
                })
        });
        crate::activity::panel(
            format!("Clusters ({})", sessions.len()),
            "Connected sessions · click a cluster to open its tab",
            cx,
        )
        .child(
            div()
                .id("cluster-activity-list")
                .min_h_0()
                .overflow_y_scroll()
                .child(v_flex().gap_1().children(rows))
                .when(sessions.is_empty(), |list| {
                    list.child("No connected clusters yet.")
                }),
        )
        .into_any_element()
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.tabs.get(self.active);
        let name = active.map(|tab| {
            self.context_entry(&tab.cluster)
                .map_or_else(|| tab.cluster.display_name(), ContextEntry::cluster_name)
        });
        let mut description = active
            .map(|tab| self.cluster_description(&tab.cluster))
            .unwrap_or_default();
        let (tone, status, watches) = match active.map(|tab| &tab.state) {
            None => (
                Tone::Unknown,
                if self.sessions.is_empty() {
                    "no cluster selected"
                } else {
                    "no tab open"
                }
                .to_string(),
                0,
            ),
            Some(TabState::Disconnected) => (
                Tone::Unknown,
                format!("{} · disconnected", name.unwrap_or_default()),
                0,
            ),
            Some(TabState::Connecting) => (
                Tone::Progressing,
                format!("{} · connecting", name.unwrap_or_default()),
                0,
            ),
            Some(TabState::Failed(reason)) => {
                description.push_str(&format!("\n{reason}"));
                (
                    Tone::Critical,
                    format!("{} · disconnected", name.unwrap_or_default()),
                    0,
                )
            }
            Some(TabState::Connected(cluster)) => {
                let cluster = cluster.read(cx);
                let session = cluster.session();
                let health = cluster.health();
                description.push_str(&format!("\nAPI server: {}", session.server()));
                if let Some(reason) = health.reason() {
                    description.push_str(&format!("\n{reason}"));
                }
                let mut status = format!(
                    "{} · Kubernetes {} · {}",
                    name.unwrap_or_default(),
                    session.version(),
                    health.label()
                );
                if self.tabs.len() > 1 {
                    status.push_str(&format!(" · {} tabs", self.tabs.len()));
                }
                let forwards = session.forwards().len();
                if forwards > 0 {
                    status.push_str(&format!(" · {forwards} forwarding"));
                }
                (
                    crate::activity::health_tone(health),
                    status,
                    session.active_watches(),
                )
            }
        };

        h_flex()
            .w_full()
            .h(px(24.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().table_header())
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(
                div()
                    .flex_shrink_0()
                    .child(format!("Beacon {}", env!("CARGO_PKG_VERSION"))),
            )
            .child(
                h_flex()
                    .min_w_0()
                    .gap_1p5()
                    .items_center()
                    .child(
                        div()
                            .flex_shrink_0()
                            .size(px(7.))
                            .rounded_full()
                            .bg(cx.theme().tone(tone)),
                    )
                    .child(
                        div()
                            .id("cluster-status")
                            .min_w_0()
                            .truncate()
                            .child(status)
                            .tooltip(move |window, cx| {
                                Tooltip::new(description.clone()).build(window, cx)
                            }),
                    )
                    .child(div().flex_shrink_0().child(self.render_activity_menu(
                        ActivityMenu::Watches,
                        watches,
                        cx,
                    )))
                    .child(div().flex_shrink_0().child(self.render_activity_menu(
                        ActivityMenu::Clusters,
                        self.sessions.len(),
                        cx,
                    ))),
            )
    }
}

impl Render for BeaconApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .track_focus(&self.focus)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(
                cx.listener(|view, _: &TogglePalette, window, cx| view.toggle_palette(window, cx)),
            )
            .on_action(cx.listener(|view, _: &NewTab, window, cx| view.new_tab(window, cx)))
            .on_action(
                cx.listener(|view, _: &CloseTab, window, cx| view.close(view.active, window, cx)),
            )
            .on_action(cx.listener(|view, _: &NextTab, window, cx| view.step(true, window, cx)))
            .on_action(
                cx.listener(|view, _: &PreviousTab, window, cx| view.step(false, window, cx)),
            )
            // Handled here rather than in ClusterView, which is where it
            // belongs and where it does not work: an action travels from the
            // focused node *upwards*, and ClusterView is a child of the node
            // that holds focus, not an ancestor of it.
            .on_action(cx.listener(|view, _: &CloseDetail, window, cx| {
                if let Some(cluster) = view.cluster() {
                    cluster.update(cx, |cluster, cx| cluster.close_detail(window, cx));
                }
            }))
            .child(
                v_flex()
                    .size_full()
                    .child(self.render_title_bar(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .child(self.render_workspace(cx)),
                    )
                    .child(self.render_status_bar(cx)),
            )
            .when(self.palette_open, |this| {
                // Deferred so it paints over the table rather than under it.
                this.child(deferred(self.render_palette(cx)))
            })
    }
}
