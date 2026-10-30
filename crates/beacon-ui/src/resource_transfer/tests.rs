use super::*;
use crate::feature_test_support as support;
use beacon_kube::{ApiResource, GroupVersionKind, Kind, ObjectRef, test_support::Fixture};
use gpui_kit::test::TestWindowExt as _;
use serde_json::json;
fn config(ns: &str, name: &str) -> serde_json::Value {
    json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":ns,"uid":format!("{ns}-{name}"),"resourceVersion":"7"},"data":{"config":"example"}})
}
fn kind(name: &str, plural: &str) -> Kind {
    Kind {
        resource: ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk("", "v1", name), plural),
        namespaced: name != "Namespace",
        verbs: vec![
            "get".into(),
            "create".into(),
            "delete".into(),
            "list".into(),
            "watch".into(),
        ],
    }
}
fn fixture(
    cx: &mut TestAppContext,
    source: serde_json::Value,
    target: Vec<serde_json::Value>,
) -> (Fixture, Fixture, Arc<ClusterSession>, Arc<ClusterSession>) {
    let runtime = cx.read(|cx| Bridge::global(cx).handle());
    runtime.block_on(async {
        let a = Fixture::start(vec![
            source,
            json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"source"}}),
        ])
        .await;
        let mut target = target;
        target.push(json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"target"}}));
        let b = Fixture::start(target).await;
        let kinds = vec![
            kind("ConfigMap", "configmaps"),
            kind("Secret", "secrets"),
            kind("Namespace", "namespaces"),
        ];
        let left = Arc::new(ClusterSession::for_testing(
            ClusterId::new("source-cluster"),
            a.url.clone(),
            kinds.clone(),
        ));
        let right = Arc::new(ClusterSession::for_testing(
            ClusterId::new("target-cluster"),
            b.url.clone(),
            kinds,
        ));
        (a, b, left, right)
    })
}
fn open(
    cx: &mut TestAppContext,
    source: Arc<ClusterSession>,
    target: Arc<ClusterSession>,
    object: serde_json::Value,
) -> (AnyWindowHandle, Entity<TransferView>) {
    cx.update(|cx| {
        let shared = cx.global::<SharedConnections>().0.clone();
        shared.update(cx, |connections, _| {
            connections
                .sessions
                .insert(target.id().clone(), target.clone());
        });
    });
    let original: beacon_kube::DynamicObject = serde_json::from_value(object).unwrap();
    let selection = Selection {
        kind: source
            .discovery()
            .find(&original.types.as_ref().unwrap().kind)
            .unwrap()
            .clone(),
        reference: ObjectRef::of(&original),
        uid: original.metadata.uid,
        name: original.metadata.name.unwrap(),
        copy_only: false,
    };
    let (handle, view) = cx.update(|cx| {
        open_window(
            source,
            vec![selection],
            Bounds::new(Point::default(), size(px(800.), px(800.))),
            cx,
        )
        .unwrap()
    });
    support::settle(cx, |cx| view.read_with(cx, |view, _| !view.loading));
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.cluster.update(cx, |picker, cx| {
                picker.set_selected_value(target.id(), window, cx)
            });
            view.invalidate(cx);
            view.load_target(window, cx);
        })
    })
    .unwrap();
    support::settle(cx, |cx| view.read_with(cx, |view, _| !view.loading));
    draw(cx, handle);
    (handle, view)
}
fn draw(cx: &mut TestAppContext, window: AnyWindowHandle) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }
}
fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: impl Into<ElementId>) {
    cx.update_window(window, |_, window, cx| window.click(id, cx))
        .unwrap();
    draw(cx, window);
}
fn idle(cx: &mut TestAppContext, window: AnyWindowHandle, view: &Entity<TransferView>) {
    support::settle(cx, |cx| view.read_with(cx, |view, _| !view.busy));
    draw(cx, window);
}
#[::core::prelude::v1::test]
fn conflict_rename_and_cross_cluster_move_require_separate_source_confirmation() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let object = config("source", "app");
    let (a, b, left, right) = fixture(cx, object.clone(), vec![config("target", "app")]);
    let (window, view) = open(cx, left, right, object);
    click(cx, window, "transfer-mode-move");
    click(cx, window, "transfer-check-target");
    idle(cx, window, &view);
    cx.update_window(window, |_, window, cx| {
        assert!(!view.read(cx).batch.as_ref().unwrap().ready());
        window.click("transfer-create-target", cx);
        assert!(view.read(cx).batch.as_ref().unwrap().items()[0].conflict);
    })
    .unwrap();
    click(cx, window, ("transfer-conflict-rename", 0usize));
    click(cx, window, "transfer-check-target");
    idle(cx, window, &view);
    click(cx, window, "transfer-create-target");
    idle(cx, window, &view);
    cx.update_window(window, |_, window, cx| {
        assert!(view.read(cx).batch.as_ref().unwrap().can_cleanup());
        assert!(!view.read(cx).cleanup_confirmed);
        window.click("transfer-delete-sources", cx);
    })
    .unwrap();
    assert!(
        !a.requests
            .lock()
            .unwrap()
            .iter()
            .any(|(url, _)| url.starts_with("DELETE"))
    );
    click(cx, window, "transfer-cleanup-confirmation");
    click(cx, window, "transfer-delete-sources");
    idle(cx, window, &view);
    assert!(view.read_with(cx, |view, _| view.batch.as_ref().unwrap().complete()));
    assert_eq!(b.object("ConfigMap", "app")["data"]["config"], "example");
    assert!(b.object("ConfigMap", "app-copy").is_object());
}
#[::core::prelude::v1::test]
fn copy_results_keep_sources_and_disable_changes_during_writes() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let object = config("source", "app");
    let (a, _b, left, right) = fixture(cx, object.clone(), vec![]);
    let (window, view) = open(cx, left, right, object);
    click(cx, window, "transfer-check-target");
    idle(cx, window, &view);
    cx.update_window(window, |_, window, cx| {
        window.click("transfer-create-target", cx);
        assert!(view.read(cx).busy);
        window.render_frame(cx);
        window.click("transfer-mode-move", cx);
        assert!(!view.read(cx).moving);
    })
    .unwrap();
    idle(cx, window, &view);
    assert!(view.read_with(cx, |view, _| {
        view.batch.as_ref().unwrap().items()[0].verified
    }));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("transfer-delete-sources").is_none());
        assert!(window.find("transfer-done").visible());
    })
    .unwrap();
    assert!(
        !a.requests
            .lock()
            .unwrap()
            .iter()
            .any(|(url, _)| url.starts_with("DELETE"))
    );
}
#[::core::prelude::v1::test]
fn secret_consent_masks_yaml_and_language_switch_preserves_destination() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let object = json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"auth","namespace":"source","uid":"auth","resourceVersion":"1"},"type":"Opaque","data":{"token":"c2VjcmV0"}});
    let (_a, _b, left, right) = fixture(cx, object.clone(), vec![]);
    let (window, view) = open(cx, left, right, object);
    click(cx, window, "transfer-check-target");
    idle(cx, window, &view);
    assert!(!view.read_with(cx, |view, _| view.batch.as_ref().unwrap().ready()));
    click(cx, window, "transfer-review-yaml");
    assert!(view.read_with(cx, |view, _| {
        !view.batch.as_ref().unwrap().items()[0]
            .preview()
            .unwrap()
            .to_string()
            .contains("c2VjcmV0")
    }));
    click(cx, window, "transfer-secret-consent");
    idle(cx, window, &view);
    assert!(view.read_with(cx, |view, _| view.batch.as_ref().unwrap().ready()));
    for language in [
        crate::i18n::Language::SimplifiedChinese,
        crate::i18n::Language::English,
    ] {
        cx.update(|cx| crate::i18n::apply(language, cx));
        draw(cx, window);
        view.read_with(cx, |view, cx| {
            assert_eq!(view.selected_id(cx).unwrap().as_str(), "target-cluster");
            assert_eq!(view.namespace.read(cx).value().as_str(), "target");
        });
    }
}

#[::core::prelude::v1::test]
fn renaming_while_namespace_pages_load_keeps_the_picker_usable() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let object = config("source", "app");
    let (_a, b, left, right) = fixture(cx, object.clone(), vec![]);
    let (window, view) = open(cx, left, right, object);
    b.put(json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"another"}}));
    let gate = b.paginate("namespaces", 1);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.load_namespaces(window, cx));
    })
    .unwrap();
    support::settle(cx, |_| gate.waiting());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.rows[0]
                .name
                .update(cx, |input, cx| input.set_value("renamed", window, cx));
        });
    })
    .unwrap();
    cx.run_until_parked();
    gate.release();
    support::settle(cx, |cx| view.read_with(cx, |view, _| !view.loading));
    view.read_with(cx, |view, cx| {
        assert_eq!(view.rows[0].name.read(cx).value().as_str(), "renamed");
        assert_eq!(view.namespace.read(cx).value().as_str(), "target");
    });
}
