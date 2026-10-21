use super::*;
use crate::feature_test_support as support;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;

#[::core::prelude::v1::test]
fn terminal_enter_and_escape_stay_in_the_dialog_and_ansi_colors_are_preserved() {
    let cx = &mut support::context();
    cx.update(super::init);
    let (_fixture, session) = support::fixture(cx, "terminal-dialog", vec![]);
    let window = support::window(cx);
    let (shell, mut input) = Session::for_testing();
    let terminal = cx
        .update_window(window, |_, window, cx| {
            let terminal = cx.new(|cx| {
                TerminalView::new(
                    session,
                    "default".into(),
                    "web".into(),
                    Some("debugger".into()),
                    window,
                    cx,
                )
            });
            terminal.update(cx, |view, cx| {
                view.set_shell("/bin/sh", window, cx);
                let command = view.command(cx);
                assert!(command[2].contains("TERM=xterm-256color"));
                assert!(command[2].contains("COLORTERM=truecolor"));
                assert!(command[2].contains("-i"));
                view.state = State::Running;
                view.shell = Some(shell);
                view.feed(b"\x1b[31mred\x1b[0mplain", cx);
                let rows = view.rows(cx);
                let red = rows[0].iter().find(|run| run.text.contains("red")).unwrap();
                let plain = rows[0]
                    .iter()
                    .find(|run| run.text.contains("plain"))
                    .unwrap();
                assert_ne!(red.fg, plain.fg);
            });
            let content = terminal.clone();
            window.open_dialog(cx, move |dialog, _, _| {
                dialog
                    .title("Debug terminal")
                    .child(div().h(px(300.)).child(content.clone()))
            });
            terminal
        })
        .unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        terminal.read(cx).focus.clone().focus(window, cx);
        window.render_frame(cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert!(
            window.has_active_dialog(cx),
            "Enter must go to the shell rather than confirming its dialog"
        );
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(
            window.has_active_dialog(cx),
            "Escape must go to the shell rather than closing its dialog"
        );
        assert_eq!(input.try_recv().unwrap(), b"\r");
        assert_eq!(input.try_recv().unwrap(), b"\x1b");

        cx.write_to_clipboard(ClipboardItem::new_string("printf '中文'\r\nnext".into()));
        for shortcut in [
            #[cfg(target_os = "macos")]
            "cmd-v",
            "ctrl-shift-v",
            "shift-insert",
        ] {
            window.press(shortcut, cx);
            window.render_frame(cx);
            assert_eq!(
                input.try_recv().unwrap(),
                "printf '中文'\rnext".as_bytes(),
                "{shortcut}"
            );
            assert!(
                input.try_recv().is_err(),
                "paste must not also type a literal v"
            );
            assert!(window.has_active_dialog(cx));
        }
        terminal.update(cx, |view, cx| view.feed(b"\x1b[?2004h", cx));
        window.press("ctrl-shift-v", cx);
        window.render_frame(cx);
        assert_eq!(
            input.try_recv().unwrap(),
            "\x1b[200~printf '中文'\nnext\x1b[201~".as_bytes()
        );

        // Ctrl-V and Ctrl-C remain terminal control characters.
        window.press("ctrl-v", cx);
        window.press("ctrl-c", cx);
        assert_eq!(input.try_recv().unwrap(), b"\x16");
        assert_eq!(input.try_recv().unwrap(), b"\x03");

        // A stopped session must not receive clipboard input.
        terminal.update(cx, |view, cx| {
            view.state = State::Ended;
            cx.notify();
        });
        window.render_frame(cx);
        window.press("ctrl-shift-v", cx);
        window.render_frame(cx);
        assert!(input.try_recv().is_err());

        // A child input keeps normal paste and never sends it to the shell.
        window.click("terminal-shell-options", cx);
        window.render_frame(cx);
        cx.write_to_clipboard(ClipboardItem::new_string("/bin/bash".into()));
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            },
            cx,
        );
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-v"
            } else {
                "ctrl-v"
            },
            cx,
        );
        window.render_frame(cx);
        assert_eq!(
            terminal.read(cx).command.read(cx).value().to_string(),
            "/bin/bash"
        );
        assert!(input.try_recv().is_err());
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn terminal_context_menu_pastes_and_returns_focus_to_the_grid() {
    let cx = &mut support::context();
    cx.update(super::init);
    let (_fixture, session) = support::fixture(cx, "terminal-context-paste", vec![]);
    let window = support::window(cx);
    let (shell, mut input) = Session::for_testing();
    let terminal = cx
        .update_window(window, |_, window, cx| {
            let terminal = cx.new(|cx| {
                TerminalView::new(session, "default".into(), "web".into(), None, window, cx)
            });
            terminal.update(cx, |view, _| {
                view.state = State::Running;
                view.shell = Some(shell);
            });
            let content = terminal.clone();
            window.open_dialog(cx, move |dialog, _, _| {
                dialog
                    .title("Shell")
                    .child(div().h(px(300.)).child(content.clone()))
            });
            terminal
        })
        .unwrap();
    support::render_until(cx, window, |window| {
        window.try_find("terminal-grid").is_some()
    });
    cx.update_window(window, |_, window, cx| {
        terminal.read(cx).focus.clone().focus(window, cx);
        window.render_frame(cx);
        cx.write_to_clipboard(ClipboardItem::new_string("echo pasted".into()));
        window.right_click("terminal-grid", cx);
    })
    .unwrap();
    support::render_until(cx, window, |window| {
        gpui_kit::base::test_support::snapshots(window)
            .iter()
            .any(|item| item.role() == Some(Role::MenuItem) && item.label() == Some("Paste"))
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("down", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(input.try_recv().unwrap(), b"echo pasted");
        assert!(terminal.read(cx).focus.is_focused(window));
        assert!(window.has_active_dialog(cx));
    })
    .unwrap();
}
