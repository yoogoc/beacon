//! Selectable messages with a way to copy their full, untruncated contents.

use gpui_kit::base::{SelectableText, TextSelection};
use gpui_kit::component::input::Copy;
use gpui_kit::component::menu::{ContextMenu, ContextMenuExt as _, PopupMenuItem};
use gpui_kit::*;

pub(crate) fn copyable_text(
    id: impl Into<ElementId>,
    text: impl Into<SharedString>,
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
        .child(SelectableText::new("text", text.clone()))
        .context_menu(move |menu, _, _| menu.item(copy_item("Copy", text.clone())))
}

pub(crate) fn copy_item(label: &'static str, text: impl Into<SharedString>) -> PopupMenuItem {
    let text = text.into();
    PopupMenuItem::new(label).on_click(move |_, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
    })
}
