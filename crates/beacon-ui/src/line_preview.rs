//! One delayed, bounded, read-only preview per file/log pane. Row renderers only
//! retain shared raw text; parsing starts after the pointer has settled.
mod format;
mod text_lines;
pub(crate) use text_lines::TextLines;

use crate::i18n;
use format::Formatted;
use gpui_kit::assets::IconName;
use gpui_kit::base::{Align, ElementExt as _, Placement, Positioner, TestSupportExt as _};
use gpui_kit::component::{
    ActiveTheme as _, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{cell::Cell, rc::Rc, sync::Arc, time::Duration};

const OPEN_DELAY: Duration = Duration::from_millis(240);
const CLOSE_DELAY: Duration = Duration::from_millis(260);

struct Active {
    id: usize,
    bounds: Bounds<Pixels>,
    data: Arc<Formatted>,
}

pub(crate) struct LinePreview {
    active: Option<Active>,
    editor: Option<Entity<EditorState>>,
    pinned: bool,
    raw: bool,
    wrap: bool,
    copied: bool,
    details: Option<AnyView>,
    details_mode: bool,
    hovered: Option<usize>,
    inside: bool,
    epoch: u64,
    _open: Option<Task<()>>,
    _close: Option<Task<()>>,
}

impl LinePreview {
    pub fn new(cx: &mut App) -> Entity<Self> {
        cx.new(|_| Self {
            active: None,
            editor: None,
            pinned: false,
            raw: false,
            wrap: true,
            copied: false,
            details: None,
            details_mode: false,
            hovered: None,
            inside: false,
            epoch: 0,
            _open: None,
            _close: None,
        })
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.epoch += 1;
        self._open = None;
        self._close = None;
        self.active = None;
        self.editor = None;
        self.pinned = false;
        self.hovered = None;
        self.inside = false;
        self.copied = false;
        self.details = None;
        self.details_mode = false;
        cx.notify();
    }

    fn enter(
        &mut self,
        id: usize,
        raw: SharedString,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request(id, raw, bounds, false, window, cx);
    }

    pub fn show(
        &mut self,
        id: usize,
        raw: SharedString,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.raw = false;
        self.details_mode = false;
        self.copied = false;
        self.request(id, raw, bounds, true, window, cx);
    }
    pub fn set_details(&mut self, details: AnyView, cx: &mut Context<Self>) {
        self.details = Some(details);
        cx.notify();
    }

    fn request(
        &mut self,
        id: usize,
        raw: SharedString,
        bounds: Bounds<Pixels>,
        pin: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._close = None;
        self.hovered = Some(id);
        if self.pinned && !pin {
            return;
        }
        self._open = None;
        self.epoch += 1;
        let epoch = self.epoch;
        if let Some(active) = &mut self.active
            && active.id == id
            && active.data.raw == raw
        {
            active.bounds = bounds;
            if pin {
                self.pin(cx);
                self.install_editor(window, cx);
            }
            cx.notify();
            return;
        }
        self.details = None;
        self.details_mode = false;
        if pin {
            self.pinned = true;
            self.active = None;
            self.editor = None;
            cx.notify();
        }
        self._open = Some(cx.spawn_in(window, async move |this, cx| {
            if !pin {
                cx.background_executor().timer(OPEN_DELAY).await;
            }
            let parsed = cx
                .background_executor()
                .spawn(async move { Arc::new(Formatted::new(raw)) })
                .await;
            let _ = this.update_in(cx, |view, window, cx| {
                if view.epoch != epoch || (!pin && (view.pinned || view.hovered != Some(id))) {
                    return;
                }
                view.active = Some(Active {
                    id,
                    bounds,
                    data: parsed,
                });
                view.copied = false;
                view.install_editor(window, cx);
                cx.notify();
            });
        }));
    }

    fn leave(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.hovered != Some(id) {
            return;
        }
        self.hovered = None;
        if self.pinned {
            return;
        }
        self._open = None;
        self.epoch += 1;
        self.schedule_close(window, cx);
    }

    fn schedule_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pinned || self.inside || self.hovered.is_some() {
            return;
        }
        self._close = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(CLOSE_DELAY).await;
            let _ = this.update_in(cx, |view, _, cx| {
                if !view.pinned && !view.inside && view.hovered.is_none() {
                    view.close(cx);
                }
            });
        }));
    }

    fn pin(&mut self, cx: &mut Context<Self>) {
        if self.pinned {
            return;
        }
        self.pinned = true;
        self.epoch += 1;
        self._open = None;
        self._close = None;
        cx.notify();
    }

    fn install_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = &self.active else {
            return;
        };
        let value = active.data.display(self.raw);
        let language = if self.raw {
            "text"
        } else {
            active.data.format.language()
        };
        self.editor = Some(cx.new(|cx| {
            let mut editor = EditorState::new(window, cx)
                .language(language)
                .soft_wrap(self.wrap)
                .auto_close(false)
                .smart_indent(false)
                .folding(true);
            editor.set_value(value, window, cx);
            editor
        }));
    }
}

/// The controller is outside the virtual list, so a pinned snapshot survives
/// source-row eviction. Text selection on the original row is left untouched.
pub(crate) fn trigger<E: IntoElement + 'static>(
    controller: &Entity<LinePreview>,
    id: usize,
    raw: SharedString,
    child: E,
    cx: &App,
) -> impl IntoElement + Styled + 'static + use<E> {
    let owner = controller.downgrade();
    let selection_owner = owner.clone();
    let bounds = Rc::new(Cell::new(Bounds::default()));
    let measured = bounds.clone();
    let hover = cx.theme().muted.opacity(0.3);
    div()
        .id(("line-preview-trigger", id))
        .test_support()
        .min_w_0()
        .hover(move |row| row.bg(hover))
        .on_prepaint(move |rect, _, _| measured.set(rect))
        .capture_any_mouse_down(move |_, _, cx| {
            let _ = selection_owner.update(cx, |view, cx| {
                if !view.pinned {
                    view.close(cx);
                }
            });
        })
        .on_hover(move |hovered, window, cx| {
            let _ = owner.update(cx, |view, cx| {
                if *hovered {
                    view.enter(id, raw.clone(), bounds.get(), window, cx);
                } else {
                    view.leave(id, window, cx);
                }
            });
        })
        .child(child)
}

impl Render for LinePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(active) = &self.active else {
            return div().into_any_element();
        };
        let bounds = active.bounds;
        let id = active.id;
        let data = active.data.clone();
        let width = px(520.).min((window.viewport_size().width - px(16.)).max(px(200.)));
        let height = px(460.).min((window.viewport_size().height - px(16.)).max(px(180.)));
        let weak = cx.entity().downgrade();
        let caption = i18n::message("messages.line", &[("value0", (id + 1).to_string())]);
        let surface = v_flex()
            .id("line-preview")
            .test_support()
            .w(width)
            .h(height)
            .min_size_0()
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .shadow_lg()
            .overflow_hidden()
            .on_hover(cx.listener(|view, inside, window, cx| {
                view.inside = *inside;
                if *inside {
                    view._close = None;
                    if !view.pinned {
                        view._open = None;
                        view.epoch += 1;
                    }
                } else {
                    view.schedule_close(window, cx);
                }
            }))
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .items_center()
                    .flex_shrink_0()
                    .child(div().text_sm().child(caption))
                    .child(
                        div()
                            .id("line-preview-format")
                            .test_support()
                            .aria_label(i18n::text(data.format.label()))
                            .px_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(i18n::text(data.format.label())),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("line-preview-wrap")
                            .xsmall()
                            .ghost()
                            .icon(IconName::TextWrap)
                            .selected(self.wrap)
                            .accessibility_label(i18n::text("Wrap lines"))
                            .tooltip(i18n::text("Wrap lines"))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.wrap = !view.wrap;
                                if let Some(editor) = &view.editor {
                                    editor.update(cx, |state, cx| {
                                        state.set_soft_wrap(view.wrap, window, cx)
                                    });
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("line-preview-pin")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Pin)
                            .selected(self.pinned)
                            .accessibility_label(i18n::text(if self.pinned {
                                "Unpin preview"
                            } else {
                                "Pin preview"
                            }))
                            .tooltip(i18n::text(if self.pinned {
                                "Unpin preview"
                            } else {
                                "Pin preview"
                            }))
                            .on_click(cx.listener(|view, _, _, cx| {
                                if view.pinned {
                                    view.pinned = false;
                                    cx.notify();
                                } else {
                                    view.pin(cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("line-preview-copy")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Copy)
                            .accessibility_label(i18n::text("Copy"))
                            .tooltip(i18n::text(if self.raw {
                                "Copy raw line"
                            } else {
                                "Copy formatted line"
                            }))
                            .on_click(cx.listener(|view, _, _, cx| {
                                if let Some(active) = &view.active {
                                    let text = if view.raw {
                                        &active.data.raw
                                    } else {
                                        &active.data.pretty
                                    };
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        text.to_string(),
                                    ));
                                    view.copied = true;
                                }
                                view.pin(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("line-preview-close")
                            .xsmall()
                            .ghost()
                            .icon(IconName::X)
                            .accessibility_label(i18n::text("Close preview"))
                            .tooltip(i18n::text("Close preview"))
                            .on_click(cx.listener(|view, _, _, cx| view.close(cx))),
                    ),
            )
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .items_center()
                    .flex_shrink_0()
                    .bg(cx.theme().muted.opacity(0.3))
                    .border_y_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("line-preview-formatted")
                            .xsmall()
                            .ghost()
                            .selected(!self.raw && !self.details_mode)
                            .label(i18n::text("Formatted"))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.raw = false;
                                view.details_mode = false;
                                view.copied = false;
                                view.install_editor(window, cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("line-preview-raw")
                            .xsmall()
                            .ghost()
                            .selected(self.raw && !self.details_mode)
                            .label(i18n::text("Raw"))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.raw = true;
                                view.details_mode = false;
                                view.copied = false;
                                view.install_editor(window, cx);
                                cx.notify();
                            })),
                    )
                    .when(self.details.is_some(), |bar| {
                        bar.child(
                            Button::new("line-preview-fields")
                                .xsmall()
                                .ghost()
                                .selected(self.details_mode)
                                .label(i18n::text("Fields"))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.details_mode = true;
                                    view.pin(cx);
                                    cx.notify();
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(i18n::text(if self.pinned {
                                "Pinned"
                            } else {
                                "Hover preview"
                            })),
                    ),
            )
            .when_some(data.notice, |surface, notice| {
                surface.child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(i18n::text(notice)),
                )
            })
            .child(
                div()
                    .id("line-preview-content")
                    .test_support()
                    .flex_1()
                    .min_size_0()
                    .overflow_hidden()
                    .capture_any_mouse_down(cx.listener(|view, _, _, cx| view.pin(cx)))
                    .when(self.details_mode, |body| {
                        body.children(self.details.clone())
                    })
                    .when(!self.details_mode, |body| {
                        body.children(self.editor.as_ref().map(|editor| {
                            Editor::new(editor)
                                .h(relative(1.))
                                .bordered(false)
                                .readonly(true)
                                .aria_label(i18n::text("Line preview contents"))
                        }))
                    }),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("{} B", data.raw.len()))
                    .when(self.copied, |footer| {
                        footer.child(
                            div()
                                .id("line-preview-copy-status")
                                .role(Role::Status)
                                .child(i18n::text("Line copied")),
                        )
                    })
                    .child(div().flex_1())
                    .child(i18n::text("Esc to close")),
            );
        div()
            .absolute()
            .top_0()
            .left_0()
            .size(px(1.))
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, cx| {
                        let handle = window.window_handle();
                        // A hover preview never steals focus. Intercept Escape
                        // before workspace/editor bindings, scoped to its window.
                        let escape = cx.intercept_keystrokes(move |event, window, cx| {
                            if window.window_handle() != handle || event.keystroke.key != "escape" {
                                return;
                            }
                            let _ = weak.update(cx, |view, cx| {
                                if view.active.is_some() {
                                    view.close(cx);
                                    cx.stop_propagation();
                                }
                            });
                        });
                        // The rendered frame owns this subscription, including
                        // retained paints; unmounted panes release their listener.
                        window.on_key_event(move |_: &KeyDownEvent, _, _, _| {
                            let _ = &escape;
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(
                deferred(
                    Positioner::side(bounds)
                        .placement(Placement::Bottom)
                        .align(Align::Start)
                        .offset(px(6.))
                        .margin(px(8.))
                        .occlude()
                        .child(surface.with_animation(
                            ("line-preview-enter", id),
                            Animation::new(Duration::from_millis(130)),
                            |view, progress| view.opacity(progress),
                        )),
                )
                .with_priority(gpui_kit::base::POPUP_PRIORITY),
            )
            .into_any_element()
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests;
