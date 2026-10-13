use super::*;
use crate::feature_test_support as support;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use gpui_kit::test::TestWindowExt as _;
use serde_json::{Value, json};

fn stored(number: i64, status: &str) -> Value {
    let payload = json!({
        "name":"web", "namespace":"default", "version":number,
        "info":{"status":status,"last_deployed":format!("2026-10-{number:02}T10:00:00Z")},
        "chart":{"metadata":{"name":"web","version":format!("1.{number}.0"),"appVersion":format!("v{number}")}},
        "config":{"password":format!("private-value-{number}"),"replicas":number},
        "manifest":format!("apiVersion: v1\nkind: Secret\nmetadata:\n  name: credentials\ndata:\n  token: private-base64-{number}\n---\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  replicas: {number}\n")
    });
    json!({"apiVersion":"v1","kind":"Secret","type":"helm.sh/release.v1",
        "metadata":{"name":format!("sh.helm.release.v1.web.v{number}"),"namespace":"default","labels":{"owner":"helm","name":"web","version":number.to_string()}},
        "data":{"release":STANDARD.encode(STANDARD.encode(serde_json::to_vec(&payload).unwrap()))}})
}
fn inspector(
    cx: &mut TestAppContext,
    session: Arc<ClusterSession>,
) -> (AnyWindowHandle, Entity<HelmDetails>) {
    cx.update(|cx| {
        open_window(
            session,
            Identity {
                name: "web".into(),
                namespace: "default".into(),
            },
            Bounds::centered(None, size(px(1120.), px(760.)), cx),
            cx,
        )
        .unwrap()
    })
}
fn settled(cx: &mut TestAppContext, view: &Entity<HelmDetails>) {
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| !view.loading && !view.preparing)
    });
}
#[::core::prelude::v1::test]
fn release_documents_reveal_switch_history_and_compare_without_writes() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let mut damaged = stored(4, "failed");
    damaged["data"]["release"] = json!("corrupted");
    let (api, session) = support::fixture(
        cx,
        "helm-inspector",
        vec![stored(1, "superseded"), stored(3, "deployed"), damaged],
    );
    let (window, view) = inspector(cx, session);
    settled(cx, &view);
    view.read_with(cx, |view, _| {
        assert_eq!(view.history.as_ref().unwrap().revisions.len(), 3);
        assert_eq!(view.revision, 4);
        assert!(view.content_error.as_ref().unwrap().contains("damaged"));
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-tab-History", cx);
        window.render_frame(cx);
        assert!(window.find("helm-history-error-4").visible());
        window.click("helm-revision-3", cx);
    })
    .unwrap();
    settled(cx, &view);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.revision, 3);
        let text = view.editor.read(cx).value();
        assert!(text.contains("password") && text.contains("concealed"));
        assert!(!text.contains("private-value"));
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-reveal", cx);
    })
    .unwrap();
    settled(cx, &view);
    view.read_with(cx, |view, cx| {
        assert!(view.editor.read(cx).value().contains("private-value-3"))
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-tab-Manifest", cx);
    })
    .unwrap();
    settled(cx, &view);
    view.read_with(cx, |view, cx| {
        assert!(view.editor.read(cx).value().contains("private-base64-3"))
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-reveal", cx);
    })
    .unwrap();
    settled(cx, &view);
    view.read_with(cx, |view, cx| {
        let text = view.editor.read(cx).value();
        assert!(!text.contains("private-base64"));
        assert!(text.contains("Deployment"));
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-tab-Compare revisions", cx);
    })
    .unwrap();
    settled(cx, &view);
    view.read_with(cx, |view, cx| {
        assert_eq!((view.from, view.to), (1, 3));
        let diff = view.diff.as_ref().unwrap().read(cx);
        assert_eq!(diff.mode(), DiffMode::Split);
        assert!(diff.files().iter().any(|file| file.additions() > 0));
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-compare-Manifest", cx);
    })
    .unwrap();
    settled(cx, &view);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("helm-diff").visible());
        window.click("helm-mode-Unified", cx);
        view.update(cx, |view, cx| {
            view.to_picker
                .update(cx, |_, cx| cx.emit(SelectEvent::Confirm(Some(1))));
        });
    })
    .unwrap();
    settled(cx, &view);
    view.read_with(cx, |view, _| {
        assert_eq!(view.mode, DiffMode::Unified);
        assert!(view.diff.is_none());
        assert!(
            view.content_error
                .as_ref()
                .unwrap()
                .contains("different revisions")
        );
    });
    assert!(
        api.requests
            .lock()
            .unwrap()
            .iter()
            .all(|(request, _)| request.starts_with("GET "))
    );
}
#[::core::prelude::v1::test]
fn permission_errors_and_removed_releases_have_copyable_empty_states() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (api, session) = support::fixture(cx, "helm-denied", vec![stored(1, "deployed")]);
    api.deny("secrets");
    let (window, view) = inspector(cx, session);
    settled(cx, &view);
    view.read_with(cx, |view, _| {
        assert!(view.error.as_ref().unwrap().contains("403"))
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("helm-error").visible());
    })
    .unwrap();
    let (_api, session) = support::fixture(cx, "helm-empty", vec![]);
    let (window, view) = inspector(cx, session);
    settled(cx, &view);
    view.read_with(cx, |view, _| {
        assert!(
            view.content_error
                .as_ref()
                .unwrap()
                .contains("No stored revisions")
        )
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("helm-error").visible());
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn clicking_the_release_list_opens_an_independent_details_window() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (_api, session) = support::fixture(cx, "helm-list-entry", vec![stored(1, "deployed")]);
    let origin = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, move |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                let mut view =
                    crate::cluster::ClusterView::new(session, None, None, None, false, window, cx);
                view.show_mode(crate::cluster::Mode::Releases, window, cx);
                view
            })
        })
        .unwrap()
        .0
    });
    support::render_until(cx, origin, |window| {
        window
            .try_find("helm-release-default-web")
            .is_some_and(|row| row.visible())
    });
    cx.update_window(origin, |_, window, cx| {
        window.click("helm-release-default-web", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let details = cx
        .windows()
        .into_iter()
        .find(|window| *window != origin)
        .expect("release details window");
    support::render_until(cx, details, |window| {
        window
            .try_find("helm-document")
            .is_some_and(|editor| editor.visible())
    });
    assert_eq!(
        VisualTestContext::from_window(details, cx)
            .window_title()
            .as_deref(),
        Some("Beacon — Helm · default / web")
    );
    assert!(cx.windows().contains(&origin));
    cx.dispatch_action(details, crate::app::CloseTab);
    cx.run_until_parked();
    assert!(!cx.windows().contains(&details));
    assert!(cx.windows().contains(&origin));
}
