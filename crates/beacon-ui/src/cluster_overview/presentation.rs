use super::*;
use beacon_kube::overview::Resources;
use std::{cell::Cell, rc::Rc};

pub(super) fn panel(header: impl IntoElement, body: impl IntoElement, cx: &App) -> AnyElement {
    v_flex()
        .w_full()
        .min_w_0()
        .p_3()
        .gap_2()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .child(div().text_sm().font_medium().child(header))
        .child(body)
        .into_any_element()
}
pub(super) fn empty(message: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .py_8()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(crate::i18n::text(message.into()))
        .into_any_element()
}
pub(super) fn error(id: &'static str, message: &str, cx: &App) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .text_sm()
        .text_color(cx.theme().tone(Tone::Warning))
        .p_2()
        .child(crate::copyable_text::copyable_text(
            format!("copy-{id}"),
            message.to_owned(),
        ))
        .into_any_element()
}

pub(super) fn node_row(
    name: String,
    cpu: String,
    memory: String,
    pods: String,
    ready: Option<bool>,
    cx: &App,
) -> AnyElement {
    h_flex()
        .w_full()
        .gap_2()
        .items_center()
        .py_2()
        .border_b_1()
        .border_color(cx.theme().border.opacity(0.6))
        .text_xs()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .truncate()
                        .text_color(if ready.is_some() {
                            cx.theme().resource_link()
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(name),
                )
                .when_some(ready, |view, ready| {
                    view.child(
                        div()
                            .text_color(cx.theme().tone(if ready {
                                Tone::Healthy
                            } else {
                                Tone::Warning
                            }))
                            .child(crate::i18n::text(if ready {
                                "● Ready"
                            } else {
                                "● NotReady"
                            })),
                    )
                }),
        )
        .children(
            [(cpu, 52.), (memory, 62.), (pods, 40.)]
                .into_iter()
                .map(|(text, width)| div().w(px(width)).flex_shrink_0().text_right().child(text)),
        )
        .into_any_element()
}
pub(super) fn event_header(cx: &App) -> AnyElement {
    h_flex()
        .gap_3()
        .py_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(div().flex_1().child(crate::i18n::text("Reason / message")))
        .child(div().w(px(190.)).child(crate::i18n::text("Object")))
        .child(div().w(px(60.)).child(crate::i18n::text("Seen")))
        .child(
            div()
                .w(px(44.))
                .text_right()
                .child(crate::i18n::text("Count")),
        )
        .into_any_element()
}
pub(super) fn event_row(
    index: usize,
    event: beacon_columns::EventSummary,
    object: &DynamicObject,
    target: Option<(String, String, ObjectRef)>,
    cx: &mut Context<OverviewView>,
) -> Stateful<Div> {
    let object_name = target
        .as_ref()
        .map(|(_, _, t)| t.name.clone())
        .unwrap_or_else(|| event.object.clone());
    let object_hint = target
        .as_ref()
        .map(|(_, k, t)| format!("{k} · {}", t.namespace.as_deref().unwrap_or("cluster")))
        .unwrap_or_else(|| object.metadata.namespace.clone().unwrap_or_default());
    let object_label = v_flex()
        .gap_1()
        .w(px(190.))
        .min_w_0()
        .flex_shrink_0()
        .child(
            Button::new(("overview-event-object", index))
                .xsmall()
                .ghost()
                .label(object_name)
                .disabled(target.is_none())
                .on_click(cx.listener(move |view, _, _, cx| {
                    cx.stop_propagation();
                    if let Some((g, k, t)) = &target {
                        view.navigate(g, k, Some(t.clone()), cx);
                    }
                })),
        )
        .child(
            div()
                .text_color(cx.theme().muted_foreground)
                .truncate()
                .child(object_hint),
        );
    h_flex()
        .id(("overview-warning", index))
        .role(Role::Button)
        .aria_label(
            crate::i18n::message(
                "messages.warning_event",
                &[("value0", (event.reason).to_string())],
            )
            .to_string(),
        )
        .gap_3()
        .w_full()
        .py_3()
        .items_start()
        .text_xs()
        .cursor_pointer()
        .hover(|s| s.bg(cx.theme().muted.opacity(0.4)))
        .border_t_1()
        .border_color(cx.theme().border.opacity(0.6))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_color(cx.theme().tone(Tone::Warning))
                        .child(event.reason),
                )
                .child(crate::copyable_text::copyable_text(
                    ("overview-warning-message", index),
                    event.message,
                )),
        )
        .child(object_label)
        .child(div().w(px(60.)).flex_shrink_0().child(event.last_seen))
        .child(
            div()
                .w(px(44.))
                .flex_shrink_0()
                .text_right()
                .child(event.count.to_string()),
        )
}

fn value(value: Option<f64>, memory: bool) -> String {
    value
        .map(|v| {
            if memory {
                format!("{:.1} GiB", v / 1024f64.powi(3))
            } else {
                format!("{v:.1} cores")
            }
        })
        .unwrap_or_else(|| "—".into())
}
fn percent(value: Option<f64>, alloc: Option<f64>) -> Option<f64> {
    value
        .zip(alloc)
        .filter(|(_, a)| *a > 0.)
        .map(|(v, a)| v / a * 100.)
}
fn bar(
    label: &'static str,
    value: Option<f64>,
    alloc: Option<f64>,
    color: Hsla,
    memory: bool,
    cx: &App,
) -> AnyElement {
    let percent = percent(value, alloc);
    let text = format!(
        "{}{}",
        self::value(value, memory),
        percent.map(|p| format!(" · {p:.0}%")).unwrap_or_default()
    );
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .justify_between()
                .text_xs()
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::text(label)),
                )
                .child(
                    div()
                        .text_color(if percent.is_some_and(|p| p > 100.) {
                            cx.theme().tone(Tone::Warning)
                        } else {
                            cx.theme().foreground
                        })
                        .child(text),
                ),
        )
        .child(
            div()
                .w_full()
                .h(px(4.))
                .rounded_full()
                .overflow_hidden()
                .bg(cx.theme().muted)
                .when_some(percent, |track, p| {
                    track.child(
                        div()
                            .h_full()
                            .w(relative((p / 100.).clamp(0., 1.) as f32))
                            .bg(color),
                    )
                }),
        )
        .into_any_element()
}
fn capacity_card(
    title: &'static str,
    icon: IconName,
    memory: bool,
    summary: &Summary,
    cx: &App,
) -> AnyElement {
    let amount = |v: Option<Resources>| v.map(|v| if memory { v.memory } else { v.cpu });
    let alloc = amount(summary.allocatable);
    let used = amount(summary.usage);
    let requests = amount(summary.requests);
    let limits = amount(summary.limits);
    let tooltip = if percent(limits, alloc).is_some_and(|p| p > 100.) {
        "Declared limits exceed allocatable capacity. Limits are ceilings, not actual usage; containers without limits are not included."
    } else {
        "Requests and declared limits are for scheduled active Pods, including init containers and Pod overhead. Containers without limits are not included."
    };
    v_flex()
        .flex_1()
        .min_w(px(230.))
        .p_3()
        .gap_3()
        .rounded_lg()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().background)
        .child(
            h_flex()
                .gap_2()
                .text_sm()
                .font_medium()
                .child(Icon::new(icon).size_4())
                .child(crate::i18n::text(title)),
        )
        .child(
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_1()
                        .items_baseline()
                        .child(div().text_lg().font_medium().child(value(used, memory)))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("/ {}", value(alloc, memory))),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::text("Usage / allocatable")),
                ),
        )
        .child(
            v_flex()
                .gap_2()
                .child(bar("Usage", used, alloc, cx.theme().chart_1, memory, cx))
                .child(bar(
                    "Requests",
                    requests,
                    alloc,
                    cx.theme().chart_3,
                    memory,
                    cx,
                ))
                .child(bar(
                    "Declared limits",
                    limits,
                    alloc,
                    cx.theme().warning,
                    memory,
                    cx,
                )),
        )
        .child(
            h_flex()
                .id(format!("capacity-explanation-{title}"))
                .gap_1()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(
                    crate::i18n::message(
                        "messages.capacity",
                        &[(
                            "value0",
                            (value(amount(summary.capacity), memory)).to_string(),
                        )],
                    )
                    .to_string(),
                )
                .child(Icon::new(IconName::Info).size_3())
                .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx)),
        )
        .into_any_element()
}

impl OverviewView {
    pub(super) fn render_capacity(&self, cx: &mut Context<Self>) -> AnyElement {
        let summary = self
            .data
            .as_ref()
            .map(|d| &d.summary)
            .cloned()
            .unwrap_or_default();
        let provider = match self.session.connection_options().metrics {
            beacon_kube::connection::MetricsSource::Kubernetes => "Kubernetes Metrics API",
            beacon_kube::connection::MetricsSource::Prometheus(_) => "Prometheus",
            beacon_kube::connection::MetricsSource::Disabled => "Metrics disabled",
        };
        let id = self.session.id().clone();
        let alloc = summary.allocatable.map(|v| v.pods);
        let scheduled = summary.scheduled.map(|v| v as f64);
        let pods = v_flex()
            .flex_1()
            .min_w(px(230.))
            .p_3()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .font_medium()
                    .child(Icon::new(IconName::Box).size_4())
                    .child(crate::i18n::text("Pods")),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_lg().font_medium().child(format!(
                            "{} / {} Pods",
                            summary
                                .scheduled
                                .map(|v| v.to_string())
                                .unwrap_or_else(|| "—".into()),
                            alloc
                                .map(|v| format!("{v:.0}"))
                                .unwrap_or_else(|| "—".into())
                        )))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(crate::i18n::text("Scheduled active Pods")),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .justify_between()
                            .text_xs()
                            .child(crate::i18n::text("Scheduled"))
                            .child(
                                percent(scheduled, alloc)
                                    .map(|p| format!("{p:.0}%"))
                                    .unwrap_or_else(|| "—".into()),
                            ),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(4.))
                            .rounded_full()
                            .overflow_hidden()
                            .bg(cx.theme().muted)
                            .when_some(percent(scheduled, alloc), |t, p| {
                                t.child(
                                    div()
                                        .h_full()
                                        .w(relative((p / 100.).clamp(0., 1.) as f32))
                                        .bg(cx.theme().chart_1),
                                )
                            }),
                    ),
            )
            .children(
                [
                    ("Ready", summary.pods.map(|v| v.0)),
                    ("Pending", summary.pending),
                    ("Completed / failed", summary.terminal),
                ]
                .into_iter()
                .map(|(label, count)| {
                    h_flex()
                        .justify_between()
                        .text_xs()
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::i18n::text(label)),
                        )
                        .child(count.map(|v| v.to_string()).unwrap_or_else(|| "—".into()))
                }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        crate::i18n::message(
                            "messages.capacity_pods",
                            &[(
                                "value0",
                                (summary
                                    .capacity
                                    .map(|v| format!("{:.0}", v.pods))
                                    .unwrap_or_else(|| "—".into()))
                                .to_string(),
                            )],
                        )
                        .to_string(),
                    ),
            );
        let mut content = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .child(crate::i18n::text("Resource capacity")),
                    )
                    .child(
                        Button::new("overview-metrics-settings")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Activity)
                            .label(provider)
                            .tooltip(crate::i18n::text(
                                "Configure metrics source in cluster settings",
                            ))
                            .on_click(move |_, _, cx| crate::preferences::cluster(id.clone(), cx)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .child(capacity_card("CPU", IconName::Cpu, false, &summary, cx))
                    .child(capacity_card(
                        "Memory",
                        IconName::MemoryStick,
                        true,
                        &summary,
                        cx,
                    ))
                    .child(pods),
            );
        if let Some(data) = &self.data {
            for (id, error) in [
                (
                    "overview-capacity-error",
                    data.snapshot.nodes.error.as_ref(),
                ),
                ("overview-requests-error", data.snapshot.pods.error.as_ref()),
                (
                    "overview-metrics-error",
                    data.snapshot.metrics.error.as_ref(),
                ),
            ] {
                if let Some(error) = error {
                    content = content.child(self::error(id, error, cx));
                }
            }
            if data.snapshot.metrics.error.is_none() && data.summary.usage.is_none() {
                content = content.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::text(
                            "Usage is unavailable until CPU and memory metrics cover every node.",
                        )),
                );
            }
        }
        content.into_any_element()
    }

    pub(super) fn render_trend(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut header = h_flex()
            .justify_between()
            .items_center()
            .child(crate::i18n::text("Usage trend"));
        if self.prometheus() {
            header = header.child(
                h_flex().gap_1().children(
                    [Range::HalfHour, Range::Hour, Range::SixHours]
                        .into_iter()
                        .map(|range| {
                            Button::new(format!("overview-range-{}", range.label()))
                                .xsmall()
                                .ghost()
                                .label(crate::i18n::text(range.label()))
                                .selected(self.range == range)
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.range = range;
                                    view.hover = None;
                                    view.refresh(window, cx);
                                }))
                        }),
                ),
            );
        } else {
            header = header.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::text("Session samples")),
            );
        }
        let Some(data) = &self.data else {
            return panel(header, empty("Collecting usage samples…", cx), cx);
        };
        if let Err(error) = &data.snapshot.history {
            return panel(header, self::error("overview-history-error", error, cx), cx);
        }
        let points: Vec<_> = if self.prometheus() {
            data.snapshot.history.as_ref().unwrap().clone()
        } else {
            self.samples.iter().cloned().collect()
        };
        let Some(alloc) = data
            .summary
            .allocatable
            .filter(|a| a.cpu > 0. && a.memory > 0.)
        else {
            return panel(
                header,
                empty("Node allocatable capacity is unavailable.", cx),
                cx,
            );
        };
        let end = data.snapshot.at.as_second();
        let start = if self.prometheus() {
            end - self.range.seconds()
        } else {
            points.first().map(|p| p.at).unwrap_or(end).max(end - 3600)
        };
        let points: Vec<_> = points
            .into_iter()
            .filter(|p| p.at >= start)
            .map(|p| Sample {
                at: p.at,
                cpu: p.cpu.map(|v| v / alloc.cpu * 100.),
                memory: p.memory.map(|v| v / alloc.memory * 100.),
            })
            .collect();
        if !points.iter().any(|p| p.cpu.is_some() || p.memory.is_some()) {
            return panel(
                header,
                empty(
                    "No usage samples available from the configured metrics source.",
                    cx,
                ),
                cx,
            );
        }
        let max = points
            .iter()
            .flat_map(|p| [p.cpu, p.memory])
            .flatten()
            .fold(100f64, f64::max)
            .ceil();
        let colors = [cx.theme().chart_1, cx.theme().chart_2];
        let grid = cx.theme().border;
        let bounds_state = Rc::new(Cell::new(Bounds::default()));
        let measure_bounds = bounds_state.clone();
        let draw_points = points.clone();
        let hover = self.hover;
        let span = (end - start).max(30) as f64;
        let gap = if self.prometheus() {
            (self.range.seconds() / 120).max(15) * 2
        } else {
            90
        };
        let plot = canvas(
            move |bounds, _, _| {
                measure_bounds.set(bounds);
            },
            move |bounds, _, window, _| {
                let x = |at: i64| {
                    bounds.origin.x
                        + bounds.size.width * ((at - start) as f64 / span).clamp(0., 1.) as f32
                };
                let y = |value: f64| {
                    bounds.origin.y + bounds.size.height * (1. - value / max).clamp(0., 1.) as f32
                };
                let mut path = PathBuilder::stroke(px(1.));
                for v in [0., max / 2., max] {
                    path.move_to(point(bounds.origin.x, y(v)));
                    path.line_to(point(bounds.right(), y(v)));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, grid);
                }
                for (series, color) in colors.into_iter().enumerate() {
                    let mut path = PathBuilder::stroke(px(2.));
                    let mut previous = None;
                    for sample in &draw_points {
                        let value = if series == 0 {
                            sample.cpu
                        } else {
                            sample.memory
                        };
                        if let Some(value) = value {
                            let p = point(x(sample.at), y(value));
                            if previous.is_some_and(|at| sample.at - at <= gap) {
                                path.line_to(p);
                            } else {
                                path.move_to(p);
                                path.line_to(p + point(px(1.), px(0.)));
                            }
                            previous = Some(sample.at);
                        } else {
                            previous = None;
                        }
                    }
                    if let Ok(path) = path.build() {
                        window.paint_path(path, color);
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
            .id("overview-trend-plot")
            .test_support()
            .relative()
            .w_full()
            .h(px(170.))
            .child(plot)
            .on_mouse_move(cx.listener(move |view, event: &MouseMoveEvent, _, cx| {
                let bounds = bounds_state.get();
                if bounds.size.width <= px(0.) {
                    return;
                }
                let at = start
                    + (((event.position.x - bounds.origin.x) / bounds.size.width) as f64 * span)
                        as i64;
                let hovered = hover_points
                    .iter()
                    .min_by_key(|p| (p.at - at).abs())
                    .map(|p| p.at);
                if view.hover != hovered {
                    view.hover = hovered;
                    cx.notify();
                }
            }));
        let description = self
            .hover
            .and_then(|at| points.iter().find(|p| p.at == at))
            .map(|p| {
                format!(
                    "{} · CPU {} · Memory {}",
                    time(p.at),
                    percent_text(p.cpu),
                    percent_text(p.memory)
                )
            })
            .unwrap_or_else(|| "% of current allocatable capacity".into());
        let body=v_flex().gap_2().child(h_flex().gap_3().text_xs().children([("CPU",colors[0]),("Memory",colors[1])].into_iter().map(|(label,color)|h_flex().gap_1().items_center().child(div().size(px(6.)).rounded_full().bg(color)).child(crate::i18n::text(label)))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(description))
            .child(h_flex().gap_2().child(v_flex().w(px(38.)).h(px(170.)).justify_between().text_xs().text_color(cx.theme().muted_foreground).child(format!("{max:.0}%")).child(format!("{:.0}%",max/2.)).child("0%"))
                .child(div().flex_1().min_w_0().child(chart)))
            .child(h_flex().ml(px(46.)).justify_between().text_xs().text_color(cx.theme().muted_foreground).child(time(start)).child(time(end)))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text(if self.prometheus(){"Historical samples from Prometheus."}else{"Samples are collected while Overview is visible; earlier history is unavailable."})));
        panel(header, body, cx)
    }
}

fn time(seconds: i64) -> String {
    beacon_columns::Timestamp::from_second(seconds)
        .map(|t| {
            let text = t.to_string();
            format!("{} UTC", &text[11..16])
        })
        .unwrap_or_else(|_| "—".into())
}
fn percent_text(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.1}%"))
        .unwrap_or_else(|| "—".into())
}
