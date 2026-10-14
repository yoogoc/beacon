use super::*;
use crate::feature_test_support as support;
use gpui_kit::test::TestWindowExt as _;
use serde_json::json;
fn objects() -> Vec<serde_json::Value> {
    vec![
        json!({"apiVersion":"v1","kind":"Service","metadata":{"name":"web","namespace":"default"},"spec":{"selector":{"app":"web"},"ports":[{"port":80,"targetPort":"http"}]}}),
        json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web-1","namespace":"default","uid":"p1","labels":{"app":"web"}},"spec":{"containers":[{"name":"app","image":"fixture","ports":[{"name":"http","containerPort":8080}]}]},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}]}}),
    ]
}
#[::core::prelude::v1::test]
fn forward_review_checks_ports_conflicts_then_starts_and_persists_without_automatic_restore() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (_fixture, session) = support::fixture(cx, "forward-review", objects());
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                ForwardDialog::new(
                    session.clone(),
                    Target {
                        kind: beacon_kube::forward::TargetKind::Service,
                        namespace: "default".into(),
                        name: "web".into(),
                        port: 80,
                    },
                    0,
                    window,
                    cx,
                )
            })
        })
        .unwrap()
    });
    assert!(session.forwards().is_empty());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.remote
                .update(cx, |input, cx| input.set_value("bad", window, cx))
        });
        window.render_frame(cx);
        window.click("forward-resolve", cx);
    })
    .unwrap();
    assert!(view.read_with(cx, |v, _| v.error.as_deref().unwrap().contains("1–65535")));
    assert!(session.forwards().is_empty());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.remote
                .update(cx, |input, cx| input.set_value("80", window, cx))
        });
        window.render_frame(cx);
        window.click("forward-resolve", cx);
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| view.resolved.is_some())
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.resolved.as_ref().unwrap().port),
        8080
    );
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.local.update(cx, |input, cx| {
                input.set_value(
                    occupied.local_addr().unwrap().port().to_string(),
                    window,
                    cx,
                )
            })
        });
        window.render_frame(cx);
        window.click("forward-start", cx);
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |v, _| !v.busy && v.error.is_some())
    });
    assert!(view.read_with(cx, |v, _| {
        v.error.as_deref().unwrap().contains("already in use")
    }));
    assert!(session.forwards().is_empty());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.local
                .update(cx, |input, cx| input.set_value("0", window, cx))
        });
        window.render_frame(cx);
        window.click("forward-save", cx);
        window.click("forward-start", cx);
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |v, _| !v.busy) && !session.forwards().is_empty()
    });
    let forward = session.forwards().pop().unwrap();
    let prefs: crate::settings::Preferences =
        serde_json::from_slice(&std::fs::read(directory.path().join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(prefs.forwards.len(), 1);
    assert_eq!(prefs.forwards[0].local_port, forward.local_port);
    assert_eq!(prefs.forwards[0].target.port, 80);
    assert_eq!(prefs.forwards[0].cluster, session.id().as_str());
    session.close_forward(forward.id);
    cx.run_until_parked();
    assert!(session.forwards().is_empty());
}
#[::core::prelude::v1::test]
fn denied_forward_target_is_explained_before_opening_a_listener() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (fixture, session) = support::fixture(cx, "forward-denied", objects());
    fixture.rules(json!([]));
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                ForwardDialog::new(
                    session.clone(),
                    Target {
                        kind: beacon_kube::forward::TargetKind::Service,
                        namespace: "default".into(),
                        name: "web".into(),
                        port: 80,
                    },
                    0,
                    window,
                    cx,
                )
            })
        })
        .unwrap()
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("forward-resolve", cx);
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |v, _| !v.checking && v.error.is_some())
    });
    assert!(view.read_with(cx, |v, _| {
        v.error.as_deref().unwrap().contains("pods/portforward")
    }));
    assert!(session.forwards().is_empty());
}
