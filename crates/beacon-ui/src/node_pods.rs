//! The Pods scheduled to one Node, using the shared Pod table and watch registry.

use std::sync::Arc;

use beacon_columns::{ColumnSet, ColumnWidth};
use beacon_kube::{ClusterSession, Delta, Kind, WatchKey};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::table::{TableEvent, TableState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;

use crate::bridge::{Bridge, drain_into};
use crate::detail::OwnerRequested;
use crate::table::ResourceTable;
use crate::theme::{BeaconTheme as _, Tone};

pub(crate) struct NodePodsView {
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    node: String,
    table: Entity<TableState<ResourceTable>>,
    search: Entity<InputState>,
    error: Option<String>,
    _list_task: Option<Task<()>>,
    _watch_task: Option<Task<()>>,
    _clock: Task<()>,
    _search_subscription: Subscription,
    _table_subscription: Subscription,
}

impl EventEmitter<OwnerRequested> for NodePodsView {}

fn columns() -> ColumnSet {
    let mut columns = ColumnSet::for_kind("", "Pod", true);
    columns
        .columns
        .retain(|column| !matches!(column.header.as_str(), "CPU" | "Memory"));
    for column in &mut columns.columns {
        column.width = ColumnWidth::Fixed(match column.header.as_str() {
            "Name" => 220.,
            "Namespace" => 120.,
            "Containers" => 96.,
            "Ready" => 64.,
            "Status" => 140.,
            "Restarts" => 80.,
            _ => 80.,
        });
    }
    columns
}

impl NodePodsView {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        kind: Arc<Kind>,
        node: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let table = cx.new(|cx| {
            TableState::new(
                ResourceTable::new(columns()).without_selection(),
                window,
                cx,
            )
        });
        let search = cx
            .new(|cx| InputState::new(window, cx).placeholder("Search Pods by name or namespace"));
        let search_subscription = cx.subscribe(&search, |view, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let query = state.read(cx).value().to_string();
                view.table.update(cx, |table, cx| {
                    if table.delegate_mut().set_filter(&query) {
                        table.refresh(cx);
                        cx.notify();
                    }
                });
                cx.notify();
            }
        });
        let table_subscription = cx.subscribe(&table, |view, table, event: &TableEvent, cx| {
            if let TableEvent::SelectRow(row) = event
                && let Some(target) = table.read(cx).delegate().key_at(*row).cloned()
            {
                cx.emit(OwnerRequested {
                    kind: view.kind.clone(),
                    target,
                });
            }
        });
        let mut this = Self {
            session,
            kind,
            node,
            table,
            search,
            error: None,
            _list_task: None,
            _watch_task: None,
            _clock: Task::ready(()),
            _search_subscription: search_subscription,
            _table_subscription: table_subscription,
        };
        this.load(window, cx);
        this._clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                if this
                    .update(cx, |view, cx| {
                        view.table.update(cx, |table, cx| {
                            table.delegate_mut().tick();
                            cx.notify();
                        });
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        this
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._watch_task = None;
        self.error = None;
        self.table.update(cx, |table, cx| {
            table.delegate_mut().reset(columns());
            table.refresh(cx);
            cx.notify();
        });
        let key = WatchKey::pods_on_node(&self.node);
        let fetching = {
            let session = self.session.clone();
            let key = key.clone();
            Bridge::global(cx).run_cancellable(async move {
                session
                    .list_objects(key)
                    .await
                    .map_err(|error| error.user_message())
            })
        };
        self._list_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = fetching.result().await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.table.update(cx, |table, cx| {
                    table.delegate_mut().finish_loading();
                    if let Ok(Ok(objects)) = &result {
                        table
                            .delegate_mut()
                            .apply(vec![Delta::Reset(objects.clone())]);
                    }
                    table.refresh(cx);
                    cx.notify();
                });
                match result {
                    Ok(Ok(_)) => {
                        view._watch_task = Some(drain_into(
                            cx,
                            view.session.subscribe(key),
                            |view, batch, _, cx| {
                                view.table.update(cx, |table, cx| {
                                    table.delegate_mut().apply(batch);
                                    table.refresh(cx);
                                    cx.notify();
                                });
                                cx.notify();
                            },
                            window,
                        ));
                    }
                    Ok(Err(error)) => view.error = Some(error),
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Render for NodePodsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let refresh = Button::new("refresh-node-pods")
            .xsmall()
            .ghost()
            .label("Refresh")
            .disabled(self.table.read(cx).delegate().is_loading())
            .on_click(cx.listener(|view, _, window, cx| view.load(window, cx)));
        render_pods_content(
            &self.table,
            &self.search,
            self.error.as_deref(),
            refresh,
            cx,
        )
    }
}

fn render_pods_content(
    table: &Entity<TableState<ResourceTable>>,
    search: &Entity<InputState>,
    error: Option<&str>,
    refresh: Button,
    cx: &App,
) -> impl IntoElement {
    let state = table.read(cx).delegate();
    let loading = state.is_loading();
    let count = if loading {
        "Loading Pods…".into()
    } else if error.is_some() {
        "Pods".into()
    } else if state.len() == state.total() {
        format!("{} Pods", state.total())
    } else {
        format!("{} / {} Pods", state.len(), state.total())
    };
    let body = if let Some(error) = error {
        div()
            .id("node-pods-error-state")
            .size_full()
            .p_4()
            .text_sm()
            .text_color(cx.theme().tone(Tone::Critical))
            .child(crate::copyable_text::copyable_text(
                "node-pods-error",
                error.to_owned(),
            ))
            .test_support()
            .into_any_element()
    } else if loading {
        // Keep the loading state lightweight until the snapshot is ready.
        div()
            .id("node-pods-loading")
            .size_full()
            .p_4()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("Loading Pods…")
            .test_support()
            .into_any_element()
    } else if state.is_empty() {
        div()
            .id("node-pods-empty")
            .size_full()
            .p_4()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(if state.total() == 0 {
                "No Pods are scheduled to this node."
            } else {
                "No Pods match the search."
            })
            .test_support()
            .into_any_element()
    } else {
        div()
            .id("node-pods-table")
            .size_full()
            .min_size_0()
            .child(table.clone())
            .test_support()
            .into_any_element()
    };
    v_flex()
        .size_full()
        .min_size_0()
        .child(
            h_flex()
                .w_full()
                .flex_shrink_0()
                .gap_2()
                .p_2()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(div().flex_1().min_w_0().child(Input::new(search).small()))
                .child(refresh)
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(count),
                ),
        )
        .child(div().flex_1().min_size_0().overflow_hidden().child(body))
}

#[cfg(all(test, feature = "ui-tests"))]
mod rendering_tests {
    use super::*;
    use gpui_kit::test::TestWindowExt as _;

    // Exercise the nested, independently refreshed Pod table without a live cluster.
    struct Pane {
        table: Entity<TableState<ResourceTable>>,
        search: Entity<InputState>,
    }

    impl Render for Pane {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            render_pods_content(
                &self.table,
                &self.search,
                None,
                Button::new("refresh-node-pods").label("Refresh"),
                cx,
            )
        }
    }

    struct Tabs {
        pane: Entity<Pane>,
        nodes: Entity<TableState<ResourceTable>>,
        pods: bool,
    }

    impl Render for Tabs {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            h_flex()
                .size_full()
                .child(div().flex_1().min_size_0().child(self.nodes.clone()))
                .child(div().w(px(420.)).h_full().min_size_0().child(if self.pods {
                    self.pane.clone().into_any_element()
                } else {
                    div().child("Overview").into_any_element()
                }))
        }
    }

    #[::core::prelude::v1::test]
    fn nested_pod_table_survives_tab_switches_and_live_updates() {
        exercise_pod_table(true);
    }

    #[::core::prelude::v1::test]
    fn nested_pod_table_renders_without_retained_view_replays() {
        exercise_pod_table(false);
    }

    fn exercise_pod_table(retained_views: bool) {
        let cx = &mut TestAppContext::single();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
        let (window, tabs) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(retained_views);
                let table = cx.new(|cx| {
                    TableState::new(
                        ResourceTable::new(columns()).without_selection(),
                        window,
                        cx,
                    )
                });
                let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search Pods"));
                let pane = cx.new(|_| Pane { table, search });
                let nodes = cx.new(|cx| {
                    let mut delegate = ResourceTable::new(ColumnSet::for_kind("", "Node", false));
                    delegate.apply(vec![Delta::Reset(vec![Arc::new(
                        serde_json::from_value(serde_json::json!({
                            "apiVersion": "v1", "kind": "Node",
                            "metadata": {"name": "test-node", "uid": "test-node"}
                        }))
                        .unwrap(),
                    )])]);
                    TableState::new(delegate, window, cx)
                });
                cx.new(|_| Tabs {
                    pane,
                    nodes,
                    pods: false,
                })
            })
            .unwrap()
        });
        let table = tabs.read_with(cx, |tabs, cx| tabs.pane.read(cx).table.clone());
        let draw = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| {
                assert_eq!(window.view_retention(), retained_views);
                window.reset_layout_stats();
                window.draw(cx).clear(cx);
                if !retained_views {
                    assert_eq!(window.layout_stats().views_reused, 0);
                }
            })
            .unwrap();
        };
        draw(cx);
        for _ in 0..4 {
            table.update(cx, |table, cx| {
                table.delegate_mut().reset(columns());
                table.refresh(cx);
                cx.notify();
            });
            tabs.update(cx, |tabs, cx| {
                tabs.pods = true;
                cx.notify();
            });
            draw(cx);
            cx.update_window(window, |_, window, _| {
                assert!(window.find("node-pods-loading").visible());
            })
            .unwrap();
            for revision in 0..5 {
                let pod = Arc::new(
                    serde_json::from_value(serde_json::json!({
                        "apiVersion": "v1", "kind": "Pod",
                        "metadata": {"name": "node-test-pod", "namespace": "default",
                            "uid": "test-pod", "resourceVersion": revision.to_string()},
                        "spec": {"nodeName": "test-node", "containers": [{"name": "app"}]},
                        "status": {"phase": "Running", "containerStatuses": [{
                            "name": "app", "ready": true, "restartCount": revision,
                            "state": {"running": {}}
                        }]}
                    }))
                    .unwrap(),
                );
                table.update(cx, |table, cx| {
                    table.delegate_mut().finish_loading();
                    table.delegate_mut().apply(vec![Delta::Reset(vec![pod])]);
                    table.refresh(cx);
                    cx.notify();
                });
                draw(cx);
                draw(cx);
                assert_eq!(table.read_with(cx, |table, _| table.delegate().len()), 1);
                cx.update_window(window, |_, window, _| {
                    assert!(window.find("node-pods-table").visible());
                })
                .unwrap();
            }
            for query in ["no-matching-pod", ""] {
                table.update(cx, |table, cx| {
                    table.delegate_mut().set_filter(query);
                    table.refresh(cx);
                    cx.notify();
                });
                draw(cx);
                draw(cx);
                cx.update_window(window, |_, window, _| {
                    assert!(
                        window
                            .find(if query.is_empty() {
                                "node-pods-table"
                            } else {
                                "node-pods-empty"
                            })
                            .visible()
                    );
                })
                .unwrap();
            }
            tabs.update(cx, |tabs, cx| {
                tabs.pods = false;
                cx.notify();
            });
            draw(cx);
        }
    }
}
