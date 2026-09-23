//! One connected cluster, on screen.
//!
//! Owns the session, so dropping this view disconnects: every watch it started
//! is aborted with it. That is what makes switching contexts a matter of
//! replacing one entity with another rather than unwinding state by hand.

use std::{sync::Arc, time::Duration};

use beacon_columns::ColumnSet;
use beacon_kube::{ClusterSession, Health, ResourceStore, WatchKey, resources};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::table::TableState;
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _, h_flex, v_flex};
use gpui_kit::*;

use crate::bridge::drain_into;
use crate::table::ResourceTable;

/// The namespace picker's entry for "do not scope at all". A namespace cannot
/// contain a space, so this can never collide with a real one.
const ALL_NAMESPACES: &str = "All namespaces";

/// How often the Age column is repainted. Ages are relative, so a table that
/// nothing is changing still has to advance.
const CLOCK: Duration = Duration::from_secs(1);

pub struct ClusterView {
    session: Arc<ClusterSession>,
    health: Health,
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
    pods: Entity<TableState<ResourceTable>>,

    // Dropping any of these stops the work behind it.
    _health: Task<()>,
    _namespaces: Task<()>,
    _pods: Task<()>,
    _clock: Task<()>,
}

impl ClusterView {
    pub fn new(
        session: Arc<ClusterSession>,
        namespace: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let namespace_picker = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(vec![SharedString::from(ALL_NAMESPACES)]),
                Some(IndexPath::default()),
                window,
                cx,
            )
            .searchable(true)
        });

        let pods = cx.new(|cx| {
            TableState::new(ResourceTable::new(columns_for(&namespace)), window, cx)
                .row_selectable(true)
        });

        let mut this = Self {
            health: Health::Connecting,
            namespace,
            namespaces: ResourceStore::new(),
            namespace_names: vec![SharedString::from(ALL_NAMESPACES)],
            namespace_picker,
            pods,
            _health: Task::ready(()),
            _namespaces: Task::ready(()),
            _pods: Task::ready(()),
            _clock: Task::ready(()),
            session,
        };

        this.watch_health(window, cx);
        this.watch_namespaces(window, cx);
        this.watch_pods(window, cx);
        this.start_clock(cx);
        this.listen_to_picker(window, cx);
        this
    }

    pub fn session(&self) -> &ClusterSession {
        &self.session
    }

    pub fn health(&self) -> &Health {
        &self.health
    }

    /// How many rows the table is showing, for the status bar.
    pub fn row_count(&self, cx: &App) -> usize {
        self.pods.read(cx).delegate().len()
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

    /// (Re)starts the pod watch for the current namespace.
    ///
    /// Dropping the previous task drops its subscription, which is what
    /// releases the old watch -- there is no separate unsubscribe to forget.
    fn watch_pods(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let columns = columns_for(&self.namespace);
        self.pods.update(cx, |state, cx| {
            state.delegate_mut().reset(columns);
            // Without this, scrolling halfway down one namespace and switching
            // to a smaller one lands on a blank stretch of table.
            state.scroll_to_row(0, cx);
            cx.notify();
        });

        let key = WatchKey::all(resources::pod()).in_namespace(self.namespace.clone());
        let subscription = self.session.subscribe(key);

        self._pods = drain_into(
            cx,
            subscription,
            |view, batch, _window, cx| {
                view.pods.update(cx, |state, cx| {
                    state.delegate_mut().apply(batch);
                    cx.notify();
                });
            },
            window,
        );
    }

    fn start_clock(&mut self, cx: &mut Context<Self>) {
        self._clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CLOCK).await;
                let updated = this.update(cx, |view, cx| {
                    view.pods.update(cx, |state, cx| {
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

    fn listen_to_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.subscribe_in(
            &self.namespace_picker.clone(),
            window,
            |view, _, event: &SelectEvent<SearchableVec<SharedString>>, window, cx| {
                let SelectEvent::Confirm(selected) = event;
                let namespace = match selected.as_deref() {
                    None | Some(ALL_NAMESPACES) => None,
                    Some(namespace) => Some(namespace.to_string()),
                };

                if view.namespace != namespace {
                    tracing::info!(namespace = ?namespace, "scoping to namespace");
                    view.namespace = namespace;
                    view.watch_pods(window, cx);
                    cx.notify();
                }
            },
        )
        .detach();
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

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.row_count(cx);

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
                            .child("Pods"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{rows}")),
                    ),
            )
            .child(
                Select::new(&self.namespace_picker)
                    .small()
                    .menu_width(px(280.))
                    .menu_max_h(px(420.))
                    .search_placeholder("Filter namespaces")
                    .accessibility_label("Namespace"),
            )
    }
}

impl Render for ClusterView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().size_full().child(self.render_toolbar(cx)).child(
            // `TableState` renders itself; it is the virtualised table,
            // not a delegate that something else draws.
            div().flex_1().overflow_hidden().child(self.pods.clone()),
        )
    }
}

/// Pods carry a Namespace column only when the list spans namespaces, the same
/// way `kubectl get pods` does and `kubectl get pods -A` does not.
fn columns_for(namespace: &Option<String>) -> ColumnSet {
    ColumnSet::for_kind("", "Pod", namespace.is_none())
}
