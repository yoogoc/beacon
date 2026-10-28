//! Cluster capacity, live session samples and Prometheus history in a normal tab.
use crate::scroll::OverflowScrollbars as _;
use crate::{
    bridge::Bridge,
    detail::{DetailClosed, DetailView, OwnerRequested},
    theme::{BeaconTheme as _, Tone},
};
use beacon_kube::{
    ClusterSession, DynamicObject, Kind, ObjectRef,
    overview::{self, Range, Sample, Snapshot, Summary},
};
use gpui_kit::assets::IconName;
use gpui_kit::base::{Selectable as _, StyledExt as _, TestSupportExt as _};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{collections::VecDeque, sync::Arc, time::Duration};

mod presentation;

pub(crate) struct Navigate {
    pub kind: Arc<Kind>,
    pub target: Option<ObjectRef>,
}
impl EventEmitter<Navigate> for OverviewView {}

struct Data {
    snapshot: Snapshot,
    summary: Summary,
    nodes: Vec<Arc<DynamicObject>>,
    node_pods: Option<std::collections::HashMap<String, usize>>,
    warnings: Vec<Arc<DynamicObject>>,
}
impl Data {
    fn new(snapshot: Snapshot) -> Self {
        let summary = Summary::read(&snapshot);
        let mut nodes = snapshot.nodes.objects.clone();
        nodes.sort_by_key(|n| (!overview::node_ready(&n.data), n.metadata.name.clone()));
        let warnings = overview::warning_events(&snapshot);
        let node_pods = overview::node_pod_counts(&snapshot);
        Self {
            snapshot,
            summary,
            nodes,
            node_pods,
            warnings,
        }
    }
}

pub(crate) struct OverviewView {
    session: Arc<ClusterSession>,
    data: Option<Data>,
    samples: VecDeque<Sample>,
    range: Range,
    hover: Option<i64>,
    visible: bool,
    loading: bool,
    error: Option<String>,
    detail: Option<Entity<DetailView>>,
    _poll: Option<Task<()>>,
    _details: Vec<Subscription>,
}
impl OverviewView {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            session,
            data: None,
            samples: VecDeque::new(),
            range: Range::default(),
            hover: None,
            visible: true,
            loading: false,
            error: None,
            detail: None,
            _poll: None,
            _details: Vec::new(),
        };
        view.refresh(window, cx);
        view
    }

    pub(crate) fn set_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.refresh(window, cx);
        } else {
            self._poll = None;
            self.loading = false;
        }
    }

    fn refresh(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self._poll = None;
        if !self.visible {
            return;
        }
        self.loading = true;
        let session = self.session.clone();
        let range = self.range;
        self._poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let reading = cx.update(|cx| {
                    let session = session.clone();
                    Bridge::global(cx).run_cancellable(async move {
                        Data::new(overview::fetch(session, range).await)
                    })
                });
                let result = reading.result().await;
                if this
                    .update(cx, |view, cx| {
                        view.loading = false;
                        match result {
                            Ok(data) => {
                                view.error = None;
                                if let Some(detail) = &view.detail {
                                    let target = detail.read(cx).target().clone();
                                    if let Some(object) = data
                                        .snapshot
                                        .nodes
                                        .objects
                                        .iter()
                                        .chain(&data.snapshot.events.objects)
                                        .find(|object| ObjectRef::of(object) == target)
                                        .cloned()
                                    {
                                        detail.update(cx, |detail, cx| {
                                            detail.refresh(
                                                object,
                                                Some(Arc::new(beacon_kube::Rules::default())),
                                                cx,
                                            )
                                        });
                                    }
                                }
                                if !view.prometheus() {
                                    view.samples.push_back(overview::session_sample(
                                        &data.snapshot,
                                        &data.summary,
                                    ));
                                    while view.samples.len() > 120 {
                                        view.samples.pop_front();
                                    }
                                }
                                view.data = Some(data);
                            }
                            Err(error) => {
                                view.error = Some(
                                    crate::i18n::message(
                                        "messages.could_not_refresh_cluster_overview_error",
                                        &[("error", (error).to_string())],
                                    )
                                    .to_string(),
                                )
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
            }
        }));
        cx.notify();
    }

    pub(crate) fn close_detail(&mut self, cx: &mut Context<Self>) {
        self.detail = None;
        self._details.clear();
        cx.notify();
    }
    fn prometheus(&self) -> bool {
        matches!(
            self.session.connection_options().metrics,
            beacon_kube::connection::MetricsSource::Prometheus(_)
        )
    }
    fn kind(&self, group: &str, name: &str) -> Option<Arc<Kind>> {
        self.session
            .discovery()
            .kinds()
            .iter()
            .find(|k| k.resource.group == group && k.resource.kind == name && k.supports("list"))
            .cloned()
            .map(Arc::new)
    }
    fn navigate(&self, group: &str, name: &str, target: Option<ObjectRef>, cx: &mut Context<Self>) {
        if let Some(kind) = self.kind(group, name) {
            cx.emit(Navigate { kind, target });
        }
    }
    fn show_detail(
        &mut self,
        name: &str,
        object: Arc<DynamicObject>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(kind) = self.kind("", name) else {
            return;
        };
        self._details.clear();
        let detail = cx.new(|cx| {
            DetailView::new(
                self.session.clone(),
                kind,
                object,
                Some(Arc::new(beacon_kube::Rules::default())),
                window,
                cx,
            )
        });
        self._details
            .push(cx.subscribe(&detail, |view, _, _: &DetailClosed, cx| {
                view.detail = None;
                view._details.clear();
                cx.notify();
            }));
        self._details
            .push(cx.subscribe(&detail, |_, _, event: &OwnerRequested, cx| {
                cx.emit(Navigate {
                    kind: event.kind.clone(),
                    target: Some(event.target.clone()),
                });
            }));
        self.detail = Some(detail);
        cx.notify();
    }
    fn cluster_name(&self, cx: &App) -> String {
        crate::settings::store(cx)
            .read(cx)
            .preferences
            .clusters
            .get(self.session.id().as_str())
            .map(|c| c.alias.trim())
            .filter(|v| !v.is_empty())
            .unwrap_or(self.session.id().display_name())
            .to_owned()
    }
    fn source_name(&self, cx: &App) -> Option<String> {
        crate::sources::store(cx)
            .read(cx)
            .catalog
            .source_for(self.session.id())
            .map(|s| s.name.clone())
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let summary = self.data.as_ref().map(|d| &d.summary);
        let (label, tone) = match summary {
            None => ("Loading", Tone::Unknown),
            Some(s) if s.partial => ("Partial access", Tone::Warning),
            Some(s) if s.attention => ("Needs attention", Tone::Warning),
            _ => ("Healthy", Tone::Healthy),
        };
        let subtitle = [
            Some(format!("Cluster overview · {}", self.session.version())),
            self.source_name(cx),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let updated = self
            .data
            .as_ref()
            .map(|d| {
                format!(
                    "Updated {} ago",
                    beacon_columns::format_duration(
                        (beacon_columns::Timestamp::now().as_second() - d.snapshot.at.as_second())
                            .max(0)
                    )
                )
            })
            .unwrap_or_else(|| "Reading cluster data…".into());
        h_flex()
            .w_full()
            .gap_4()
            .justify_between()
            .items_start()
            .child(
                v_flex()
                    .gap_1()
                    .min_w_0()
                    .child(div().text_xl().font_semibold().child(
                        crate::copyable_text::copyable_text(
                            "overview-cluster-name",
                            self.cluster_name(cx),
                        ),
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(subtitle),
                    ),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .items_end()
                    .child(
                        h_flex()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .text_xs()
                            .text_color(cx.theme().tone(tone))
                            .bg(cx.theme().tone_surface(tone))
                            .child(
                                Icon::new(if tone == Tone::Healthy {
                                    IconName::CircleCheck
                                } else {
                                    IconName::Info
                                })
                                .size_3(),
                            )
                            .child(crate::i18n::text(label)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(updated)
                            .child(
                                Button::new("refresh-cluster-overview")
                                    .small()
                                    .ghost()
                                    .icon(IconName::RefreshCw)
                                    .tooltip(crate::i18n::text("Refresh overview"))
                                    .disabled(self.loading)
                                    .on_click(
                                        cx.listener(|view, _, window, cx| view.refresh(window, cx)),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_stats(&self, cx: &mut Context<Self>) -> AnyElement {
        let summary = self
            .data
            .as_ref()
            .map(|d| &d.summary)
            .cloned()
            .unwrap_or_default();
        let pair = |value: Option<(usize, usize)>| {
            value
                .map(|(a, b)| format!("{a} / {b}"))
                .unwrap_or_else(|| "—".into())
        };
        let cards = [
            (
                "Nodes",
                pair(summary.nodes),
                "Ready",
                IconName::Server,
                "",
                "Node",
            ),
            (
                "Pods",
                pair(summary.pods),
                "Ready · active Pods",
                IconName::Box,
                "",
                "Pod",
            ),
            (
                "Workloads",
                pair(summary.workloads),
                "Available",
                IconName::Layers,
                "apps",
                "Deployment",
            ),
            (
                "Warnings",
                summary
                    .warnings
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".into()),
                "Distinct events · last 1h",
                IconName::TriangleAlert,
                "",
                "Event",
            ),
        ];
        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .children(
                cards
                    .into_iter()
                    .map(|(label, value, hint, icon, group, kind)| {
                        v_flex()
                            .id(format!("overview-stat-{label}"))
                            .role(Role::Button)
                            .aria_label(format!("View {label}: {value}"))
                            .aria_description(if label == "Workloads" {
                                "Deployments, StatefulSets and DaemonSets"
                            } else {
                                hint
                            })
                            .test_support()
                            .flex_1()
                            .min_w(px(170.))
                            .gap_2()
                            .p_3()
                            .rounded_lg()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().background)
                            .cursor_pointer()
                            .hover(|s| s.bg(cx.theme().muted.opacity(0.4)))
                            .child(
                                h_flex()
                                    .justify_between()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(crate::i18n::text(label))
                                    .child(Icon::new(icon).size_4()),
                            )
                            .child(div().text_xl().font_medium().child(value))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(crate::i18n::text(hint)),
                            )
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.navigate(group, kind, None, cx)
                            }))
                    }),
            )
            .into_any_element()
    }

    fn render_nodes(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = v_flex().gap_0();
        if let Some(data) = &self.data {
            if let Some(error) = &data.snapshot.nodes.error {
                body = body.child(presentation::error("overview-nodes-error", error, cx));
            } else if data.nodes.is_empty() {
                body = body.child(presentation::empty("No nodes found", cx));
            } else {
                body = body.child(presentation::node_row(
                    "Node".into(),
                    "CPU".into(),
                    "Memory".into(),
                    "Pods".into(),
                    None,
                    cx,
                ));
                for (index, node) in data.nodes.iter().take(6).enumerate() {
                    let name = node.metadata.name.clone().unwrap_or_default();
                    let usage = overview::node_usage(&data.snapshot.metrics.metrics, node);
                    let cpu = usage
                        .map(|v| format!("{:.0}%", v.0))
                        .unwrap_or_else(|| "—".into());
                    let memory = usage
                        .map(|v| format!("{:.0}%", v.1))
                        .unwrap_or_else(|| "—".into());
                    let pods = data
                        .node_pods
                        .as_ref()
                        .map(|counts| counts.get(&name).copied().unwrap_or(0).to_string())
                        .unwrap_or_else(|| "—".into());
                    let ready = overview::node_ready(&node.data);
                    let object = node.clone();
                    body = body.child(
                        div()
                            .id(("overview-node", index))
                            .role(Role::Button)
                            .aria_label(
                                crate::i18n::message(
                                    "messages.node_details_name",
                                    &[("name", (name).to_string())],
                                )
                                .to_string(),
                            )
                            .test_support()
                            .cursor_pointer()
                            .hover(|s| s.bg(cx.theme().muted.opacity(0.4)))
                            .child(presentation::node_row(
                                name,
                                cpu,
                                memory,
                                pods,
                                Some(ready),
                                cx,
                            ))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.show_detail("Node", object.clone(), window, cx)
                            })),
                    );
                }
            }
        } else {
            body = body.child(presentation::empty("Reading nodes…", cx));
        }
        let count = self
            .data
            .as_ref()
            .and_then(|d| d.summary.nodes)
            .map(|v| {
                crate::i18n::message("messages.view_all", &[("value0", (v.1).to_string())])
                    .to_string()
            })
            .unwrap_or_else(|| "View all".into());
        presentation::panel(
            h_flex()
                .justify_between()
                .items_center()
                .child(crate::i18n::text("Nodes"))
                .child(
                    Button::new("overview-all-nodes")
                        .xsmall()
                        .ghost()
                        .label(count)
                        .on_click(
                            cx.listener(|view, _, _, cx| view.navigate("", "Node", None, cx)),
                        ),
                ),
            body,
            cx,
        )
    }

    fn render_warnings(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = v_flex();
        if let Some(data) = &self.data {
            if let Some(error) = &data.snapshot.events.error {
                body = body.child(presentation::error("overview-events-error", error, cx));
            } else if data.warnings.is_empty() {
                body = body.child(presentation::empty(
                    "No warning events in the last hour.",
                    cx,
                ));
            } else {
                body = body.child(presentation::event_header(cx));
                for (index, object) in data.warnings.iter().take(8).enumerate() {
                    let event = beacon_columns::EventSummary::read(
                        &object.data,
                        beacon_columns::Timestamp::now(),
                    );
                    let target = overview::event_target(object)
                        .map(|(g, k, t)| (g.to_owned(), k.to_owned(), t));
                    let object = object.clone();
                    body = body.child(
                        presentation::event_row(index, event, &object, target, cx)
                            .test_support()
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.show_detail("Event", object.clone(), window, cx)
                            })),
                    );
                }
            }
        } else {
            body = body.child(presentation::empty("Reading warning events…", cx));
        }
        presentation::panel(
            h_flex()
                .gap_2()
                .justify_between()
                .items_center()
                .child(
                    h_flex()
                        .gap_2()
                        .child(crate::i18n::text("Warning events"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::i18n::text("Last 1h")),
                        ),
                )
                .child(
                    Button::new("overview-all-events")
                        .xsmall()
                        .ghost()
                        .label(crate::i18n::text("View events"))
                        .on_click(
                            cx.listener(|view, _, _, cx| view.navigate("", "Event", None, cx)),
                        ),
                ),
            body,
            cx,
        )
    }
}

impl Render for OverviewView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = v_flex()
            .id("cluster-overview-content")
            .test_support()
            .w_full()
            .gap_4()
            .p_5()
            .child(self.render_header(cx))
            .child(self.render_stats(cx))
            .when_some(self.error.clone(), |view, error| {
                view.child(presentation::error("overview-refresh-error", &error, cx))
            })
            .child(self.render_capacity(cx))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .flex_basis(px(360.))
                            .min_w_0()
                            .child(self.render_trend(cx)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex_basis(px(360.))
                            .min_w_0()
                            .child(self.render_nodes(cx)),
                    ),
            )
            .child(self.render_warnings(cx))
            .when_some(
                self.data
                    .as_ref()
                    .and_then(|d| d.snapshot.workloads.error.clone()),
                |view, error| {
                    view.child(presentation::error("overview-workloads-error", &error, cx))
                },
            );
        div()
            .relative()
            .size_full()
            .min_size_0()
            .bg(cx.theme().background)
            .child(
                div()
                    .id("cluster-overview-scroll")
                    .size_full()
                    .overflow_y_scrollbar()
                    .child(content),
            )
            .when_some(self.detail.clone(), |view, detail| {
                view.child(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(cx.theme().background.opacity(0.45))
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|view, _, _, cx| {
                                cx.stop_propagation();
                                view.detail = None;
                                view._details.clear();
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .right_0()
                                .bottom_0()
                                .w(px(470.))
                                .max_w_full()
                                .bg(cx.theme().background)
                                .border_l_1()
                                .border_color(cx.theme().border)
                                .shadow_lg()
                                .occlude()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                                .child(detail),
                        ),
                )
            })
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;

    #[::core::prelude::v1::test]
    fn overview_renders_real_capacity_and_details_and_handles_denied_nodes() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let (fixture, session) = support::fixture(
            cx,
            "overview-ui",
            vec![
                json!({"apiVersion":"v1","kind":"Node","metadata":{"name":"worker","uid":"worker","resourceVersion":"1"},"status":{"capacity":{"cpu":"4","memory":"8Gi","pods":"110"},"allocatable":{"cpu":"3800m","memory":"7Gi","pods":"100"},"conditions":[{"type":"Ready","status":"True"}]}}),
                json!({"apiVersion":"metrics.k8s.io/v1beta1","kind":"NodeMetrics","metadata":{"name":"worker"},"usage":{"cpu":"1","memory":"2Gi"}}),
                json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"api","namespace":"default","uid":"api","resourceVersion":"1"},"spec":{"nodeName":"worker","containers":[{"name":"app","image":"busybox","resources":{"requests":{"cpu":"250m","memory":"128Mi"},"limits":{"cpu":"1","memory":"1Gi"}}}]},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}]}}),
                json!({"apiVersion":"v1","kind":"Event","metadata":{"name":"warning","namespace":"default","uid":"warning","resourceVersion":"1"},"type":"Warning","reason":"BackOff","message":"Fixture warning","lastTimestamp":beacon_columns::Timestamp::now().to_string(),"involvedObject":{"apiVersion":"v1","kind":"Pod","name":"api","namespace":"default"}}),
            ],
        );
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| OverviewView::new(session, window, cx))
            })
            .unwrap()
        });
        support::settle(cx, |cx| view.read_with(cx, |v, _| v.data.is_some()));
        view.read_with(cx, |view, _| {
            let summary = &view.data.as_ref().unwrap().summary;
            assert_eq!(summary.nodes, Some((1, 1)));
            assert_eq!(summary.usage.unwrap().cpu, 1.);
            assert_eq!(summary.warnings, Some(1));
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("overview-trend-plot").visible());
            window.click(("overview-node", 0usize), cx);
            assert!(view.read(cx).detail.is_some());
            view.update(cx, |view, cx| view.close_detail(cx));
            window.render_frame(cx);
            window.click(("overview-warning", 0usize), cx);
            assert!(view.read(cx).detail.is_some());
            view.update(cx, |view, cx| {
                view.close_detail(cx);
                view.set_visible(false, window, cx);
            });
            assert!(view.read(cx)._poll.is_none());
            fixture.deny("nodes");
            view.update(cx, |view, cx| view.set_visible(true, window, cx));
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |v, _| {
                v.data.as_ref().is_some_and(|d| d.summary.nodes.is_none())
            })
        });
        support::render_until(cx, window, |window| {
            window.find("overview-nodes-error").visible()
        });
        view.read_with(cx, |view, _| {
            let summary = &view.data.as_ref().unwrap().summary;
            assert!(summary.capacity.is_none());
            assert!(summary.usage.is_none());
            assert_eq!(summary.pods, Some((1, 1)));
        });
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
