use super::*;
use crate::feature_test_support as support;
use alacritty_terminal::grid::Scroll;
use futures::channel::mpsc::UnboundedReceiver;
use gpui_kit::InputEvent as _;
use gpui_kit::test::TestWindowExt as _;

fn open(
    cx: &mut TestAppContext,
    output: &[u8],
) -> (
    beacon_kube::test_support::Fixture,
    AnyWindowHandle,
    Entity<TerminalView>,
    UnboundedReceiver<Vec<u8>>,
) {
    open_sized(cx, output, size(px(800.), px(460.)))
}

fn open_sized(
    cx: &mut TestAppContext,
    output: &[u8],
    viewport: gpui_kit::Size<Pixels>,
) -> (
    beacon_kube::test_support::Fixture,
    AnyWindowHandle,
    Entity<TerminalView>,
    UnboundedReceiver<Vec<u8>>,
) {
    cx.update(super::init);
    let (fixture, session) = support::fixture(cx, "terminal-interactions", vec![]);
    let (shell, input) = Session::for_testing();
    let (window, terminal) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    gpui_kit::Point::default(),
                    viewport,
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    let mut view = TerminalView::new(
                        session,
                        "default".into(),
                        "web".into(),
                        None,
                        window,
                        cx,
                    );
                    view.state = State::Running;
                    view.shell = Some(shell);
                    view.feed(output, cx);
                    view
                })
            },
        )
        .unwrap()
    });
    support::render_until(cx, window, |window| {
        window.try_find("terminal-grid").is_some()
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        terminal.read(cx).focus.clone().focus(window, cx);
        window.render_frame(cx);
    })
    .unwrap();
    (fixture, window, terminal, input)
}

fn position(view: &TerminalView, column: f32, row: f32) -> gpui_kit::Point<Pixels> {
    view.grid_bounds.get().origin
        + point(
            px(column * view.metrics.width),
            px(row * view.metrics.height),
        )
}

#[::core::prelude::v1::test]
fn ended_sessions_keep_the_transcript_selectable_and_hide_input_until_requested() {
    let cx = &mut support::context();
    let (_fixture, window, terminal, mut input) = open(
        cx,
        b"alpha beta\r\n$ exit\r\nexit\r\n\x1b[?1000h\x1b[?1006h",
    );
    cx.update_window(window, |_, window, cx| {
        terminal.update(cx, |view, cx| {
            view.composition.text = "uncommitted".into();
            view.finish(State::Ended);
            assert!(view.composition.text.is_empty());
            assert!(!view.rows(cx).iter().flatten().any(|run| run.cursor));
            cx.notify();
        });
        window.render_frame(cx);
        let status = window.find("terminal-session-status");
        assert_eq!(status.label(), Some("Session ended"));
        assert!(status.bounds().size.height < px(56.));
        assert!(window.try_find("terminal-shell-command").is_none());
        assert!(window.find("start-shell").visible());

        // A program's mouse-capture mode must not survive the stopped session.
        let start = position(terminal.read(cx), 0.1, 0.5);
        let end = position(terminal.read(cx), 5.1, 0.5);
        window.drag(start, end, cx);
        assert_eq!(
            terminal.read(cx).term.selection_to_string().as_deref(),
            Some("alpha")
        );
        window.click("terminal-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("alpha")
        );
        window.input("ignored", cx);
        assert!(input.try_recv().is_err());

        window.click("terminal-find", cx);
        window.render_frame(cx);
        window.input("alpha", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(terminal.read(cx).search.matches.len(), 1);
        window.click("terminal-search-close", cx);
        window.render_frame(cx);
        window.click("terminal-shell-options", cx);
        window.render_frame(cx);
        assert!(window.find("terminal-shell-command").visible());
        assert!(
            terminal
                .read(cx)
                .command
                .focus_handle(cx)
                .is_focused(window)
        );
        window.input("/bin/sh", cx);
        assert_eq!(terminal.read(cx).command(cx), ["/bin/sh"]);
        window.click("terminal-shell-options", cx);
        window.render_frame(cx);
        assert!(window.try_find("terminal-shell-command").is_none());
        assert!(terminal.read(cx).focus.is_focused(window));
        window.click("terminal-shell-options", cx);
        window.render_frame(cx);
        assert_eq!(
            terminal.read(cx).command.read(cx).value().as_str(),
            "/bin/sh"
        );
        window.click("start-shell", cx);
        assert!(matches!(terminal.read(cx).state, State::Connecting));
        assert!(!terminal.read(cx).launcher_options);
        window.render_frame(cx);
        assert!(window.try_find("start-shell").is_none());
        assert!(window.try_find("terminal-shell-command").is_none());
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn failed_sessions_show_the_error_and_keep_retry_available_in_a_narrow_pane() {
    let cx = &mut support::context();
    let (_fixture, window, terminal, _) =
        open_sized(cx, b"previous output", size(px(360.), px(300.)));
    cx.update_window(window, |_, window, cx| {
        terminal.update(cx, |view, cx| {
            view.finish(State::Failed(
                "Unable to open /bin/bash in this container".into(),
            ));
            cx.notify();
        });
        window.render_frame(cx);
        window.render_frame(cx);
        assert_eq!(window.viewport_size().width, px(360.));
        assert_eq!(
            window.find("terminal-session-status").label(),
            Some("Connection failed")
        );
        let reconnect = window.find("start-shell");
        assert!(reconnect.visible());
        assert!(reconnect.bounds().right() <= window.viewport_size().width);
        assert!(window.find("terminal-session-error").visible());
        window.click("terminal-shell-options", cx);
        window.render_frame(cx);
        assert!(window.find("terminal-shell-command").visible());
        assert!(window.find("start-shell").visible());
        crate::i18n::init(crate::i18n::Language::SimplifiedChinese, cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("terminal-session-status").label(),
            Some("连接失败")
        );
        assert!(window.find("start-shell").bounds().right() <= px(360.));
        crate::i18n::init(crate::i18n::Language::English, cx);
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn dragging_selects_and_copies_text_and_release_cannot_leave_a_sticky_selection() {
    let cx = &mut support::context();
    let (_fixture, window, terminal, mut input) = open(cx, b"alpha beta gamma");
    cx.update_window(window, |_, window, cx| {
        let start = position(terminal.read(cx), 0.1, 0.5);
        let end = position(terminal.read(cx), 5.1, 0.5);
        window.drag(start, end, cx);
        assert_eq!(
            terminal.read(cx).term.selection_to_string().as_deref(),
            Some("alpha")
        );
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-c"
            } else {
                "ctrl-shift-c"
            },
            cx,
        );
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("alpha")
        );
        let frozen = terminal.read(cx).term.selection.clone();
        window.dispatch_event(
            MouseMoveEvent {
                position: position(terminal.read(cx), 12.1, 0.5),
                pressed_button: Some(MouseButton::Left),
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        assert_eq!(terminal.read(cx).term.selection, frozen);
        assert!(input.try_recv().is_err());
        window.dispatch_event(
            MouseDownEvent {
                position: position(terminal.read(cx), 7.5, 0.5),
                button: MouseButton::Left,
                click_count: 2,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseUpEvent {
                position: end,
                button: MouseButton::Left,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        assert_eq!(
            terminal.read(cx).term.selection_to_string().as_deref(),
            Some("beta")
        );
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn history_scrolls_searches_and_returns_to_live_output_on_input() {
    let cx = &mut support::context();
    let (_fixture, window, terminal, mut input) = open(cx, b"");
    let output = (0..200)
        .map(|line| format!("line-{line:03}\r\n"))
        .collect::<String>();
    cx.update_window(window, |_, window, cx| {
        terminal.update(cx, |view, cx| {
            view.feed(output.as_bytes(), cx);
            cx.notify();
        });
        window.render_frame(cx);
        let rail = window.find("terminal-scrollbar").bounds();
        let rows = terminal.read(cx).term.screen_lines();
        let total = terminal.read(cx).term.total_lines();
        let thumb = (rail.size.height.as_f32() * rows as f32 / total as f32).max(20.);
        window.drag(
            point(rail.center().x, rail.bottom() - px(thumb / 2.)),
            point(rail.center().x, rail.top() + px(thumb / 2.)),
            cx,
        );
        assert!(terminal.read(cx).term.grid().display_offset() > 50);
        assert!(terminal.read(cx).scrollbar_drag.is_none());
        window.press("shift-end", cx);
        window.render_frame(cx);
        assert_eq!(terminal.read(cx).term.grid().display_offset(), 0);
        let before = window.find("terminal-grid").bounds();
        window.scroll("terminal-grid", ScrollDelta::Lines(point(0., 4.)), cx);
        assert!(terminal.read(cx).term.grid().display_offset() > 0);
        assert_eq!(window.find("terminal-grid").bounds(), before);
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-f"
            } else {
                "ctrl-shift-f"
            },
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(terminal.read(cx).search.open);
        window.input("line-0", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(terminal.read(cx).search.matches.len(), 100);
        let current = terminal.read(cx).search.current;
        window.click("terminal-search-next", cx);
        assert_ne!(terminal.read(cx).search.current, current);
        window.click("terminal-search-close", cx);
        window.render_frame(cx);
        window.press("a", cx);
        assert_eq!(input.try_recv().unwrap(), b"a");
        assert!(
            input.try_recv().is_err(),
            "text must pass through the IME once"
        );
        assert_eq!(terminal.read(cx).term.grid().display_offset(), 0);
        window.click("terminal-zoom-in", cx);
        window.render_frame(cx);
        assert!(terminal.read(cx).metrics.height > 19.);
        terminal.update(cx, |view, cx| view.zoom(100., cx));
        window.render_frame(cx);
        let maximum = terminal.read(cx).metrics.width;
        terminal.update(cx, |view, cx| view.zoom(-1., cx));
        window.render_frame(cx);
        assert!(
            terminal.read(cx).metrics.width < maximum,
            "a single zoom-out must work immediately after reaching the limit"
        );
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn chinese_composition_stays_local_until_commit_and_cells_keep_their_columns() {
    let cx = &mut support::context();
    let (_fixture, window, terminal, mut input) = open(cx, "A中e\u{301}Z".as_bytes());
    cx.update_window(window, |_, window, cx| {
        terminal.update(cx, |view, cx| {
            assert_eq!(view.term.grid().cursor.point.column.0, 5);
            let runs = view.rows(cx);
            let wide = runs[0].iter().find(|run| run.text == "中").unwrap();
            assert_eq!((wide.column, wide.cells), (1, 2));
            let combining = runs[0]
                .iter()
                .find(|run| run.text.contains("e\u{301}Z"))
                .unwrap();
            assert_eq!(combining.column, 3);
            view.replace_and_mark_text_in_range(None, "ni", None, window, cx);
            assert_eq!(view.marked_text_range(window, cx), Some(0..2));
            view.replace_and_mark_text_in_range(None, "你😀", Some(3..3), window, cx);
            assert_eq!(view.marked_text_range(window, cx), Some(0..3));
            assert!(input.try_recv().is_err());
            let bounds = view
                .bounds_for_range(0..0, view.grid_bounds.get(), window, cx)
                .unwrap();
            assert!(bounds.size.width > px(0.) && bounds.size.height > px(0.));
            view.replace_text_in_range(None, "你好", window, cx);
            assert_eq!(input.try_recv().unwrap(), "你好".as_bytes());
            assert!(input.try_recv().is_err());
            assert!(view.composition.text.is_empty());
        });
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn tui_mouse_reporting_and_alternate_scroll_do_not_break_shift_selection() {
    let cx = &mut support::context();
    let (_fixture, window, terminal, mut input) = open(cx, b"alpha beta\x1b[?1000h\x1b[?1006h");
    let help_opened = Rc::new(StdCell::new(false));
    let help = help_opened.clone();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    cx.update(|cx| {
        cx.on_action(move |_: &crate::app::OpenShortcuts, _| help.set(true));
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.press("f1", cx);
        assert_eq!(input.try_recv().unwrap(), b"\x1bOP");
        assert!(
            !help_opened.get(),
            "terminal F1 must override the global shortcut guide"
        );
        let at = position(terminal.read(cx), 3.5, 2.5);
        window.dispatch_event(
            MouseDownEvent {
                position: at,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseUpEvent {
                position: at,
                button: MouseButton::Left,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        assert_eq!(input.try_recv().unwrap(), b"\x1b[<0;4;3M");
        assert_eq!(input.try_recv().unwrap(), b"\x1b[<0;4;3m");
        window.dispatch_event(
            MouseDownEvent {
                position: at,
                button: MouseButton::Right,
                click_count: 1,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseUpEvent {
                position: at,
                button: MouseButton::Right,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        assert_eq!(input.try_recv().unwrap(), b"\x1b[<2;4;3M");
        assert_eq!(input.try_recv().unwrap(), b"\x1b[<2;4;3m");
        window.render_frame(cx);
        assert!(
            !gpui_kit::base::test_support::snapshots(window)
                .iter()
                .any(|item| item.role() == Some(Role::MenuItem))
        );
        terminal.update(cx, |view, cx| {
            let modifiers = Modifiers {
                shift: true,
                ..Default::default()
            };
            view.mouse_down(
                &MouseDownEvent {
                    position: position(view, 0.1, 0.5),
                    button: MouseButton::Left,
                    modifiers,
                    click_count: 1,
                    ..Default::default()
                },
                window,
                cx,
            );
            view.mouse_move(
                &MouseMoveEvent {
                    position: position(view, 5.1, 0.5),
                    pressed_button: Some(MouseButton::Left),
                    modifiers,
                },
                cx,
            );
            view.mouse_up(
                &MouseUpEvent {
                    position: at,
                    button: MouseButton::Left,
                    ..Default::default()
                },
                cx,
            );
            assert_eq!(view.term.selection_to_string().as_deref(), Some("alpha"));
            assert!(input.try_recv().is_err());
            view.feed(b"\x1b[?1000l\x1b[?1049h\x1b[?1007h\x1b[?1h", cx);
            view.wheel(
                &ScrollWheelEvent {
                    delta: ScrollDelta::Lines(point(0., 1.)),
                    ..Default::default()
                },
                cx,
            );
            for _ in 0..3 {
                assert_eq!(input.try_recv().unwrap(), b"\x1bOA");
            }
            assert!(input.try_recv().is_err());
            view.scroll(Scroll::Bottom, cx);
            view.feed(b"\x1b[?1003h", cx);
            view.open_search(window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(!terminal.read(cx).focus.is_focused(window));
        window.dispatch_event(
            MouseMoveEvent {
                position: position(terminal.read(cx), 5.5, 2.5),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        assert!(
            input.try_recv().is_err(),
            "search focus must stop remote mouse-motion reports"
        );
    })
    .unwrap();
}
