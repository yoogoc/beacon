//! A bounded review surface: resource identity, diff controls and a fixed footer.
use super::*;
use crate::{
    copyable_text::copyable_text,
    theme::{BeaconTheme as _, Tone},
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    diff::{Diff, DiffHunkSeparator},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;

fn count_badge(count: usize, added: bool, cx: &App) -> AnyElement {
    let color = if added {
        cx.theme().success
    } else {
        cx.theme().danger
    };
    div()
        .px_1p5()
        .py_0p5()
        .rounded_sm()
        .text_xs()
        .text_color(color)
        .bg(color.opacity(0.1))
        .child(format!(
            "{}{} {}",
            if added { "+" } else { "−" },
            count,
            if added { "added" } else { "removed" }
        ))
        .into_any_element()
}

impl ReviewView {
    fn step_change(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.preview.changes.len();
        if count == 0 {
            return;
        }
        self.change = if forward {
            (self.change + 1) % count
        } else {
            (self.change + count - 1) % count
        };
        let position = self.preview.changes[self.change].clone();
        self.diff
            .update(cx, |state, cx| state.scroll_to_line(position, cx));
        cx.notify();
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let manifest = &self.preview.manifest;
        let name = manifest
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .or_else(|| {
                manifest
                    .pointer("/metadata/generateName")
                    .and_then(Value::as_str)
            })
            .map(str::to_owned)
            .unwrap_or_else(|| {
                self.context
                    .rsplit(" · ")
                    .next()
                    .unwrap_or(&self.context)
                    .to_owned()
            });
        let subtitle = if manifest.is_string() {
            self.context.clone()
        } else {
            let kind = manifest
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("Resource");
            let namespace = manifest
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .unwrap_or("Cluster-scoped");
            let cluster = self.context.split(" · ").next().unwrap_or(&self.context);
            format!("{kind} · {namespace} · {cluster}")
        };
        let added = self.preview.files.iter().map(DiffFile::additions).sum();
        let removed = self.preview.files.iter().map(DiffFile::deletions).sum();
        v_flex()
            .w_full()
            .flex_shrink_0()
            .px_4()
            .pt_4()
            .pb_3()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.title),
                    )
                    .child(
                        Button::new("close-yaml-review")
                            .ghost()
                            .small()
                            .icon(IconName::X)
                            .accessibility_label(crate::i18n::text("Back to editing"))
                            .tooltip(crate::i18n::text("Back to editing"))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.cancel(cx);
                                window.defer(cx, |window, cx| window.close_dialog(cx));
                            })),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(34.))
                            .flex_shrink_0()
                            .rounded_md()
                            .bg(cx.theme().muted)
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(cx.theme().muted_foreground)
                            .child(Icon::new(IconName::FileCode).size_4()),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(copyable_text("yaml-review-name", name)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(copyable_text("yaml-review-context", subtitle)),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_shrink_0()
                            .gap_1p5()
                            .child(count_badge(added, true, cx))
                            .child(count_badge(removed, false, cx)),
                    ),
            )
            .when(self.force, |header| {
                header.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().tone(Tone::Warning))
                        .child(crate::i18n::text(
                            "Force apply takes ownership of conflicting fields.",
                        )),
                )
            })
            .into_any_element()
    }

    fn toolbar(&self, mode: DiffMode, cx: &mut Context<Self>) -> AnyElement {
        let count = self.preview.changes.len();
        h_flex()
            .w_full()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_y_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(0.4))
            .child(
                h_flex()
                    .gap_0p5()
                    .p_0p5()
                    .rounded_md()
                    .bg(cx.theme().muted)
                    .children(
                        [
                            ("diff-split", "Side by side", DiffMode::Split),
                            ("diff-unified", "Unified", DiffMode::Unified),
                        ]
                        .into_iter()
                        .map(|(id, label, selected)| {
                            Button::new(id)
                                .small()
                                .ghost()
                                .label(crate::i18n::text(label))
                                .selected(mode == selected)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.diff
                                        .update(cx, |state, cx| state.set_mode(selected, cx));
                                    cx.notify();
                                }))
                        }),
                    ),
            )
            .child(
                Button::new("diff-show-all")
                    .small()
                    .ghost()
                    .icon(IconName::List)
                    .label(crate::i18n::text(if self.show_context {
                        "Hide context"
                    } else {
                        "Show context"
                    }))
                    .selected(self.show_context)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.show_context = !view.show_context;
                        view.diff.update(cx, |state, cx| {
                            state.set_context_lines(
                                if view.show_context { None } else { Some(2) },
                                cx,
                            )
                        });
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                h_flex()
                    .gap_0p5()
                    .items_center()
                    .child(
                        Button::new("diff-previous")
                            .small()
                            .ghost()
                            .icon(IconName::ChevronUp)
                            .disabled(count == 0)
                            .accessibility_label(crate::i18n::text("Previous change"))
                            .tooltip(crate::i18n::text("Previous change"))
                            .on_click(cx.listener(|view, _, _, cx| view.step_change(false, cx))),
                    )
                    .child(
                        div()
                            .id("diff-change-position")
                            .test_support()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} / {count}",
                                if count > 0 { self.change + 1 } else { 0 }
                            )),
                    )
                    .child(
                        Button::new("diff-next")
                            .small()
                            .ghost()
                            .icon(IconName::ChevronDown)
                            .disabled(count == 0)
                            .accessibility_label(crate::i18n::text("Next change"))
                            .tooltip(crate::i18n::text("Next change"))
                            .on_click(cx.listener(|view, _, _, cx| view.step_change(true, cx))),
                    ),
            )
            .into_any_element()
    }

    fn column_labels(&self, mode: DiffMode, cx: &App) -> AnyElement {
        let file = self.preview.manifest.is_string();
        let column = |name: &str, description: &str| {
            h_flex()
                .flex_1()
                .min_w_0()
                .justify_between()
                .items_center()
                .px_3()
                .py_2()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .child(name.to_owned()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::text(description)),
                )
        };
        h_flex()
            .w_full()
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(column(
                if mode == DiffMode::Unified {
                    "Current → Proposed"
                } else {
                    "Current"
                },
                if self.create {
                    "New resource"
                } else if file {
                    "Saved file"
                } else {
                    "Cluster YAML"
                },
            ))
            .when(mode == DiffMode::Split, |row| {
                row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .child(column("Proposed", "Your changes")),
                )
            })
            .into_any_element()
    }

    fn footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let label = self
            .confirmation_label
            .as_deref()
            .unwrap_or(if self.create {
                "Confirm & create"
            } else if self.force {
                "Confirm force apply"
            } else {
                "Confirm & apply"
            });
        let changes = self.preview.changes.len();
        h_flex()
            .w_full()
            .flex_shrink_0()
            .px_4()
            .py_3()
            .gap_3()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::GitCompareArrows).size_3())
                    .child(if changes == 0 {
                        "No changes to save".to_owned()
                    } else {
                        format!("{changes} change{}", if changes == 1 { "" } else { "s" })
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        Button::new("cancel-yaml-review")
                            .ghost()
                            .small()
                            .label(crate::i18n::text("Back to editing"))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.cancel(cx);
                                // Keep the emitter alive until its subscriber has handled the draft.
                                window.defer(cx, |window, cx| window.close_dialog(cx));
                            })),
                    )
                    .child(
                        Button::new("confirm-yaml-review")
                            .primary()
                            .small()
                            .icon(IconName::Check)
                            .label(crate::i18n::text(label))
                            .disabled(
                                !self.preview.changed
                                    || self.resolved
                                    || !self.confirmation_allowed,
                            )
                            .on_click(cx.listener(|view, _, window, cx| {
                                if view.confirm(cx) {
                                    window.defer(cx, |window, cx| window.close_dialog(cx));
                                }
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl Render for ReviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.diff.read(cx).mode();
        v_flex()
            .w_full()
            .h((window.viewport_size().height - px(96.)).min(px(760.)))
            .min_h_0()
            .child(self.header(cx))
            .child(self.toolbar(mode, cx))
            .child(self.column_labels(mode, cx))
            .child(
                div()
                    .id("yaml-review-diff")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        Diff::new(&self.diff)
                            .header_visible(false)
                            .hunk_separator(DiffHunkSeparator::Simple)
                            .soft_wrap(true)
                            .size_full()
                            .border_0(),
                    ),
            )
            .child(self.footer(cx))
    }
}
