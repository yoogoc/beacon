use super::*;
use crate::feature_test_support as support;
use beacon_kube::test_support::HelmFixture;
use gpui_kit::test::TestWindowExt as _;
use serde_json::{Value, json};
use std::{cell::Cell, rc::Rc};

fn context() -> (TestAppContext, tempfile::TempDir) {
    let cx = support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(&cx, directory.path());
    (cx, directory)
}
type Scene = (
    Arc<beacon_kube::test_support::Fixture>,
    Arc<ClusterSession>,
    AnyWindowHandle,
    Entity<OperationView>,
    Rc<Cell<usize>>,
);
fn setup(
    cx: &mut TestAppContext,
    fake: &HelmFixture,
    objects: Vec<Value>,
    change: Change,
    values: Value,
) -> Scene {
    let (api, session) = support::fixture(cx, "helm-operations", objects);
    let session = Arc::new(
        Arc::try_unwrap(session)
            .unwrap_or_else(|_| panic!("fixture session must be unique"))
            .with_helm_test_program(fake.program.clone()),
    );
    let complete = Rc::new(Cell::new(0));
    let done = complete.clone();
    let (window, view) = cx.update(|cx| {
        operation_window(
            session.clone(),
            Identity {
                name: "web".into(),
                namespace: "default".into(),
            },
            change,
            values,
            Bounds::centered(None, size(px(1080.), px(760.)), cx),
            Box::new(move |_| done.set(done.get() + 1)),
            cx,
        )
        .unwrap()
    });
    (Arc::new(api), session, window, view, complete)
}
fn chart() -> Chart {
    Chart {
        reference: "repo/web".into(),
        version: "2.0.0".into(),
        repository: String::new(),
    }
}
fn wait_review(cx: &mut TestAppContext, view: &Entity<OperationView>) {
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| {
            view.stage == Stage::Review && view.diff.is_some()
                || view.stage == Stage::Draft && view.message.is_some()
        })
    });
}
#[::core::prelude::v1::test]
fn installation_requires_review_and_confirmation_and_refreshes_release_history() {
    let (mut cx, _directory) = context();
    let fake = HelmFixture::new();
    let (api, session, window, view, done) = setup(
        &mut cx,
        &fake,
        vec![],
        Change::Install(chart()),
        json!({"password":"private-password","replicas":2}),
    );
    let main = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                let mut view = crate::cluster::ClusterView::new(
                    session.clone(),
                    None,
                    None,
                    None,
                    false,
                    window,
                    cx,
                );
                view.show_mode(crate::cluster::Mode::Releases, window, cx);
                view
            })
        })
        .unwrap()
        .0
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-find-versions", cx);
    })
    .unwrap();
    support::settle(&mut cx, |cx| view.read_with(cx, |view, _| !view.searching));
    view.read_with(&cx, |view, cx| {
        assert_eq!(view.version.read(cx).value().as_ref(), "2.0.0")
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-prepare-operation", cx);
    })
    .unwrap();
    wait_review(&mut cx, &view);
    view.read_with(&cx, |view, _| {
        assert!(view.stage == Stage::Review, "{:?}", view.message);
        let plan = view.plan.as_ref().unwrap();
        assert!(plan.after_manifest.contains("Secret"));
        let text = review_text(
            plan.after_values.clone(),
            &plan.after_manifest,
            Document::Manifest,
            false,
        )
        .unwrap();
        assert!(!text.contains("private-base64"));
        assert!(text.contains("concealed"));
    });
    assert!(
        !api.requests
            .lock()
            .unwrap()
            .iter()
            .any(|(line, _)| line.starts_with("POST /api/v1/namespaces/default/secrets "))
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-review-Values", cx);
    })
    .unwrap();
    wait_review(&mut cx, &view);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        // Changed inputs cannot retarget an already reviewed, immutable plan.
        view.update(cx, |view, cx| {
            view.name
                .update(cx, |name, cx| name.set_value("unreviewed-name", window, cx))
        });
        window.click("helm-confirm-operation", cx);
        assert_eq!(pending(cx).len(), 1);
        window.dispatch_action(Box::new(crate::app::CloseTab), cx);
    })
    .unwrap();
    assert!(cx.windows().contains(&window));
    support::settle(&mut cx, |cx| {
        view.read_with(cx, |view, _| view.stage == Stage::Finished)
    });
    assert_eq!(done.get(), 1);
    support::render_until(&mut cx, main, |window| {
        window
            .try_find("helm-release-default-web")
            .is_some_and(|row| row.visible())
    });
    cx.update_window(main, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-release-default-web", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let details = cx
        .windows()
        .into_iter()
        .find(|candidate| *candidate != main && *candidate != window)
        .expect("refreshed release detail window");
    support::render_until(&mut cx, details, |window| {
        window
            .try_find("helm-document")
            .is_some_and(|document| document.visible())
    });
    let history = cx
        .read(|cx| Bridge::global(cx).handle())
        .block_on(helm::history(
            session.client(),
            &Identity {
                name: "web".into(),
                namespace: "default".into(),
            },
        ))
        .unwrap();
    assert_eq!(
        history.revisions[0]
            .revision
            .as_ref()
            .unwrap()
            .summary
            .status,
        "deployed"
    );
    assert!(api.requests.lock().unwrap().iter().any(|(_, value)| {
        value.pointer("/metadata/name").and_then(Value::as_str) == Some("sh.helm.release.v1.web.v1")
    }));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-operation-close", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!cx.windows().contains(&window));
}
#[::core::prelude::v1::test]
fn upgrades_keep_values_covered_allow_draft_edits_and_preserve_cancelled_plans() {
    let (mut cx, _directory) = context();
    let fake = HelmFixture::new();
    let old = super::super::integration_tests::stored(1, "deployed");
    let (_api, session, window, view, _done) = setup(
        &mut cx,
        &fake,
        vec![old],
        Change::Upgrade(chart()),
        json!({"password":"private-password","replicas":1}),
    );
    let (_details_window, details) = cx.update(|cx| {
        super::super::open_window(
            session.clone(),
            Identity {
                name: "web".into(),
                namespace: "default".into(),
            },
            Bounds::centered(None, size(px(1120.), px(760.)), cx),
            cx,
        )
        .unwrap()
    });
    support::settle(&mut cx, |cx| {
        details.read_with(cx, |view, _| !view.loading && !view.preparing)
    });
    view.read_with(&cx, |view, cx| {
        assert!(!view.values_visible);
        assert!(view.values.read(cx).value().is_empty());
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-edit-reveal", cx);
    })
    .unwrap();
    view.read_with(&cx, |view, cx| {
        assert!(view.values.read(cx).value().contains("private-password"))
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.values.update(cx, |editor, cx| {
                editor.set_value("password: private-password\nreplicas: 2\n", window, cx)
            })
        });
        window.render_frame(cx);
        window.click("helm-prepare-operation", cx);
    })
    .unwrap();
    wait_review(&mut cx, &view);
    view.read_with(&cx, |view, _| {
        assert_eq!(view.plan.as_ref().unwrap().after_values["replicas"], 2)
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-review-back", cx);
    })
    .unwrap();
    view.read_with(&cx, |view, cx| {
        assert!(view.stage == Stage::Draft && view.plan.is_none());
        assert!(view.values.read(cx).value().contains("replicas: 2"));
    });
    assert!(
        !std::fs::read_to_string(fake.records)
            .unwrap()
            .contains("--wait")
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-prepare-operation", cx);
    })
    .unwrap();
    wait_review(&mut cx, &view);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-confirm-operation", cx);
    })
    .unwrap();
    support::settle(&mut cx, |cx| {
        view.read_with(cx, |view, _| view.stage == Stage::Finished)
    });
    support::settle(&mut cx, |cx| {
        details.read_with(cx, |view, _| {
            !view.loading && !view.preparing && view.revision == 2
        })
    });
    details.read_with(&cx, |view, _| {
        assert_eq!(view.selected().unwrap().summary.chart, "web-2.0.0");
        assert_eq!(view.history.as_ref().unwrap().revisions.len(), 2);
    });
}
#[::core::prelude::v1::test]
fn rollback_uninstall_and_failed_operations_show_their_outcomes_without_secret_output() {
    for change in [Change::Rollback(1), Change::Uninstall] {
        let (mut cx, _directory) = context();
        let fake = HelmFixture::new();
        let objects = vec![
            super::super::integration_tests::stored(1, "superseded"),
            super::super::integration_tests::stored(3, "deployed"),
        ];
        let (_api, _session, window, view, done) = setup(
            &mut cx,
            &fake,
            objects,
            change,
            json!({"password":"private-password"}),
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("helm-prepare-operation", cx);
        })
        .unwrap();
        wait_review(&mut cx, &view);
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("helm-confirm-operation", cx);
        })
        .unwrap();
        support::settle(&mut cx, |cx| {
            view.read_with(cx, |view, _| view.stage == Stage::Finished)
        });
        assert_eq!(done.get(), 1);
        view.read_with(&cx, |view, _| {
            assert!(view.message.as_ref().unwrap().contains("completed"))
        });
    }
    let (mut cx, _directory) = context();
    let fake = HelmFixture::new();
    let (_api, _session, window, view, done) = setup(
        &mut cx,
        &fake,
        vec![],
        Change::Install(chart()),
        json!({"password":"private-password"}),
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-prepare-operation", cx);
    })
    .unwrap();
    wait_review(&mut cx, &view);
    std::fs::write(fake.directory.path().join("fail"), "").unwrap();
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("helm-confirm-operation", cx);
    })
    .unwrap();
    support::settle(&mut cx, |cx| {
        view.read_with(cx, |view, _| view.stage == Stage::Finished)
    });
    assert_eq!(done.get(), 1);
    view.read_with(&cx, |view, _| {
        let message = view.message.as_ref().unwrap();
        assert!(message.contains("mocked permission failure"));
        assert!(!message.contains("private-password"));
        assert!(!message.contains("private-base64"));
    });
}
