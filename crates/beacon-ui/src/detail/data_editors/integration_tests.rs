use super::*;
use crate::{data_syntax::Format, feature_test_support as support};
use gpui_kit::test::TestWindowExt as _;
use serde_json::json;
use std::time::Duration;

fn key<'a>(view: &'a DetailView, name: &str) -> &'a DataKey {
    let Data::Ready(keys) = &view.data else {
        panic!("Data not loaded")
    };
    keys.iter().find(|key| key.entry.key == name).unwrap()
}

fn pick(view: &Entity<DetailView>, name: &str, window: &mut Window, cx: &mut App) {
    view.update(cx, |view, cx| {
        let Data::Ready(keys) = &view.data else {
            panic!("Data not loaded")
        };
        let index = keys.iter().position(|key| key.entry.key == name).unwrap();
        view.select_data_key(index, window, cx);
    });
    window.render_frame(cx);
}

fn edit(view: &Entity<DetailView>, name: &str, value: &str, window: &mut Window, cx: &mut App) {
    pick(view, name, window, cx);
    let editor = key(view.read(cx), name).editor.clone().unwrap();
    editor.read(cx).focus_handle(cx).focus(window, cx);
    window.press(
        if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        },
        cx,
    );
    // Paste is one undo transaction; per-character typing may create several
    // undo groups depending on elapsed time in the rest of the workspace suite.
    cx.write_to_clipboard(ClipboardItem::new_string(value.into()));
    window.press(
        if cfg!(target_os = "macos") {
            "cmd-v"
        } else {
            "ctrl-v"
        },
        cx,
    );
}

fn reviewed(cx: &mut TestAppContext, window: AnyWindowHandle) {
    support::render_until(cx, window, |window| {
        window
            .try_find("confirm-yaml-review")
            .is_some_and(|button| button.visible())
    });
}

fn writes(fixture: &beacon_kube::test_support::Fixture) -> Vec<Value> {
    fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(line, _)| line.starts_with("PATCH"))
        .map(|(_, body)| body.clone())
        .collect()
}

#[::core::prelude::v1::test]
fn thousands_of_keys_use_virtual_rows_and_bounded_editors_without_losing_drafts() {
    let cx = &mut support::context();
    let data: serde_json::Map<String, Value> = (0..5000)
        .map(|i| (format!("key-{i:05}"), json!(format!("value {i}"))))
        .collect();
    let object = json!({"apiVersion":"v1", "kind":"ConfigMap", "metadata":{"name":"many-keys", "namespace":"default", "uid":"many-keys", "resourceVersion":"1"}, "data":data});
    let (fixture, session) = support::fixture(cx, "many-keys", vec![object.clone()]);
    let (window, view) = open(cx, session, "ConfigMap", object);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let state = view.read(cx);
        let Data::Ready(keys) = &state.data else {
            panic!()
        };
        assert_eq!(keys.len(), 5000);
        assert_eq!(keys.iter().filter(|key| key.editor.is_some()).count(), 1);
        assert_eq!(keys.iter().filter(|key| key._detect.is_some()).count(), 1);
        let mounted = (0..5000)
            .filter(|i| {
                window
                    .try_find(SharedString::from(format!("data-key-data:key-{i:05}")))
                    .is_some()
            })
            .count();
        assert!(mounted > 0 && mounted < 40, "mounted {mounted} rows");
        edit(&view, "key-00000", "unsaved", window, cx);
        window.render_frame(cx);
        for index in 1..30 {
            view.update(cx, |view, cx| view.select_data_key(index, window, cx));
        }
        let Data::Ready(keys) = &view.read(cx).data else {
            panic!()
        };
        assert!(keys.iter().filter(|key| key.editor.is_some()).count() <= 9);
        assert_eq!(
            key(view.read(cx), "key-00000")
                .editor
                .as_ref()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            "unsaved"
        );
        let search = view.read(cx).data_browser.search.clone().unwrap();
        search.read(cx).focus_handle(cx).focus(window, cx);
        window.input("KEY-04999", cx);
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| view.data_browser.visible.len() == 1)
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).data_browser.visible.as_ref(), &[4999]);
        window.click("data-key-data:key-04999", cx);
        assert_eq!(view.read(cx).data_browser.selected, Some(4999));
        assert!(view.read(cx).data_changed(cx));
        assert_eq!(
            view.read(cx).data_draft(cx).unwrap()["data"]["key-00000"],
            "unsaved"
        );
        // Restore the modified key and verify its editor undo history survived eviction.
        pick(&view, "key-00000", window, cx);
        let editor = key(view.read(cx), "key-00000").editor.clone().unwrap();
        editor.read(cx).focus_handle(cx).focus(window, cx);
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-z"
            } else {
                "ctrl-z"
            },
            cx,
        );
        assert_eq!(editor.read(cx).value().as_ref(), "value 0");
        window.render_frame(cx);
        assert!(!view.read(cx).data_changed(cx));
    })
    .unwrap();
    assert!(writes(&fixture).is_empty());
}

#[::core::prelude::v1::test]
fn deleted_text_and_binary_keys_are_restorable_and_saved_only_after_review() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let object = json!({"apiVersion":"v1", "kind":"ConfigMap", "metadata":{"name":"delete-keys", "namespace":"default", "uid":"delete-keys", "resourceVersion":"1", "labels":{"keep":"label"}}, "data":{"app.yaml":"before", "keep":"untouched", "remove.txt":"remove me"}, "binaryData":{"archive":"AP8="}});
    let (fixture, session) = support::fixture(cx, "delete-keys", vec![object.clone()]);
    let (window, view) = open(cx, session, "ConfigMap", object);
    cx.update_window(window, |_, window, cx| {
        edit(&view, "app.yaml", "after", window, cx);
        view.update(cx, |view, _| view.save_method = SaveMethod::ServerSideApply);
        window.click("delete-data-key", cx);
        assert_eq!(view.read(cx).save_method, SaveMethod::Patch);
        window.click("save-method-ssa", cx);
        assert_eq!(view.read(cx).save_method, SaveMethod::Patch);
        assert!(
            !view.read(cx).data_draft(cx).unwrap()["data"]
                .as_object()
                .unwrap()
                .contains_key("app.yaml")
        );
        window.click("restore-data-key", cx);
        assert_eq!(
            key(view.read(cx), "app.yaml")
                .editor
                .as_ref()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            "after"
        );
        pick(&view, "archive", window, cx);
        assert!(key(view.read(cx), "archive").editor.is_none());
        window.click("delete-data-key", cx);
        pick(&view, "remove.txt", window, cx);
        window.click("delete-data-key", cx);
        // The shared save method may have changed while visiting the YAML tab.
        view.update(cx, |view, cx| {
            view.save_method = SaveMethod::ServerSideApply;
            view.select(DetailTab::Overview, window, cx);
            view.select(DetailTab::Data, window, cx);
        });
        assert_eq!(view.read(cx).save_method, SaveMethod::Patch);
        let draft = view.read(cx).data_draft(cx).unwrap();
        assert_eq!(
            draft["data"],
            json!({"app.yaml":"after", "keep":"untouched"})
        );
        assert_eq!(draft["binaryData"], json!({}));
        assert!(view.read(cx).has_pending_edits(cx));
        window.click("save-data", cx);
    })
    .unwrap();
    reviewed(cx, window);
    assert!(writes(&fixture).is_empty());
    cx.update_window(window, |_, window, cx| {
        window.click("cancel-yaml-review", cx)
    })
    .unwrap();
    support::settle(cx, |cx| !view.read_with(cx, |view, _| view.reviewing));
    assert_eq!(
        view.read_with(cx, |view, _| view.data_browser.deleted.len()),
        2
    );
    cx.update_window(window, |_, window, cx| window.click("save-data", cx))
        .unwrap();
    reviewed(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click("confirm-yaml-review", cx)
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| matches!(view.apply, Apply::Done))
    });
    assert!(!view.read_with(cx, |view, cx| view.has_pending_edits(cx)));
    let saved = fixture.object("ConfigMap", "delete-keys");
    assert_eq!(
        saved["data"],
        json!({"app.yaml":"after", "keep":"untouched"})
    );
    assert_eq!(saved["binaryData"], json!({}));
    assert_eq!(saved["metadata"]["labels"]["keep"], "label");
    let patches = writes(&fixture);
    assert_eq!(patches.len(), 1);
    assert!(
        patches[0]
            .as_array()
            .unwrap()
            .contains(&json!({"op":"remove","path":"/binaryData/archive"}))
    );
    assert!(
        patches[0]
            .as_array()
            .unwrap()
            .contains(&json!({"op":"remove","path":"/data/remove.txt"}))
    );
    assert_eq!(
        patches[0][0],
        json!({"op":"test","path":"/metadata/uid","value":"delete-keys"})
    );
    assert_eq!(
        patches[0][1],
        json!({"op":"test","path":"/metadata/resourceVersion","value":"1"})
    );
}

#[::core::prelude::v1::test]
fn last_secret_key_requires_reveal_and_can_be_deleted_without_losing_save_controls() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let object = json!({"apiVersion":"v1", "kind":"Secret", "metadata":{"name":"delete-last", "namespace":"default", "uid":"delete-last", "resourceVersion":"1"}, "type":"Opaque", "data":{"token":"c2VjcmV0"}});
    let (fixture, session) = support::fixture(cx, "delete-last", vec![object.clone()]);
    let (window, view) = open(cx, session, "Secret", object);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(key(view.read(cx), "token").editor.is_none());
        window.click("delete-data-key", cx);
        assert!(!view.read(cx).has_pending_edits(cx));
        view.update(cx, |view, cx| view.delete_data_key(0, window, cx));
        assert!(!view.read(cx).has_pending_edits(cx));
        window.click("reveal", cx);
        window.click("delete-data-key", cx);
        assert_eq!(view.read(cx).data_draft(cx).unwrap()["data"], json!({}));
        assert!(window.find("save-data").visible());
        window.click("save-data", cx);
    })
    .unwrap();
    reviewed(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click("confirm-yaml-review", cx)
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| matches!(view.apply, Apply::Done))
    });
    let saved = fixture.object("Secret", "delete-last");
    assert_eq!(saved["data"], json!({}));
    assert_eq!(saved["type"], "Opaque");
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("save-data", cx);
        assert!(!view.read(cx).reviewing);
        assert!(view.read(cx).data_browser.deleted.is_empty());
        assert!(!view.read(cx).has_pending_edits(cx));
    })
    .unwrap();
    assert_eq!(writes(&fixture).len(), 1);
}

#[::core::prelude::v1::test]
fn deletion_rejects_stale_versions_and_review_latest_preserves_live_keys() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let mut object = json!({"apiVersion":"v1", "kind":"ConfigMap", "metadata":{"name":"delete-stale", "namespace":"default", "uid":"delete-stale", "resourceVersion":"1"}, "data":{"remove":"delete me", "keep":"original"}});
    let (fixture, session) = support::fixture(cx, "delete-stale", vec![object.clone()]);
    let (window, view) = open(cx, session, "ConfigMap", object.clone());
    cx.update_window(window, |_, window, cx| {
        pick(&view, "remove", window, cx);
        window.click("delete-data-key", cx);
    })
    .unwrap();
    object["metadata"]["resourceVersion"] = json!("2");
    object["data"]["keep"] = json!("live change");
    object["data"]["new"] = json!("live addition");
    fixture.put(object);
    cx.update_window(window, |_, window, cx| window.click("save-data", cx))
        .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| matches!(view.apply, Apply::Stale(_)))
    });
    assert!(writes(&fixture).is_empty());
    assert_eq!(
        view.read_with(cx, |view, _| view.data_browser.deleted.len()),
        1
    );
    cx.update_window(window, |_, window, cx| {
        window.click("review-latest-data", cx)
    })
    .unwrap();
    reviewed(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click("confirm-yaml-review", cx)
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| matches!(view.apply, Apply::Done))
    });
    assert_eq!(
        fixture.object("ConfigMap", "delete-stale")["data"],
        json!({"keep":"live change", "new":"live addition"})
    );
}

#[::core::prelude::v1::test]
fn read_only_and_busy_data_cannot_stage_deletions() {
    let cx = &mut support::context();
    let object = json!({"apiVersion":"v1", "kind":"ConfigMap", "metadata":{"name":"readonly-keys", "namespace":"default", "uid":"readonly-keys", "resourceVersion":"1"}, "data":{"key":"keep"}});
    let (fixture, session) = support::fixture(cx, "readonly-keys", vec![object.clone()]);
    let (window, view) = open(cx, session, "ConfigMap", object);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.reviewing = true;
            view.delete_data_key(0, window, cx);
            assert!(view.data_browser.deleted.is_empty());
            view.reviewing = false;
            view.rules = Some(Arc::new(Rules::default()));
            view.delete_data_key(0, window, cx);
            assert!(view.data_browser.deleted.is_empty());
            cx.notify();
        });
        window.render_frame(cx);
        window.click("delete-data-key", cx);
        window.click("save-data", cx);
        assert!(view.read(cx).data_browser.deleted.is_empty());
        assert!(!view.read(cx).has_pending_edits(cx));
    })
    .unwrap();
    assert!(writes(&fixture).is_empty());
}

#[track_caller]
fn detect(cx: &mut TestAppContext, view: &Entity<DetailView>, name: &str, format: Format) {
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(200));
    support::settle(cx, |cx| {
        view.read_with(cx, |view, cx| {
            let key = key(view, name);
            key.syntax.format == format
                && key
                    .editor
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .language_name()
                    .as_ref()
                    == format.language()
        })
    });
}

fn open(
    cx: &mut TestAppContext,
    session: Arc<ClusterSession>,
    kind: &str,
    object: Value,
) -> (AnyWindowHandle, Entity<DetailView>) {
    open_width(cx, session, kind, object, 1000.)
}

fn open_width(
    cx: &mut TestAppContext,
    session: Arc<ClusterSession>,
    kind: &str,
    object: Value,
    width: f32,
) -> (AnyWindowHandle, Entity<DetailView>) {
    let plural = if kind == "Secret" {
        "secrets"
    } else {
        "configmaps"
    };
    let mut object: DynamicObject = serde_json::from_value(object).unwrap();
    object.types = None;
    let object = Arc::new(object);
    cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(width), px(1000.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    let mut view = DetailView::new(
                        session,
                        Arc::new(support::kind("", kind, plural)),
                        object,
                        None,
                        window,
                        cx,
                    );
                    view.select(DetailTab::Data, window, cx);
                    view
                })
            },
        )
        .unwrap()
    })
}

#[::core::prelude::v1::test]
fn key_list_and_value_fit_narrow_and_wide_panels_without_overlap() {
    let cx = &mut support::context();
    let object = json!({"apiVersion":"v1", "kind":"ConfigMap", "metadata":{"name":"layout-keys", "namespace":"default", "uid":"layout-keys"}, "data":{"settings.yaml":"enabled: true\n"}});
    let (_fixture, session) = support::fixture(cx, "layout-keys", vec![object.clone()]);
    for width in [440., 1000.] {
        let (window, _view) = open_width(cx, session.clone(), "ConfigMap", object.clone(), width);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let row = window.find("data-key-data:settings.yaml").bounds();
            let editor = window.find("data-editor-data:settings.yaml").bounds();
            if width < 680. {
                assert!(
                    row.bottom() <= editor.top(),
                    "row {row:?} overlaps editor {editor:?}"
                );
            } else {
                assert!(
                    row.right() <= editor.left(),
                    "row {row:?} overlaps editor {editor:?}"
                );
            }
            assert!(editor.right() <= px(width));
            let save = window.find("save-data");
            assert!(save.visible());
            assert!(save.bounds().right() <= px(width));
        })
        .unwrap();
    }
}

#[::core::prelude::v1::test]
fn long_values_scroll_horizontally_and_wrap_toggle_preserves_cached_drafts_and_undo() {
    let cx = &mut support::context();
    let long = "configuration value 配置 ".repeat(100);
    let changed = format!("{long}edited");
    let object = json!({"apiVersion":"v1", "kind":"ConfigMap", "metadata":{"name":"wide-data", "namespace":"default", "uid":"wide-data"}, "data":{"long.txt":long, "other.txt":long}});
    let (fixture, session) = support::fixture(cx, "wide-data", vec![object.clone()]);
    let (window, view) = open_width(cx, session, "ConfigMap", object, 440.);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(!view.read(cx).data_browser.wrap);
        let editor = key(view.read(cx), "long.txt").editor.clone().unwrap();
        window.scroll(
            "data-editor-data:long.txt",
            ScrollDelta::Pixels(point(px(-180.), px(0.))),
            cx,
        );
        assert!(editor.read(cx).scroll_offset().x < px(0.));
        edit(&view, "long.txt", &changed, window, cx);
        window.click("data-wrap-lines", cx);
        assert!(view.read(cx).data_browser.wrap);
        assert_eq!(editor.read(cx).scroll_offset().x, px(0.));
        assert_eq!(editor.read(cx).value().as_ref(), changed);
        assert!(view.read(cx).has_pending_edits(cx));
        pick(&view, "other.txt", window, cx);
        let other = key(view.read(cx), "other.txt").editor.clone().unwrap();
        window.scroll(
            "data-editor-data:other.txt",
            ScrollDelta::Pixels(point(px(-180.), px(0.))),
            cx,
        );
        assert_eq!(other.read(cx).scroll_offset().x, px(0.));
        pick(&view, "long.txt", window, cx);
        assert_eq!(
            key(view.read(cx), "long.txt").editor.as_ref(),
            Some(&editor)
        );
        assert_eq!(editor.read(cx).value().as_ref(), changed);
        window.click("data-wrap-lines", cx);
        assert!(!view.read(cx).data_browser.wrap);
        window.scroll(
            "data-editor-data:long.txt",
            ScrollDelta::Pixels(point(px(-180.), px(0.))),
            cx,
        );
        assert!(editor.read(cx).scroll_offset().x < px(0.));
        editor.read(cx).focus_handle(cx).focus(window, cx);
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-z"
            } else {
                "ctrl-z"
            },
            cx,
        );
        assert_eq!(editor.read(cx).value().as_ref(), long);
        pick(&view, "other.txt", window, cx);
        window.scroll(
            "data-editor-data:other.txt",
            ScrollDelta::Pixels(point(px(-180.), px(0.))),
            cx,
        );
        assert!(other.read(cx).scroll_offset().x < px(0.));
        assert!(!view.read(cx).has_pending_edits(cx));
    })
    .unwrap();
    assert!(writes(&fixture).is_empty());
}

fn fold_and_unfold(view: &Entity<DetailView>, name: &str, window: &mut Window, cx: &mut App) {
    let entry = key(view.read(cx), name);
    let scope = SharedString::from(format!("data-editor-{}", id(&entry.entry)));
    let editor = entry.editor.clone().unwrap();
    let original = editor.read(cx).value();
    editor.read(cx).focus_handle(cx).focus(window, cx);
    let start = if cfg!(target_os = "macos") {
        "cmd-up"
    } else {
        "ctrl-home"
    };
    window.press(start, cx);
    // Exercise the actual parser-provided gutter control, without supplying
    // artificial fold ranges or changing the buffer contents.
    window
        .within(scope.clone())
        .click(("fold-icon", 0usize), cx);
    window.press("down", cx);
    assert!(editor.read(cx).cursor_position().line > 1);
    assert_eq!(editor.read(cx).value(), original);
    assert!(!view.read(cx).has_pending_edits(cx));
    window.within(scope).click(("fold-icon", 0usize), cx);
    window.press(start, cx);
    window.press("down", cx);
    assert_eq!(editor.read(cx).cursor_position().line, 1);
    assert_eq!(editor.read(cx).value(), original);
}

#[::core::prelude::v1::test]
fn configmap_content_highlights_and_resize_preserve_drafts_and_undo() {
    let cx = &mut support::context();
    let json = "{\n  \"enabled\": true,\n  \"name\": \"配置\"\n}\n";
    let ini = "# 说明\n[server]\nname=\"应用\"\nport=80\n";
    let object = json!({"apiVersion":"v1", "kind":"ConfigMap", "metadata":{"name":"syntax", "namespace":"default", "uid":"syntax"}, "data":{"settings":json, "server.ini":ini}});
    let (_fixture, session) = support::fixture(cx, "data-syntax", vec![object.clone()]);
    let (window, view) = open(cx, session, "ConfigMap", object);
    cx.update_window(window, |_, window, cx| pick(&view, "settings", window, cx))
        .unwrap();
    detect(cx, &view, "settings", Format::Json);
    cx.update_window(window, |_, window, cx| {
        pick(&view, "server.ini", window, cx)
    })
    .unwrap();
    detect(cx, &view, "server.ini", Format::Ini);
    view.read_with(cx, |view, cx| {
        assert!(
            !key(view, "server.ini")
                .decorations
                .as_ref()
                .unwrap()
                .get_ranges(cx)
                .is_empty()
        );
        assert!(!view.has_pending_edits(cx));
    });
    let (height, other_height) = view.read_with(cx, |view, _| {
        (
            view.data_heights["data:settings"],
            view.data_heights["data:server.ini"],
        )
    });
    cx.update_window(window, |_, window, cx| {
        pick(&view, "settings", window, cx);
        fold_and_unfold(&view, "settings", window, cx);
        window.render_frame(cx);
        let before = window
            .find("data-editor-data:settings")
            .bounds()
            .size
            .height;
        let handle = window.find("resize-data-data:settings").bounds().center();
        window.drag(handle, handle + point(px(0.), px(120.)), cx);
        window.render_frame(cx);
        assert_eq!(
            window
                .find("data-editor-data:settings")
                .bounds()
                .size
                .height,
            before + px(120.)
        );
        assert_eq!(
            view.read(cx).data_heights["data:settings"],
            height + px(120.)
        );
        assert_eq!(view.read(cx).data_heights["data:server.ini"], other_height);
        assert!(view.read(cx).data_resize.is_none());
        let handle = window.find("resize-data-data:settings").bounds().center();
        window.drag(handle, handle - point(px(0.), px(1000.)), cx);
        assert_eq!(view.read(cx).data_heights["data:settings"], px(64.));
        view.update(cx, |view, cx| {
            view.select(DetailTab::Overview, window, cx);
            view.select(DetailTab::Data, window, cx);
        });
        assert_eq!(view.read(cx).data_heights["data:settings"], px(64.));
        let editor = key(view.read(cx), "settings").editor.clone().unwrap();
        assert_eq!(editor.read(cx).value().as_ref(), json);
        editor.read(cx).focus_handle(cx).focus(window, cx);
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            },
            cx,
        );
        cx.write_to_clipboard(ClipboardItem::new_string(
            "enabled: false\nname: 配置\n".into(),
        ));
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-v"
            } else {
                "ctrl-v"
            },
            cx,
        );
        assert_eq!(
            editor.read(cx).value().as_ref(),
            "enabled: false\nname: 配置\n"
        );
    })
    .unwrap();
    detect(cx, &view, "settings", Format::Yaml);
    view.read_with(cx, |view, cx| {
        assert_eq!(
            key(view, "settings")
                .editor
                .as_ref()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            "enabled: false\nname: 配置\n"
        );
        assert!(view.has_pending_edits(cx));
    });
    cx.update_window(window, |_, window, cx| {
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-z"
            } else {
                "ctrl-z"
            },
            cx,
        )
    })
    .unwrap();
    view.read_with(cx, |view, cx| {
        assert_eq!(
            key(view, "settings")
                .editor
                .as_ref()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            json
        );
    });
    detect(cx, &view, "settings", Format::Json);
    view.read_with(cx, |view, cx| {
        assert_eq!(
            key(view, "settings")
                .editor
                .as_ref()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            json
        );
        assert!(!view.has_pending_edits(cx));
    });
}

#[::core::prelude::v1::test]
fn secret_highlights_decoded_text_only_after_reveal_and_preserves_binary_values() {
    let cx = &mut support::context();
    let text = "{\n  \"auth\": \"配置\",\n  \"enabled\": true\n}\n";
    let encoded = data::encode(beacon_kube::DataField::Data, true, text);
    let object = json!({"apiVersion":"v1", "kind":"Secret", "metadata":{"name":"syntax", "namespace":"default", "uid":"syntax"}, "type":"Opaque", "data":{"settings":encoded, "binary":"/w=="}});
    let (fixture, session) = support::fixture(cx, "secret-syntax", vec![object.clone()]);
    let (window, view) = open(cx, session, "Secret", object);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("data-editor-data:settings").is_none());
        assert!(window.try_find("resize-data-data:settings").is_none());
        assert!(key(view.read(cx), "binary").editor.is_none());
        assert!(key(view.read(cx), "settings")._detect.is_none());
        window.click("reveal", cx);
    })
    .unwrap();
    detect(cx, &view, "settings", Format::Json);
    cx.update_window(window, |_, window, cx| {
        fold_and_unfold(&view, "settings", window, cx);
        window.render_frame(cx);
        let height = window
            .find("data-editor-data:settings")
            .bounds()
            .size
            .height;
        let handle = window.find("resize-data-data:settings").bounds().center();
        window.drag(handle, handle + point(px(0.), px(100.)), cx);
        window.render_frame(cx);
        assert_eq!(
            window
                .find("data-editor-data:settings")
                .bounds()
                .size
                .height,
            height + px(100.)
        );
        assert_eq!(
            key(view.read(cx), "settings")
                .editor
                .as_ref()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            text
        );
        assert!(!view.read(cx).has_pending_edits(cx));
        window.click("reveal", cx);
        window.render_frame(cx);
        assert!(window.try_find("data-editor-data:settings").is_none());
        assert!(window.try_find("resize-data-data:binary").is_none());
    })
    .unwrap();
    assert_eq!(
        fixture.object("Secret", "syntax")["data"]["settings"],
        encoded
    );
    assert_eq!(fixture.object("Secret", "syntax")["data"]["binary"], "/w==");
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(line, _)| line.starts_with("PATCH"))
    );
}
