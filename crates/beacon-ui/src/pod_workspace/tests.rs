use super::*;
use crate::feature_test_support as support;
use gpui_kit::test::TestWindowExt as _;

fn pod(namespace: &str, name: &str, uid: &str) -> Arc<DynamicObject> {
    Arc::new(serde_json::from_value(serde_json::json!({
        "apiVersion":"v1", "kind":"Pod", "metadata":{"name":name,"namespace":namespace,"uid":uid},
        "spec":{"containers":[{"name":"app"},{"name":"sidecar"}]}
    })).unwrap())
}

#[::core::prelude::v1::test]
fn retains_sessions_and_reuses_exact_pod_identity_in_split_view() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let a = pod("default", "web", "web-1");
    let b = pod("other", "web", "web-2");
    let c = pod("default", "api", "api-1");
    let objects = vec![a.clone(), b.clone(), c.clone()];
    let (_fixture, session) = support::fixture(
        cx,
        "pod-workspace",
        objects
            .iter()
            .map(|o| serde_json::to_value(o).unwrap())
            .collect(),
    );
    let (handle, workspace) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                let mut view = PodWorkspace::new(session, window, cx);
                view.refresh(objects.clone(), None, cx);
                view.open(a.clone(), None, PodToolTab::Logs, window, cx);
                view.open(b.clone(), None, PodToolTab::Logs, window, cx);
                view.open(c.clone(), None, PodToolTab::Logs, window, cx);
                view
            })
        })
        .unwrap()
    });
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        let original = workspace.read(cx).sessions[0].tools.entity_id();
        workspace.update(cx, |view, cx| {
            view.open(a, None, PodToolTab::Logs, window, cx)
        });
        assert_eq!(workspace.read(cx).sessions.len(), 3);
        assert_eq!(workspace.read(cx).sessions[0].tools.entity_id(), original);
        window.render_frame(cx);
        window.click("split-pod-sessions", cx);
        window.render_frame(cx);
        assert!(window.find("pod-session-primary").visible());
        assert!(window.find("pod-session-secondary").visible());
        assert_eq!(workspace.read(cx).primary, Some(0));
        assert_eq!(workspace.read(cx).secondary, Some(1));
        window.click(("activate-pod-session", 1usize), cx);
        assert_eq!(workspace.read(cx).primary, Some(1));
        assert_eq!(workspace.read(cx).secondary, Some(0));
        workspace.update(cx, |view, cx| view.close(1, window, cx));
        assert_eq!(workspace.read(cx).sessions.len(), 2);
        assert_ne!(workspace.read(cx).primary, workspace.read(cx).secondary);
        workspace.update(cx, |view, cx| view.toggle_split(cx));
        assert_eq!(workspace.read(cx).sessions.len(), 2);
        assert!(workspace.read(cx).secondary.is_none());
        let replacement = pod("default", "web", "web-recreated");
        workspace.update(cx, |view, cx| {
            view.refresh(vec![replacement.clone()], None, cx);
            view.open(replacement, None, PodToolTab::Logs, window, cx);
        });
        assert_eq!(workspace.read(cx).sessions.len(), 3);
        assert_eq!(workspace.read(cx).sessions[0].uid.as_deref(), Some("web-1"));
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn search_pause_and_filters_are_independent_and_switching_does_not_reconnect() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let a = pod("default", "web", "web-1");
    let b = pod("default", "api", "api-1");
    let objects = vec![a.clone(), b.clone()];
    let (fixture, session) = support::fixture(
        cx,
        "independent-logs",
        objects
            .iter()
            .map(|o| serde_json::to_value(o).unwrap())
            .collect(),
    );
    let (handle, workspace) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1000.), px(650.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    let mut view = PodWorkspace::new(session, window, cx);
                    view.refresh(objects, None, cx);
                    view.open(a, None, PodToolTab::Logs, window, cx);
                    view
                })
            },
        )
        .unwrap()
    });
    support::render_until(cx, handle, |window| {
        window.try_find(("inspect-log", 0usize)).is_some()
    });
    cx.update_window(handle, |_, window, cx| {
        window.click("pod-log-search", cx);
        window.input("first-pod-query", cx);
        window.click("pause-log-follow", cx);
        window.click("pod-log-filters", cx);
        window.click("log-level-Error", cx);
        workspace.update(cx, |view, cx| {
            view.open(b, None, PodToolTab::Logs, window, cx)
        });
    })
    .unwrap();
    support::render_until(cx, handle, |window| {
        window.try_find(("inspect-log", 0usize)).is_some()
    });
    cx.update_window(handle, |_, window, cx| {
        assert_eq!(window.find("pause-log-follow").checked(), Some(false));
        window.click("pod-log-search", cx);
        window.input("second-pod-query", cx);
        window.click(("activate-pod-session", 0usize), cx);
        window.render_frame(cx);
        assert_eq!(window.find("pause-log-follow").checked(), Some(true));
        assert_eq!(window.find("log-level-Error").checked(), Some(true));
        assert_eq!(
            window.find("pod-log-search").value(),
            Some("first-pod-query")
        );
        window.click(("activate-pod-session", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("pod-log-search").value(),
            Some("second-pod-query")
        );
        assert!(window.try_find("structured-log-toolbar").is_none());
        window.click("split-pod-sessions", cx);
        window.render_frame(cx);
        assert_eq!(
            window
                .within("pod-session-primary")
                .find("pod-log-search")
                .value(),
            Some("second-pod-query")
        );
        assert_eq!(
            window
                .within("pod-session-secondary")
                .find("pod-log-search")
                .value(),
            Some("first-pod-query")
        );
    })
    .unwrap();
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(url, _)| url.contains("/log?"))
            .count(),
        2
    );
    for width in [520., 1000.] {
        cx.simulate_window_resize(handle, size(px(width), px(800.)));
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            let left = window.find("pod-session-primary").bounds();
            let right = window.find("pod-session-secondary").bounds();
            if width < 620. {
                assert!(right.origin.y >= left.bottom());
            } else {
                assert!(right.origin.x >= left.right());
            }
            for pane in ["pod-session-primary", "pod-session-secondary"] {
                let bounds = window.find(pane).bounds();
                let control = window.within(pane).find("pod-log-options").bounds();
                assert!(control.right() <= bounds.right());
            }
        })
        .unwrap();
    }
}

#[::core::prelude::v1::test]
fn pod_picker_search_enter_and_last_session_close() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let a = pod("default", "web", "web-1");
    let b = pod("other", "api", "api-1");
    let objects = vec![a.clone(), b];
    let (_fixture, session) = support::fixture(
        cx,
        "pick-pod",
        objects
            .iter()
            .map(|o| serde_json::to_value(o).unwrap())
            .collect(),
    );
    let (handle, workspace) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                let mut view = PodWorkspace::new(session, window, cx);
                view.refresh(objects, None, cx);
                view.open(a, None, PodToolTab::Logs, window, cx);
                view
            })
        })
        .unwrap()
    });
    let closed = Rc::new(Cell::new(false));
    let observed = closed.clone();
    let _closed =
        cx.update(|cx| cx.subscribe(&workspace, move |_, _: &Closed, _| observed.set(true)));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("add-pod-session-button", cx);
    })
    .unwrap();
    draw(cx, handle);
    cx.update_window(handle, |_, window, cx| {
        window.click("pod-session-picker", cx);
        window.input("other/api", cx);
    })
    .unwrap();
    draw(cx, handle);
    cx.update_window(handle, |_, window, cx| window.press("enter", cx))
        .unwrap();
    support::settle(cx, |cx| {
        workspace.read_with(cx, |view, _| view.sessions.len() == 2)
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(workspace.read(cx).primary, Some(1));
        window.click(("close-pod-session", 0usize), cx);
        window.click(("close-pod-session", 1usize), cx);
    })
    .unwrap();
    support::settle(cx, |_| closed.get());
}

#[::core::prelude::v1::test]
fn closing_an_executing_session_can_be_cancelled_without_losing_other_tabs() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let a = pod("default", "web", "web-1");
    let b = pod("default", "api", "api-1");
    let (_fixture, session) = support::fixture(
        cx,
        "close-active-pod",
        vec![
            serde_json::to_value(&a).unwrap(),
            serde_json::to_value(&b).unwrap(),
        ],
    );
    let (handle, workspace) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                let mut view = PodWorkspace::new(session, window, cx);
                view.open(a, None, PodToolTab::Logs, window, cx);
                view.open(b, None, PodToolTab::Exec, window, cx);
                view
            })
        })
        .unwrap()
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("run", cx);
        assert!(workspace.read(cx).has_active_session(cx));
        window.click(("activate-pod-session", 0usize), cx);
        assert!(
            workspace.read(cx).has_active_session(cx),
            "the running command in a hidden tab must still be protected"
        );
        window.click(("close-pod-session", 1usize), cx);
        assert_eq!(workspace.read(cx).closing, Some(1));
    })
    .unwrap();
    cx.simulate_prompt_answer("Cancel");
    support::settle(cx, |cx| {
        workspace.read_with(cx, |view, _| view.closing.is_none())
    });
    assert_eq!(workspace.read_with(cx, |view, _| view.sessions.len()), 2);
    cx.update_window(handle, |_, window, cx| {
        workspace.update(cx, |view, cx| view.close(1, window, cx));
        assert_eq!(workspace.read(cx).primary, Some(0));
    })
    .unwrap();
}

fn draw(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }
}
