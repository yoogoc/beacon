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

use std::{collections::HashMap, sync::Arc};

use beacon_kube::{ClusterId, ClusterSession, config::Contexts};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab as TabItem, TabBar};
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::bridge::Bridge;
use crate::cluster::ClusterView;
use crate::palette::{self, Choice, Palette, PaletteEvent};
use crate::theme::{BeaconTheme as _, Tone, toggle_mode};

gpui_kit::actions!(
    beacon,
    [TogglePalette, NewTab, CloseTab, NextTab, PreviousTab]
);

/// How wide a tab is allowed to get before its label ellipsizes.
///
/// Context names are usually short. The ones that are not -- an EKS ARN, say --
/// would otherwise push every other tab off the bar, and the title bar and the
/// status bar both show the full name anyway.
const TAB_WIDTH: Pixels = px(240.);

/// Installs the keys everything else is reachable from.
///
/// Bound without a context, so they work wherever focus happens to be -- the
/// palette is no use if it only opens when nothing is selected, and neither is
/// a tab shortcut that stops working once you click into the table.
pub fn init(cx: &mut App) {
    let modifier = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    cx.bind_keys([
        KeyBinding::new(&format!("{modifier}-k"), TogglePalette, None),
        KeyBinding::new(&format!("{modifier}-t"), NewTab, None),
        KeyBinding::new(&format!("{modifier}-w"), CloseTab, None),
        // Not `cmd-shift-[` and friends: ctrl-tab is the one pair that means
        // the same thing on all three platforms.
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
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
    state: TabState,
    /// Dropped with the tab, which abandons a connection nobody is waiting on.
    _connect: Option<Task<()>>,
}

enum TabState {
    Connecting,
    Connected(Entity<ClusterView>),
    Failed(String),
}

pub struct BeaconApp {
    contexts: Result<Contexts, String>,
    context_picker: Option<Entity<SelectState<SearchableVec<SharedString>>>>,

    /// Every cluster connected in this session, shared by every tab on it.
    ///
    /// A session is a client, a discovery cache, a permission cache and any
    /// port forwards -- all cheap to hold and slow to rebuild. They outlive
    /// the tabs that opened them, which is what makes reopening one instant.
    /// The *watches* belong to the views, so closing a tab stops what it was
    /// watching.
    sessions: HashMap<ClusterId, Arc<ClusterSession>>,

    tabs: Vec<Tab>,
    /// Index into `tabs`. Meaningless, and never read, while `tabs` is empty.
    active: usize,
    next_tab: u64,

    palette: Entity<Palette>,
    palette_open: bool,
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

        let context_picker = contexts.as_ref().ok().filter(|c| !c.is_empty()).map(|c| {
            let names: Vec<SharedString> = c
                .entries()
                .iter()
                .map(|entry| SharedString::from(entry.id.to_string()))
                .collect();
            let current = c
                .entries()
                .iter()
                .position(|entry| entry.is_current)
                .unwrap_or(0);

            cx.new(|cx| {
                SelectState::new(
                    SearchableVec::new(names),
                    Some(IndexPath::default().row(current)),
                    window,
                    cx,
                )
                .searchable(true)
            })
        });

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

        let mut this = Self {
            contexts,
            context_picker,
            sessions: HashMap::new(),
            tabs: Vec::new(),
            active: 0,
            next_tab: 0,
            palette,
            palette_open: false,
            _subscriptions: vec![palette_events],
        };

        if let Some(picker) = this.context_picker.clone() {
            cx.subscribe_in(
                &picker,
                window,
                |view, _, event: &SelectEvent<SearchableVec<SharedString>>, window, cx| {
                    let SelectEvent::Confirm(Some(name)) = event else {
                        return;
                    };
                    // The picker changes what *this* tab shows. Opening another
                    // cluster beside it is `ctx` in the palette, or ⌘T.
                    view.retarget(ClusterId::new(name.to_string()), window, cx);
                },
            )
            .detach();
        }

        // Start on whatever `kubectl` would have used. Opening to an empty
        // window and making the user pick the context they already picked is
        // the kind of small friction that adds up.
        if let Some(id) = this.default_cluster() {
            this.open(id, window, cx);
        }

        this
    }

    /// The context to open when nothing else says which.
    fn default_cluster(&self) -> Option<ClusterId> {
        let contexts = self.contexts.as_ref().ok()?;
        contexts
            .current()
            .or_else(|| contexts.entries().first())
            .map(|entry| entry.id.clone())
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
        let namespace = self.namespace_for(&cluster);
        let id = self.next_tab;
        self.next_tab += 1;

        self.tabs.push(Tab {
            id,
            cluster,
            namespace,
            state: TabState::Connecting,
            _connect: None,
        });

        let index = self.tabs.len() - 1;
        self.activate(index, window, cx);
        self.connect(index, window, cx);
    }

    /// Goes to `cluster`: its tab if one is open, a new tab if not.
    ///
    /// Two tabs on one cluster are a deliberate thing to ask for -- ⌘T -- not
    /// something to get by mistake from picking the same cluster twice.
    fn go_to(&mut self, cluster: ClusterId, window: &mut Window, cx: &mut Context<Self>) {
        match self.tabs.iter().position(|tab| tab.cluster == cluster) {
            Some(index) => self.activate(index, window, cx),
            None => self.open(cluster, window, cx),
        }
    }

    /// Points the tab on screen at a different cluster, in place.
    fn retarget(&mut self, cluster: ClusterId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = (!self.tabs.is_empty()).then_some(self.active) else {
            self.open(cluster, window, cx);
            return;
        };
        if self.tabs[index].cluster == cluster {
            return;
        }

        let namespace = self.namespace_for(&cluster);
        let tab = &mut self.tabs[index];
        tab.cluster = cluster;
        tab.namespace = namespace;
        tab.state = TabState::Connecting;
        tab._connect = None;

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

        // Already connected: opening another tab on this cluster is building a
        // view over a session that still has its client, its discovery and its
        // forwards.
        if let Some(session) = self.sessions.get(&cluster).cloned() {
            tracing::info!(context = %cluster, "reusing the session");
            self.show(index, session, namespace, window, cx);
            return;
        }

        tracing::info!(context = %cluster, namespace = ?namespace, "connecting");
        self.tabs[index].state = TabState::Connecting;
        cx.notify();

        let connecting = {
            let cluster = cluster.clone();
            Bridge::global(cx).run(async move { ClusterSession::connect(cluster).await })
        };

        let task = cx.spawn_in(window, async move |this, cx| {
            let result = connecting.await;

            let _ = this.update_in(cx, |view, window, cx| {
                // The tab may have been closed, or pointed somewhere else,
                // while this was in flight. Either way it is not ours to fill.
                let Some(index) = view.index_of(tab_id) else {
                    return;
                };
                if view.tabs[index].cluster != cluster {
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
                            .entry(cluster)
                            .or_insert_with(|| Arc::new(session))
                            .clone();
                        let namespace = view.tabs[index].namespace.clone();
                        view.show(index, session, namespace, window, cx);
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(context = %cluster, %error, "could not connect");
                        view.tabs[index].state = TabState::Failed(error.to_string());
                        cx.notify();
                    }
                    Err(error) => {
                        tracing::error!(context = %cluster, %error, "the connect task failed");
                        view.tabs[index].state = TabState::Failed(error.to_string());
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
        let view = cx.new(|cx| ClusterView::new(session, namespace, window, cx));

        // A tab that connected in the background must not start its timers: a
        // view is visible until told otherwise, and nothing else would tell it.
        if self.active != index {
            view.update(cx, |view, cx| view.set_visible(false, window, cx));
        }

        self.tabs[index].state = TabState::Connected(view);
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

        self.sync_picker(window, cx);
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

        self.sync_picker(window, cx);
        cx.notify();
    }

    /// Another tab on the cluster in front, so it can be narrowed to something
    /// else.
    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cluster = self
            .tabs
            .get(self.active)
            .map(|tab| tab.cluster.clone())
            .or_else(|| self.default_cluster());

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

    /// Keeps the title bar's picker showing the tab that is in front.
    ///
    /// Setting the value does not emit `Confirm`, so this cannot loop back
    /// into [`Self::retarget`].
    fn sync_picker(&self, window: &mut Window, cx: &mut App) {
        let (Some(picker), Some(tab)) = (self.context_picker.clone(), self.tabs.get(self.active))
        else {
            return;
        };
        let value = SharedString::from(tab.cluster.to_string());
        picker.update(cx, |state, cx| {
            state.set_selected_value(&value, window, cx);
        });
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
            .map(|contexts| {
                contexts
                    .entries()
                    .iter()
                    .map(|entry| entry.id.clone())
                    .collect()
            })
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
            Choice::Kind(kind) => cluster.show_kind(kind, window, cx),
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
                palette::Action::ToggleTheme | palette::Action::NewTab | palette::Action::CloseTab,
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
                        .items_center()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child("Beacon"))
                        .children(self.context_picker.as_ref().map(|picker| {
                            Select::new(picker)
                                .small()
                                .menu_width(px(420.))
                                .menu_max_h(px(420.))
                                .search_placeholder("Filter contexts")
                                .accessibility_label("Cluster")
                        })),
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

    /// The tab bar, always present so that `+` is always somewhere to click.
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.tabs.iter().enumerate().map(|(index, tab)| {
            let id = tab.id;
            let what = match &tab.state {
                TabState::Connected(view) => view.read(cx).title(),
                TabState::Connecting => SharedString::from("connecting"),
                TabState::Failed(_) => SharedString::from("unreachable"),
            };

            TabItem::new()
                // What, then where. The prefix holds its size and the label is
                // the part that ellipsizes, and this is the way round that
                // survives a long context name: two tabs both reading
                // `arn:aws:eks:us-east-1:…` say nothing, whereas `Pod` and
                // `Service` beside a truncated cluster still say which is
                // which. The full name is in the title bar and the status bar.
                .prefix(div().pl_2().child(format!("{what} ·")))
                .label(tab.cluster.to_string())
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

        TabBar::new("cluster-tabs")
            .small()
            .max_width(TAB_WIDTH)
            .selected_index(self.active)
            .children(tabs)
            .suffix(
                Button::new("new-tab")
                    .xsmall()
                    .ghost()
                    .label("+")
                    .tooltip("Open another tab on this cluster")
                    .on_click(cx.listener(|view, _, window, cx| view.new_tab(window, cx))),
            )
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
            (Some((id, TabState::Connecting)), _) => (
                Tone::Progressing,
                format!("Connecting to {id}"),
                "Authenticating and reaching the API server.".to_string(),
            ),
            (Some((id, TabState::Failed(diagnosis))), _) => (
                Tone::Critical,
                format!("Could not connect to {id}"),
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
                "No tab open".to_string(),
                "Press ⌘T for a tab, or ⌘K and `ctx` to go to a cluster.".to_string(),
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

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (tone, status) = match self.tabs.get(self.active).map(|tab| &tab.state) {
            None => (Tone::Unknown, "no tab open".to_string()),
            Some(TabState::Connecting) => (Tone::Progressing, "connecting".to_string()),
            Some(TabState::Failed(_)) => (Tone::Critical, "disconnected".to_string()),
            Some(TabState::Connected(cluster)) => {
                let cluster = cluster.read(cx);
                let health = cluster.health();
                let tone = match health {
                    beacon_kube::Health::Connected => Tone::Healthy,
                    beacon_kube::Health::Connecting => Tone::Progressing,
                    beacon_kube::Health::Degraded { .. } => Tone::Warning,
                };
                // The reason, not just the word: "Degraded" alone sends people
                // to the log file for something we already know.
                let detail = health
                    .reason()
                    .map(|reason| format!("{} — {reason}", health.label()))
                    .unwrap_or_else(|| health.label().to_string());

                let forwards = cluster.session().forwards().len();
                let mut status = format!(
                    "{} · {} · {} watches",
                    cluster.session().server(),
                    detail,
                    cluster.session().active_watches()
                );
                if self.tabs.len() > 1 {
                    status.push_str(&format!(" · {} tabs", self.tabs.len()));
                }
                if self.sessions.len() > 1 {
                    status.push_str(&format!(" · {} clusters", self.sessions.len()));
                }
                if forwards > 0 {
                    status.push_str(&format!(" · {forwards} forwarding"));
                }

                (tone, status)
            }
        };

        h_flex()
            .w_full()
            .h(px(24.))
            .px_3()
            .gap_2()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().table_header())
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(format!("Beacon {}", env!("CARGO_PKG_VERSION")))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(div().size(px(7.)).rounded_full().bg(cx.theme().tone(tone)))
                    .child(status),
            )
    }
}

impl Render for BeaconApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
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
            .child(
                v_flex()
                    .size_full()
                    .child(self.render_title_bar(cx))
                    .child(self.render_tabs(cx))
                    .child(div().flex_1().overflow_hidden().child(self.render_body(cx)))
                    .child(self.render_status_bar(cx)),
            )
            .when(self.palette_open, |this| {
                // Deferred so it paints over the table rather than under it.
                this.child(deferred(self.render_palette(cx)))
            })
    }
}
