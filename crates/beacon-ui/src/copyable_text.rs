//! Selectable messages with a way to copy their full, untruncated contents.

use gpui_kit::base::{SelectableText, TextSelection};
use gpui_kit::component::input::Copy;
use gpui_kit::component::menu::{ContextMenu, ContextMenuExt as _, PopupMenuItem};
use gpui_kit::*;

pub(crate) fn copyable_text(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
) -> ContextMenu<Stateful<Div>> {
    highlighted_text(id, text, Vec::new())
}

pub(crate) fn highlighted_text(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
    highlights: Vec<(std::ops::Range<usize>, HighlightStyle)>,
) -> ContextMenu<Stateful<Div>> {
    let text = text.into();
    div()
        .id(id)
        .min_w_0()
        .focusable()
        .cursor_text()
        // Move focus away from editors/tables so their Copy action cannot
        // consume the shortcut. Also keep selection from activating a row.
        .on_click(|_, _, cx| cx.stop_propagation())
        .on_action(|_: &Copy, window, cx| {
            let selected = TextSelection::selected_text(window, cx);
            if selected.is_empty() {
                cx.propagate();
            } else {
                cx.write_to_clipboard(ClipboardItem::new_string(selected));
            }
        })
        .child(SelectableText::new("text", text.clone()).highlights(highlights))
        .context_menu(move |menu, _, _| menu.item(copy_item("Copy", text.clone())))
}

pub(crate) fn copy_item(label: &'static str, text: impl Into<SharedString>) -> PopupMenuItem {
    let text = text.into();
    PopupMenuItem::new(crate::i18n::text(label)).on_click(move |_, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
    })
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;

    struct CopyableMessage {
        row_clicks: usize,
    }
    impl Render for CopyableMessage {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().size_full().p_4().child(
                div()
                    .id("message-row")
                    .test_support()
                    .w_full()
                    .h(px(40.))
                    .child(copyable_text(
                        "message",
                        "alpha beta gamma delta epsilon zeta eta theta iota kappa",
                    ))
                    .on_click(cx.listener(|view, _, _, _| view.row_clicks += 1)),
            )
        }
    }
    fn open(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<CopyableMessage>) {
        cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(640.), px(240.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|_| CopyableMessage { row_clicks: 0 })
                },
            )
            .unwrap()
        })
    }
    fn move_pointer(
        position: Point<Pixels>,
        pressed_button: Option<MouseButton>,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.dispatch_event(
            MouseMoveEvent {
                position,
                pressed_button,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
    }

    #[::core::prelude::v1::test]
    fn consumed_mouse_up_finishes_drag_without_losing_selected_text_or_copy() {
        let cx = &mut support::context();
        let (window, view) = open(cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let origin = window.find("message-row").bounds().origin;
            window.drag(
                origin + point(px(1.), px(12.)),
                origin + point(px(100.), px(12.)),
                cx,
            );
            let selected = TextSelection::selected_text(window, cx);
            assert!(!selected.is_empty());
            // A new held button elsewhere must not resume an already-ended drag.
            move_pointer(
                origin + point(px(240.), px(12.)),
                Some(MouseButton::Left),
                window,
                cx,
            );
            assert_eq!(TextSelection::selected_text(window, cx), selected);
            move_pointer(origin + point(px(320.), px(12.)), None, window, cx);
            assert_eq!(TextSelection::selected_text(window, cx), selected);
            window.press(
                if cfg!(target_os = "macos") {
                    "cmd-c"
                } else {
                    "ctrl-c"
                },
                cx,
            );
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(selected.as_str())
            );
            assert_eq!(view.read(cx).row_clicks, 0);
        })
        .unwrap();
    }

    #[::core::prelude::v1::test]
    fn missing_mouse_up_recovers_on_a_move_without_a_held_button() {
        let cx = &mut support::context();
        let (window, _) = open(cx);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let origin = window.find("message-row").bounds().origin;
            window.dispatch_event(
                MouseDownEvent {
                    position: origin + point(px(1.), px(12.)),
                    button: MouseButton::Left,
                    modifiers: Default::default(),
                    click_count: 1,
                    first_mouse: false,
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
            move_pointer(
                origin + point(px(100.), px(12.)),
                Some(MouseButton::Left),
                window,
                cx,
            );
            let selected = TextSelection::selected_text(window, cx);
            assert!(!selected.is_empty());
            // Models releasing outside the window, where no MouseUp was delivered.
            move_pointer(origin + point(px(240.), px(12.)), None, window, cx);
            assert_eq!(TextSelection::selected_text(window, cx), selected);
            move_pointer(
                origin + point(px(320.), px(12.)),
                Some(MouseButton::Left),
                window,
                cx,
            );
            assert_eq!(TextSelection::selected_text(window, cx), selected);
        })
        .unwrap();
    }
}
