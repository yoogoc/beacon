use super::*;
use crate::{feature_test_support as support, resource_menu::HeaderItem};
use beacon_kube::test_support::Fixture;
use gpui_kit::InputEvent as _;
use gpui_kit::test::TestWindowExt as _;
use serde_json::{Value, json};

fn open(
    cx: &mut TestAppContext,
    object: Value,
    kind: Kind,
    width: f32,
) -> (
    tempfile::TempDir,
    Fixture,
    AnyWindowHandle,
    Entity<ClusterView>,
    ObjectRef,
) {
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let target = ObjectRef::new(
        Some("default".into()),
        object["metadata"]["name"].as_str().unwrap(),
    );
    let (fixture, session) = support::fixture(cx, "header-actions", vec![object]);
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Point::default(),
                    size(px(width), px(800.)),
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
                        Some(Arc::new(kind)),
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
        view.read_with(cx, |view, cx| {
            view.counts(cx).0 == 1 && view.rules.is_some()
        })
    });
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.open_target(&target, DetailTab::Overview, window, cx)
        });
    })
    .unwrap();
    support::render_until(cx, window, |window| {
        window.try_find("detail-resource-icon").is_some()
    });
    (directory, fixture, window, view, target)
}

fn actions(view: &Entity<ClusterView>, target: &ObjectRef, cx: &App) -> Vec<(usize, String, bool)> {
    view.read(cx)
        .table
        .read(cx)
        .delegate()
        .resource_menu(target.clone(), cx)
        .header()
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| match item {
            HeaderItem::Action {
                label, disabled, ..
            } => Some((index, label.to_string(), disabled)),
            HeaderItem::Menu { label, items, .. } => Some((
                index,
                label.to_string(),
                items
                    .iter()
                    .all(|item| matches!(item, HeaderItem::Action { disabled: true, .. })),
            )),
            _ => None,
        })
        .collect()
}

fn leaf_actions(items: Vec<HeaderItem>) -> Vec<(String, bool)> {
    items
        .into_iter()
        .flat_map(|item| match item {
            HeaderItem::Action {
                label, disabled, ..
            } => vec![(label.to_string(), disabled)],
            HeaderItem::Menu { items, .. } => leaf_actions(items),
            HeaderItem::Separator => vec![],
        })
        .collect()
}

fn click_menu(cx: &mut TestAppContext, window: AnyWindowHandle, label: &str) {
    cx.run_until_parked();
    support::render_until(cx, window, |window| {
        gpui_kit::base::test_support::snapshots(window)
            .iter()
            .any(|item| item.role() == Some(Role::MenuItem) && item.label() == Some(label))
    });
    cx.update_window(window, |_, window, cx| {
        let item = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .find(|item| item.role() == Some(Role::MenuItem) && item.label() == Some(label))
            .unwrap();
        let id = item.path().last().unwrap().clone();
        window.within("popup-menu").hover(id.clone(), cx);
        window.render_frame(cx);
        assert!(
            window.find("detail-header-actions").visible(),
            "the toolbar must remain open while reaching its menu"
        );
        window.within("popup-menu").click(id, cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn pod() -> Value {
    json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"header-pod","namespace":"default","uid":"header-pod","resourceVersion":"1"},"spec":{"containers":[{"name":"app","image":"busybox","ports":[{"containerPort":8080}]}]},"status":{"phase":"Running"}})
}

#[track_caller]
fn header_is_open(cx: &mut TestAppContext, window: AnyWindowHandle, open: bool) {
    // Let deferred hover updates run between frames, as in a real window.
    for _ in 0..2 {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }
    cx.update_window(window, |_, window, _| {
        assert_eq!(window.find("detail-header-actions").visible(), open);
        assert_eq!(window.find("detail-header-identity").visible(), !open);
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn horizontal_swipes_and_scrollbar_drags_keep_the_header_open() {
    let cx = &mut support::context();
    let (_directory, _fixture, window, _view, _target) =
        open(cx, pod(), support::kind("", "Pod", "pods"), 720.);
    cx.update_window(window, |_, window, cx| {
        window.hover("detail-resource-icon", cx)
    })
    .unwrap();
    header_is_open(cx, window, true);
    for amount in [-100., -100., 100., 100.] {
        cx.update_window(window, |_, window, cx| {
            window.scroll(
                "detail-header-actions",
                ScrollDelta::Pixels(point(px(amount), px(0.))),
                cx,
            );
        })
        .unwrap();
        header_is_open(cx, window, true);
    }
    let (start, inside, outside, before) = cx
        .update_window(window, |_, window, cx| {
            let rail = window.find("detail-header-actions").bounds();
            let start = point(rail.left() + px(10.), rail.bottom() - px(6.));
            let inside = point(rail.right() - px(12.), start.y);
            let outside = point(
                inside.x,
                window.find("detail-header-row").bounds().bottom() + px(20.),
            );
            let before = window
                .find(("detail-resource-action", 0usize))
                .bounds()
                .origin
                .x;
            window.dispatch_event(
                MouseMoveEvent {
                    position: start,
                    pressed_button: None,
                    modifiers: Default::default(),
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
            window.dispatch_event(
                MouseDownEvent {
                    position: start,
                    button: MouseButton::Left,
                    click_count: 1,
                    first_mouse: false,
                    modifiers: Default::default(),
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
            (start, inside, outside, before)
        })
        .unwrap();
    header_is_open(cx, window, true);
    for position in [inside, outside] {
        cx.update_window(window, |_, window, cx| {
            window.dispatch_event(
                MouseMoveEvent {
                    position,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Default::default(),
                }
                .to_platform_input(),
                cx,
            );
            window.render_frame(cx);
        })
        .unwrap();
        header_is_open(cx, window, true);
    }
    cx.update_window(window, |_, window, cx| {
        assert!(
            window
                .find(("detail-resource-action", 0usize))
                .bounds()
                .origin
                .x
                < before,
            "dragging the scrollbar must move the menu"
        );
        window.dispatch_event(
            MouseUpEvent {
                position: outside,
                button: MouseButton::Left,
                click_count: 1,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    header_is_open(cx, window, false);
    cx.update_window(window, |_, window, cx| {
        window.hover("detail-resource-icon", cx);
        window.render_frame(cx);
        assert!(
            window
                .find(("detail-resource-action", 0usize))
                .bounds()
                .origin
                .x
                > start.x - px(20.)
        );
        window.dispatch_event(
            MouseExitEvent {
                position: point(px(-1.), px(-1.)),
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
    header_is_open(cx, window, false);
}

#[::core::prelude::v1::test]
fn hover_keeps_header_actions_reachable_and_restores_the_resource_identity() {
    let cx = &mut support::context();
    let (_directory, _fixture, window, view, target) =
        open(cx, pod(), support::kind("", "Pod", "pods"), 1500.);
    let expected = cx.read(|cx| actions(&view, &target, cx));
    assert!(expected.iter().any(|(_, label, _)| label == "View logs"));
    assert!(expected.iter().any(|(_, label, _)| label == "Delete"));
    assert!(
        !expected.iter().any(|(_, label, _)| {
            matches!(label.as_str(), "Open details" | "View configuration")
        })
    );
    cx.update_window(window, |_, window, cx| {
        assert!(window.find("detail-header-identity").visible());
        window.hover("detail-resource-icon", cx);
        window.render_frame(cx);
        assert!(!window.find("detail-header-identity").visible());
        for (index, label, disabled) in &expected {
            let button = window.find(("detail-resource-action", *index));
            assert_eq!(button.label(), Some(label.as_str()));
            assert_eq!(button.disabled().unwrap_or(false), *disabled);
        }
        window.hover(("detail-resource-action", expected[0].0), cx);
        window.render_frame(cx);
        assert!(window.find("detail-header-actions").visible());
        window.hover("resource-search", cx);
        window.render_frame(cx);
        assert!(window.find("detail-header-identity").visible());
        assert!(!window.find("detail-header-actions").visible());
        window.right_click("detail-resource-icon", cx);
    })
    .unwrap();
    cx.run_until_parked();
    support::render_until(cx, window, |window| {
        gpui_kit::base::test_support::snapshots(window)
            .iter()
            .any(|item| item.role() == Some(Role::MenuItem))
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        let mut menu: Vec<_> = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .filter(|item| {
                item.role() == Some(Role::MenuItem)
                    && item
                        .label()
                        .is_some_and(|label| label != "Pod · header-pod")
            })
            .map(|item| {
                (
                    item.label().unwrap_or_default().to_owned(),
                    item.disabled().unwrap_or(false),
                )
            })
            .collect();
        for label in ["Open details", "View configuration"] {
            assert!(menu.iter().any(|(item, _)| item == label));
        }
        menu.retain(|(label, _)| !matches!(label.as_str(), "Open details" | "View configuration"));
        let mut expected_menu = leaf_actions(
            view.read(cx)
                .table
                .read(cx)
                .delegate()
                .resource_menu(target.clone(), cx)
                .header(),
        );
        menu.sort();
        expected_menu.sort();
        assert_eq!(menu, expected_menu);
        window.press("escape", cx);
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn header_actions_dispatch_on_the_current_resource_and_keep_delete_confirmation() {
    let cx = &mut support::context();
    let (_directory, fixture, window, view, target) =
        open(cx, pod(), support::kind("", "Pod", "pods"), 1500.);
    let items = cx.read(|cx| actions(&view, &target, cx));
    let index = |name: &str| items.iter().find(|(_, label, _)| label == name).unwrap().0;
    cx.update_window(window, |_, window, cx| {
        window.hover("detail-resource-icon", cx);
        window.scroll(
            "detail-header-actions",
            ScrollDelta::Pixels(point(px(-10000.), px(0.))),
            cx,
        );
        window.click(("detail-resource-action", index("Copy")), cx);
    })
    .unwrap();
    click_menu(cx, window, "Copy name");
    cx.update(|cx| {
        assert_eq!(
            cx.read_from_clipboard().unwrap().text().unwrap(),
            "header-pod"
        )
    });
    cx.update_window(window, |_, window, cx| {
        window.hover("detail-resource-icon", cx);
        window.scroll(
            "detail-header-actions",
            ScrollDelta::Pixels(point(px(-10000.), px(0.))),
            cx,
        );
        window.click(("detail-resource-action", index("Delete")), cx);
    })
    .unwrap();
    support::settle(cx, |cx| view.read_with(cx, |view, _| view.prompt.is_some()));
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(request, _)| request.starts_with("DELETE "))
    );
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("cancel", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.prompt.is_none()));
}

#[::core::prelude::v1::test]
fn hovering_opens_and_switches_menus_and_leaving_or_escape_dismisses_them() {
    let cx = &mut support::context();
    let (_directory, _fixture, window, view, target) =
        open(cx, pod(), support::kind("", "Pod", "pods"), 1500.);
    let items = cx.read(|cx| actions(&view, &target, cx));
    let index = |label: &str| items.iter().find(|(_, name, _)| name == label).unwrap().0;
    let menu_has = |window: &Window, label: &str| {
        gpui_kit::base::test_support::snapshots(window)
            .iter()
            .any(|item| item.role() == Some(Role::MenuItem) && item.label() == Some(label))
    };
    cx.update_window(window, |_, window, cx| {
        window.hover("detail-resource-icon", cx);
        window.scroll(
            "detail-header-actions",
            ScrollDelta::Pixels(point(px(-10000.), px(0.))),
            cx,
        );
        window.hover(("detail-resource-action", index("Copy")), cx);
    })
    .unwrap();
    support::render_until(cx, window, |window| menu_has(window, "Copy name"));
    header_is_open(cx, window, true);
    cx.update_window(window, |_, window, cx| {
        let item = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .find(|item| {
                item.role() == Some(Role::MenuItem) && item.label() == Some("Copy container states")
            })
            .unwrap();
        window
            .within("popup-menu")
            .hover(item.path().last().unwrap().clone(), cx);
        window.render_frame(cx);
        assert!(menu_has(window, "Copy name"));
        assert!(window.find("detail-header-actions").visible());
        window.scroll(
            "detail-header-actions",
            ScrollDelta::Pixels(point(px(10000.), px(0.))),
            cx,
        );
        window.hover(("detail-resource-action", index("Port forward…")), cx);
    })
    .unwrap();
    support::render_until(cx, window, |window| menu_has(window, "Forward port 8080…"));
    cx.update_window(window, |_, window, cx| {
        assert!(!menu_has(window, "Copy name"));
        window.hover("resource-search", cx);
    })
    .unwrap();
    header_is_open(cx, window, false);
    cx.update_window(window, |_, window, cx| {
        assert!(window.try_find("popup-menu").is_none());
        window.hover("detail-resource-icon", cx);
        window.scroll(
            "detail-header-actions",
            ScrollDelta::Pixels(point(px(-10000.), px(0.))),
            cx,
        );
        window.hover(("detail-resource-action", index("Copy")), cx);
    })
    .unwrap();
    support::render_until(cx, window, |window| menu_has(window, "Copy name"));
    cx.update_window(window, |_, window, cx| window.press("escape", cx))
        .unwrap();
    header_is_open(cx, window, true);
    cx.update_window(window, |_, window, cx| {
        assert!(window.try_find("popup-menu").is_none());
        assert!(window.find("detail-resource-icon").visible());
        window.hover("resource-search", cx);
    })
    .unwrap();
    header_is_open(cx, window, false);
}

#[::core::prelude::v1::test]
fn copy_and_port_options_have_one_menu_each_and_preserve_each_operation() {
    let cx = &mut support::context();
    let mut object = pod();
    object["spec"]["containers"][0]["ports"]
        .as_array_mut()
        .unwrap()
        .push(json!({"containerPort":9090}));
    let (_directory, fixture, window, view, target) =
        open(cx, object, support::kind("", "Pod", "pods"), 1500.);
    let items = cx.read(|cx| actions(&view, &target, cx));
    assert_eq!(
        items.iter().filter(|(_, label, _)| label == "Copy").count(),
        1
    );
    assert_eq!(
        items
            .iter()
            .filter(|(_, label, _)| label == "Port forward…")
            .count(),
        1
    );
    assert!(
        !items.iter().any(|(_, label, _)| (label.starts_with("Copy ")
            && label != "Copy or move resource…")
            || label.starts_with("Forward port "))
    );
    assert_eq!(
        items
            .iter()
            .filter(|(_, label, _)| label == "Copy or move resource…")
            .count(),
        1,
        "resource transfer is independent of clipboard copy options"
    );
    let index = |name: &str| items.iter().find(|(_, label, _)| label == name).unwrap().0;
    for label in ["Copy status", "Copy container states"] {
        cx.update_window(window, |_, window, cx| {
            window.hover("detail-resource-icon", cx);
            window.scroll(
                "detail-header-actions",
                ScrollDelta::Pixels(point(px(-10000.), px(0.))),
                cx,
            );
            window.click(("detail-resource-action", index("Copy")), cx);
        })
        .unwrap();
        click_menu(cx, window, label);
        let copied = cx.update(|cx| cx.read_from_clipboard().unwrap().text().unwrap());
        if label == "Copy status" {
            assert_eq!(copied, "Running");
        } else {
            assert!(copied.contains("app"));
        }
    }
    for (label, port) in [
        ("Forward port 8080…", Some("8080")),
        ("Forward port 9090…", Some("9090")),
        ("Port forward…", None),
    ] {
        cx.update_window(window, |_, window, cx| {
            window.hover("detail-resource-icon", cx);
            window.scroll(
                "detail-header-actions",
                ScrollDelta::Pixels(point(px(10000.), px(0.))),
                cx,
            );
            window.click(("detail-resource-action", index("Port forward…")), cx);
        })
        .unwrap();
        click_menu(cx, window, label);
        support::render_until(cx, window, |window| {
            window
                .try_find("forward-dialog")
                .is_some_and(|item| item.visible())
        });
        cx.update_window(window, |_, window, cx| {
            let snapshots = gpui_kit::base::test_support::snapshots(window);
            assert!(
                snapshots
                    .iter()
                    .filter(|item| item.path().contains(&ElementId::from("forward-dialog")))
                    .any(|item| item.value() == Some(port.unwrap_or_default())),
                "the selected remote port must be retained"
            );
            window.click("forward-cancel", cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
    assert!(view.read_with(cx, |view, _| view.session().forwards().is_empty()));
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(request, _)| request.contains("portforward"))
    );
}

#[::core::prelude::v1::test]
fn resource_specific_actions_follow_permissions_and_fit_a_narrow_detail_header() {
    let cx = &mut support::context();
    let deployment = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"api","namespace":"default","uid":"api","resourceVersion":"1"},"spec":{"replicas":2,"template":{"spec":{"containers":[{"name":"app","image":"busybox"}]}}}});
    let (_directory, fixture, window, view, target) = open(
        cx,
        deployment,
        support::kind("apps", "Deployment", "deployments"),
        720.,
    );
    let items = cx.read(|cx| actions(&view, &target, cx));
    assert!(items.iter().any(|(_, label, _)| label == "Restart"));
    assert!(items.iter().any(|(_, label, _)| label == "Scale…"));
    assert!(!items.iter().any(|(_, label, _)| label == "Open shell"));
    cx.update_window(window, |_, window, cx| {
        window.hover("detail-resource-icon", cx);
        window.render_frame(cx);
        let row = window.find("detail-header-row").bounds();
        let toolbar = window.find("detail-header-actions").bounds();
        assert!(row.contains(&toolbar.origin));
        assert!(row.contains(&toolbar.bottom_right()));
        let first = window.find(("detail-resource-action", items[0].0)).bounds();
        window.scroll(
            "detail-header-actions",
            ScrollDelta::Pixels(point(px(-100.), px(0.))),
            cx,
        );
        let moved = window.find(("detail-resource-action", items[0].0)).bounds();
        assert!(moved.origin.x < first.origin.x);
        assert_eq!(window.find("detail-header-row").bounds(), row);
        view.update(cx, |view, cx| {
            view.rules = Some(Arc::new(Rules::default()));
            view.refresh_detail(cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    let denied = cx.read(|cx| actions(&view, &target, cx));
    for (index, label, disabled) in denied {
        if !matches!(label.as_str(), "Restart" | "Scale…" | "Delete") {
            continue;
        }
        assert!(disabled, "{label} must follow the updated permissions");
        cx.update_window(window, |_, window, cx| {
            window.hover("detail-resource-icon", cx);
            window.render_frame(cx);
            let rail = window.find("detail-header-actions").bounds();
            let button = window.find(("detail-resource-action", index)).bounds();
            window.scroll(
                "detail-header-actions",
                ScrollDelta::Pixels(point(rail.center().x - button.center().x, px(0.))),
                cx,
            );
            // GPUI does not expose aria-disabled for native buttons. Exercise
            // pointer dispatch instead of treating an absent flag as enabled.
            window.click(("detail-resource-action", index), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            view.read_with(cx, |view, _| view.prompt.is_none()),
            "disabled {label} must not start an operation"
        );
    }
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(request, _)| request.starts_with("DELETE ") || request.starts_with("PATCH "))
    );
}
