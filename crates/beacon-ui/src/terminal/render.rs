//! A cell-aligned GPUI view over Alacritty's screen and history.
use super::*;
use alacritty_terminal::{grid::Scroll, vte::ansi::CursorShape};
use gpui_kit::assets::IconName;
use gpui_kit::base::Selectable as _;
use gpui_kit::component::{Disableable as _, Icon, input::Escape, spinner::Spinner};

impl TerminalView {
    fn session_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (title, icon, tone) = match &self.state {
            State::Connecting => ("Connecting…", IconName::LoaderCircle, Tone::Progressing),
            State::Ended => (
                if self.container_stopped {
                    "Debug container stopped"
                } else {
                    "Session ended"
                },
                IconName::Terminal,
                Tone::Unknown,
            ),
            State::Failed(_) => ("Connection failed", IconName::CircleX, Tone::Critical),
            _ => return None,
        };
        let stopped = !self.is_active() && !self.container_stopped;
        Some(
            v_flex()
                .id("terminal-session-status")
                .aria_label(crate::i18n::text(title))
                .test_support()
                .w_full()
                .flex_shrink_0()
                .border_t_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.3))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .px_3()
                        .py_2()
                        .gap_3()
                        .items_center()
                        .child(
                            div()
                                .size(px(28.))
                                .flex_shrink_0()
                                .rounded_md()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(cx.theme().muted)
                                .child(if self.is_active() {
                                    Spinner::new()
                                        .small()
                                        .color(cx.theme().tone(tone))
                                        .into_any_element()
                                } else {
                                    Icon::new(icon)
                                        .size_4()
                                        .text_color(cx.theme().tone(tone))
                                        .into_any_element()
                                }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .when(matches!(self.state, State::Failed(_)), |this| {
                                    this.text_color(cx.theme().tone(tone))
                                })
                                .child(crate::i18n::text(title)),
                        )
                        .when(stopped, |this| {
                            this.child(
                                Button::new("terminal-shell-options")
                                    .small()
                                    .ghost()
                                    .icon(IconName::SlidersHorizontal)
                                    .selected(self.launcher_options)
                                    .accessibility_label(crate::i18n::text("Shell options"))
                                    .tooltip(crate::i18n::text("Shell options"))
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.launcher_options = !view.launcher_options;
                                        if view.launcher_options {
                                            view.command.focus_handle(cx).focus(window, cx);
                                        } else {
                                            view.focus.focus(window, cx);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("start-shell")
                                    .small()
                                    .primary()
                                    .icon(IconName::RefreshCw)
                                    .label(crate::i18n::text("Reconnect"))
                                    .on_click(
                                        cx.listener(|view, _, window, cx| view.start(window, cx)),
                                    ),
                            )
                        }),
                )
                .when_some(
                    match &self.state {
                        State::Failed(error) => Some(error.clone()),
                        _ => None,
                    },
                    |this, error| {
                        this.child(
                            div()
                                .id("terminal-session-error")
                                .test_support()
                                .px_3()
                                .pb_2()
                                .max_h(px(96.))
                                .overflow_y_scroll()
                                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::copyable_text::copyable_text("shell-status", error)),
                        )
                    },
                )
                .when(stopped && self.launcher_options, |this| {
                    this.child(
                        h_flex()
                            .id("terminal-shell-command")
                            .test_support()
                            .w_full()
                            .min_w_0()
                            .gap_3()
                            .px_3()
                            .pb_2()
                            .items_center()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(crate::i18n::text("Shell command")),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&self.command).small()),
                            ),
                    )
                })
                .into_any_element(),
        )
    }

    fn measure_font(&mut self, window: &mut Window, cx: &App) -> (SharedString, f32) {
        let family = cx.theme().mono_font_family.clone();
        let font_size = (cx.theme().mono_font_size.as_f32() + self.font_zoom).clamp(8., 32.);
        let text: SharedString = "M".into();
        let font = Font {
            family: family.clone(),
            ..window.text_style().font()
        };
        let width = window
            .text_system()
            .shape_line(
                text,
                px(font_size),
                &[TextRun {
                    len: 1,
                    font,
                    color: cx.theme().foreground,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            )
            .width
            .as_f32()
            .max(1.);
        self.metrics = Metrics {
            width,
            height: (font_size * 1.4).ceil(),
        };
        (family, font_size)
    }

    fn toolbar(&self, font_size: f32, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("terminal-find")
                    .ghost()
                    .small()
                    .icon(IconName::Search)
                    .tooltip(crate::i18n::text("Find in terminal"))
                    .on_click(cx.listener(|view, _, window, cx| view.open_search(window, cx))),
            )
            .child(
                Button::new("terminal-copy")
                    .ghost()
                    .small()
                    .icon(IconName::Copy)
                    .disabled(
                        self.term
                            .selection_to_string()
                            .is_none_or(|text| text.is_empty()),
                    )
                    .tooltip(crate::i18n::text("Copy selection"))
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.copy_selection(cx);
                        view.focus.focus(window, cx);
                    })),
            )
            .when(self.term.grid().display_offset() > 0, |this| {
                this.child(
                    Button::new("terminal-bottom")
                        .ghost()
                        .small()
                        .icon(IconName::ChevronDown)
                        .label(crate::i18n::text("Back to bottom"))
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.scroll(Scroll::Bottom, cx);
                            view.focus.focus(window, cx);
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                Button::new("terminal-zoom-out")
                    .ghost()
                    .small()
                    .label("−")
                    .tooltip(crate::i18n::text("Decrease terminal font size"))
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.zoom(-1., cx);
                        view.focus.focus(window, cx);
                    })),
            )
            .child(
                Button::new("terminal-zoom-reset")
                    .ghost()
                    .small()
                    .label(format!("{font_size:.0}"))
                    .tooltip(crate::i18n::text("Reset terminal font size"))
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.font_zoom = 0.;
                        view.focus.focus(window, cx);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("terminal-zoom-in")
                    .ghost()
                    .small()
                    .label("+")
                    .tooltip(crate::i18n::text("Increase terminal font size"))
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.zoom(1., cx);
                        view.focus.focus(window, cx);
                    })),
            )
    }

    fn search_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.search.matches.len();
        let index = self.search.current.map_or(0, |index| index + 1);
        h_flex()
            .w_full()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .capture_action(cx.listener(|view, _: &Escape, window, cx| {
                view.close_search(window, cx);
                cx.stop_propagation();
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.search_input).small()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .px_2()
                    .child(format!("{index} / {count}")),
            )
            .child(
                Button::new("terminal-search-prev")
                    .small()
                    .ghost()
                    .icon(IconName::ChevronUp)
                    .disabled(count == 0)
                    .tooltip(crate::i18n::text("Previous match"))
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.find(true, cx);
                        view.search_input.focus_handle(cx).focus(window, cx);
                    })),
            )
            .child(
                Button::new("terminal-search-next")
                    .small()
                    .ghost()
                    .icon(IconName::ChevronDown)
                    .disabled(count == 0)
                    .tooltip(crate::i18n::text("Next match"))
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.find(false, cx);
                        view.search_input.focus_handle(cx).focus(window, cx);
                    })),
            )
            .child(
                Button::new("terminal-search-close")
                    .small()
                    .ghost()
                    .icon(IconName::Close)
                    .tooltip(crate::i18n::text("Close search"))
                    .on_click(cx.listener(|view, _, window, cx| view.close_search(window, cx))),
            )
    }

    fn scrollbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let history = self.term.grid().history_size();
        let height = self.grid_bounds.get().size.height.as_f32().max(1.);
        let thumb = (height * self.term.screen_lines() as f32 / self.term.total_lines() as f32)
            .max(20.)
            .min(height);
        let fraction = if history == 0 {
            1.
        } else {
            1. - self.term.grid().display_offset() as f32 / history as f32
        };
        div()
            .id("terminal-scrollbar")
            .test_support()
            .absolute()
            .right(px(2.))
            .top(px(8.))
            .bottom(px(8.))
            .w(px(8.))
            .when(history == 0, |this| this.invisible())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, event: &MouseDownEvent, _, cx| {
                    let y = (event.position.y - view.grid_bounds.get().origin.y).as_f32();
                    let top = fraction * (height - thumb);
                    if y < top || y > top + thumb {
                        let fraction = ((y - thumb / 2.) / (height - thumb).max(1.)).clamp(0., 1.);
                        let target = ((1. - fraction) * history as f32).round() as i32;
                        let current = view.term.grid().display_offset() as i32;
                        view.scroll(Scroll::Delta(target - current), cx);
                    }
                    view.scrollbar_drag =
                        Some((event.position.y.as_f32(), view.term.grid().display_offset()));
                    cx.stop_propagation();
                }),
            )
            .child(
                div()
                    .absolute()
                    .top(px(fraction * (height - thumb)))
                    .w_full()
                    .h(px(thumb))
                    .rounded_full()
                    .bg(cx.theme().muted_foreground.opacity(0.4)),
            )
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (family, font_size) = self.measure_font(window, cx);
        if let Some((columns, rows)) = self.measured.take()
            && self.fit(columns, rows)
        {
            cx.notify();
        }
        self.refresh_search();
        if matches!(self.state, State::Idle) {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::text("An interactive shell in this container.")),
                )
                .child(self.render_launcher("Open a shell", cx))
                .into_any_element();
        }
        let rows = self.rows(cx);
        let output = rows
            .iter()
            .map(|runs| {
                runs.iter()
                    .map(|run| run.text.as_str())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let selected = self
            .term
            .selection_to_string()
            .filter(|text| !text.is_empty());
        let can_paste = matches!(self.state, State::Running) && self.shell.is_some();
        let paste_focus = self.focus.clone();
        let metrics = self.metrics;
        let cursor = self.term.renderable_content().cursor;
        let offset = self.term.grid().display_offset();
        let show_cursor = matches!(self.state, State::Running)
            && offset == 0
            && self.focus.is_focused(window)
            && cursor.shape != CursorShape::Hidden;

        v_flex()
            .size_full()
            .min_h_0()
            .track_focus(&self.focus)
            .key_context("BeaconTerminal")
            .on_action(cx.listener(|view, _: &Paste, window, cx| {
                if view.focus.is_focused(window) {
                    view.paste(cx);
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|view, _: &Copy, window, cx| {
                if view.focus.is_focused(window) {
                    view.copy_selection(cx);
                    cx.stop_propagation();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|view, _: &Find, window, cx| view.open_search(window, cx)))
            .on_action(cx.listener(|view, _: &ZoomIn, _, cx| view.zoom(1., cx)))
            .on_action(cx.listener(|view, _: &ZoomOut, _, cx| view.zoom(-1., cx)))
            .on_action(cx.listener(|view, _: &ResetZoom, _, cx| {
                view.font_zoom = 0.;
                cx.notify();
            }))
            .on_action(cx.listener(|view, _: &HistoryUp, _, cx| view.scroll(Scroll::PageUp, cx)))
            .on_action(
                cx.listener(|view, _: &HistoryDown, _, cx| view.scroll(Scroll::PageDown, cx)),
            )
            .on_action(
                cx.listener(|view, _: &HistoryBottom, _, cx| view.scroll(Scroll::Bottom, cx)),
            )
            .on_action(cx.listener(|view, _: &F1, window, cx| {
                if view.focus.is_focused(window) {
                    view.key(&Keystroke::parse("f1").unwrap(), cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(
                cx.listener(|view, _: &gpui_kit::base::actions::Confirm, window, cx| {
                    if view.focus.is_focused(window) {
                        if view.composition.text.is_empty() {
                            view.key(&Keystroke::parse("enter").unwrap(), cx);
                        }
                        cx.stop_propagation();
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .on_action(
                cx.listener(|view, _: &gpui_kit::base::actions::Cancel, window, cx| {
                    if view.focus.is_focused(window) {
                        if view.composition.text.is_empty() {
                            view.key(&Keystroke::parse("escape").unwrap(), cx);
                        } else {
                            view.composition.clear();
                            cx.notify();
                        }
                        cx.stop_propagation();
                    } else {
                        cx.propagate();
                    }
                }),
            )
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if !view.focus.is_focused(window)
                    || input::text_key(&event.keystroke)
                    || !view.composition.text.is_empty()
                {
                    return;
                }
                if encode(
                    &event.keystroke,
                    view.term.mode().contains(TermMode::APP_CURSOR),
                )
                .is_some()
                {
                    view.key(&event.keystroke, cx);
                    cx.stop_propagation();
                }
            }))
            .child(self.toolbar(font_size, cx))
            .when(self.search.open, |this| this.child(self.search_bar(cx)))
            .child(
                div()
                    .id("terminal-grid")
                    .test_support()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .bg(cx.theme().background)
                    .cursor_text()
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
                    .on_scroll_wheel(cx.listener(|view, event, _, cx| view.wheel(event, cx)))
                    .child({
                        let measured = self.measured.clone();
                        let grid_bounds = self.grid_bounds.clone();
                        let entity_id = cx.entity_id();
                        let current = (self.term.columns(), self.term.screen_lines());
                        let entity = cx.entity();
                        let focus = self.focus.clone();
                        canvas(
                            move |bounds, _, cx| {
                                let content = Bounds::new(
                                    bounds.origin + point(px(8.), px(8.)),
                                    size(
                                        (bounds.size.width - px(24.)).max(px(1.)),
                                        (bounds.size.height - px(16.)).max(px(1.)),
                                    ),
                                );
                                grid_bounds.set(content);
                                let size = (
                                    (content.size.width.as_f32() / metrics.width) as usize,
                                    (content.size.height.as_f32() / metrics.height) as usize,
                                );
                                let size = (size.0.max(2), size.1.max(2));
                                if size != current {
                                    measured.set(Some(size));
                                    cx.notify(entity_id);
                                }
                                content
                            },
                            move |_, content, window, cx| {
                                window.handle_input(
                                    &focus,
                                    ElementInputHandler::new(content, entity.clone()),
                                    cx,
                                );
                                let weak = entity.downgrade();
                                window.on_mouse_event(
                                    move |event: &MouseDownEvent, phase, window, cx| {
                                        if phase == DispatchPhase::Capture
                                            && event.button == MouseButton::Right
                                        {
                                            let _ = weak.update(cx, |view, cx| {
                                                if view.focus.is_focused(window)
                                                    && view.mouse_mode(event.modifiers)
                                                    && view
                                                        .grid_bounds
                                                        .get()
                                                        .contains(&event.position)
                                                {
                                                    view.mouse_down(event, window, cx);
                                                }
                                            });
                                        }
                                    },
                                );
                                let weak = entity.downgrade();
                                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                                    if phase == DispatchPhase::Capture {
                                        let _ = weak.update(cx, |view, cx| {
                                            if view.selecting
                                                || view.scrollbar_drag.is_some()
                                                || view.mouse_button.is_some()
                                            {
                                                view.mouse_up(event, cx);
                                            }
                                        });
                                    }
                                });
                                let weak = entity.downgrade();
                                window.on_mouse_event(
                                    move |event: &MouseMoveEvent, phase, window, cx| {
                                        if phase == DispatchPhase::Capture {
                                            let _ = weak.update(cx, |view, cx| {
                                                if view.selecting
                                                    || view.scrollbar_drag.is_some()
                                                    || view.mouse_button.is_some()
                                                    || view.focus.is_focused(window)
                                                {
                                                    view.mouse_move(event, cx);
                                                }
                                            });
                                        }
                                    },
                                );
                            },
                        )
                        .absolute()
                        .size_full()
                    })
                    .children(rows.into_iter().enumerate().flat_map(|(row, runs)| {
                        let family = family.clone();
                        runs.into_iter().map(move |run| {
                            div()
                                .absolute()
                                .left(px(8. + run.column as f32 * metrics.width))
                                .top(px(8. + row as f32 * metrics.height))
                                .w(px(run.cells as f32 * metrics.width))
                                .h(px(metrics.height))
                                .overflow_hidden()
                                .font_family(family.clone())
                                .whitespace_nowrap()
                                .text_size(px(font_size))
                                .line_height(px(metrics.height))
                                .text_color(run.fg)
                                .when_some(run.bg, |this, bg| this.bg(bg))
                                .when(run.bold, |this| this.font_weight(FontWeight::BOLD))
                                .child(run.text)
                        })
                    }))
                    .when(
                        show_cursor
                            && matches!(cursor.shape, CursorShape::Beam | CursorShape::Underline),
                        |this| {
                            this.child(
                                div()
                                    .absolute()
                                    .left(px(8. + cursor.point.column.0 as f32 * metrics.width))
                                    .top(px(8.
                                        + cursor.point.line.0.max(0) as f32 * metrics.height
                                        + if cursor.shape == CursorShape::Underline {
                                            metrics.height - 2.
                                        } else {
                                            0.
                                        }))
                                    .w(px(if cursor.shape == CursorShape::Beam {
                                        2.
                                    } else {
                                        metrics.width
                                    }))
                                    .h(px(if cursor.shape == CursorShape::Beam {
                                        metrics.height
                                    } else {
                                        2.
                                    }))
                                    .bg(cx.theme().foreground),
                            )
                        },
                    )
                    .when(!self.composition.text.is_empty(), |this| {
                        this.child(
                            div()
                                .absolute()
                                .left(px(8. + cursor.point.column.0 as f32 * metrics.width))
                                .top(px(8. + cursor.point.line.0.max(0) as f32 * metrics.height))
                                .font_family(family)
                                .whitespace_nowrap()
                                .text_size(px(font_size))
                                .line_height(px(metrics.height))
                                .bg(cx.theme().background)
                                .text_color(cx.theme().foreground)
                                .border_b_1()
                                .border_color(cx.theme().foreground)
                                .child(self.composition.text.clone()),
                        )
                    })
                    .child(self.scrollbar(cx))
                    .context_menu(move |menu, _, cx| {
                        let menu = menu.action_context(paste_focus.clone()).item(
                            PopupMenuItem::new(crate::i18n::text("Paste"))
                                .action(Box::new(Paste))
                                .disabled(
                                    !can_paste
                                        || cx
                                            .read_from_clipboard()
                                            .and_then(|item| item.text())
                                            .is_none_or(|text| text.is_empty()),
                                ),
                        );
                        let menu = if let Some(text) = &selected {
                            menu.item(crate::copyable_text::copy_item(
                                "Copy selection",
                                text.clone(),
                            ))
                        } else {
                            menu
                        };
                        menu.item(crate::copyable_text::copy_item(
                            "Copy terminal output",
                            output.clone(),
                        ))
                        .item(
                            PopupMenuItem::new(crate::i18n::text("Find in terminal"))
                                .action(Box::new(Find)),
                        )
                    }),
            )
            .children(self.session_status(cx))
            .into_any_element()
    }
}
