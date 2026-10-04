//! A searchable guide built from the keymap installed on this platform.
use gpui_kit::base::SelectableText;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Root, TitleBar, h_flex, v_flex};
use gpui_kit::*;

#[derive(Default)]
struct ShortcutWindow(Option<WindowHandle<Root>>);
impl Global for ShortcutWindow {}

pub(crate) fn init(cx: &mut App) {
    cx.set_global(ShortcutWindow::default());
}

pub(crate) fn open(cx: &mut App) {
    cx.defer(|cx| {
        if let Some(handle) = cx.global::<ShortcutWindow>().0
            && handle
                .update(cx, |_, window, _| window.activate_window())
                .is_ok()
        {
            return;
        }
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(980.), px(700.)),
                cx,
            ))),
            window_min_size: Some(size(px(680.), px(400.))),
            ..TitleBar::window_options()
        };
        match cx.open_window(options, |window, cx| {
            window.set_window_title("Beacon — Keyboard shortcuts");
            let view = cx.new(|cx| Shortcuts::new(window, cx));
            window.activate_window();
            cx.new(|cx| Root::new(view, window, cx))
        }) {
            Ok(handle) => cx.global_mut::<ShortcutWindow>().0 = Some(handle),
            Err(error) => tracing::error!(%error, "could not open keyboard shortcuts"),
        }
    });
}

struct Entry {
    scope: String,
    keys: String,
    function: String,
    description: String,
}
struct Shortcuts {
    focus: FocusHandle,
    search: Entity<InputState>,
    query: String,
    entries: Vec<Entry>,
    _search: Subscription,
}

impl Shortcuts {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search shortcuts or functions"));
        let subscription = cx.subscribe(&search, |view, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                view.query = input.read(cx).value().to_lowercase();
                cx.notify();
            }
        });
        let keymap = cx.key_bindings();
        let mut entries = Vec::new();
        for binding in keymap.borrow().bindings() {
            let action = binding.action().name();
            let name = action.rsplit("::").next().unwrap_or(action);
            let context = binding
                .predicate()
                .map(|p| p.to_string())
                .unwrap_or_default();
            let scope = if action.starts_with("beacon::") {
                "Application"
            } else if context == "Input" {
                "Text fields / YAML editor"
            } else if context == "DataTable" {
                "Resource table"
            } else if context == "Command" {
                "Command palette"
            } else if context.contains("PopupMenu") {
                "Menus"
            } else {
                continue;
            };
            let (function, description) = describe(name, scope);
            entries.push(Entry {
                scope: scope.into(),
                keys: binding
                    .keystrokes()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" "),
                function,
                description,
            });
        }
        let copy = if cfg!(target_os = "macos") {
            "cmd-c"
        } else {
            "ctrl-c"
        };
        entries.extend([
            Entry {
                scope: "Overview / messages".into(),
                keys: copy.into(),
                function: "Copy selected values".into(),
                description: "Drag to select a value or error message, then copy the selection. Right-click a message to copy its full text."
                    .into(),
            },
            Entry {
                scope: "Pod shell".into(),
                keys: "ctrl-c".into(),
                function: "Interrupt command".into(),
                description: "Send an interrupt to the process running in the focused shell."
                    .into(),
            },
            Entry {
                scope: "Pod shell".into(),
                keys: "ctrl-d".into(),
                function: "End input".into(),
                description:
                    "Send end-of-input; on an empty shell prompt this usually exits the shell."
                        .into(),
            },
            Entry {
                scope: "Pod shell".into(),
                keys: "ctrl-z".into(),
                function: "Suspend command".into(),
                description: "Ask the remote shell to suspend its foreground process.".into(),
            },
        ]);
        entries.sort_by_key(|entry| {
            (
                if entry.scope == "Application" { 0 } else { 1 },
                entry.scope.clone(),
                entry.function.clone(),
                entry.keys.clone(),
            )
        });
        entries.dedup_by(|a, b| a.scope == b.scope && a.keys == b.keys && a.function == b.function);
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            focus,
            search,
            query: String::new(),
            entries,
            _search: subscription,
        }
    }
}

impl Render for Shortcuts {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let visible: Vec<_> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                format!("{} {} {} {}", e.scope, e.keys, e.function, e.description)
                    .to_lowercase()
                    .contains(&self.query)
            })
            .collect();
        v_flex().size_full().track_focus(&self.focus).bg(cx.theme().background).text_color(cx.theme().foreground)
            .on_action(|_: &crate::app::CloseTab, window, cx| window.defer(cx, |window,_| window.remove_window()))
            .child(TitleBar::new().child(div().font_weight(FontWeight::SEMIBOLD).child("Beacon — Keyboard shortcuts")))
            .child(v_flex().p_4().gap_2().child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Keyboard shortcuts"))
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Shortcuts depend on focus. Pod Shell captures keys for the container. Focus another control to use application shortcuts."))
                .child(Input::new(&self.search)))
            .child(div().id("shortcut-list").flex_1().min_h_0().overflow_y_scroll().child(
                v_flex().px_4().children(visible.into_iter().map(|(index,e)| {
                    h_flex().id(("shortcut", index)).w_full().gap_4().py_3().items_start().border_b_1().border_color(cx.theme().border)
                        .child(div().w(px(180.)).flex_shrink_0().font_family("monospace").text_sm().child(SelectableText::new("keys",e.keys.clone())))
                        .child(v_flex().flex_1().min_w_0().gap_1()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(e.function.clone()))
                            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{} · {}",e.scope,e.description))))
                }))))
            .child(div().p_3().text_xs().text_color(cx.theme().muted_foreground).child("Command palette: @ resource kind · # namespace · ctx cluster · > command. Select with ↑ / ↓ and confirm with Enter."))
    }
}

fn describe(name: &str, scope: &str) -> (String, String) {
    let description = match name {
        "Quit" => "Quit Beacon and close its windows.",
        "TogglePalette" => {
            "Find resources, switch namespaces or clusters, and run commands using @, #, ctx and >."
        }
        "OpenAppLogs" => "Open the application's own logs in a separate window.",
        "OpenSettings" => "Open application preferences for themes, fonts and the global proxy.",
        "OpenShortcuts" => "Open this searchable guide to shortcuts and their functions.",
        "NewTab" => {
            "Open another tab for the current cluster; without a cluster, open the cluster picker."
        }
        "CloseTab" => {
            "Close the current resource tab. With no tabs open, ask before quitting Beacon. In App logs, Keyboard shortcuts or Settings, close that window."
        }
        "NextTab" => "Switch to the next resource tab in the focused pane.",
        "PreviousTab" => "Switch to the previous resource tab in the focused pane.",
        "SplitRight" => {
            "Open an independent resource view to the right, keeping the namespace and filters. Drag the divider to resize."
        }
        "SplitDown" => {
            "Open an independent resource view below, keeping the namespace and filters. Splits can be nested."
        }
        "DetachTab" => {
            "Move the current tab to a separate window, preserving its resource view and Pod tools."
        }
        "MergeToMain" => "Move the current detached tab back into the main window's focused pane.",
        "CloseDetail" => {
            "Close the detail or Pod tools panel when the focused control does not consume Escape. Shell keeps Escape for the remote process."
        }
        "Cancel" => {
            if scope == "Resource table" {
                "Clear the table selection."
            } else {
                "Dismiss the open palette or menu."
            }
        }
        "Confirm" => "Activate the selected command or menu item.",
        "Enter" => "Confirm a single-line input or insert a line break in a multiline editor.",
        "Escape" => "Dismiss search or clear a search field configured to clear on Escape.",
        "Copy" => "Copy the selected text to the clipboard.",
        "Cut" => "Copy and remove the selected text.",
        "Paste" => "Insert the clipboard text into the focused field.",
        "SelectAll" => "Select all text in the focused field.",
        "Undo" => "Undo the last text edit.",
        "Redo" => "Redo the last undone text edit.",
        "Backspace" => "Delete the character before the cursor or the selected text.",
        "Delete" => "Delete the character after the cursor or the selected text.",
        "Search" => "Find text in the focused multiline editor.",
        "Replace" => "Find and replace text in the focused multiline editor.",
        "SelectUp" => "Select the previous resource row, command or menu item.",
        "SelectDown" => "Select the next resource row, command or menu item.",
        "SelectPrevColumn" => "Move to the previous table column.",
        "SelectNextColumn" => "Move to the next table column.",
        "SelectFirst" => "Select the first resource row.",
        "SelectLast" => "Select the last resource row.",
        "SelectPageUp" => "Move selection up by one table page.",
        "SelectPageDown" => "Move selection down by one table page.",
        "AddCursorAbove" => "Add a text cursor on the previous editor line.",
        "AddCursorBelow" => "Add a text cursor on the next editor line.",
        "ShowCharacterPalette" => "Open the system character and emoji picker.",
        "ToggleCodeActions" => "Show available editor actions at the cursor, when supported.",
        _ => "Use this key in the focused control.",
    };
    let function = words(name);
    let description = if name.starts_with("Move") {
        format!(
            "Move the text cursor: {}.",
            words(name.trim_start_matches("Move")).to_lowercase()
        )
    } else if name.starts_with("SelectTo")
        || matches!(
            name,
            "SelectLeft" | "SelectRight" | "SelectUp" | "SelectDown"
        ) && scope == "Text fields / YAML editor"
    {
        format!(
            "Extend the text selection: {}.",
            words(name.trim_start_matches("Select")).to_lowercase()
        )
    } else if name.starts_with("DeleteTo") {
        format!(
            "Delete text from the cursor to {}.",
            words(name.trim_start_matches("DeleteTo")).to_lowercase()
        )
    } else if matches!(name, "Indent" | "IndentInline") {
        "Indent the selected lines or insert indentation.".into()
    } else if matches!(name, "Outdent" | "OutdentInline") {
        "Remove one indentation level from the selected lines.".into()
    } else {
        description.into()
    };
    (function, description)
}

fn words(name: &str) -> String {
    let mut words = String::new();
    for (i, c) in name.chars().enumerate() {
        if i > 0 && c.is_uppercase() {
            words.push(' ');
        }
        words.push(c);
    }
    words
}
