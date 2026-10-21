use super::*;
use crate::feature_test_support as support;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;

#[::core::prelude::v1::test]
fn terminal_enter_and_escape_stay_in_the_dialog_and_ansi_colors_are_preserved() {
    let cx = &mut support::context();
    let (_fixture, session) = support::fixture(cx, "terminal-dialog", vec![]);
    let window = support::window(cx);
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
    })
    .unwrap();
}
