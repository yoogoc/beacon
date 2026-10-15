use super::*;
use crate::feature_test_support as support;
use gpui_kit::test::TestWindowExt as _;
use serde_json::json;

fn object() -> Value {
    json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"yaml-save","namespace":"default","uid":"yaml-save","resourceVersion":"1","labels":{"keep":"label"}},"data":{"setting":"before","untouched":"keep"}})
}
fn open(
    cx: &mut TestAppContext,
    session: Arc<ClusterSession>,
) -> (AnyWindowHandle, Entity<DetailView>) {
    let object = Arc::new(serde_json::from_value(object()).unwrap());
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                let mut view = DetailView::new(
                    session,
                    Arc::new(support::kind("", "ConfigMap", "configmaps")),
                    object,
                    None,
                    window,
                    cx,
                );
                view.select(DetailTab::Yaml, window, cx);
                view
            })
        })
        .unwrap()
    });
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| matches!(view.yaml, Yaml::Ready))
    });
    (window, view)
}
fn draft(view: &Entity<DetailView>, value: &str, window: &mut Window, cx: &mut App) {
    view.update(cx, |view, cx| {
        let mut object = parse(&view.yaml_baseline).unwrap();
        object["data"]["setting"] = json!(value);
        view.yaml_editor.update(cx, |editor, cx| {
            editor.set_value(serde_saphyr::to_string(&object).unwrap(), window, cx)
        });
    });
}
fn reviewed(cx: &mut TestAppContext, window: AnyWindowHandle) {
    support::render_until(cx, window, |window| {
        window
            .try_find("confirm-yaml-review")
            .is_some_and(|button| button.visible())
    });
}

#[::core::prelude::v1::test]
fn yaml_patch_default_cancel_preserves_draft_and_confirm_sends_only_edits() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (fixture, session) = support::fixture(cx, "yaml-patch-default", vec![object()]);
    let (window, view) = open(cx, session);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(view.read(cx).save_method, SaveMethod::Patch);
        assert_eq!(window.find("apply").label(), Some("Save"));
        draft(&view, "mine", window, cx);
        window.click("apply", cx);
    })
    .unwrap();
    reviewed(cx, window);
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            window.find("confirm-yaml-review").label(),
            Some("Confirm & save")
        );
        window.click("cancel-yaml-review", cx);
    })
    .unwrap();
    support::settle(cx, |cx| !view.read_with(cx, |view, _| view.reviewing));
    assert!(view.read_with(cx, |view, cx| view.has_pending_edits(cx)));
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(line, _)| line.starts_with("PATCH"))
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("apply", cx);
    })
    .unwrap();
    reviewed(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click("confirm-yaml-review", cx)
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| {
            matches!(view.apply, Apply::Done) && matches!(view.yaml, Yaml::Ready)
        })
    });
    assert!(!view.read_with(cx, |view, cx| view.has_pending_edits(cx)));
    let saved = fixture.object("ConfigMap", "yaml-save");
    assert_eq!(saved["data"], json!({"setting":"mine","untouched":"keep"}));
    let requests = fixture.requests.lock().unwrap();
    let patches: Vec<_> = requests
        .iter()
        .filter(|(line, _)| line.starts_with("PATCH"))
        .collect();
    assert_eq!(patches.len(), 1);
    let patch = patches[0].1.as_array().unwrap();
    assert_eq!(patch.len(), 3);
    assert_eq!(
        patch[2],
        json!({"op":"replace","path":"/data/setting","value":"mine"})
    );
}

#[::core::prelude::v1::test]
fn yaml_patch_version_conflict_preserves_draft_and_review_latest_keeps_unedited_live_data() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (fixture, session) = support::fixture(cx, "yaml-patch-version", vec![object()]);
    let (window, view) = open(cx, session);
    let mut live = object();
    live["metadata"]["resourceVersion"] = json!("2");
    live["data"]["untouched"] = json!("someone else's change");
    live["data"]["added"] = json!("new key");
    fixture.put(live);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        draft(&view, "mine", window, cx);
        window.click("apply", cx);
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| matches!(view.apply, Apply::Stale(_)))
    });
    assert!(view.read_with(cx, |view, cx| view.has_pending_edits(cx)));
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(line, _)| line.starts_with("PATCH"))
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("save-version-conflict").visible());
        assert!(window.try_find("force-apply").is_none());
        window.click("review-latest-yaml", cx);
    })
    .unwrap();
    reviewed(cx, window);
    assert!(view.read_with(cx, |view, cx| {
        view.yaml_editor.read(cx).value().contains("mine")
    }));
    cx.update_window(window, |_, window, cx| {
        window.click("confirm-yaml-review", cx)
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| matches!(view.apply, Apply::Done))
    });
    assert_eq!(
        fixture.object("ConfigMap", "yaml-save")["data"],
        json!({"setting":"mine","untouched":"someone else's change","added":"new key"})
    );
}

#[::core::prelude::v1::test]
fn yaml_patch_rejects_a_race_after_review_without_force_or_automatic_retry() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (fixture, session) = support::fixture(cx, "yaml-patch-race", vec![object()]);
    let (window, view) = open(cx, session);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        draft(&view, "mine", window, cx);
        window.click("apply", cx);
    })
    .unwrap();
    reviewed(cx, window);
    let mut live = object();
    live["metadata"]["resourceVersion"] = json!("2");
    live["data"]["setting"] = json!("new live value");
    fixture.put(live.clone());
    cx.update_window(window, |_, window, cx| {
        window.click("confirm-yaml-review", cx)
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| matches!(view.apply, Apply::Stale(_)))
    });
    assert_eq!(fixture.object("ConfigMap", "yaml-save"), live);
    assert!(view.read_with(cx, |view, cx| view.has_pending_edits(cx)));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("force-apply").is_none());
    })
    .unwrap();
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(line, _)| line.starts_with("PATCH"))
            .count(),
        1
    );
}

#[::core::prelude::v1::test]
fn ssa_is_explicit_and_only_field_ownership_refusals_offer_reviewed_force() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (fixture, session) = support::fixture(cx, "yaml-save-ssa", vec![object()]);
    let (window, view) = open(cx, session);
    fixture.patch_error(Some(json!({"apiVersion":"v1","kind":"Status","code":409,"reason":"Conflict","message":"Apply failed with 1 conflict: conflict with \"kubectl\" using v1: .data.setting","details":{"causes":[{"reason":"FieldManagerConflict","field":".data.setting"}]}})));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        draft(&view, "mine", window, cx);
        window.click("save-method-ssa", cx);
        window.click("apply", cx);
    })
    .unwrap();
    reviewed(cx, window);
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            window.find("confirm-yaml-review").label(),
            Some("Confirm & apply (SSA)")
        );
        window.click("confirm-yaml-review", cx);
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| matches!(view.apply, Apply::Refused(_)))
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("force-apply", cx);
    })
    .unwrap();
    reviewed(cx, window);
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(line, _)| line.starts_with("PATCH"))
            .count(),
        1
    );
    fixture.patch_error(None);
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            window.find("confirm-yaml-review").label(),
            Some("Confirm force apply")
        );
        window.click("confirm-yaml-review", cx);
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| {
            matches!(view.apply, Apply::Done) && matches!(view.yaml, Yaml::Ready)
        })
    });
    {
        let requests = fixture.requests.lock().unwrap();
        let patches: Vec<_> = requests
            .iter()
            .filter(|(line, _)| line.starts_with("PATCH"))
            .collect();
        assert_eq!(patches.len(), 2);
        assert!(!patches[0].0.contains("force=true"));
        assert!(patches[1].0.contains("force=true"));
        assert!(patches[1].1.is_object());
    }
    fixture.patch_error(Some(json!({"apiVersion":"v1","kind":"Status","code":409,"reason":"Conflict","message":"the object has been modified; use the latest version"})));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        draft(&view, "next", window, cx);
        window.click("apply", cx);
    })
    .unwrap();
    reviewed(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click("confirm-yaml-review", cx)
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| matches!(view.apply, Apply::Stale(_)))
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("force-apply").is_none());
        assert!(window.find("review-latest-yaml").visible());
        window.click("save-method-patch", cx);
    })
    .unwrap();
    assert!(
        view.read_with(cx, |view, cx| view.save_method == SaveMethod::Patch
            && view.has_pending_edits(cx))
    );
}
