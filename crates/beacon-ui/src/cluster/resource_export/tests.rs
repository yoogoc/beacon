use super::*;
use crate::feature_test_support as support;
use beacon_kube::test_support::Fixture;
use gpui_kit::test::TestWindowExt as _;
use serde_json::{Value, json};

fn object(name: &str) -> Value {
    json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":"default","uid":name,"resourceVersion":"7"},"data":{"content":"complete content"}})
}
fn open(
    cx: &mut TestAppContext,
    objects: Vec<Value>,
) -> (Fixture, AnyWindowHandle, Entity<ClusterView>) {
    let (fixture, session) = support::fixture(cx, "resource-export", objects);
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Point::default(),
                    size(px(1200.), px(700.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    ClusterView::new(
                        session,
                        Some("default".into()),
                        Some(Arc::new(support::kind("", "ConfigMap", "configmaps"))),
                        None,
                        false,
                        window,
                        cx,
                    )
                })
            },
        )
        .unwrap()
    });
    support::settle(cx, |cx| {
        view.read_with(cx, |view, cx| view.counts(cx).0 > 0)
    });
    view.update(cx, |view, cx| {
        view.table.update(cx, |table, cx| {
            table.delegate_mut().toggle_all_visible();
            cx.notify();
        });
        cx.notify();
    });
    draw(cx, window);
    (fixture, window, view)
}
fn draw(cx: &mut TestAppContext, window: AnyWindowHandle) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }
}
fn export(cx: &mut TestAppContext, window: AnyWindowHandle) {
    cx.update_window(window, |_, window, cx| window.click("export-selected", cx))
        .unwrap();
}
fn finish(cx: &mut TestAppContext, view: &Entity<ClusterView>) {
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| !view.resource_exporting)
    });
}

#[::core::prelude::v1::test]
fn export_toolbar_saves_single_resource_as_yaml_and_preserves_selection() {
    let cx = &mut support::context();
    let dir = tempfile::tempdir().unwrap();
    support::workspace(cx, dir.path());
    let (_fixture, window, view) = open(cx, vec![object("settings")]);
    export(cx, window);
    let path = dir.path().join("settings.yaml");
    let destination = path.clone();
    cx.simulate_new_path_selection(move |_| Some(destination));
    finish(cx, &view);
    let value: Value = serde_saphyr::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(value["data"]["content"], "complete content");
    view.read_with(cx, |view, cx| {
        assert_eq!(view.table.read(cx).delegate().selected_count(), 1);
        assert!(
            matches!(&view.outcome,Some(Outcome::Done(message)) if message.contains("1 resources"))
        );
    });
}
#[::core::prelude::v1::test]
fn bulk_export_uses_the_frozen_selection_after_resource_navigation() {
    let cx = &mut support::context();
    let dir = tempfile::tempdir().unwrap();
    support::workspace(cx, dir.path());
    let (fixture, window, view) = open(cx, vec![object("web"), object("worker")]);
    export(cx, window);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.show(Arc::new(support::kind("", "Pod", "pods")), window, cx)
        });
    })
    .unwrap();
    let path = dir.path().join("resources.zip");
    let destination = path.clone();
    cx.simulate_new_path_selection(move |_| Some(destination));
    finish(cx, &view);
    let bytes = std::fs::read(path).unwrap();
    assert!(bytes.starts_with(b"PK\x03\x04"));
    for name in ["web", "worker"] {
        assert!(fixture.requests.lock().unwrap().iter().any(|(url, _)| {
            url.starts_with(&format!("GET /api/v1/namespaces/default/configmaps/{name}"))
        }));
    }
    assert!(view.read_with(cx, |view, _| matches!(view.outcome, Some(Outcome::Done(_)))));
}
#[::core::prelude::v1::test]
fn cancelling_and_failed_reads_do_not_overwrite_local_files() {
    let cx = &mut support::context();
    let dir = tempfile::tempdir().unwrap();
    support::workspace(cx, dir.path());
    let (fixture, window, view) = open(cx, vec![object("web"), object("worker")]);
    export(cx, window);
    cx.simulate_new_path_selection(|_| None);
    finish(cx, &view);
    assert!(view.read_with(cx, |view, _| view.outcome.is_none()));
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(url, _)| url.starts_with("GET /api/v1/namespaces/default/configmaps/"))
    );
    fixture.deny("get/configmaps/worker");
    draw(cx, window);
    export(cx, window);
    let path = dir.path().join("previous.zip");
    std::fs::write(&path, b"keep existing export").unwrap();
    let destination = path.clone();
    cx.simulate_new_path_selection(move |_| Some(destination));
    finish(cx, &view);
    assert_eq!(std::fs::read(path).unwrap(), b"keep existing export");
    assert!(view.read_with(cx,|view,_| matches!(&view.outcome,Some(Outcome::Failed(message)) if message.contains("worker"))));
}
