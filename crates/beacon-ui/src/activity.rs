//! The details behind the status bar counters. These only read local state.

use crate::scroll::OverflowScrollbars as _;
use beacon_kube::{ClusterSession, Health};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::theme::Tone;

pub(crate) fn health_tone(health: &Health) -> Tone {
    match health {
        Health::Connected => Tone::Healthy,
        Health::Connecting => Tone::Progressing,
        Health::Degraded { .. } => Tone::Warning,
    }
}

pub(crate) fn panel(title: String, detail: impl Into<SharedString>, cx: &App) -> Div {
    v_flex()
        .w(px(520.))
        .min_h_0()
        .max_h(px(420.))
        .gap_2()
        .text_sm()
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(detail.into()),
        )
}

pub(crate) fn watches(session: &ClusterSession, cx: &App) -> AnyElement {
    let watches = session.watches();
    let rows = watches.iter().map(|watch| {
        let key = &watch.key;
        let scope = key.namespace.as_deref().unwrap_or_else(|| {
            if session
                .discovery()
                .kinds()
                .iter()
                .any(|kind| kind.resource == key.resource && kind.namespaced)
            {
                "All namespaces"
            } else {
                "Cluster-wide"
            }
        });
        let objects = watch.objects.map_or_else(
            || "Loading objects".to_string(),
            |count| {
                crate::i18n::message("display.count_objects", &[("count", (count).to_string())])
                    .to_string()
            },
        );

        v_flex()
            .gap_1()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_2()
                    .justify_between()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(key.resource.kind.clone()),
                    )
                    .when(watch.subscribers == 0, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::i18n::text("Idle · closing soon")),
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} · {scope}", key.resource.api_version)),
            )
            .child(
                div().text_xs().child(
                    crate::i18n::message(
                        "display.objects_subscribers",
                        &[
                            ("objects", (objects).to_string()),
                            ("value0", (watch.subscribers).to_string()),
                        ],
                    )
                    .to_string(),
                ),
            )
            .when_some(key.labels.as_ref(), |row, selector| {
                row.child(
                    div().text_xs().child(
                        crate::i18n::message(
                            "display.labels_selector",
                            &[("selector", (selector).to_string())],
                        )
                        .to_string(),
                    ),
                )
            })
            .when_some(key.fields.as_ref(), |row, selector| {
                row.child(
                    div().text_xs().child(
                        crate::i18n::message(
                            "display.fields_selector",
                            &[("selector", (selector).to_string())],
                        )
                        .to_string(),
                    ),
                )
            })
    });

    panel(
        crate::i18n::message(
            "messages.watches",
            &[("value0", (watches.len()).to_string())],
        )
        .to_string(),
        format!(
            "{} · Kubernetes {}",
            session.id().display_name(),
            session.version()
        ),
        cx,
    )
    .child(
        div()
            .id("watch-activity-list")
            .min_h_0()
            .overflow_y_scrollbar()
            .child(v_flex().children(rows))
            .when(watches.is_empty(), |list| {
                list.child(crate::i18n::text("No active watches in this cluster."))
            }),
    )
    .child(
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(crate::i18n::text(
                "Shared between tabs. Idle watches close after 30 seconds.",
            )),
    )
    .into_any_element()
}
