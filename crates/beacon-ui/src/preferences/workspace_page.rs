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
                .accessibility_label(crate::i18n::text(label))
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
            .child(self.container_layout_form(horizontal, available, card_width, cx))
            .into_any_element()
    }

    fn container_layout_form(
        &self,
        horizontal: bool,
        available: Pixels,
        card_width: Pixels,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected_layout = settings::store(cx).read(cx).preferences.container_layout;
        let cards = [
            (
                ContainerLayout::Workbench,
                "container-layout-workbench",
                "Container workbench",
                "Switch between ports, health checks, environment and mounts.",
            ),
            (
                ContainerLayout::Grouped,
                "container-layout-grouped",
                "Grouped sections",
                "Scan ports and probes together; expand other configuration as needed.",
            ),
        ]
        .into_iter()
        .map(|(layout, id, title, description)| {
            let selected = selected_layout == layout;
            Button::new(id)
                .outline()
                .selected(selected)
                .w(card_width)
                .min_w_0()
                .h_auto()
                .p_3()
                .rounded_lg()
                .accessibility_label(crate::i18n::text(title))
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
                        .child(Self::container_layout_thumbnail(layout, cx))
                        .child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(crate::i18n::text(title)),
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
                    view.message = settings::save_container_layout(layout, cx)
                        .err()
                        .map(|error| (true, crate::i18n::text(error).to_string()));
                    cx.notify();
                }))
        });
        v_flex()
            .w(available)
            .min_w_0()
            .gap_3()
            .mt_5()
            .pt_5()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(heading("Container details"))
            .child(hint(
                "Choose the container layout for Pods and workload templates.",
                cx,
            ))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_stretch()
                    .gap_3()
                    .when(!horizontal, |row| row.flex_col())
                    .children(cards),
            )
            .child(hint(
                "Container layout saves automatically and applies immediately to all windows.",
                cx,
            ))
            .into_any_element()
    }

    fn container_layout_thumbnail(layout: ContainerLayout, cx: &App) -> AnyElement {
        let line = |width: f32| {
            div()
                .h(px(5.))
                .w(relative(width))
                .rounded_sm()
                .bg(cx.theme().muted_foreground.opacity(0.22))
        };
        let row = |label: &'static str| {
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .py_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .child(crate::i18n::text(label)),
                )
                .child(
                    Icon::new(IconName::ChevronRight)
                        .size_3()
                        .text_color(cx.theme().muted_foreground),
                )
        };
        v_flex()
            .w_full()
            .h(px(160.))
            .p_2()
            .gap_2()
            .rounded_md()
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .items_center()
                    .gap_1p5()
                    .text_xs()
                    .child(Icon::new(IconName::Box).size_3())
                    .child("main"),
            )
            .when(layout == ContainerLayout::Workbench, |this| {
                this.child(h_flex().w_full().gap_1().children((0..4).map(|index| {
                    div().flex_1().h(px(13.)).rounded_sm().bg(if index == 0 {
                        cx.theme().muted_foreground.opacity(0.25)
                    } else {
                        cx.theme().muted
                    })
                })))
                .child(row("Ports & resources"))
                .child(line(0.8))
                .child(line(0.6))
                .child(line(0.7))
            })
            .when(layout == ContainerLayout::Grouped, |this| {
                this.child(row("Ports"))
                    .child(line(0.8))
                    .child(row("Health checks"))
                    .child(line(0.6))
            })
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

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;

    #[::core::prelude::v1::test]
    fn container_layout_cards_save_immediately_without_committing_other_drafts() {
        let directory = tempfile::tempdir().unwrap();
        let cx = &mut support::context();
        support::workspace(cx, directory.path());
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(880.), px(1700.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|cx| PreferencesView::new(None, window, cx))
                },
            )
            .unwrap()
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("settings-workspace", cx);
            view.update(cx, |view, cx| {
                view.proxy.url.update(cx, |input, cx| {
                    input.set_value("http://draft.example:8080", window, cx)
                });
                view.context_display = ContextDisplay::SourceLabels;
                view.dirty = true;
            });
            window.render_frame(cx);
            assert_eq!(
                window.find("container-layout-workbench").label(),
                Some("Container workbench")
            );
            window.click("container-layout-grouped", cx);
            assert_eq!(
                settings::store(cx).read(cx).preferences.container_layout,
                ContainerLayout::Grouped
            );
            assert_eq!(
                settings::store(cx).read(cx).preferences.context_display,
                ContextDisplay::SourceGroups
            );
            assert!(view.read(cx).dirty);
            assert_eq!(
                value(&view.read(cx).proxy.url, cx),
                "http://draft.example:8080"
            );
            let saved: settings::Preferences = serde_json::from_slice(
                &std::fs::read(directory.path().join("settings.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(saved.container_layout, ContainerLayout::Grouped);
            assert_eq!(saved.context_display, ContextDisplay::SourceGroups);
            // A failed write must keep the active layout and the other drafts.
            let blocked = directory.path().join("not-a-directory");
            std::fs::write(&blocked, "blocked").unwrap();
            settings::store(cx).update(cx, |state, _| state.directory = blocked);
            window.click("container-layout-workbench", cx);
            assert_eq!(
                settings::store(cx).read(cx).preferences.container_layout,
                ContainerLayout::Grouped
            );
            assert!(
                view.read(cx)
                    .message
                    .as_ref()
                    .is_some_and(|(error, _)| *error)
            );
            settings::store(cx)
                .update(cx, |state, _| state.directory = directory.path().to_owned());
            window.click("container-layout-workbench", cx);
            window.click("save-preferences", cx);
            assert!(!view.read(cx).dirty);
            assert_eq!(
                settings::store(cx).read(cx).preferences.container_layout,
                ContainerLayout::Workbench
            );
            assert_eq!(
                settings::store(cx).read(cx).preferences.context_display,
                ContextDisplay::SourceLabels
            );
            window.remove_window();
        })
        .unwrap();
    }
}
