//! Compact controls shared by single and split Pod workspaces.
use super::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    Selectable as _,
    menu::{DropdownMenu as _, PopupMenuItem},
};

impl PodToolsView {
    pub(super) fn workspace_log_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let levels = self.log_workbench.read(cx).filter(cx).levels;
        let label = match levels.len() {
            0 => crate::i18n::text("All levels"),
            1 => crate::i18n::text(levels.first().unwrap()),
            _ => crate::i18n::message(
                "pod_sessions.level_count",
                &[("count", levels.len().to_string())],
            ),
        };
        let workbench = self.log_workbench.downgrade();
        h_flex()
            .id("pod-log-toolbar")
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex_wrap()
            .gap_1()
            .px_2()
            .py_1()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div().w(px(135.)).min_w_0().child(
                    Select::new(&self.container_picker)
                        .id("pod-log-container")
                        .xsmall()
                        .w_full()
                        .menu_width(px(260.))
                        .title_prefix(crate::i18n::text("Container: "))
                        .placeholder(crate::i18n::text("Select container"))
                        .accessibility_label(crate::i18n::text("Log container"))
                        .search_placeholder(crate::i18n::text("Search containers"))
                        .disabled(self.container_groups.is_empty()),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(100.))
                    .child(Input::new(&self.log_search).id("pod-log-search").xsmall()),
            )
            .child(
                Button::new("pod-log-levels")
                    .xsmall()
                    .ghost()
                    .label(label)
                    .dropdown_menu(move |mut menu, _, _| {
                        let all = workbench.clone();
                        menu = menu.item(
                            PopupMenuItem::new(crate::i18n::text("All levels"))
                                .checked(levels.is_empty())
                                .on_click(move |_, _, cx| {
                                    let _ = all.update(cx, |view, cx| view.set_level(None, cx));
                                }),
                        );
                        for level in crate::pod_logs::Level::ALL {
                            let label = level.label();
                            let owner = workbench.clone();
                            menu = menu.item(
                                PopupMenuItem::new(crate::i18n::text(label))
                                    .checked(levels.contains(label))
                                    .on_click(move |_, _, cx| {
                                        let _ = owner
                                            .update(cx, |view, cx| view.set_level(Some(label), cx));
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .child(
                Button::new("pause-log-follow")
                    .xsmall()
                    .ghost()
                    .selected(!self.log_follow)
                    .toggled(!self.log_follow)
                    .icon(if self.log_follow {
                        IconName::Pause
                    } else {
                        IconName::Play
                    })
                    .accessibility_label(crate::i18n::text(if self.log_follow {
                        "Pause follow"
                    } else {
                        "Resume follow"
                    }))
                    .tooltip(crate::i18n::text(if self.log_follow {
                        "Pause follow"
                    } else {
                        "Resume follow"
                    }))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.set_log_follow(!view.log_follow);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("pod-log-filters")
                    .xsmall()
                    .ghost()
                    .selected(self.log_advanced)
                    .toggled(self.log_advanced)
                    .icon(IconName::SlidersHorizontal)
                    .accessibility_label(crate::i18n::text("Advanced log controls"))
                    .tooltip(crate::i18n::text("Advanced log controls"))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.log_advanced = !view.log_advanced;
                        cx.notify();
                    })),
            )
            .child(self.workspace_log_menu(cx))
    }

    fn workspace_log_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        let wrap = self.log_wrap;
        let previous = self.log_options.previous;
        let timestamps = self.log_options.timestamps;
        let disabled = self.download_active()
            || self.log_options.container.is_none()
            || self.log_status == LogStatus::Waiting;
        let cancelling = matches!(self.download, Download::Running(_));
        Button::new("pod-log-options")
            .xsmall()
            .ghost()
            .icon(IconName::Ellipsis)
            .accessibility_label(crate::i18n::text("Log display and export"))
            .tooltip(crate::i18n::text("Log display and export"))
            .dropdown_menu(move |menu, _, _| {
                let wrapping = owner.clone();
                let old = owner.clone();
                let times = owner.clone();
                let download = owner.clone();
                let copy = owner.clone();
                let cancel = owner.clone();
                menu.item(
                    PopupMenuItem::new(crate::i18n::text("Wrap lines"))
                        .checked(wrap)
                        .on_click(move |_, _, cx| {
                            let _ = wrapping.update(cx, |view, cx| view.toggle_log_wrap(cx));
                        }),
                )
                .item(
                    PopupMenuItem::new(crate::i18n::text("Previous"))
                        .checked(previous)
                        .on_click(move |_, window, cx| {
                            let _ = old.update(cx, |view, cx| {
                                view.set_log_options(
                                    LogOptions {
                                        previous: !view.log_options.previous,
                                        ..view.log_options.clone()
                                    },
                                    window,
                                    cx,
                                )
                            });
                        }),
                )
                .item(
                    PopupMenuItem::new(crate::i18n::text("Timestamps"))
                        .checked(timestamps)
                        .on_click(move |_, window, cx| {
                            let _ = times.update(cx, |view, cx| {
                                view.set_log_options(
                                    LogOptions {
                                        timestamps: !view.log_options.timestamps,
                                        ..view.log_options.clone()
                                    },
                                    window,
                                    cx,
                                )
                            });
                        }),
                )
                .separator()
                .item(
                    PopupMenuItem::new(crate::i18n::text("Copy matching logs")).on_click(
                        move |_, _, cx| {
                            let _ = copy.update(cx, |view, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    view.log_display.borrow().text(),
                                ))
                            });
                        },
                    ),
                )
                .item(
                    PopupMenuItem::new(crate::i18n::text("Export all…"))
                        .disabled(disabled)
                        .on_click(move |_, window, cx| {
                            let _ = download.update(cx, |view, cx| view.download_logs(window, cx));
                        }),
                )
                .when(cancelling, |menu| {
                    menu.item(
                        PopupMenuItem::new(crate::i18n::text("Cancel log export")).on_click(
                            move |_, _, cx| {
                                let _ = cancel.update(cx, |view, cx| {
                                    if let Some(abort) = &view.download_abort {
                                        abort.abort();
                                        view.download = Download::Cancelling;
                                        cx.notify();
                                    }
                                });
                            },
                        ),
                    )
                })
            })
    }

    pub(super) fn workspace_log_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.log_display.borrow().rows.len().to_string();
        let total = self.logs.len().to_string();
        let dropped = self.logs.dropped();
        let text = crate::i18n::message(
            match (dropped, &self.log_status) {
                (0, LogStatus::Ended) => "messages.count_total_lines_ended",
                (0, _) => "messages.count_total_lines",
                _ => "messages.count_total_lines_dropped_older_dropped",
            },
            &[
                ("count", count),
                ("total", total),
                ("dropped", dropped.to_string()),
            ],
        );
        h_flex()
            .id("pod-log-footer")
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .px_2()
            .py_1()
            .gap_2()
            .items_center()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(div().flex_1().min_w_0().truncate().child(text))
            .when(!self.log_follow, |bar| {
                bar.child(crate::i18n::text("Paused"))
            })
            .child(
                Button::new("latest-log-line")
                    .xsmall()
                    .ghost()
                    .label(crate::i18n::text("Latest"))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.set_log_follow(true);
                        cx.notify();
                    })),
            )
    }
}
