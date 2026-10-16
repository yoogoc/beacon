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
            &GroupVersionKind::gvk(
                group,
                if group == "argoproj.io" {
                    "v1alpha1"
                } else {
                    "v1"
                },
                name,
            ),
            plural,
        ),
        namespaced: !matches!(
            name,
            "Namespace" | "Node" | "PersistentVolume" | "ClusterWorkflowTemplate"
        ),
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
        let mut job_kind = kind("batch", "Job", "jobs");
        job_kind.verbs.push("create".into());
        let mut workflow_kind = kind("argoproj.io", "Workflow", "workflows");
        workflow_kind.verbs.push("create".into());
        let kinds = vec![
            workflow_kind,
            kind("argoproj.io", "WorkflowTemplate", "workflowtemplates"),
            kind(
                "argoproj.io",
                "ClusterWorkflowTemplate",
                "clusterworkflowtemplates",
            ),
            kind("argoproj.io", "CronWorkflow", "cronworkflows"),
            kind("", "Pod", "pods"),
            kind("", "Node", "nodes"),
            kind("", "Event", "events"),
            kind("apps", "StatefulSet", "statefulsets"),
            kind("apps", "DaemonSet", "daemonsets"),
            kind("", "Service", "services"),
            kind("networking.k8s.io", "Ingress", "ingresses"),
            kind("discovery.k8s.io", "EndpointSlice", "endpointslices"),
            kind("apps", "Deployment", "deployments"),
            kind("apps", "ReplicaSet", "replicasets"),
            kind("", "Namespace", "namespaces"),
            kind("", "ConfigMap", "configmaps"),
            kind("", "Secret", "secrets"),
            job_kind,
            kind("batch", "CronJob", "cronjobs"),
            kind("policy", "PodDisruptionBudget", "poddisruptionbudgets"),
            kind("", "PersistentVolumeClaim", "persistentvolumeclaims"),
            kind("", "PersistentVolume", "persistentvolumes"),
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

/// Exercise real nested viewports, including both scroll boundaries.
pub(crate) fn assert_scroll_contained(
    viewport: &'static str,
    content: &'static str,
    axis: Axis,
    window: &mut Window,
    cx: &mut App,
) {
    use gpui_kit::test::TestWindowExt as _;
    // Settle responsive layout before comparing positions across input events.
    window.render_frame(cx);
    window.render_frame(cx);
    let viewport_bounds = window.find(viewport).bounds();
    let initial = window.find(content).bounds();
    let coordinate = |bounds: Bounds<Pixels>| {
        if axis == Axis::Horizontal {
            bounds.origin.x
        } else {
            bounds.origin.y
        }
    };
    assert!(
        if axis == Axis::Horizontal {
            initial.size.width > viewport_bounds.size.width
        } else {
            initial.size.height > viewport_bounds.size.height
        },
        "{content} must overflow its viewport"
    );
    let delta = |amount| {
        ScrollDelta::Pixels(if axis == Axis::Horizontal {
            point(px(amount), px(0.))
        } else {
            point(px(0.), px(amount))
        })
    };
    window.scroll(viewport, delta(-60.), cx);
    assert_eq!(
        window.find(viewport).bounds(),
        viewport_bounds,
        "inner scrolling moved its ancestor"
    );
    assert!(
        coordinate(window.find(content).bounds()) < coordinate(initial),
        "inner content did not scroll"
    );
    for (edge, continuation) in [(-100_000., -60.), (100_000., 60.)] {
        window.scroll(viewport, delta(edge), cx);
        let at_edge = window.find(content).bounds();
        window.scroll(viewport, delta(continuation), cx);
        assert_eq!(
            window.find(content).bounds(),
            at_edge,
            "inner content escaped its scroll boundary"
        );
        assert_eq!(
            window.find(viewport).bounds(),
            viewport_bounds,
            "scrolling at an inner boundary moved its ancestor"
        );
    }
    window.scroll(
        viewport,
        ScrollDelta::Lines(if axis == Axis::Horizontal {
            point(-3., 0.)
        } else {
            point(0., -3.)
        }),
        cx,
    );
    assert_eq!(window.find(viewport).bounds(), viewport_bounds);
    assert!(coordinate(window.find(content).bounds()) < coordinate(initial));
}

pub(crate) fn assert_outer_scrolls(
    viewport: &'static str,
    marker: &'static str,
    window: &mut Window,
    cx: &mut App,
) {
    use gpui_kit::test::TestWindowExt as _;
    // Both pages leave padding between their outer viewport and inner content.
    let position = window.find(viewport).bounds().origin + point(px(4.), px(4.));
    window.dispatch_event(
        MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Default::default(),
        }
        .to_platform_input(),
        cx,
    );
    for amount in [100_000., -60.] {
        let before = window.find(marker).bounds().origin.y;
        window.dispatch_event(
            ScrollWheelEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(0.), px(amount))),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        window.render_frame(cx);
        if amount < 0. {
            assert!(
                window.find(marker).bounds().origin.y < before,
                "outer page no longer scrolls outside the inner viewport"
            );
        }
    }
}
