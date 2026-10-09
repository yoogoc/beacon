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
//! cluster in the shared connection registry, so a second tab on a cluster
//! costs a view and nothing else.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
};

use beacon_kube::{
    ClusterId, ClusterSession, Kind,
    config::{ContextEntry, Contexts},
};
use gpui_kit::base::{Tab as TabItem, Tabs};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Root, Sizable as _, TitleBar, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app_logs;
use crate::cluster::{ClusterView, Mode, NavigationChanged, ResourceRequested};
use crate::connections::{Connections, SharedConnections};
use crate::copyable_text::{copy_item, copyable_text};
use crate::palette::{self, Choice, Palette, PaletteEvent};
use crate::theme::{BeaconTheme as _, Tone, toggle_mode};
use crate::workspace::{Direction, Node, Pane, PaneId, Workspace};
use gpui_kit::base::ElementExt as _;

gpui_kit::actions!(
    beacon,
    [
        TogglePalette,
        OpenAppLogs,
        OpenShortcuts,
        OpenSettings,
        Quit,
        NewTab,
        CloseTab,
        NextTab,
        PreviousTab,
        SplitRight,
        SplitDown,
        DetachTab,
        MergeToMain,
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
    let connections = cx.new(|_| Connections::default());
    cx.set_global(SharedConnections(connections));
    cx.set_global(WorkspaceWindows::default());
    crate::settings::init(cx);
    crate::preferences::init(cx);
    app_logs::init(log_directory, cx);
    cx.on_action(|_: &OpenSettings, cx| crate::preferences::application(cx));
    crate::shortcuts::init(cx);
    cx.on_action(|_: &OpenShortcuts, cx| crate::shortcuts::open(cx));
    cx.on_action(|_: &OpenAppLogs, cx| app_logs::open(cx));
    cx.on_action(|_: &Quit, cx| cx.quit());

    #[cfg(target_os = "macos")]
    cx.set_menus([
        Menu::new("Beacon").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Beacon", Quit),
        ]),
        Menu::new("File").items([MenuItem::action("Close", CloseTab)]),
        Menu::new("View").items([
            MenuItem::action("Split right", SplitRight),
            MenuItem::action("Split down", SplitDown),
            MenuItem::action("Move tab to new window", DetachTab),
            MenuItem::action("Move tab to main window", MergeToMain),
            MenuItem::separator(),
            MenuItem::action("App logs", OpenAppLogs),
        ]),
        Menu::new("Help").items([MenuItem::action("Keyboard shortcuts", OpenShortcuts)]),
    ]);
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    cx.bind_keys([
        KeyBinding::new("f1", OpenShortcuts, None),
        KeyBinding::new(&format!("{modifier}-,"), OpenSettings, None),
        KeyBinding::new(&format!("{modifier}-q"), Quit, None),
        KeyBinding::new(&format!("{modifier}-k"), TogglePalette, None),
        KeyBinding::new(&format!("{modifier}-shift-l"), OpenAppLogs, None),
        KeyBinding::new(&format!("{modifier}-t"), NewTab, None),
        KeyBinding::new(&format!("{modifier}-w"), CloseTab, None),
        // Not `cmd-shift-[` and friends: ctrl-tab is the one pair that means
        // the same thing on all three platforms.
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        KeyBinding::new(&format!("{modifier}-alt-right"), SplitRight, None),
        KeyBinding::new(&format!("{modifier}-alt-down"), SplitDown, None),
        // The one key here that is *not* global. Escape belongs to whatever
        // has focus -- a shell, a search box -- and only reaches the cluster
        // view when nothing nearer wanted it.
        // Modal sheets and popup menus consume Escape before the workspace.
        // A global binding would override their more specific cancel actions.
        KeyBinding::new(
            "escape",
            CloseDetail,
            Some("BeaconWorkspace && !Sheet && !PopupMenu"),
        ),
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

#[derive(Default)]
struct WorkspaceWindows {
    windows: HashMap<EntityId, WorkspaceWindow>,
    next_tab: u64,
    drag: Option<DraggedTab>,
    drop_target: Option<TabDropTarget>,
}
impl Global for WorkspaceWindows {}

struct WorkspaceWindow {
    view: WeakEntity<BeaconApp>,
    handle: AnyWindowHandle,
    main: bool,
    // Hit-test geometry is written during rendering. Keep it outside tracked
    // global mutations so measuring a frame does not invalidate retained views.
    bounds: Cell<Bounds<Pixels>>,
    panes: RefCell<HashMap<PaneId, Bounds<Pixels>>>,
    tabs: RefCell<HashMap<u64, Bounds<Pixels>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TabDropTarget {
    window: EntityId,
    pane: PaneId,
    before: Option<u64>,
    split: Option<Direction>,
}

#[derive(Clone)]
struct DraggedTab {
    source: WeakEntity<BeaconApp>,
    id: u64,
    title: SharedString,
}
struct DragLabel(SharedString);
impl Render for DragLabel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .rounded_md()
            .shadow_md()
            .bg(cx.theme().tab_active)
            .text_color(cx.theme().foreground)
            .border_1()
            .border_color(cx.theme().primary)
            .text_sm()
            .child(self.0.clone())
    }
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
    tab_scrolls: HashMap<PaneId, ScrollHandle>,
    layout: Workspace,
    pane_splits: HashMap<u64, Entity<ResizableState>>,
    main_window: bool,
    sidebar_collapsed: bool,
    sidebar_split: Entity<ResizableState>,
    contexts: Result<Contexts, String>,
    preferences: crate::settings::Preferences,

    /// Every cluster connected in this session, shared by every tab on it.
    ///
    /// A session is a client, a discovery cache, a permission cache and any
    /// port forwards -- all cheap to hold and slow to rebuild. They outlive
    /// the tabs that opened them, which is what makes reopening one instant.
    /// The *watches* belong to the views, so closing a tab stops what it was
    /// watching.
    connections: Entity<Connections>,

    tabs: Vec<Tab>,
    /// Index into `tabs`. Meaningless, and never read, while `tabs` is empty.
    active: usize,

    palette: Entity<Palette>,
    palette_open: bool,
    activity_open: Option<ActivityMenu>,
    activity_refresh: Option<Task<()>>,
    quit_prompt_open: bool,
    _subscriptions: Vec<Subscription>,
}

impl BeaconApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new_workspace(true, window, cx)
    }

    fn new_workspace(main_window: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
                    view.focus.focus(window, cx);
                    view.choose(choice.clone(), window, cx);
                }
                PaletteEvent::Dismissed => {
                    view.palette_open = false;
                    view.focus.focus(window, cx);
                    cx.notify();
                }
            },
        );

        let settings = crate::settings::store(cx);
        let preferences = settings.read(cx).preferences.clone();
        let preferences_events = cx.subscribe_in(
            &settings,
            window,
            move |view, state, event: &crate::settings::Changed, window, cx| {
                view.preferences = state.read(cx).preferences.clone();
                if main_window && let Some(cluster) = &event.0 {
                    view.reconnect(cluster.clone(), window, cx);
                }
                cx.notify();
            },
        );
        let connections = cx.global::<SharedConnections>().0.clone();
        let connection_events = cx.subscribe_in(
            &connections,
            window,
            |view, _, event: &crate::connections::Changed, window, cx| {
                view.sync_connection(&event.0, window, cx);
            },
        );
        let id = cx.entity_id();
        let weak = cx.weak_entity();
        cx.global_mut::<WorkspaceWindows>().windows.insert(
            id,
            WorkspaceWindow {
                view: weak,
                handle: window.window_handle(),
                main: main_window,
                bounds: Cell::new(window.bounds()),
                panes: RefCell::new(HashMap::new()),
                tabs: RefCell::new(HashMap::new()),
            },
        );
        cx.on_release(move |_, cx| {
            cx.global_mut::<WorkspaceWindows>().windows.remove(&id);
        })
        .detach();
        let this = Self {
            focus: cx.focus_handle(),
            tab_scrolls: HashMap::from([(0, ScrollHandle::new())]),
            layout: Workspace::default(),
            pane_splits: HashMap::new(),
            main_window,
            sidebar_collapsed: false,
            sidebar_split: cx.new(|_| ResizableState::default()),
            contexts,
            preferences,
            connections,
            tabs: Vec::new(),
            active: 0,
            palette,
            palette_open: false,
            activity_open: None,
            activity_refresh: None,
            quit_prompt_open: false,
            _subscriptions: vec![palette_events, preferences_events, connection_events],
        };

        // Native window closing (including macOS's close-window shortcut)
        // does not pass through the resource-tab action handler.
        let view = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = view.update(cx, |view, cx| {
                if view.main_window {
                    view.request_quit(window, cx);
                } else {
                    window.defer(cx, |window, _| window.remove_window());
                }
            });
            false
        });

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
        let id = cx.global::<WorkspaceWindows>().next_tab;
        cx.global_mut::<WorkspaceWindows>().next_tab += 1;

        self.tabs.push(Tab {
            id,
            cluster,
            namespace,
            initial_kind,
            initial_scope,
            initial_mode: Mode::Objects,
            state: TabState::Connecting,
            _navigation: None,
            _resource_requests: None,
        });
        self.layout.insert(id, self.layout.focused, None);

        let index = self.tabs.len() - 1;
        self.activate(index, window, cx);
        self.connect(index, window, cx);
    }

    /// Goes to `cluster`: its tab if one is open, a new tab if not.
    ///
    /// The resource context menu and ⌘T open new tabs; selecting a cluster
    /// reuses one in the focused pane.
    fn go_to(&mut self, cluster: ClusterId, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .tabs
            .get(self.active)
            .is_some_and(|tab| tab.cluster == cluster)
        {
            return;
        }
        match self.tabs.iter().position(|tab| {
            tab.cluster == cluster && self.layout.owner(tab.id) == Some(self.layout.focused)
        }) {
            Some(index) => self.activate(index, window, cx),
            None => self.open(cluster, window, cx),
        }
    }

    /// One ordinary tab per cluster and resource kind. Explicit context-menu
    /// opens may create duplicates; ordinary selection prefers the active one,
    /// then the most recently opened matching tab in the focused pane.
    fn go_to_kind(
        &mut self,
        cluster: ClusterId,
        kind: Arc<Kind>,
        scope: BTreeSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let matches = |tab: &Tab| {
            if tab.cluster != cluster || self.layout.owner(tab.id) != Some(self.layout.focused) {
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

    fn disconnect(&mut self, cluster: &ClusterId, window: &mut Window, cx: &mut Context<Self>) {
        self.connections
            .update(cx, |state, cx| state.disconnect(cluster, cx));
        self.sync_connection(cluster, window, cx);
    }

    fn reconnect(&mut self, cluster: ClusterId, window: &mut Window, cx: &mut Context<Self>) {
        self.disconnect(&cluster, window, cx);
        let options = self.preferences.connection(&cluster);
        self.connections.update(cx, |state, cx| {
            state.disconnected.remove(&cluster);
            state.connect(cluster.clone(), options, cx);
        });
        if !self.tabs.iter().any(|tab| tab.cluster == cluster)
            && !cx
                .global::<WorkspaceWindows>()
                .windows
                .values()
                .any(|entry| {
                    entry.view.entity_id() != cx.entity_id()
                        && entry.view.upgrade().is_some_and(|app| {
                            app.read(cx).tabs.iter().any(|tab| tab.cluster == cluster)
                        })
                })
        {
            self.open(cluster.clone(), window, cx);
        }
        self.sync_connection(&cluster, window, cx);
    }

    fn connect(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cluster) = self.tabs.get(index).map(|tab| tab.cluster.clone()) else {
            return;
        };
        let options = self.preferences.connection(&cluster);
        self.connections
            .update(cx, |state, cx| state.connect(cluster.clone(), options, cx));
        self.sync_connection(&cluster, window, cx);
    }

    /// Broadcast connection changes across all windows; rebuild only after a
    /// session replacement. Moving a tab does not use this path.
    fn sync_connection(
        &mut self,
        cluster: &ClusterId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.connections.read(cx);
        let session = state.sessions.get(cluster).cloned();
        let disconnected = state.disconnected.contains(cluster);
        let error = state.errors.get(cluster).cloned();
        let connecting = state.connecting(cluster);
        let indices: Vec<_> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| &tab.cluster == cluster)
            .map(|(ix, _)| ix)
            .collect();
        for index in indices {
            if let Some(session) = &session {
                if !matches!(self.tabs[index].state, TabState::Connected(_)) {
                    let namespace = self.tabs[index].namespace.clone();
                    self.show(index, session.clone(), namespace, window, cx);
                }
                continue;
            }
            let tab = &mut self.tabs[index];
            if let TabState::Connected(view) = &tab.state {
                let (kind, scope, mode) = view.read(cx).navigation();
                tab.initial_kind = kind;
                tab.initial_scope = Some(scope);
                tab.initial_mode = mode;
            }
            tab._navigation = None;
            tab._resource_requests = None;
            tab.state = if disconnected {
                TabState::Disconnected
            } else if let Some(error) = &error {
                TabState::Failed(error.clone())
            } else if connecting {
                TabState::Connecting
            } else {
                TabState::Disconnected
            };
        }
        if session.is_none()
            && self
                .tabs
                .get(self.active)
                .is_some_and(|tab| &tab.cluster == cluster)
        {
            self.activity_open = None;
            self.activity_refresh = None;
            self.palette_open = false;
            self.focus.focus(window, cx);
        }
        cx.notify();
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
        let is_eks = self
            .context_entry(session.id())
            .is_some_and(|entry| entry.eks().is_some());
        let view = cx.new(|cx| {
            ClusterView::new(
                session,
                namespace,
                initial_kind,
                initial_scope,
                is_eks,
                window,
                cx,
            )
        });

        if initial_mode != Mode::Objects {
            view.update(cx, |view, cx| view.show_mode(initial_mode, window, cx));
        }

        // A tab that connected in the background must not start its timers: a
        // view is visible until told otherwise, and nothing else would tell it.
        if !self.layout.visible(self.tabs[index].id) {
            view.update(cx, |view, cx| view.set_visible(false, window, cx));
        }

        self.tabs[index].state = TabState::Connected(view);
        self.bind_tab(index, window, cx);
        cx.notify();
    }

    fn bind_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.view(index) else {
            return;
        };

        let navigation = cx.subscribe(&view, |_, _, _: &NavigationChanged, cx| cx.notify());
        let cluster = self.tabs[index].cluster.clone();
        let id = self.tabs[index].id;
        let resource_requests = cx.subscribe_in(
            &view,
            window,
            move |app, _, event: &ResourceRequested, window, cx| {
                if let Some(index) = app.index_of(id) {
                    app.activate(index, window, cx);
                }
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
        self.tabs[index]._navigation = Some(navigation);
        self.tabs[index]._resource_requests = Some(resource_requests);
        cx.notify();
    }

    /// Focus a tab's pane while leaving the other panes visible.
    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.tabs.get(index).map(|tab| tab.id) else {
            return;
        };
        self.layout.select(id);
        self.sync_layout(window, cx);
    }

    fn sync_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut splits = Vec::new();
        self.layout.root.split_ids(&mut splits);
        self.pane_splits.retain(|id, _| splits.contains(id));
        self.active = self
            .layout
            .active()
            .and_then(|id| self.index_of(id))
            .unwrap_or(0);
        let panes = self.layout.panes();
        self.tab_scrolls
            .retain(|id, _| panes.iter().any(|pane| &pane.id == id));
        for pane in &panes {
            self.tab_scrolls.entry(pane.id).or_default();
        }
        for tab in &self.tabs {
            if let TabState::Connected(view) = &tab.state {
                view.update(cx, |view, cx| {
                    view.set_visible(self.layout.visible(tab.id), window, cx)
                });
            }
        }
        cx.notify();
    }

    fn focus_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.focused == pane {
            return;
        }
        self.layout.focus(pane);
        self.sync_layout(window, cx);
    }

    fn take_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) -> Option<Tab> {
        let index = self.index_of(id)?;
        let mut tab = self.tabs.remove(index);
        // Subscriptions belong to the containing workspace, not to the view.
        tab._navigation = None;
        tab._resource_requests = None;
        self.layout.remove(id);
        self.focus.focus(window, cx);
        self.sync_layout(window, cx);
        Some(tab)
    }

    fn close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.tabs.get(index).map(|tab| tab.id) {
            self.take_tab(id, window, cx);
            self.close_empty_window(window, cx);
        }
    }

    fn close_empty_window(&self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.main_window && self.tabs.is_empty() {
            window.defer(cx, |window, _| window.remove_window());
        }
    }

    fn receive_tab(
        &mut self,
        tab: Tab,
        pane: PaneId,
        before: Option<u64>,
        split: Option<Direction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pane = if self.layout.pane(pane).is_some() {
            pane
        } else {
            self.layout.focused
        };
        let id = tab.id;
        let cluster = tab.cluster.clone();
        self.tabs.push(tab);
        let index = self.tabs.len() - 1;
        if let Some(direction) = split {
            self.layout.split(pane, id, direction);
        } else {
            self.layout.insert(id, pane, before);
        }
        self.bind_tab(index, window, cx);
        self.sync_layout(window, cx);
        if !matches!(self.tabs[index].state, TabState::Connected(_)) {
            self.sync_connection(&cluster, window, cx);
        }
        self.focus.focus(window, cx);
        window.activate_window();
    }

    fn split_tab(
        &mut self,
        id: u64,
        direction: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        self.activate(index, window, cx);
        let pane = self.layout.focused;
        let tab = &self.tabs[index];
        let cluster = tab.cluster.clone();
        let (kind, scope, mode, filters) = match &tab.state {
            TabState::Connected(view) => {
                let view = view.read(cx);
                let (kind, scope, mode) = view.navigation();
                (kind, Some(scope), mode, Some(view.view_filters(cx)))
            }
            _ => (
                tab.initial_kind.clone(),
                tab.initial_scope.clone(),
                tab.initial_mode,
                None,
            ),
        };
        self.open_kind(cluster, kind, scope, window, cx);
        let new_index = self.active;
        let new_id = self.tabs[new_index].id;
        self.tabs[new_index].initial_mode = mode;
        if let Some(view) = self.view(new_index) {
            view.update(cx, |view, cx| {
                if mode != Mode::Objects {
                    view.show_mode(mode, window, cx);
                }
                if let Some(filters) = filters {
                    view.restore_filters(filters, window, cx);
                }
            });
        }
        self.layout.move_tab(new_id, pane, None, Some(direction));
        self.sync_layout(window, cx);
        self.focus.focus(window, cx);
    }

    fn detach_tab(
        &mut self,
        id: u64,
        position: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.index_of(id).is_none() {
            return;
        }
        let source = cx.weak_entity();
        let source_handle = window.window_handle();
        let bounds = position
            .map(|point| {
                Bounds::new(
                    point - gpui_kit::point(px(40.), px(20.)),
                    size(px(1000.), px(700.)),
                )
            })
            .unwrap_or_else(|| {
                Bounds::centered(
                    window.display(cx).map(|display| display.id()),
                    size(px(1000.), px(700.)),
                    cx,
                )
            });
        cx.defer(move |cx| {
            let mut destination = None;
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(560.), px(400.))),
                    ..TitleBar::window_options()
                },
                |window, cx| {
                    window.set_window_title("Beacon — Detached workspace");
                    let app = cx.new(|cx| Self::new_workspace(false, window, cx));
                    destination = Some(app.clone());
                    cx.new(|cx| Root::new(app, window, cx))
                },
            );
            match (opened, destination) {
                (Ok(handle), Some(destination)) => {
                    let target = TabDropTarget {
                        window: destination.entity_id(),
                        pane: 0,
                        before: None,
                        split: None,
                    };
                    Self::transfer_tab(source, source_handle, id, target, cx);
                    if destination.read(cx).tabs.is_empty() {
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                    }
                }
                (Err(error), _) => tracing::error!(%error, "could not detach tab"),
                _ => {}
            }
        });
    }

    /// Two sequential window updates make the transfer atomic for tab ownership.
    /// If a destination closed while a deferred drop was queued, restore the tab.
    fn transfer_tab(
        source: WeakEntity<Self>,
        source_handle: AnyWindowHandle,
        id: u64,
        target: TabDropTarget,
        cx: &mut App,
    ) {
        let Some(entry) = cx.global::<WorkspaceWindows>().windows.get(&target.window) else {
            return;
        };
        let destination = entry.view.clone();
        let destination_handle = entry.handle;
        if destination.entity_id() == source.entity_id() {
            let _ = source_handle.update(cx, |_, window, cx| {
                let _ = source.update(cx, |app, cx| {
                    if app
                        .layout
                        .move_tab(id, target.pane, target.before, target.split)
                    {
                        app.sync_layout(window, cx);
                        app.focus.focus(window, cx);
                    }
                });
            });
            return;
        }
        if !destination
            .upgrade()
            .is_some_and(|app| app.read(cx).layout.pane(target.pane).is_some())
        {
            return;
        }
        let mut original_pane = 0;
        let taken = source_handle
            .update(cx, |_, window, cx| {
                source
                    .update(cx, |app, cx| {
                        original_pane = app.layout.owner(id).unwrap_or(app.layout.focused);
                        app.take_tab(id, window, cx)
                    })
                    .ok()
                    .flatten()
            })
            .ok()
            .flatten();
        let Some(tab) = taken else {
            return;
        };
        let mut payload = Some(tab);
        let _ = destination_handle.update(cx, |_, window, cx| {
            let _ = destination.update(cx, |app, cx| {
                app.receive_tab(
                    payload.take().unwrap(),
                    target.pane,
                    target.before,
                    target.split,
                    window,
                    cx,
                );
            });
            let source = source.clone();
            // Async callbacks resolve the last rendered window of the moved
            // subtree. Draw it in the destination before closing its source.
            window.on_next_frame(move |_, cx| {
                let _ = source_handle.update(cx, |_, window, cx| {
                    let _ = source.update(cx, |app, cx| app.close_empty_window(window, cx));
                });
            });
        });
        if let Some(tab) = payload {
            let _ = source_handle.update(cx, |_, window, cx| {
                let _ = source.update(cx, |app, cx| {
                    app.receive_tab(tab, original_pane, None, None, window, cx)
                });
            });
        }
    }

    fn merge_to_main(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.main_window {
            return;
        }
        let target = cx
            .global::<WorkspaceWindows>()
            .windows
            .iter()
            .find(|(_, entry)| entry.main)
            .and_then(|(id, entry)| {
                entry.view.upgrade().map(|app| TabDropTarget {
                    window: *id,
                    pane: app.read(cx).layout.focused,
                    before: None,
                    split: None,
                })
            });
        if let Some(target) = target {
            let source = cx.weak_entity();
            let handle = window.window_handle();
            cx.defer(move |cx| Self::transfer_tab(source, handle, id, target, cx));
        }
    }

    fn drag_target(
        position: Point<Pixels>,
        preferred: EntityId,
        cx: &App,
    ) -> Option<TabDropTarget> {
        let windows = &cx.global::<WorkspaceWindows>().windows;
        let (id, entry) = if let Some(stack) = cx.window_stack() {
            stack.iter().find_map(|handle| {
                windows.iter().find(|(_, entry)| {
                    entry.handle == *handle && entry.bounds.get().contains(&position)
                })
            })
        } else {
            // Platforms without native stacking information still route drags
            // outside the source window to another workspace.
            windows
                .get(&preferred)
                .filter(|entry| entry.bounds.get().contains(&position))
                .map(|entry| (&preferred, entry))
                .or_else(|| {
                    windows
                        .iter()
                        .find(|(_, entry)| entry.bounds.get().contains(&position))
                })
        }?;
        let panes = entry.panes.borrow();
        let (pane, bounds) = panes
            .iter()
            .find(|(_, bounds)| bounds.contains(&position))?;
        let local = position - bounds.origin;
        let tab_bar = local.y < px(30.);
        let before = if tab_bar {
            entry
                .tabs
                .borrow()
                .iter()
                .filter(|(_, tab)| {
                    tab.origin.y >= bounds.origin.y
                        && tab.origin.y < bounds.origin.y + px(30.)
                        && tab.origin.x >= bounds.origin.x
                        && tab.origin.x < bounds.right()
                        && tab.right() > position.x
                })
                .min_by(|(_, left), (_, right)| {
                    left.origin
                        .x
                        .partial_cmp(&right.origin.x)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(id, _)| *id)
        } else {
            None
        };
        let split = if tab_bar {
            None
        } else {
            let x = local.x / bounds.size.width;
            let y = local.y / bounds.size.height;
            crate::workspace::split_at(x, y)
        };
        Some(TabDropTarget {
            window: *id,
            pane: *pane,
            before,
            split,
        })
    }

    fn drag_move(
        &mut self,
        event: &DragMoveEvent<DraggedTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let position = window.bounds().origin + event.event.position;
        let target = Self::drag_target(position, cx.entity_id(), cx);
        if cx.global::<WorkspaceWindows>().drop_target != target {
            cx.global_mut::<WorkspaceWindows>().drop_target = target;
            cx.refresh_windows();
        }
    }

    fn finish_drag(
        event: &MouseUpEvent,
        phase: DispatchPhase,
        current: EntityId,
        window: &mut Window,
        cx: &mut App,
    ) {
        if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
            return;
        }
        let Some(drag) = cx.global_mut::<WorkspaceWindows>().drag.take() else {
            return;
        };
        let dragging = cx.has_active_drag();
        let position = window.bounds().origin + event.position;
        let target = Self::drag_target(position, current, cx);
        cx.global_mut::<WorkspaceWindows>().drop_target = None;
        cx.stop_active_drag(window);
        cx.refresh_windows();
        if !dragging {
            return;
        }
        let source_handle = cx
            .global::<WorkspaceWindows>()
            .windows
            .get(&drag.source.entity_id())
            .map(|entry| entry.handle);
        let Some(source_handle) = source_handle else {
            return;
        };
        if let Some(target) = target {
            cx.defer(move |cx| Self::transfer_tab(drag.source, source_handle, drag.id, target, cx));
        } else if !cx
            .global::<WorkspaceWindows>()
            .windows
            .values()
            .any(|entry| entry.bounds.get().contains(&position))
        {
            cx.defer(move |cx| {
                let _ = source_handle.update(cx, |_, window, cx| {
                    let _ = drag.source.update(cx, |app, cx| {
                        app.detach_tab(drag.id, Some(position), window, cx)
                    });
                });
            });
        }
    }

    fn close_tab_or_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            if self.main_window {
                self.request_quit(window, cx);
            } else {
                self.close_empty_window(window, cx);
            }
        } else {
            self.close(self.active, window, cx);
        }
    }

    /// Keep the window and its sessions alive until an explicit confirmation.
    /// Repeated close requests share the same outstanding prompt.
    fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_prompt_open {
            return;
        }
        self.quit_prompt_open = true;
        let answer = window.prompt(
            PromptLevel::Warning,
            "Quit Beacon?",
            Some("All Beacon windows will close. Active cluster connections, shells and port forwards will stop."),
            &[PromptButton::cancel("Cancel"), PromptButton::ok("Quit")],
            cx,
        );
        cx.spawn_in(window, async move |view, cx| {
            let confirmed = matches!(answer.await, Ok(1));
            let _ = view.update_in(cx, |view, _, cx| {
                view.quit_prompt_open = false;
                if confirmed {
                    cx.quit();
                }
            });
        })
        .detach();
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
        let Some(pane) = self.layout.pane(self.layout.focused) else {
            return;
        };
        if pane.tabs.len() < 2 {
            return;
        }
        let current = pane
            .active
            .and_then(|id| pane.tabs.iter().position(|tab| *tab == id))
            .unwrap_or(0);
        let next = if forward {
            (current + 1) % pane.tabs.len()
        } else {
            (current + pane.tabs.len() - 1) % pane.tabs.len()
        };
        if let Some(index) = self.index_of(pane.tabs[next]) {
            self.activate(index, window, cx);
        }
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

        sources.cluster_aliases = self
            .preferences
            .clusters
            .iter()
            .filter(|(_, c)| !c.alias.is_empty())
            .map(|(id, c)| (ClusterId::new(id), c.alias.clone()))
            .collect();
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
            Choice::Action(
                action @ (palette::Action::SplitRight
                | palette::Action::SplitDown
                | palette::Action::DetachTab
                | palette::Action::MergeToMain),
            ) => {
                if let Some(id) = self.tabs.get(self.active).map(|tab| tab.id) {
                    match action {
                        palette::Action::SplitRight => {
                            self.split_tab(id, Direction::Right, window, cx)
                        }
                        palette::Action::SplitDown => {
                            self.split_tab(id, Direction::Down, window, cx)
                        }
                        palette::Action::DetachTab => self.detach_tab(id, None, window, cx),
                        palette::Action::MergeToMain => self.merge_to_main(id, window, cx),
                        _ => unreachable!(),
                    }
                }
                return;
            }
            Choice::Cluster(id) => {
                self.go_to(id, window, cx);
                return;
            }
            Choice::Action(palette::Action::NewTab) => {
                self.new_tab(window, cx);
                return;
            }
            Choice::Action(palette::Action::CloseTab) => {
                self.close_tab_or_quit(window, cx);
                return;
            }
            Choice::Action(palette::Action::OpenSettings) => {
                crate::preferences::application(cx);
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
                | palette::Action::OpenSettings
                | palette::Action::OpenAppLogs
                | palette::Action::OpenShortcuts
                | palette::Action::NewTab
                | palette::Action::SplitRight
                | palette::Action::SplitDown
                | palette::Action::DetachTab
                | palette::Action::MergeToMain
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
        // TitleBar marks its content as a native window drag area. Each
        // control must occlude that hitbox so Windows delivers client clicks
        // to the button instead of treating them as caption interactions.
        let is_dark = cx.theme().is_dark();
        let title_bar = TitleBar::new().on_close_window(cx.listener(|view, _, window, cx| {
            if view.main_window {
                view.request_quit(window, cx);
            } else {
                window.defer(cx, |window, _| window.remove_window());
            }
        }));

        title_bar.child(
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
                                .occlude()
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
                                    .occlude()
                                    .ghost()
                                    .small()
                                    .label("Menu")
                                    .dropdown_menu(|menu, _, _| {
                                        menu.menu("Settings…", Box::new(OpenSettings))
                                            .menu("App logs", Box::new(OpenAppLogs))
                                            .menu("Keyboard shortcuts", Box::new(OpenShortcuts))
                                    }),
                            )
                        }),
                )
                .child(
                    Button::new("toggle-theme")
                        .occlude()
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
        if let Some(settings) = self.preferences.clusters.get(id.as_str())
            && !settings.alias.is_empty()
        {
            return &settings.alias;
        }
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
            let connected = self.connections.read(cx).sessions.contains_key(&id);
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
            let icon = self
                .preferences
                .clusters
                .get(id.as_str())
                .map(|c| c.icon.clone())
                .unwrap_or_default();
            let description = self.cluster_description(&id);
            let tooltip_id = SharedString::from(format!("cluster-info-{id}"));
            SidebarMenuItem::new(self.cluster_display_name(&id).to_string())
                .icon(crate::settings::icon(&icon).text_color(color))
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
                    let settings_target = menu_target.clone();
                    menu.item(
                        PopupMenuItem::new("Cluster settings…").on_click(move |_, _, cx| {
                            crate::preferences::cluster(settings_target.clone(), cx)
                        }),
                    )
                    .separator()
                    .item(
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

    fn render_workspace(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let root = self.layout.root.clone();
        let content = self.render_node(&root, cx);
        if self.sidebar_collapsed {
            return content;
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
                    .size_range(px(280.)..px(10000.))
                    .child(content),
            )
            .into_any_element()
    }

    fn render_node(&mut self, node: &Node, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Node::Pane(pane) => self.render_pane(pane, cx),
            Node::Split {
                id,
                horizontal,
                first,
                second,
            } => {
                let state = self
                    .pane_splits
                    .entry(*id)
                    .or_insert_with(|| cx.new(|_| ResizableState::default()))
                    .clone();
                let first = self.render_node(first, cx);
                let second = self.render_node(second, cx);
                let min = if *horizontal { px(220.) } else { px(140.) };
                let group = if *horizontal {
                    h_resizable(("workspace-split", *id as usize))
                } else {
                    v_resizable(("workspace-split", *id as usize))
                };
                group
                    .with_state(&state)
                    .child(resizable_panel().size_range(min..px(10000.)).child(first))
                    .child(resizable_panel().size_range(min..px(10000.)).child(second))
                    .into_any_element()
            }
        }
    }

    fn render_pane(&self, pane: &Pane, cx: &mut Context<Self>) -> AnyElement {
        let pane_id = pane.id;
        let workspace_id = cx.entity_id();
        let drop = cx
            .global::<WorkspaceWindows>()
            .drop_target
            .filter(|target| target.window == workspace_id && target.pane == pane_id);
        v_flex()
            .id(("workspace-pane", pane_id as usize))
            .relative()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .capture_any_mouse_down(
                cx.listener(move |app, _, window, cx| app.focus_pane(pane_id, window, cx)),
            )
            .on_prepaint(move |bounds, window, cx| {
                if let Some(entry) = cx.global::<WorkspaceWindows>().windows.get(&workspace_id) {
                    entry.panes.borrow_mut().insert(
                        pane_id,
                        Bounds::new(window.bounds().origin + bounds.origin, bounds.size),
                    );
                }
            })
            .child(self.render_tabs(pane, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(self.render_body(pane.active, cx)),
            )
            .when_some(drop, |pane, target| {
                pane.child(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(cx.theme().primary.opacity(0.16))
                        .border_2()
                        .border_color(cx.theme().primary)
                        .when(target.split == Some(Direction::Left), |overlay| {
                            overlay.right(relative(0.5))
                        })
                        .when(target.split == Some(Direction::Right), |overlay| {
                            overlay.left(relative(0.5))
                        })
                        .when(target.split == Some(Direction::Up), |overlay| {
                            overlay.bottom(relative(0.5))
                        })
                        .when(target.split == Some(Direction::Down), |overlay| {
                            overlay.top(relative(0.5))
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .bg(cx.theme().background)
                                .text_sm()
                                .child(match target.split {
                                    Some(Direction::Left) => "Move to left split",
                                    Some(Direction::Right) => "Move to right split",
                                    Some(Direction::Up) => "Move to upper split",
                                    Some(Direction::Down) => "Move to lower split",
                                    None => "Merge into tab group",
                                }),
                        ),
                )
            })
            .into_any_element()
    }

    fn tab_title(&self, tab: &Tab, cx: &App) -> SharedString {
        match &tab.state {
            TabState::Connected(view) => view.read(cx).title(),
            TabState::Disconnected => {
                if tab.initial_mode == Mode::Objects {
                    tab.initial_kind
                        .as_ref()
                        .map(|kind| SharedString::from(kind.resource.kind.clone()))
                        .unwrap_or_else(|| "Cluster".into())
                } else {
                    tab.initial_mode.label().into()
                }
            }
            TabState::Connecting => "connecting".into(),
            TabState::Failed(_) => "unreachable".into(),
        }
    }

    fn render_tabs(&self, pane: &Pane, cx: &mut Context<Self>) -> impl IntoElement {
        let pane_id = pane.id;
        let workspace_id = cx.entity_id();
        let tabs = pane
            .tabs
            .iter()
            .filter_map(|id| self.index_of(*id).map(|index| &self.tabs[index]))
            .map(|tab| {
                let id = tab.id;
                let color = self.cluster_color(&tab.cluster, cx);
                let what = self.tab_title(tab, cx);
                let accessible_name =
                    format!("{what} · {}", self.cluster_display_name(&tab.cluster));
                let drag = DraggedTab {
                    source: cx.weak_entity(),
                    id,
                    title: accessible_name.clone().into(),
                };
                let menu_view = cx.weak_entity();
                let detached = !self.main_window;
                let item = TabItem::new(("resource-tab", id as usize))
                    .selected(pane.active == Some(id))
                    .accessibility_label(accessible_name)
                    .set_position(
                        pane.tabs.iter().position(|tab| *tab == id).unwrap_or(0) + 1,
                        pane.tabs.len(),
                    )
                    .h(px(28.))
                    .max_w(TAB_WIDTH)
                    .min_w(px(100.))
                    .flex_shrink_0()
                    .px_2()
                    .gap_2()
                    .text_sm()
                    .cursor_pointer()
                    .border_r_1()
                    .border_color(cx.theme().border)
                    .bg(if pane.active == Some(id) {
                        cx.theme().tab_active
                    } else {
                        cx.theme().tab_bar
                    })
                    .hover(|tab| tab.bg(cx.theme().tab_active))
                    .child(div().size(px(7.)).flex_shrink_0().rounded_full().bg(color))
                    .child(div().min_w_0().truncate().child(what))
                    .on_prepaint(move |bounds, window, cx| {
                        if let Some(entry) =
                            cx.global::<WorkspaceWindows>().windows.get(&workspace_id)
                        {
                            entry.tabs.borrow_mut().insert(
                                id,
                                Bounds::new(window.bounds().origin + bounds.origin, bounds.size),
                            );
                        }
                    })
                    .on_drag(drag, |drag, _, _, cx| {
                        cx.global_mut::<WorkspaceWindows>().drag = Some(drag.clone());
                        cx.new(|_| DragLabel(drag.title.clone()))
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        if let Some(index) = view.index_of(id) {
                            view.activate(index, window, cx);
                        }
                        view.focus.focus(window, cx);
                    }))
                    .child(
                        Button::new(SharedString::from(format!("close-tab-{id}")))
                            .xsmall()
                            .ghost()
                            .label("×")
                            .tooltip("Close tab")
                            .on_click(cx.listener(move |view, _, window, cx| {
                                if let Some(index) = view.index_of(id) {
                                    view.close(index, window, cx);
                                }
                            })),
                    );
                div()
                    .id(("tab-menu", id as usize))
                    .flex_shrink_0()
                    .child(item)
                    .context_menu(move |menu, _, _| {
                        let right = menu_view.clone();
                        let down = menu_view.clone();
                        let detach = menu_view.clone();
                        let merge = menu_view.clone();
                        let close = menu_view.clone();
                        menu.item(PopupMenuItem::new("Split right — side by side").on_click(
                            move |_, window, cx| {
                                let _ = right.update(cx, |app, cx| {
                                    app.split_tab(id, Direction::Right, window, cx)
                                });
                            },
                        ))
                        .item(PopupMenuItem::new("Split down — stacked").on_click(
                            move |_, window, cx| {
                                let _ = down.update(cx, |app, cx| {
                                    app.split_tab(id, Direction::Down, window, cx)
                                });
                            },
                        ))
                        .separator()
                        .item(PopupMenuItem::new("Move to new window").on_click(
                            move |_, window, cx| {
                                let _ = detach
                                    .update(cx, |app, cx| app.detach_tab(id, None, window, cx));
                            },
                        ))
                        .when(detached, |menu| {
                            menu.item(PopupMenuItem::new("Move to main window").on_click(
                                move |_, window, cx| {
                                    let _ = merge
                                        .update(cx, |app, cx| app.merge_to_main(id, window, cx));
                                },
                            ))
                        })
                        .separator()
                        .item(
                            PopupMenuItem::new("Close tab").on_click(move |_, window, cx| {
                                let _ = close.update(cx, |app, cx| {
                                    if let Some(index) = app.index_of(id) {
                                        app.close(index, window, cx);
                                    }
                                });
                            }),
                        )
                    })
            });
        let new_tab = Button::new(("new-tab", pane_id as usize))
            .xsmall()
            .ghost()
            .label("+")
            .tooltip("Open another tab in this pane")
            .on_click(cx.listener(move |view, _, window, cx| {
                view.focus_pane(pane_id, window, cx);
                view.new_tab(window, cx);
            }));
        let choices: Vec<_> = pane
            .tabs
            .iter()
            .filter_map(|id| {
                self.index_of(*id)
                    .map(|index| (*id, self.tab_title(&self.tabs[index], cx)))
            })
            .collect();
        let menu_view = cx.weak_entity();
        h_flex()
            .h(px(28.))
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .bg(cx.theme().tab_bar)
            .child(
                Tabs::new(("cluster-tabs", pane_id as usize))
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scrolls[&pane_id])
                    .children(tabs),
            )
            .child(new_tab)
            .child(
                Button::new(("tab-list", pane_id as usize))
                    .xsmall()
                    .ghost()
                    .label("⌄")
                    .tooltip("Tabs in this pane")
                    .dropdown_menu(move |menu, _, _| {
                        choices.iter().fold(menu, |menu, (id, title)| {
                            let id = *id;
                            let app = menu_view.clone();
                            menu.item(PopupMenuItem::new(title.clone()).on_click(
                                move |_, window, cx| {
                                    let _ = app.update(cx, |app, cx| {
                                        if let Some(index) = app.index_of(id) {
                                            app.activate(index, window, cx);
                                            app.focus.focus(window, cx);
                                        }
                                    });
                                },
                            ))
                        })
                    }),
            )
    }

    fn render_body(&self, tab: Option<u64>, cx: &mut Context<Self>) -> AnyElement {
        let index = tab.and_then(|id| self.index_of(id));
        if let Some(view) = index.and_then(|index| self.view(index)) {
            return view.into_any_element();
        }
        let state = index
            .and_then(|index| self.tabs.get(index))
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
                if self.connections.read(cx).sessions.is_empty() { "No cluster selected" } else { "No tab open" }.to_string(),
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
                    .child(copyable_text("connection-headline", headline)),
            )
            .child(
                div()
                    .max_w(px(640.))
                    .text_sm()
                    .text_center()
                    .text_color(cx.theme().muted_foreground)
                    .child(copyable_text("connection-detail", detail)),
            )
            .when_some(
                state.and_then(|(id, state)| {
                    matches!(state, TabState::Connecting).then(|| id.clone())
                }),
                |body, cluster| {
                    body.child(
                        Button::new("cancel-cluster-connection")
                            .outline()
                            .small()
                            .label("Cancel connection")
                            .on_click(cx.listener(move |view, _, window, cx| {
                                // A queued click must not disconnect a session
                                // that finished connecting in the meantime.
                                if view.connections.read(cx).connecting(&cluster) {
                                    view.disconnect(&cluster, window, cx);
                                }
                            })),
                    )
                },
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
        let mut sessions: Vec<_> = self.connections.read(cx).sessions.values().collect();
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
                            .child(copyable_text("connection-reason", reason.to_string())),
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
            if let Some(settings) = self.preferences.clusters.get(tab.cluster.as_str())
                && !settings.alias.is_empty()
            {
                return settings.alias.as_str();
            }
            self.context_entry(&tab.cluster)
                .map_or_else(|| tab.cluster.display_name(), ContextEntry::cluster_name)
        });
        let mut description = active
            .map(|tab| self.cluster_description(&tab.cluster))
            .unwrap_or_default();
        let (tone, status, watches) = match active.map(|tab| &tab.state) {
            None => (
                Tone::Unknown,
                if self.connections.read(cx).sessions.is_empty() {
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

        let copy_description = description.clone();
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
                            })
                            .context_menu(move |menu, _, _| {
                                menu.item(copy_item(
                                    "Copy connection details",
                                    copy_description.clone(),
                                ))
                            }),
                    )
                    .child(div().flex_shrink_0().child(self.render_activity_menu(
                        ActivityMenu::Watches,
                        watches,
                        cx,
                    )))
                    .child(div().flex_shrink_0().child(self.render_activity_menu(
                        ActivityMenu::Clusters,
                        self.connections.read(cx).sessions.len(),
                        cx,
                    ))),
            )
    }
}

impl Render for BeaconApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = cx.entity_id();
        if let Some(entry) = cx.global::<WorkspaceWindows>().windows.get(&id) {
            entry.bounds.set(window.bounds());
            entry.panes.borrow_mut().clear();
            entry.tabs.borrow_mut().clear();
        }
        div()
            .id("beacon-workspace")
            .key_context("BeaconWorkspace")
            .relative()
            .size_full()
            .track_focus(&self.focus)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, _cx| {
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                            Self::finish_drag(event, phase, id, window, cx)
                        });
                        window.on_key_event(|event: &KeyDownEvent, phase, window, cx| {
                            if phase == DispatchPhase::Capture
                                && event.keystroke.key == "escape"
                                && cx.global::<WorkspaceWindows>().drag.is_some()
                            {
                                cx.global_mut::<WorkspaceWindows>().drag = None;
                                cx.global_mut::<WorkspaceWindows>().drop_target = None;
                                cx.stop_active_drag(window);
                                cx.refresh_windows();
                                cx.stop_propagation();
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .on_drag_move(cx.listener(Self::drag_move))
            .on_action(
                cx.listener(|view, _: &TogglePalette, window, cx| view.toggle_palette(window, cx)),
            )
            .on_action(cx.listener(|view, _: &NewTab, window, cx| view.new_tab(window, cx)))
            .on_action(
                cx.listener(|view, _: &CloseTab, window, cx| view.close_tab_or_quit(window, cx)),
            )
            .on_action(cx.listener(|view, _: &NextTab, window, cx| view.step(true, window, cx)))
            .on_action(
                cx.listener(|view, _: &PreviousTab, window, cx| view.step(false, window, cx)),
            )
            .on_action(cx.listener(|app, _: &SplitRight, window, cx| {
                if let Some(id) = app.tabs.get(app.active).map(|tab| tab.id) {
                    app.split_tab(id, Direction::Right, window, cx);
                }
            }))
            .on_action(cx.listener(|app, _: &SplitDown, window, cx| {
                if let Some(id) = app.tabs.get(app.active).map(|tab| tab.id) {
                    app.split_tab(id, Direction::Down, window, cx);
                }
            }))
            .on_action(cx.listener(|app, _: &DetachTab, window, cx| {
                if let Some(id) = app.tabs.get(app.active).map(|tab| tab.id) {
                    app.detach_tab(id, None, window, cx);
                }
            }))
            .on_action(cx.listener(|app, _: &MergeToMain, window, cx| {
                if let Some(id) = app.tabs.get(app.active).map(|tab| tab.id) {
                    app.merge_to_main(id, window, cx);
                }
            }))
            // Handled here rather than in ClusterView, which is where it
            // belongs and where it does not work: an action travels from the
            // focused node *upwards*, and ClusterView is a child of the node
            // that holds focus, not an ancestor of it.
            .on_action(cx.listener(|view, _: &CloseDetail, window, cx| {
                if window.has_active_sheet(cx) {
                    window.close_sheet(cx);
                    return;
                }
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
