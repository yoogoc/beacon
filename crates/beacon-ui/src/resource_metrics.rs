//! Per-resource metrics: Metrics API session sampling and Prometheus history.
use crate::{
    bridge::Bridge,
    theme::{BeaconTheme as _, Tone},
};
use beacon_kube::{
    ClusterSession, DynamicObject, Kind, ObjectRef,
    connection::MetricsSource,
    overview::Range,
    resource_metrics::{self, Amount, Marker, Sample, Snapshot},
};
use gpui_kit::assets::IconName;
use gpui_kit::base::{Selectable as _, StyledExt as _, TestSupportExt as _};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectGroup, SelectState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{cell::Cell, collections::BTreeMap, rc::Rc, sync::Arc, time::Duration};

type Choices = SearchableVec<SelectGroup<String>>;
const TOTAL: &str = "Pod total";
fn choices(containers: &[resource_metrics::Container]) -> Choices {
    let mut groups = vec![SelectGroup::new("Scope").items(vec![TOTAL.to_owned()])];
    for group in ["Containers", "Init containers", "Ephemeral containers"] {
        let names = containers
            .iter()
            .filter(|c| c.group == group)
            .map(|c| c.name.clone())
            .collect::<Vec<_>>();
        if !names.is_empty() {
            groups.push(SelectGroup::new(group).items(names));
        }
    }
    SearchableVec::new(groups)
}

pub(crate) struct ResourceMetricsView {
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    target: ObjectRef,
    uid: Option<String>,
    container: Entity<SelectState<Choices>>,
    containers: Vec<resource_metrics::Container>,
    selected: Option<String>,
    range: Range,
    data: Option<Snapshot>,
    observed_at: f64,
    samples: Vec<Sample>,
    markers: BTreeMap<String, Marker>,
    restarts: BTreeMap<String, i64>,
    hover: Option<f64>,
    visible: bool,
    paused: bool,
    loading: bool,
    error: Option<String>,
    expanded_events: bool,
    _poll: Option<Task<()>>,
    _container: Subscription,
}
impl ResourceMetricsView {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        kind: Arc<Kind>,
        object: Arc<DynamicObject>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let containers = resource_metrics::containers(&object);
        let container = cx.new(|cx| {
            let mut state =
                SelectState::new(choices(&containers), None, window, cx).searchable(true);
            state.set_selected_value(&TOTAL.to_owned(), window, cx);
            state
        });
        let subscription = cx.subscribe_in(
            &container,
            window,
            |view: &mut Self, _, event: &SelectEvent<Choices>, window, cx| {
                if let SelectEvent::Confirm(Some(name)) = event {
                    let selected = (name != TOTAL).then(|| name.clone());
                    if selected != view.selected {
                        view.selected = selected;
                        view.data = None;
                        view.samples.clear();
                        view.markers.clear();
                        view.restarts.clear();
                        view.hover = None;
                        view.refresh(window, cx);
                    }
                }
            },
        );
        let mut view = Self {
            session,
            kind,
            target: ObjectRef::of(&object),
            uid: object.metadata.uid.clone(),
            container,
            containers,
            selected: None,
            range: Range::default(),
            data: None,
            observed_at: beacon_columns::Timestamp::now().as_millisecond() as f64 / 1000.,
            samples: vec![],
            markers: BTreeMap::new(),
            restarts: BTreeMap::new(),
            hover: None,
            visible: true,
            paused: false,
            loading: false,
            error: None,
            expanded_events: false,
            _poll: None,
            _container: subscription,
        };
        view.refresh(window, cx);
        view
    }
    fn prometheus(&self) -> bool {
        matches!(
            self.session.connection_options().metrics,
            MetricsSource::Prometheus(_)
        )
    }
    fn source(&self) -> &'static str {
        match self.session.connection_options().metrics {
            MetricsSource::Kubernetes => "Kubernetes Metrics API",
            MetricsSource::Prometheus(_) => "Prometheus",
            MetricsSource::Disabled => "Metrics disabled",
        }
    }
    pub(crate) fn set_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if visible == self.visible {
            return;
        }
        self.visible = visible;
        self.refresh(window, cx);
    }
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._poll = None;
        self.loading = false;
        if !self.visible || self.paused {
            cx.notify();
            return;
        }
        self.loading = true;
        let session = self.session.clone();
        let resource = self.kind.resource.clone();
        let target = self.target.clone();
        let uid = self.uid.clone();
        let selected = self.selected.clone();
        let seconds = self.range.seconds();
        self._poll = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                let Ok(reading) = cx.update(|_, cx| {
                    let session = session.clone();
                    let resource = resource.clone();
                    let target = target.clone();
                    let uid = uid.clone();
                    let selected = selected.clone();
                    Bridge::global(cx).run_cancellable(async move {
                        resource_metrics::fetch(
                            session,
                            resource,
                            target.namespace,
                            target.name,
                            uid,
                            selected,
                            seconds,
                        )
                        .await
                    })
                }) else {
                    break;
                };
                let result = reading.result().await;
                if this
                    .update_in(cx, |view, window, cx| {
                        view.loading = false;
                        match result {
                            Ok(Ok(data)) => view.receive(data, window, cx),
                            Ok(Err(error)) => view.failed(error),
                            Err(error) => {
                                view.failed(format!("Could not refresh metrics: {error}"))
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_secs(15))
                    .await;
            }
        }));
        cx.notify();
    }
    fn failed(&mut self, error: String) {
        self.error = Some(error);
        self.observed_at = beacon_columns::Timestamp::now().as_millisecond() as f64 / 1000.;
        if !self.prometheus() {
            self.insert_sample(Sample {
                at: self.observed_at,
                ..Default::default()
            });
        }
    }
    fn insert_sample(&mut self, sample: Sample) {
        match self
            .samples
            .binary_search_by(|s| s.at.total_cmp(&sample.at))
        {
            Ok(index) => self.samples[index] = sample,
            Err(index) => self.samples.insert(index, sample),
        }
        if self.samples.len() > 2000 {
            self.samples.drain(..self.samples.len() - 2000);
        }
    }
    fn receive(&mut self, data: Snapshot, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        self.observed_at = data.observed_at;
        if !self.prometheus() {
            self.insert_sample(data.sample);
            self.samples
                .retain(|sample| sample.at >= data.observed_at - 21600.);
        }
        for marker in &data.markers {
            self.markers.insert(marker.id.clone(), marker.clone());
        }
        for (container, count) in &data.restarts {
            if let Some(previous) = self
                .restarts
                .get(container)
                .filter(|previous| count > previous)
            {
                let marker = Marker {
                    id: format!("restart/{container}/{count}"),
                    at: data.observed_at,
                    reason: "Restart observed".into(),
                    message: format!(
                        "{} · +{} restart(s); observed at refresh, exact restart time unavailable.",
                        container.split('/').skip(1).collect::<Vec<_>>().join(" / "),
                        count - previous
                    ),
                };
                self.markers.insert(marker.id.clone(), marker);
            }
        }
        self.restarts = data.restarts.clone();
        self.markers
            .retain(|_, marker| marker.at >= data.observed_at - 21600.);
        while self.markers.len() > 2000 {
            if let Some(id) = self
                .markers
                .values()
                .min_by(|a, b| a.at.total_cmp(&b.at))
                .map(|m| m.id.clone())
            {
                self.markers.remove(&id);
            }
        }
        if self.containers != data.containers {
            self.container.update(cx, |picker, cx| {
                picker.set_items(choices(&data.containers), window, cx);
                picker.set_selected_value(
                    &self.selected.clone().unwrap_or_else(|| TOTAL.into()),
                    window,
                    cx,
                );
            });
            self.containers = data.containers.clone();
        }
        self.data = Some(data);
    }
    fn points(&self) -> Vec<Sample> {
        let Some(data) = &self.data else {
            return vec![];
        };
        let start = self.observed_at - self.range.seconds() as f64;
        let points = if self.prometheus() {
            &data.history
        } else {
            &self.samples
        };
        points.iter().filter(|p| p.at >= start).copied().collect()
    }
    fn visible_markers(&self) -> Vec<Marker> {
        let end = self.observed_at;
        let mut markers = self
            .markers
            .values()
            .filter(|m| m.at >= end - self.range.seconds() as f64 && m.at <= end)
            .cloned()
            .collect::<Vec<_>>();
        markers.sort_by(|a, b| b.at.total_cmp(&a.at));
        markers
    }
    fn chart(&self, memory: bool, cx: &mut Context<Self>) -> AnyElement {
        let title = if memory { "Memory" } else { "CPU" };
        let points = self.points();
        let reference = self
            .data
            .as_ref()
            .map(|d| d.reference.clone())
            .unwrap_or_default();
        let used = self.data.as_ref().and_then(|d| value(d.sample, memory));
        let refs = [
            (
                "Requests",
                amount(reference.requests, memory),
                cx.theme().chart_3,
            ),
            (
                "Declared limits",
                amount(reference.limits, memory),
                cx.theme().warning,
            ),
            (
                "Allocatable",
                amount(reference.allocatable, memory),
                cx.theme().muted_foreground,
            ),
        ];
        let colors = [cx.theme().chart_1, cx.theme().chart_3, cx.theme().warning];
        let end = self.observed_at;
        let start = if self.prometheus() {
            self.data.as_ref().map_or(end, |d| d.history_start)
        } else {
            points
                .first()
                .map_or(end, |p| p.at)
                .max(end - self.range.seconds() as f64)
        };
        let span = (end - start).max(30.);
        let max = points
            .iter()
            .filter_map(|p| value(*p, memory))
            .chain(refs.iter().filter_map(|(_, v, _)| *v))
            .fold(if memory { 1024. * 1024. } else { 0.1 }, f64::max)
            * 1.15;
        let markers = self.visible_markers();
        let gap = if self.prometheus() {
            (self.range.seconds() / 240).max(15) as f64 * 2.
        } else {
            90.
        };
        let bounds_state = Rc::new(Cell::new(Bounds::default()));
        let measured = bounds_state.clone();
        let draw_points = points.clone();
        let grid = cx.theme().border;
        let marker_color = cx.theme().danger;
        let hover = self.hover;
        let plot = canvas(
            move |bounds, _, _| {
                measured.set(bounds);
            },
            move |bounds, _, window, _| {
                let x = |at: f64| {
                    bounds.left() + bounds.size.width * ((at - start) / span).clamp(0., 1.) as f32
                };
                let y = |v: f64| {
                    bounds.top() + bounds.size.height * (1. - v / max).clamp(0., 1.) as f32
                };
                let mut path = PathBuilder::stroke(px(1.));
                for v in [0., max / 2., max] {
                    path.move_to(point(bounds.left(), y(v)));
                    path.line_to(point(bounds.right(), y(v)));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, grid);
                }
                for (_, v, color) in refs {
                    if let Some(v) = v {
                        let mut path = PathBuilder::stroke(px(1.));
                        path.move_to(point(bounds.left(), y(v)));
                        path.line_to(point(bounds.right(), y(v)));
                        if let Ok(path) = path.build() {
                            window.paint_path(path, color.opacity(0.6));
                        }
                    }
                }
                let mut path = PathBuilder::stroke(px(2.));
                let mut previous = None;
                for sample in &draw_points {
                    if let Some(v) = value(*sample, memory) {
                        let p = point(x(sample.at), y(v));
                        if previous.is_some_and(|at| sample.at - at <= gap) {
                            path.line_to(p);
                        } else {
                            path.move_to(p);
                            path.line_to(p + point(px(2.), px(0.)));
                        }
                        previous = Some(sample.at);
                    } else {
                        previous = None;
                    }
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, colors[0]);
                }
                for marker in &markers {
                    if marker.at >= start && marker.at <= end {
                        let mut path = PathBuilder::stroke(px(1.));
                        path.move_to(point(x(marker.at), bounds.top()));
                        path.line_to(point(x(marker.at), bounds.top() + px(10.)));
                        if let Ok(path) = path.build() {
                            window.paint_path(path, marker_color);
                        }
                    }
                }
                if let Some(at) = hover {
                    let mut path = PathBuilder::stroke(px(1.));
                    path.move_to(point(x(at), bounds.top()));
                    path.line_to(point(x(at), bounds.bottom()));
                    if let Ok(path) = path.build() {
                        window.paint_path(path, grid);
                    }
                }
            },
        )
        .size_full();
        let hover_points = points.clone();
        let chart = div()
            .id(if memory {
                "resource-memory-chart"
            } else {
                "resource-cpu-chart"
            })
            .test_support()
            .w_full()
            .h(px(150.))
            .child(plot)
            .on_mouse_move(cx.listener(move |view, event: &MouseMoveEvent, _, cx| {
                let bounds = bounds_state.get();
                if bounds.size.width <= px(0.) {
                    return;
                }
                let at =
                    start + ((event.position.x - bounds.left()) / bounds.size.width) as f64 * span;
                let hovered = hover_points
                    .iter()
                    .min_by(|a, b| (a.at - at).abs().total_cmp(&(b.at - at).abs()))
                    .map(|p| p.at);
                if hovered != view.hover {
                    view.hover = hovered;
                    cx.notify();
                }
            }));
        let description = self
            .hover
            .and_then(|at| points.iter().find(|p| p.at == at))
            .map(|p| {
                let nearby = self
                    .visible_markers()
                    .into_iter()
                    .filter(|marker| (marker.at - p.at).abs() <= gap)
                    .map(|marker| marker.reason)
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "{} · {}{}",
                    time(p.at, true),
                    format_value(value(*p, memory), memory),
                    if nearby.is_empty() {
                        String::new()
                    } else {
                        format!(" · {nearby}")
                    }
                )
            })
            .unwrap_or_else(|| {
                if points.iter().any(|p| value(*p, memory).is_some()) {
                    format!(
                        "Latest sample · {}",
                        self.data
                            .as_ref()
                            .map_or("—".into(), |d| time(d.sample.at, true))
                    )
                } else {
                    "No usage samples available for this scope.".into()
                }
            });
        v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .justify_between()
                    .child(div().font_medium().text_sm().child(title))
                    .child(
                        div()
                            .id(if memory {
                                "resource-memory-value"
                            } else {
                                "resource-cpu-value"
                            })
                            .test_support()
                            .text_lg()
                            .font_semibold()
                            .child(format_value(used, memory)),
                    ),
            )
            .child(
                div().flex().flex_wrap().gap_3().text_xs().children(
                    refs.into_iter()
                        .filter(|(label, _, _)| {
                            *label != "Allocatable" || self.kind.resource.kind == "Node"
                        })
                        .map(|(label, v, color)| {
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(div().size(px(5.)).rounded_full().bg(color))
                                .child(format!("{label} {}", format_value(v, memory)))
                        }),
                ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(description),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .w(px(58.))
                            .h(px(150.))
                            .justify_between()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format_value(Some(max), memory))
                            .child(format_value(Some(max / 2.), memory))
                            .child("0"),
                    )
                    .child(div().flex_1().min_w_0().child(chart)),
            )
            .child(
                h_flex()
                    .ml(px(66.))
                    .justify_between()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(time(start, false))
                    .child(time(end, false)),
            )
            .into_any_element()
    }
    fn events(&self, cx: &mut Context<Self>) -> AnyElement {
        let markers = self.visible_markers();
        let count = markers.len();
        let restarts = self.restarts.values().sum::<i64>();
        let rows = markers
            .into_iter()
            .take(if self.expanded_events { usize::MAX } else { 6 })
            .enumerate()
            .map(|(index, marker)| {
                let tone = if marker.reason == "OOMKilled" {
                    Tone::Critical
                } else {
                    Tone::Warning
                };
                v_flex()
                    .id(("resource-metrics-marker", index))
                    .test_support()
                    .gap_1()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().border.opacity(0.5))
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_2()
                            .text_xs()
                            .child(div().text_color(cx.theme().tone(tone)).child(marker.reason))
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(time(marker.at, true)),
                            ),
                    )
                    .child(div().text_xs().child(crate::copyable_text::copyable_text(
                        ("metrics-event-copy", index),
                        marker.message,
                    )))
            });
        v_flex().gap_2()
            .child(h_flex().justify_between()
                .child(div().text_sm().font_medium().child("Restarts & events"))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{restarts} total restarts · {count} markers in range"))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("Red chart ticks mark OOM, termination and warning events. Available Kubernetes events only; no historical event archive."))
            .children(rows)
            .when(count == 0, |v| v.child(div().text_xs().py_2().text_color(cx.theme().muted_foreground).child("No restart or warning markers in the selected range.")))
            .when(count > 6, |v| v.child(Button::new("resource-metrics-more-events").small().ghost().label(if self.expanded_events { "Show less" } else { "Show all events" })
                .on_click(cx.listener(|view, _, _, cx| {
                    view.expanded_events = !view.expanded_events;
                    cx.notify();
                }))))
            .into_any_element()
    }
}
fn value(sample: Sample, memory: bool) -> Option<f64> {
    if memory { sample.memory } else { sample.cpu }
}
fn amount(amount: Amount, memory: bool) -> Option<f64> {
    if memory { amount.memory } else { amount.cpu }
}
fn format_value(value: Option<f64>, memory: bool) -> String {
    value
        .map(|v| {
            if memory {
                if v >= 1024f64.powi(3) {
                    format!("{:.2} GiB", v / 1024f64.powi(3))
                } else {
                    format!("{:.1} MiB", v / 1024f64.powi(2))
                }
            } else if v < 1. {
                format!("{:.0}m", v * 1000.)
            } else {
                format!("{v:.2} cores")
            }
        })
        .unwrap_or_else(|| "—".into())
}
fn time(at: f64, full: bool) -> String {
    beacon_columns::Timestamp::from_second(at as i64)
        .map(|t| {
            let t = t.to_string();
            format!("{} UTC", if full { &t[..19] } else { &t[11..19] })
        })
        .unwrap_or_else(|_| "—".into())
}
impl Render for ResourceMetricsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.session.id().clone();
        let points = self.points();
        let coverage = format!(
            "{} range · {} · {} samples",
            if self.prometheus() {
                "Historical"
            } else {
                "Session"
            },
            self.range.label(),
            points.len()
        );
        let available = points.first().zip(points.last()).map(|(first, last)| {
            format!(
                "Available {} — {}",
                time(first.at, true),
                time(last.at, true)
            )
        });
        let controls = h_flex()
            .gap_1()
            .children(
                [Range::HalfHour, Range::Hour, Range::SixHours]
                    .into_iter()
                    .map(|range| {
                        Button::new(format!("resource-metrics-range-{}", range.label()))
                            .xsmall()
                            .ghost()
                            .label(range.label())
                            .selected(self.range == range)
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.range = range;
                                view.hover = None;
                                view.refresh(window, cx);
                            }))
                    }),
            )
            .child(
                Button::new("resource-metrics-pause")
                    .xsmall()
                    .ghost()
                    .icon(if self.paused {
                        IconName::Play
                    } else {
                        IconName::Pause
                    })
                    .tooltip(if self.paused {
                        "Resume sampling"
                    } else {
                        "Pause sampling"
                    })
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.paused = !view.paused;
                        view.refresh(window, cx);
                    })),
            )
            .child(
                Button::new("resource-metrics-refresh")
                    .xsmall()
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .tooltip("Refresh metrics")
                    .disabled(self.loading || self.paused)
                    .on_click(cx.listener(|view, _, window, cx| view.refresh(window, cx))),
            );
        let header = h_flex()
            .flex_wrap()
            .gap_2()
            .items_center()
            .justify_between()
            .child(
                Button::new("resource-metrics-source")
                    .small()
                    .ghost()
                    .icon(IconName::Activity)
                    .label(self.source())
                    .tooltip("Configure metrics source in cluster settings")
                    .on_click(move |_, _, cx| crate::preferences::cluster(id.clone(), cx)),
            )
            .when(self.kind.resource.kind == "Pod", |v| {
                v.child(
                    div().w(px(195.)).child(
                        Select::new(&self.container)
                            .id("resource-metrics-scope")
                            .accessibility_label("Metrics scope")
                            .small()
                            .title_prefix("Scope: "),
                    ),
                )
            })
            .child(controls);
        v_flex().id("resource-metrics").size_full().min_size_0().gap_3().p_4().overflow_y_scroll()
            .child(header)
            .child(div().id("resource-metrics-coverage").test_support().text_xs().text_color(cx.theme().muted_foreground).child(coverage))
            .when_some(available, |v, available| v.child(div().text_xs().text_color(cx.theme().muted_foreground).child(available)))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(if self.prometheus() {
                "Prometheus historical samples for the current resource lifetime. Gaps indicate missing data."
            } else {
                "Samples collected every 15s while this tab is visible. Earlier history is unavailable; gaps remain when paused or inactive."
            }))
            .when_some(self.error.as_ref(), |v, error| v.child(div().id("resource-metrics-error").test_support().text_sm().text_color(cx.theme().danger)
                .child(crate::copyable_text::copyable_text("metrics-failure-copy", error.clone()))))
            .children(self.data.as_ref().into_iter().flat_map(|data| data.errors.iter()).enumerate().map(|(index, error)| {
                div().id(("resource-metrics-partial-error", index)).test_support().text_xs().text_color(cx.theme().tone(Tone::Warning))
                    .child(crate::copyable_text::copyable_text(("metrics-partial-copy", index), error.clone()))
            }))
            .when(self.data.is_none(), |v| v.child(div().text_sm().py_4().text_color(cx.theme().muted_foreground)
                .child(if self.loading { "Reading metrics…" } else { "No metrics snapshot available." })))
            .child(self.chart(false, cx)).child(self.chart(true, cx))
            .child(div().text_xs().text_color(cx.theme().muted_foreground)
                .child("Reference lines use current resource settings. Declared limits sum configured budgets; containers without limits remain unbounded. Pod totals include effective init/sidecar requests."))
            .child(self.events(cx))
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::{Value, json};
    fn timestamp(offset: i64) -> String {
        beacon_columns::Timestamp::from_second(
            beacon_columns::Timestamp::now().as_second() + offset,
        )
        .unwrap()
        .to_string()
    }
    fn pod() -> Value {
        json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"api","namespace":"default","uid":"api","resourceVersion":"1"},"spec":{"containers":[{"name":"app","resources":{"requests":{"cpu":"250m","memory":"128Mi"},"limits":{"cpu":"1","memory":"1Gi"}}},{"name":"side","resources":{"requests":{"cpu":"50m"}}}],"initContainers":[{"name":"init"}]},"status":{"phase":"Running","containerStatuses":[{"name":"app","restartCount":1,"lastState":{"terminated":{"reason":"OOMKilled","exitCode":137,"finishedAt":timestamp(-10)}}},{"name":"side","restartCount":0}]}})
    }
    fn metrics(offset: i64, cpu: &str) -> Value {
        json!({"apiVersion":"metrics.k8s.io/v1beta1","kind":"PodMetrics","metadata":{"name":"api","namespace":"default"},"timestamp":timestamp(offset),"containers":[{"name":"app","usage":{"cpu":cpu,"memory":"64Mi"}},{"name":"side","usage":{"cpu":"25m","memory":"16Mi"}}]})
    }
    fn ready(cx: &mut TestAppContext, view: &Entity<ResourceMetricsView>) {
        support::settle(cx, |cx| {
            view.read_with(cx, |v, _| v.data.is_some() && !v.loading)
        });
    }
    #[::core::prelude::v1::test]
    fn resource_metrics_native_controls_show_samples_scope_and_real_restart_markers() {
        let cx = &mut support::context();
        let dir = tempfile::tempdir().unwrap();
        support::workspace(cx, dir.path());
        let (fixture, session) =
            support::fixture(cx, "metrics-ui", vec![pod(), metrics(-20, "100m")]);
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(760.), px(1100.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|cx| {
                        ResourceMetricsView::new(
                            session,
                            Arc::new(support::kind("", "Pod", "pods")),
                            Arc::new(serde_json::from_value(pod()).unwrap()),
                            window,
                            cx,
                        )
                    })
                },
            )
            .unwrap()
        });
        ready(cx, &view);
        view.read_with(cx, |view, _| {
            assert_eq!(view.samples.len(), 1);
            assert_eq!(view.data.as_ref().unwrap().sample.cpu, Some(0.125));
            assert_eq!(view.visible_markers().len(), 1);
            assert_eq!(view.source(), "Kubernetes Metrics API");
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            for id in [
                "resource-cpu-chart",
                "resource-memory-chart",
                "resource-metrics-coverage",
                "resource-metrics-scope",
            ] {
                assert!(window.find(id).visible(), "{id}");
            }
            window.hover("resource-cpu-chart", cx);
            assert!(view.read(cx).hover.is_some());
            window.click("resource-metrics-pause", cx);
            assert!(view.read(cx).paused);
            assert!(view.read(cx)._poll.is_none());
            window.click("resource-metrics-range-6h", cx);
            assert_eq!(view.read(cx).range, Range::SixHours);
            assert_eq!(view.read(cx).samples.len(), 1);
            window.click("resource-metrics-pause", cx);
        })
        .unwrap();
        ready(cx, &view);
        fixture.put(metrics(-5, "300m"));
        let mut updated = pod();
        updated["status"]["containerStatuses"][0]["restartCount"] = json!(2);
        fixture.put(updated);
        cx.update_window(window, |_, window, cx| {
            window.click("resource-metrics-refresh", cx)
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |v, _| v.samples.len() == 2));
        view.read_with(cx, |v, _| {
            assert_eq!(v.data.as_ref().unwrap().sample.cpu, Some(0.325));
            assert!(
                v.visible_markers()
                    .iter()
                    .any(|m| m.reason == "Restart observed")
            );
        });
        cx.update_window(window, |_, window, cx| {
            window.click("resource-metrics-scope", cx);
            window.input("side", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            window.press("down", cx);
            window.press("enter", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |v, _| {
                v.selected.as_deref() == Some("side") && v.data.is_some() && !v.loading
            })
        });
        view.read_with(cx, |v, _| {
            assert_eq!(v.samples.len(), 1);
            assert_eq!(v.data.as_ref().unwrap().sample.cpu, Some(0.025));
            assert_eq!(v.data.as_ref().unwrap().reference.limits.cpu, None);
            assert!(v.visible_markers().is_empty());
        });
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |v, cx| v.set_visible(false, window, cx))
        })
        .unwrap();
        assert!(view.read_with(cx, |v, _| v._poll.is_none()));
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(line, _)| line.starts_with("GET "))
        );
    }
    #[::core::prelude::v1::test]
    fn resource_metrics_detail_tab_preserves_partial_errors_and_stops_inactive_sampling() {
        let cx = &mut support::context();
        let dir = tempfile::tempdir().unwrap();
        support::workspace(cx, dir.path());
        let (fixture, session) = support::fixture(cx, "metrics-detail-ui", vec![pod()]);
        let (window, detail) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(760.), px(900.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|cx| {
                        crate::detail::DetailView::new(
                            session,
                            Arc::new(support::kind("", "Pod", "pods")),
                            Arc::new(serde_json::from_value(pod()).unwrap()),
                            None,
                            window,
                            cx,
                        )
                    })
                },
            )
            .unwrap()
        });
        cx.update_window(window, |_, window, cx| {
            detail.update(cx, |v, cx| {
                v.select(crate::detail::DetailTab::Metrics, window, cx)
            })
        })
        .unwrap();
        support::render_until(cx, window, |w| {
            w.try_find(("resource-metrics-partial-error", 0usize))
                .is_some_and(|e| e.visible())
        });
        cx.update_window(window, |_, window, cx| {
            assert!(window.find("resource-cpu-chart").visible());
            detail.update(cx, |v, cx| {
                v.select(crate::detail::DetailTab::Overview, window, cx)
            });
        })
        .unwrap();
        cx.run_until_parked();
        let requests = fixture.requests.lock().unwrap().len();
        cx.executor().advance_clock(Duration::from_secs(31));
        cx.run_until_parked();
        assert_eq!(fixture.requests.lock().unwrap().len(), requests);
    }
    #[::core::prelude::v1::test]
    fn resource_metrics_node_shows_allocatable_and_no_container_picker() {
        let cx = &mut support::context();
        let dir = tempfile::tempdir().unwrap();
        support::workspace(cx, dir.path());
        let node = json!({"apiVersion":"v1","kind":"Node","metadata":{"name":"worker","uid":"worker"},"status":{"allocatable":{"cpu":"4","memory":"8Gi"}}});
        let (_fixture, session) = support::fixture(
            cx,
            "node-metrics-ui",
            vec![
                node.clone(),
                json!({"apiVersion":"metrics.k8s.io/v1beta1","kind":"NodeMetrics","metadata":{"name":"worker"},"timestamp":timestamp(-15),"usage":{"cpu":"1","memory":"2Gi"}}),
            ],
        );
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    ResourceMetricsView::new(
                        session,
                        Arc::new(support::kind("", "Node", "nodes")),
                        Arc::new(serde_json::from_value(node).unwrap()),
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        ready(cx, &view);
        view.read_with(cx, |v, _| {
            assert_eq!(v.data.as_ref().unwrap().sample.cpu, Some(1.));
            assert_eq!(v.data.as_ref().unwrap().reference.allocatable.cpu, Some(4.));
            assert_eq!(v.data.as_ref().unwrap().reference.requests.cpu, Some(0.));
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("resource-cpu-chart").visible());
            assert!(window.find("resource-memory-chart").visible());
            assert!(window.try_find("resource-metrics-scope").is_none());
        })
        .unwrap();
    }
    #[::core::prelude::v1::test]
    fn resource_metrics_prometheus_history_and_range_remain_separate_from_session_samples() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let cx = &mut support::context();
        let dir = tempfile::tempdir().unwrap();
        support::workspace(cx, dir.path());
        let (fixture, _) = support::fixture(cx, "metrics-prom-ui", vec![pod()]);
        let runtime = cx.read(|cx| Bridge::global(cx).handle());
        let(at,config,server)=runtime.block_on(async{
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();let at=beacon_columns::Timestamp::now().as_second()-60;
            let server=tokio::spawn(async move{for _ in 0..4{let(mut stream,_)=listener.accept().await.unwrap();let mut request=vec![0;16384];let _=stream.read(&mut request).await.unwrap();let body=json!({"status":"success","data":{"resultType":"matrix","result":[{"metric":{"namespace":"default","pod":"api"},"values":[[at-300,"0.1"],[at,"0.2"]]}]}}).to_string();stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();}});
            (at,beacon_kube::connection::Prometheus{url:format!("http://{address}"),..Default::default()},server)
        });
        let session = runtime.block_on(async {
            Arc::new(
                ClusterSession::for_testing(
                    beacon_kube::ClusterId::new("prom-ui"),
                    fixture.url.clone(),
                    vec![support::kind("", "Pod", "pods")],
                )
                .with_metrics_source(MetricsSource::Prometheus(config)),
            )
        });
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    ResourceMetricsView::new(
                        session,
                        Arc::new(support::kind("", "Pod", "pods")),
                        Arc::new(serde_json::from_value(pod()).unwrap()),
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        ready(cx, &view);
        view.read_with(cx, |v, _| {
            assert_eq!(v.source(), "Prometheus");
            assert!(v.samples.is_empty());
            assert_eq!(v.points().len(), 2);
            assert_eq!(v.points()[1].at, at as f64);
        });
        cx.update_window(window, |_, window, cx| {
            window.click("resource-metrics-range-1h", cx)
        })
        .unwrap();
        ready(cx, &view);
        assert_eq!(view.read_with(cx, |v, _| v.range), Range::Hour);
        assert!(view.read_with(cx, |v, _| v.samples.is_empty()));
        runtime.block_on(server).unwrap();
    }
}
