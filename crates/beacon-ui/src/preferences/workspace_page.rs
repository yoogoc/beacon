use super::*;

impl PreferencesView {
    pub(super) fn context_display_form(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let horizontal = window.bounds().size.width >= px(760.);
        let available = window.bounds().size.width - px(228.);
        let card_width = if horizontal {
            (available - px(12.)) / 2.
        } else {
            available
        };
        let cards = [
            (
                ContextDisplay::SourceGroups,
                "context-display-groups",
                "Source groups",
                "Group contexts under collapsible sources.",
            ),
            (
                ContextDisplay::SourceLabels,
                "context-display-labels",
                "Source labels",
                "Show each context's source in a flat list.",
            ),
        ]
        .into_iter()
        .map(|(mode, id, label, description)| {
            let selected = self.context_display == mode;
            Button::new(id)
                .outline()
                .selected(selected)
                .accessibility_label(label)
                .w(card_width)
                .min_w_0()
                .h_auto()
                .p_3()
                .rounded_lg()
                .bg(if selected {
                    cx.theme().muted
                } else {
                    cx.theme().background
                })
                .border_color(if selected {
                    cx.theme().primary
                } else {
                    cx.theme().border
                })
                .text_color(cx.theme().foreground)
                .child(
                    v_flex()
                        .w_full()
                        .gap_3()
                        .child(Self::context_display_thumbnail(mode, cx))
                        .child(
                            h_flex()
                                .w_full()
                                .gap_2()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(crate::i18n::text(label)),
                                )
                                .child(Icon::new(IconName::CircleCheck).size_4().text_color(
                                    if selected {
                                        cx.theme().primary
                                    } else {
                                        cx.theme().transparent
                                    },
                                )),
                        )
                        .child(
                            div()
                                .w_full()
                                .whitespace_normal()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::i18n::text(description)),
                        ),
                )
                .on_click(cx.listener(move |view, _, _, cx| {
                    if view.context_display != mode {
                        view.context_display = mode;
                        view.dirty = true;
                        view.message = None;
                        cx.notify();
                    }
                }))
        });
        v_flex().w(available).min_w_0().gap_3()
            .child(heading("Context sources"))
            .child(hint("Choose how you distinguish contexts from different kubeconfig sources.",cx))
            .child(div().w_full().flex().gap_3().items_stretch()
                .when(!horizontal,|row| row.flex_col()).children(cards))
            .child(v_flex().gap_2().mt_2()
                .child(hint("Source indicators appear when contexts come from more than one source.",cx))
                .child(hint("File names distinguish matching contexts from the same folder. Hover a context to see its full origin.",cx)))
            .into_any_element()
    }

    fn context_display_thumbnail(mode: ContextDisplay, cx: &App) -> AnyElement {
        let grouped = mode == ContextDisplay::SourceGroups;
        let source_header = |name: &'static str| {
            h_flex()
                .gap_1()
                .items_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::ChevronDown).size_3())
                .child(Icon::new(IconName::FileKey).size_3())
                .child(div().flex_1().child(name))
                .child("2")
        };
        let row = |name: &'static str, source: &'static str| {
            h_flex()
                .w_full()
                .min_w_0()
                .gap_1p5()
                .items_center()
                .text_xs()
                .child(Icon::new(IconName::ChevronRight).size_3())
                .child(crate::icons::kubernetes().size_3())
                .child(div().flex_1().min_w_0().truncate().child(name))
                .when(!grouped, |row| {
                    row.child(
                        div()
                            .px_1()
                            .rounded_sm()
                            .bg(cx.theme().muted)
                            .text_color(cx.theme().muted_foreground)
                            .child(source),
                    )
                })
        };
        v_flex()
            .w_full()
            .h(px(170.))
            .p_2()
            .gap_2()
            .rounded_md()
            .bg(cx.theme().tokens.sidebar)
            .border_1()
            .border_color(cx.theme().border)
            .when(grouped, |preview| {
                preview.child(source_header("System kubeconfig"))
            })
            .child(row("development", "System"))
            .child(row("production", "System"))
            .when(grouped, |preview| {
                preview.child(source_header("Platform team"))
            })
            .child(row("development", "Platform"))
            .child(row("production", "Platform"))
            .into_any_element()
    }
}
