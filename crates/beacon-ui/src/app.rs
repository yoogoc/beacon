//! The root view: the chrome around a cluster, and the connection to it.
//!
//! Everything cluster-shaped lives in [`ClusterView`]. What is here is the part
//! that exists before and between connections -- which contexts there are,
//! which one is being connected to, and what went wrong if it failed.

use std::{collections::HashMap, sync::Arc};

use beacon_kube::{ClusterId, ClusterSession, config::Contexts};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::bridge::Bridge;
use crate::cluster::ClusterView;
use crate::palette::{self, Choice, Palette, PaletteEvent};
use crate::theme::{BeaconTheme as _, Tone, toggle_mode};

gpui_kit::actions!(beacon, [TogglePalette]);

/// Installs the one key everything else is reachable from.
///
/// Bound without a context, so it works wherever focus happens to be -- the
/// palette is no use if it only opens when nothing is selected.
pub fn init(cx: &mut App) {
    let stroke = if cfg!(target_os = "macos") {
        "cmd-k"
    } else {
        "ctrl-k"
    };
    cx.bind_keys([KeyBinding::new(stroke, TogglePalette, None)]);
}

pub struct BeaconApp {
    contexts: Result<Contexts, String>,
    context_picker: Option<Entity<SelectState<SearchableVec<SharedString>>>>,
    connection: Connection,
    /// Every cluster connected in this session, kept alive across switches.
    ///
    /// A session is a client, a discovery cache, a permission cache and any
    /// port forwards -- all cheap to hold and slow to rebuild. The *watches*
    /// are not kept: they belong to the view, and the registry's linger means
    /// switching back within half a minute reuses them anyway.
    sessions: HashMap<ClusterId, Arc<ClusterSession>>,
    palette: Entity<Palette>,
    palette_open: bool,
    _connect: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

enum Connection {
    /// There is nothing to connect to, or nothing has been chosen yet.
    Idle,
    Connecting(ClusterId),
    Connected(Entity<ClusterView>),
    Failed {
        id: ClusterId,
        diagnosis: String,
    },
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
            connection: Connection::Idle,
            sessions: HashMap::new(),
            palette,
            palette_open: false,
            _connect: None,
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
                    view.connect(ClusterId::new(name.to_string()), window, cx);
                },
            )
            .detach();
        }

        // Start on whatever `kubectl` would have used. Opening to an empty
        // window and making the user pick the context they already picked is
        // the kind of small friction that adds up.
        if let Ok(contexts) = &this.contexts
            && let Some(current) = contexts.current()
        {
            let id = current.id.clone();
            this.connect(id, window, cx);
        }

        this
    }

    /// Replaces whatever is connected with a connection to `id`.
    ///
    /// The old [`ClusterView`] is dropped here, and with it the session and
    /// every watch the session was running.
    fn connect(&mut self, id: ClusterId, window: &mut Window, cx: &mut Context<Self>) {
        let namespace = self
            .contexts
            .as_ref()
            .ok()
            .and_then(|contexts| contexts.get(&id))
            .and_then(|entry| entry.namespace.clone());

        // Already connected: switching back is rebuilding a view over a
        // session that still has its client, its discovery and its forwards.
        if let Some(session) = self.sessions.get(&id).cloned() {
            tracing::info!(context = %id, "switching back");
            self.connection = Connection::Connected(
                cx.new(|cx| ClusterView::new(session, namespace, window, cx)),
            );
            cx.notify();
            return;
        }

        tracing::info!(context = %id, namespace = ?namespace, "connecting");
        self.connection = Connection::Connecting(id.clone());
        cx.notify();

        let connecting = {
            let id = id.clone();
            Bridge::global(cx).run(async move { ClusterSession::connect(id).await })
        };

        self._connect = Some(cx.spawn_in(window, async move |this, cx| {
            let result = connecting.await;

            let _ = this.update_in(cx, |view, window, cx| {
                // A connection that finished after the user moved on is not
                // this view's business any more.
                if !matches!(&view.connection, Connection::Connecting(pending) if pending == &id) {
                    return;
                }

                view.connection = match result {
                    Ok(Ok(session)) => {
                        let session = Arc::new(session);
                        view.sessions.insert(id.clone(), session.clone());
                        Connection::Connected(
                            cx.new(|cx| ClusterView::new(session, namespace, window, cx)),
                        )
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(context = %id, %error, "could not connect");
                        Connection::Failed {
                            id,
                            diagnosis: error.to_string(),
                        }
                    }
                    Err(error) => {
                        tracing::error!(context = %id, %error, "the connect task failed");
                        Connection::Failed {
                            id,
                            diagnosis: error.to_string(),
                        }
                    }
                };
                cx.notify();
            });
        }));
    }

    fn cluster(&self) -> Option<&Entity<ClusterView>> {
        match &self.connection {
            Connection::Connected(cluster) => Some(cluster),
            _ => None,
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

        // Clusters come from the kubeconfig, not from the connected session --
        // switching to one Beacon is not connected to is the point.
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
        match choice {
            Choice::Cluster(id) => {
                // A context switch is the one choice that does not need a
                // connected cluster, and the one that replaces it.
                self.connect(id, window, cx);
                return;
            }
            Choice::Action(palette::Action::ToggleTheme) => {
                toggle_mode(window, cx);
                cx.notify();
                return;
            }
            _ => {}
        }

        let Some(cluster) = self.cluster().cloned() else {
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
            Choice::Cluster(_) | Choice::Action(palette::Action::ToggleTheme) => {}
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

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Connection::Connected(cluster) = &self.connection {
            return cluster.clone().into_any_element();
        }

        let (tone, headline, detail) = match (&self.connection, &self.contexts) {
            (Connection::Connecting(id), _) => (
                Tone::Progressing,
                format!("Connecting to {id}"),
                "Authenticating and reaching the API server.".to_string(),
            ),
            (Connection::Failed { id, diagnosis }, _) => (
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
                "No cluster selected".to_string(),
                "Pick a context from the menu in the title bar.".to_string(),
            ),
        };

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_8()
            .child(
                div()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(cx.theme().tone_surface(tone))
                    .text_color(cx.theme().tone(tone))
                    .text_sm()
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
        let (tone, status) = match &self.connection {
            Connection::Idle => (Tone::Unknown, "not connected".to_string()),
            Connection::Connecting(id) => (Tone::Progressing, format!("connecting to {id}")),
            Connection::Failed { .. } => (Tone::Critical, "disconnected".to_string()),
            Connection::Connected(cluster) => {
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
            .child(
                v_flex()
                    .size_full()
                    .child(self.render_title_bar(cx))
                    .child(div().flex_1().overflow_hidden().child(self.render_body(cx)))
                    .child(self.render_status_bar(cx)),
            )
            .when(self.palette_open, |this| {
                // Deferred so it paints over the table rather than under it.
                this.child(deferred(self.render_palette(cx)))
            })
    }
}
