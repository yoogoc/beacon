use super::*;
use crate::feature_test_support as support;
use gpui_kit::test::TestWindowExt as _;

struct Pane {
    preview: Entity<LinePreview>,
}
impl Render for Pane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .gap_2()
            .p_4()
            .children(
                [
                    "{\"message\":\"你好\",\"items\":[true,null,42]}",
                    "{\"other\":2}",
                    "{\"incomplete\":",
                    "level=info msg=ready count=2",
                    "{name: api, ports: [80, 443]}",
                    "<root><child>ready</child></root>",
                ]
                .into_iter()
                .enumerate()
                .map(|(id, raw)| {
                    trigger(
                        &self.preview,
                        id,
                        raw.into(),
                        div().w_full().h(px(32.)).child(raw),
                        cx,
                    )
                }),
            )
            .child(self.preview.clone())
    }
}

#[::core::prelude::v1::test]
fn scrolling_to_the_end_of_a_hover_preview_keeps_it_open() {
    struct LastLine {
        preview: Entity<LinePreview>,
        raw: SharedString,
    }
    impl Render for LastLine {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .size_full()
                .p_4()
                .child(div().flex_1())
                .child(trigger(
                    &self.preview,
                    0,
                    self.raw.clone(),
                    div().h(px(32.)).child("JSON log"),
                    cx,
                ))
                .child(self.preview.clone())
        }
    }

    let cx = &mut support::context();
    let dir = tempfile::tempdir().unwrap();
    support::workspace(cx, dir.path());
    let (window, preview) = cx.update(|cx| {
        let preview = LinePreview::new(cx);
        let (window, _) = gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
            cx.new(|_| LastLine {
                preview: preview.clone(),
                raw: serde_json::json!({"items": (0..200).collect::<Vec<_>>()})
                    .to_string()
                    .into(),
            })
        })
        .unwrap();
        (window, preview)
    });
    hover(cx, window, 0);
    cx.run_until_parked();
    cx.executor().advance_clock(OPEN_DELAY);
    support::render_until(cx, window, |window| {
        window
            .try_find("line-preview-content")
            .is_some_and(|body| body.visible())
    });
    cx.update_window(window, |_, window, cx| {
        assert!(!preview.read(cx).pinned);
        let editor = preview.read(cx).editor.clone().unwrap();
        let initial = editor.read(cx).scroll_offset().y;
        window.scroll(
            "line-preview-content",
            ScrollDelta::Pixels(point(px(0.), px(-10000.))),
            cx,
        );
        let end = editor.read(cx).scroll_offset().y;
        assert!(end < initial);
        window.scroll(
            "line-preview-content",
            ScrollDelta::Pixels(point(px(0.), px(-10000.))),
            cx,
        );
        assert_eq!(editor.read(cx).scroll_offset().y, end);
        assert!(preview.read(cx).pinned);
        let outside = point(window.viewport_size().width - px(2.), px(2.));
        window.dispatch_event(
            MouseMoveEvent {
                position: outside,
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(CLOSE_DELAY * 2);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("line-preview").visible());
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("line-preview").is_none());
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn non_json_formats_use_the_same_preview_and_raw_copy_stays_exact() {
    let cx = &mut support::context();
    let dir = tempfile::tempdir().unwrap();
    support::workspace(cx, dir.path());
    let (window, preview) = open(cx, 760.);
    for (id, label, original) in [
        (3, "logfmt", "level=info msg=ready count=2"),
        (4, "YAML", "{name: api, ports: [80, 443]}"),
        (5, "XML", "<root><child>ready</child></root>"),
    ] {
        hover(cx, window, id);
        cx.run_until_parked();
        cx.executor().advance_clock(OPEN_DELAY);
        support::settle(cx, |cx| {
            preview.read_with(cx, |view, _| view.active.is_some())
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("line-preview-format").label(), Some(label));
            window.click("line-preview-copy", cx);
            assert!(
                cx.read_from_clipboard()
                    .unwrap()
                    .text()
                    .unwrap()
                    .contains('\n')
            );
            window.click("line-preview-raw", cx);
            window.click("line-preview-copy", cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some(original)
            );
            window.click("line-preview-close", cx);
            window.render_frame(cx);
        })
        .unwrap();
        preview.update(cx, |view, cx| {
            view.raw = false;
            cx.notify();
        });
    }
}
fn open(cx: &mut TestAppContext, width: f32) -> (AnyWindowHandle, Entity<LinePreview>) {
    open_with_delay(cx, width, OPEN_DELAY)
}

fn open_with_delay(
    cx: &mut TestAppContext,
    width: f32,
    delay: Duration,
) -> (AnyWindowHandle, Entity<LinePreview>) {
    cx.update(|cx| {
        let preview = LinePreview::with_open_delay(delay, cx);
        let child = preview.clone();
        let (window, _) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(width), px(700.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                window.set_view_retention(false);
                cx.new(|_| Pane { preview: child })
            },
        )
        .unwrap();
        (window, preview)
    })
}

#[::core::prelude::v1::test]
fn three_second_hover_restarts_on_another_row_cancels_on_leave_and_click_opens_immediately() {
    let cx = &mut support::context();
    let dir = tempfile::tempdir().unwrap();
    support::workspace(cx, dir.path());
    let (window, preview) = open_with_delay(cx, 760., Duration::from_secs(3));
    hover(cx, window, 0);
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(2));
    hover(cx, window, 1);
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(2999));
    cx.run_until_parked();
    assert!(preview.read_with(cx, |view, _| view.active.is_none()));
    cx.executor().advance_clock(Duration::from_millis(1));
    support::settle(cx, |cx| {
        preview.read_with(cx, |view, _| {
            view.active.as_ref().is_some_and(|active| active.id == 1)
        })
    });

    preview.update(cx, |view, cx| view.close(cx));
    hover(cx, window, 2);
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.update_window(window, |_, window, cx| {
        window.dispatch_event(
            MouseMoveEvent {
                position: point(px(750.), px(650.)),
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(3));
    cx.run_until_parked();
    assert!(preview.read_with(cx, |view, _| view.active.is_none()));

    cx.update_window(window, |_, window, cx| {
        let bounds = window.find(("line-preview-trigger", 3usize)).bounds();
        preview.update(cx, |view, cx| {
            view.show(3, "level=info msg=ready".into(), bounds, window, cx);
        });
    })
    .unwrap();
    support::settle(cx, |cx| {
        preview.read_with(cx, |view, _| {
            view.pinned && view.active.as_ref().is_some_and(|active| active.id == 3)
        })
    });
}
fn hover(cx: &mut TestAppContext, window: AnyWindowHandle, id: usize) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.hover(("line-preview-trigger", id), cx);
    })
    .unwrap();
}
#[::core::prelude::v1::test]
fn delayed_preview_pins_switches_copy_modes_and_escape_keeps_original_bytes() {
    let cx = &mut support::context();
    let dir = tempfile::tempdir().unwrap();
    support::workspace(cx, dir.path());
    let (window, preview) = open(cx, 760.);
    hover(cx, window, 0);
    assert!(preview.read_with(cx, |view, _| view.active.is_none()));
    cx.run_until_parked();
    cx.executor().advance_clock(OPEN_DELAY);
    support::settle(cx, |cx| {
        preview.read_with(cx, |view, _| view.active.is_some())
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("line-preview").visible());
        window.hover("line-preview-content", cx);
        window.click("line-preview-copy", cx);
        let text = cx.read_from_clipboard().unwrap().text().unwrap();
        assert!(text.contains('\n') && text.contains("你好"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            serde_json::json!({"message":"你好","items":[true,null,42]})
        );
    })
    .unwrap();
    hover(cx, window, 1);
    assert!(preview.read_with(cx, |view, _| view.pinned
        && view.active.as_ref().unwrap().id == 0));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("line-preview-raw", cx);
        window.click("line-preview-copy", cx);
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().as_deref(),
            Some("{\"message\":\"你好\",\"items\":[true,null,42]}")
        );
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("line-preview").is_none());
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn delayed_work_cannot_reopen_a_replaced_source_and_narrow_bilingual_previews_fit() {
    let cx = &mut support::context();
    let dir = tempfile::tempdir().unwrap();
    support::workspace(cx, dir.path());
    let (window, preview) = open(cx, 340.);
    hover(cx, window, 0);
    preview.update(cx, |view, cx| view.close(cx));
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert!(preview.read_with(cx, |view, _| view.active.is_none()));
    for language in [i18n::Language::SimplifiedChinese, i18n::Language::English] {
        cx.update(|cx| i18n::apply(language, cx));
        hover(cx, window, 1);
        hover(cx, window, 2);
        cx.run_until_parked();
        cx.executor().advance_clock(OPEN_DELAY);
        support::settle(cx, |cx| {
            preview.read_with(cx, |view, _| view.active.is_some())
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            let bounds = window.find("line-preview").bounds();
            assert!(bounds.left() >= px(0.) && bounds.right() <= px(340.));
            window.click("line-preview-copy", cx);
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().as_deref(),
                Some("{\"incomplete\":")
            );
            window.click("line-preview-close", cx);
        })
        .unwrap();
    }
}
