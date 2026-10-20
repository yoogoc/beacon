use super::*;
use gpui_kit::component::{Icon, scroll::ScrollableElement as _, tooltip::Tooltip};

impl SourceEditor {
    pub(super) fn file_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let replacing = self.source.is_some();
        let list = v_flex()
            .id("source-file-list")
            .test_support()
            .min_w_0()
            .max_h(px(264.))
            .overflow_y_scroll()
            .track_scroll(&self.file_scroll)
            .vertical_scrollbar(&self.file_scroll)
            .children(self.files.iter().enumerate().map(|(index, file)| {
                let id = file.name.entity_id();
                let remove_id = file.id.clone();
                let path = file.path.display().to_string();
                let filename = file
                    .path
                    .file_name()
                    .unwrap_or(file.path.as_os_str())
                    .to_string_lossy()
                    .into_owned();
                let directory = file
                    .path
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .map(|path| path.display().to_string());
                h_flex()
                    .id(("source-file-row", id))
                    .test_support()
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .p_3()
                    .gap_3()
                    .items_center()
                    .when(index + 1 < self.files.len(), |row| {
                        row.border_b_1().border_color(cx.theme().border)
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .size(px(32.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(cx.theme().muted.opacity(0.65))
                            .child(
                                Icon::new(IconName::FileKey)
                                    .size_4()
                                    .text_color(cx.theme().muted_foreground),
                            ),
                    )
                    .child(
                        v_flex()
                            .id(("source-file-location", id))
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(copyable_text(("source-file-filename", id), filename)),
                            )
                            .when_some(directory, |column, directory| {
                                column.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .truncate()
                                        .child(copyable_text(
                                            ("source-file-directory", id),
                                            directory,
                                        )),
                                )
                            }),
                    )
                    .child(
                        div().w(px(160.)).flex_shrink_0().child(
                            Input::new(&file.name)
                                .id(("source-file-name", id))
                                .small()
                                .aria_label(crate::i18n::text("Source name")),
                        ),
                    )
                    .child(
                        Button::new(("remove-source-file", id))
                            .xsmall()
                            .ghost()
                            .icon(IconName::X)
                            .disabled(self.busy)
                            .tooltip(crate::i18n::text("Remove this file from the selection"))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.files.retain(|file| file.id != remove_id);
                                view.invalidate(cx);
                            })),
                    )
            }));
        v_flex()
            .gap_2()
            .min_w_0()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(crate::i18n::text("Kubeconfig files")),
                    )
                    .when(!self.files.is_empty(), |row| {
                        row.child(
                            div()
                                .id("source-file-count")
                                .test_support()
                                .px_1p5()
                                .rounded_sm()
                                .bg(cx.theme().muted)
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(self.files.len().to_string()),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("browse-kubeconfig")
                            .small()
                            .outline()
                            .icon(if replacing {
                                IconName::FolderOpen
                            } else {
                                IconName::Plus
                            })
                            .label(crate::i18n::text(if replacing {
                                "Replace file…"
                            } else {
                                "Add files…"
                            }))
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|view, _, window, cx| view.choose(false, window, cx)),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .id("source-file-panel")
                    .test_support()
                    .w_full()
                    .min_w_0()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .overflow_hidden()
                    // Keep wheel events inside this list at its scroll boundaries.
                    .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                    .when(self.files.is_empty(), |panel| {
                        panel.child(
                            v_flex()
                                .id("source-files-empty")
                                .test_support()
                                .h(px(144.))
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .child(
                                    Icon::new(IconName::Files)
                                        .size_6()
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .child(crate::i18n::text("Choose kubeconfig files")),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(crate::i18n::text(
                                            "Each file becomes a separate source.",
                                        )),
                                ),
                        )
                    })
                    .when(!self.files.is_empty(), |panel| {
                        panel
                            .child(
                                h_flex()
                                    .px_3()
                                    .py_2()
                                    .gap_3()
                                    .bg(cx.theme().muted.opacity(0.4))
                                    .border_b_1()
                                    .border_color(cx.theme().border)
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(div().flex_1().child(crate::i18n::text("File")))
                                    .child(
                                        div()
                                            .w(px(160.))
                                            .flex_shrink_0()
                                            .child(crate::i18n::text("Source name")),
                                    )
                                    .child(div().w(px(24.)).flex_shrink_0()),
                            )
                            .child(list)
                    }),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Link)
                            .size_3()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(crate::i18n::text("Original files stay in place.")),
                    )
                    .when(!self.manual_path, |row| {
                        row.child(
                            Button::new("enter-kubeconfig-path")
                                .xsmall()
                                .ghost()
                                .icon(IconName::TextCursorInput)
                                .label(crate::i18n::text("Enter a path…"))
                                .disabled(self.busy)
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.manual_path = true;
                                    view.path.read(cx).focus_handle(cx).focus(window, cx);
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .when(self.manual_path, |column| {
                column.child(
                    h_flex()
                        .id("source-manual-path")
                        .test_support()
                        .gap_2()
                        .items_center()
                        .on_action(|_: &gpui_kit::component::input::Enter, _, cx| {
                            cx.stop_propagation();
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&self.path).id("source-path-input").small()),
                        )
                        .child(
                            Button::new("add-kubeconfig-path")
                                .small()
                                .outline()
                                .label(crate::i18n::text(if replacing {
                                    "Replace file"
                                } else {
                                    "Add file"
                                }))
                                .disabled(self.busy || self.path.read(cx).value().trim().is_empty())
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.add_entered_path(window, cx)
                                })),
                        )
                        .child(
                            Button::new("cancel-kubeconfig-path")
                                .xsmall()
                                .ghost()
                                .icon(IconName::X)
                                .tooltip(crate::i18n::text("Cancel"))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.manual_path = false;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .into_any_element()
    }
}
