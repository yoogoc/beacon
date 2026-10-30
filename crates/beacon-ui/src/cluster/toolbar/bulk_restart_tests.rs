use super::*;
use crate::feature_test_support as support;
use gpui_kit::{
    component::{Theme, ThemeMode},
    test::TestWindowExt as _,
};
use serde_json::{Value, json};

struct Pane {
    view: Entity<ClusterView>,
    width: Pixels,
}
impl Render for Pane {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(div().w(self.width).h_full().child(self.view.clone()))
    }
}
fn deployment(name: &str) -> Value {
    json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":name,"namespace":"default","uid":name,"resourceVersion":"1"},
        "spec":{"replicas":2,"selector":{"matchLabels":{"app":name}},"template":{"metadata":{"labels":{"app":name}},"spec":{"containers":[{"name":"app","image":"nginx"}]}}},"status":{"readyReplicas":2}})
}
fn open(
    cx: &mut TestAppContext,
    session: Arc<beacon_kube::ClusterSession>,
) -> (AnyWindowHandle, Entity<Pane>, Entity<ClusterView>) {
    let (window, pane) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Default::default(),
                    size: size(px(1360.), px(720.)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| {
                window.set_view_retention(false);
                let view = cx.new(|cx| {
                    ClusterView::new(
                        session,
                        Some("default".into()),
                        Some(Arc::new(support::kind("apps", "Deployment", "deployments"))),
                        None,
                        false,
                        window,
                        cx,
                    )
                });
                cx.new(|_| Pane {
                    view,
                    width: px(1200.),
                })
            },
        )
        .unwrap()
    });
    let view = pane.read_with(cx, |pane, _| pane.view.clone());
    (window, pane, view)
}
fn draw(cx: &mut TestAppContext, window: AnyWindowHandle) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }
}
fn select_all(view: &Entity<ClusterView>, cx: &mut TestAppContext) {
    view.update(cx, |view, cx| {
        view.table.update(cx, |table, cx| {
            table.delegate_mut().toggle_all_visible();
            cx.notify();
        });
        cx.notify();
    });
}
fn absent(window: &Window, id: &str) -> bool {
    !gpui_kit::base::test_support::snapshots(window)
        .iter()
        .any(|item| item.path().contains(&ElementId::from(id.to_owned())))
}

#[::core::prelude::v1::test]
fn bulk_restart_toolbar_is_a_separate_row_without_squeezing_filters_in_both_themes() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (fixture, session) = support::fixture(
        cx,
        "selection-actions",
        vec![
            deployment("web"),
            deployment("worker"),
            json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"secret","namespace":"default","uid":"secret","resourceVersion":"1"},"type":"Opaque"}),
            json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"pod","namespace":"default","uid":"pod","resourceVersion":"1"},"spec":{"containers":[{"name":"app","image":"nginx"}]},"status":{"phase":"Running"}}),
        ],
    );
    let (window, pane, view) = open(cx, session);
    support::settle(cx, |cx| {
        view.read_with(cx, |view, cx| {
            view.counts(cx).0 == 2 && view.rules.is_some()
        })
    });
    for theme in [ThemeMode::Light, ThemeMode::Dark] {
        cx.update(|cx| Theme::change(theme, None, cx));
        for width in [1200., 880., 640., 360., 280.] {
            pane.update(cx, |pane, cx| {
                pane.width = px(width);
                cx.notify();
            });
            draw(cx, window);
            let search_before = cx
                .update_window(window, |_, window, _| {
                    assert!(absent(window, "resource-selection-toolbar"));
                    window.find("resource-search").bounds().size.width
                })
                .unwrap();
            select_all(&view, cx);
            draw(cx, window);
            cx.update_window(window, |_, window, cx| {
                let filters = window.find("resource-toolbar").bounds();
                let actions = window.find("resource-selection-toolbar").bounds();
                assert_eq!(filters.bottom(), actions.origin.y);
                assert_eq!(actions.size.width, px(width));
                assert_eq!(
                    window.find("resource-search").bounds().size.width,
                    search_before
                );
                assert!(filters.size.height <= px(44.));
                assert!(actions.size.height <= px(44.));
                for id in [
                    "clear-selected",
                    "compare-selected",
                    "export-selected",
                    "transfer-selected",
                    "restart-selected",
                    "delete-selected",
                ] {
                    let control = window.find(id);
                    assert!(
                        control.bounds().origin.x >= actions.origin.x
                            && control.bounds().right() <= actions.right(),
                        "{id} escaped pane at {width}"
                    );
                    assert!(
                        control.bounds().origin.y >= actions.origin.y
                            && control.bounds().bottom() <= actions.bottom()
                    );
                }
                assert_eq!(
                    window.find("restart-selected").label(),
                    Some("Restart selected Deployments")
                );
                assert_ne!(window.find("restart-selected").disabled(), Some(true));
                window.click("clear-selected", cx);
            })
            .unwrap();
        }
    }
    for (name, plural) in [("Secret", "secrets"), ("Pod", "pods")] {
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.show(Arc::new(support::kind("", name, plural)), window, cx)
            })
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, cx| view.counts(cx).0 == 1)
        });
        select_all(&view, cx);
        draw(cx, window);
        cx.update_window(window, |_, window, cx| {
            assert!(window.find("resource-selection-toolbar").visible());
            assert!(window.find("delete-selected").visible());
            assert!(absent(window, "restart-selected"));
            window.click("clear-selected", cx);
        })
        .unwrap();
    }
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(request, _)| request.starts_with("PATCH") || request.starts_with("DELETE"))
    );
}

#[::core::prelude::v1::test]
fn bulk_restart_reviews_targets_before_writing_and_retains_each_failure() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let mut paused = deployment("paused");
    paused["spec"]["paused"] = json!(true);
    let (fixture, session) = support::fixture(
        cx,
        "restart-review",
        vec![deployment("healthy"), paused, deployment("denied")],
    );
    fixture.rules(json!([
        {"verbs":["get","list","watch"],"apiGroups":["apps"],"resources":["deployments"]},
        {"verbs":["patch"],"apiGroups":["apps"],"resources":["deployments"],"resourceNames":["healthy","paused"]}
    ]));
    let (window, _, view) = open(cx, session);
    support::settle(cx, |cx| {
        view.read_with(cx, |view, cx| {
            view.counts(cx).0 == 3 && view.rules.is_some()
        })
    });
    select_all(&view, cx);
    draw(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click("restart-selected", cx);
        window.render_frame(cx);
        assert!(window.find("bulk-restart-target-table").visible());
        for (index, name) in ["denied", "healthy", "paused"].into_iter().enumerate() {
            let row = window.find(("bulk-restart-target", index));
            assert_eq!(row.role(), Some(Role::Row));
            assert_eq!(row.label(), Some(format!("default / {name}").as_str()));
        }
        window.click("bulk-restart-close", cx);
    })
    .unwrap();
    assert!(view.read_with(cx, |view, cx| view.bulk_restart.is_none()
        && view.table.read(cx).delegate().selected_count() == 3));
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(request, _)| request.starts_with("PATCH"))
    );
    draw(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click("restart-selected", cx);
        window.render_frame(cx);
        window.click("bulk-restart-confirm", cx);
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, cx| {
            matches!(view.outcome, Some(Outcome::Failed(_))) && view.table.read(cx).delegate()
                .object(&ObjectRef::new(Some("default".into()), "healthy"))
                .is_some_and(|object| object.data.pointer("/spec/template/metadata/annotations/kubectl.kubernetes.io~1restartedAt").is_some())
        })
    });
    draw(cx, window);
    view.read_with(cx, |view, cx| {
        let table = view.table.read(cx).delegate();
        let targets = table.selected_targets();
        assert_eq!(targets.len(), 2);
        assert!(
            targets
                .iter()
                .all(|target| target.reference.name != "healthy")
        );
        assert!(
            table
                .object(&ObjectRef::new(Some("default".into()), "healthy"))
                .unwrap()
                .data
                .pointer("/spec/template/metadata/annotations/kubectl.kubernetes.io~1restartedAt")
                .is_some()
        );
        assert!(!view.bulk_restart.as_ref().unwrap().read(cx).busy());
    });
    cx.update_window(window, |_, window, cx| {
        let labels: Vec<_> = gpui_kit::base::test_support::snapshots(window)
            .iter()
            .filter_map(|item| item.label().map(str::to_owned))
            .collect();
        assert!(labels.iter().any(|label| label == "Restart requested"));
        assert!(labels.iter().any(|label| label.contains("paused")));
        assert!(
            labels
                .iter()
                .any(|label| label.contains("Forbidden: cannot patch"))
        );
        assert!(absent(window, "bulk-restart-confirm"));
        window.click("bulk-restart-close", cx);
    })
    .unwrap();
    assert!(view.read_with(cx, |view, _| view.bulk_restart.is_none()));
    let requests = fixture.requests.lock().unwrap();
    let patches: Vec<_> = requests
        .iter()
        .filter(|(request, _)| request.starts_with("PATCH"))
        .collect();
    assert_eq!(patches.len(), 1);
    assert!(patches[0].0.contains("/deployments/healthy"));
}
