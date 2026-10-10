//! Shared local API and foreground pump for integration checks.
use beacon_kube::{
    ApiResource, ClusterId, ClusterSession, GroupVersionKind, Kind, test_support::Fixture,
};
use gpui_kit::*;
use serde_json::Value;
use std::{sync::Arc, time::Duration};

pub(crate) fn workspace(cx: &TestAppContext, directory: &std::path::Path) {
    cx.update(|cx| {
        crate::app::init(directory.join("logs"), cx);
        crate::settings::store(cx).update(cx, |state, _| {
            state.preferences = Default::default();
            state.directory = directory.to_owned();
        });
        crate::settings::apply(None, cx);
    });
}

pub(crate) fn context() -> TestAppContext {
    let cx = TestAppContext::single();
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        crate::bridge::Bridge::init(cx).unwrap();
    });
    cx
}
pub(crate) struct Empty;
impl Render for Empty {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}
pub(crate) fn window(cx: &mut TestAppContext) -> AnyWindowHandle {
    cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|_| Empty)
        })
        .unwrap()
        .0
    })
}
pub(crate) fn kind(group: &str, name: &str, plural: &str) -> Kind {
    Kind {
        resource: ApiResource::from_gvk_with_plural(
            &GroupVersionKind::gvk(group, "v1", name),
            plural,
        ),
        namespaced: name != "Namespace",
        verbs: vec!["get".into(), "list".into(), "watch".into(), "patch".into()],
    }
}
pub(crate) fn fixture(
    cx: &mut TestAppContext,
    id: &str,
    objects: Vec<Value>,
) -> (Fixture, Arc<ClusterSession>) {
    let runtime = cx.read(|cx| crate::bridge::Bridge::global(cx).handle());
    runtime.block_on(async move {
        let fixture = Fixture::start(objects).await;
        let kinds = vec![
            kind("", "Pod", "pods"),
            kind("", "Service", "services"),
            kind("networking.k8s.io", "Ingress", "ingresses"),
            kind("discovery.k8s.io", "EndpointSlice", "endpointslices"),
            kind("apps", "Deployment", "deployments"),
            kind("apps", "ReplicaSet", "replicasets"),
            kind("", "Namespace", "namespaces"),
            kind("", "ConfigMap", "configmaps"),
            kind("", "Secret", "secrets"),
        ];
        let session = Arc::new(ClusterSession::for_testing(
            ClusterId::new(id),
            fixture.url.clone(),
            kinds,
        ));
        (fixture, session)
    })
}
#[track_caller]
pub(crate) fn settle(cx: &mut TestAppContext, mut ready: impl FnMut(&TestAppContext) -> bool) {
    for _ in 0..200 {
        cx.run_until_parked();
        if ready(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(ready(cx), "foreground/API operation did not settle");
}

#[track_caller]
pub(crate) fn render_until(
    cx: &mut TestAppContext,
    window: AnyWindowHandle,
    mut ready: impl FnMut(&Window) -> bool,
) {
    use gpui_kit::test::TestWindowExt as _;
    for _ in 0..200 {
        cx.run_until_parked();
        if cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                ready(window)
            })
            .unwrap()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("UI operation did not settle");
}
