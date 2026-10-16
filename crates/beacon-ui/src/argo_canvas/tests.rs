use super::*;
use crate::feature_test_support as support;
use gpui_kit::InputEvent as _;
use gpui_kit::test::TestWindowExt as _;
use serde_json::json;
use std::cell::RefCell;

fn graph(count: usize) -> Graph {
    Graph::layout(
        (0..count + 2)
            .map(|i| GraphNode {
                id: format!("task-{i}"),
                label: format!("task-{i}"),
                data: json!({"id":format!("task-{i}"),"phase":"Succeeded","type":"Pod","templateName":"worker"}),
                runtime: true,
                template_context: None,
                x: 0.,
                y: 0.,
            })
            .collect(),
        (1..=count).flat_map(|i| [(0, i), (i, count + 1)]).collect(),
    )
}

fn fixture() -> (TestAppContext, AnyWindowHandle, Entity<ArgoCanvas>) {
    let cx = support::context();
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Point::default(),
                    size(px(736.), px(600.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| ArgoCanvas::new(window, cx))
            },
        )
        .unwrap()
    });
    (cx, window, view)
}

fn populate(view: &Entity<ArgoCanvas>, count: usize, cx: &mut App) {
    let graph = graph(count);
    let nodes = Arc::new(
        graph
            .nodes
            .iter()
            .map(|n| (n.id.clone(), n.data.clone()))
            .collect(),
    );
    view.update(cx, |view, cx| view.replace(Prepared::new(graph), nodes, cx));
}

fn assert_fit(window: &mut Window, cx: &mut App) {
    window.render_frame(cx);
    window.render_frame(cx);
    let viewport = window.find("argo-execution-graph").bounds();
    let surface = window.find("argo-graph-surface").bounds();
    assert!(
        viewport.contains(&surface.origin),
        "{surface:?} outside {viewport:?}"
    );
    assert!(
        viewport.contains(&surface.bottom_right()),
        "{surface:?} outside {viewport:?}"
    );
    assert!(surface.size.width > px(0.) && surface.size.height > px(0.));
}

fn click_node(index: usize, view: &Entity<ArgoCanvas>, window: &mut Window, cx: &mut App) {
    let view = view.read(cx);
    let bounds = view
        .resolved()
        .bounds(view.scene.node_bounds(index), view.bounds.get().size);
    window.click_at("argo-execution-graph", bounds.center(), cx);
}

#[::core::prelude::v1::test]
fn large_graph_fits_resizes_drills_and_returns_without_a_blank_view() {
    let (mut cx, window, view) = fixture();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.loading(Some(1202), "workflow/".into(), cx)
        });
        window.render_frame(cx);
        assert_eq!(
            window.find("argo-graph-loading").bounds().size.height,
            px(420.)
        );
        assert!(window.try_find("argo-graph-surface").is_none());
        populate(&view, 1200, cx);
        assert_fit(window, cx);
        assert_eq!(view.read(cx).scene.members.len(), 3);
        click_node(1, &view, window, cx);
        assert_fit(window, cx);
        assert_eq!(view.read(cx).stack.len(), 1);
        assert_eq!(view.read(cx).scene.members.len(), 6);
        assert!(
            view.read(cx).resolved().scale >= 0.95,
            "parallel groups should remain readable"
        );
        let scene = &view.read(cx).scene;
        assert!(scene.graph.nodes[0].y < scene.graph.nodes[3].y);
        assert_ne!(scene.graph.nodes[0].label, scene.graph.nodes[1].label);
        window.click("argo-group-back", cx);
        assert_fit(window, cx);
        assert!(view.read(cx).stack.is_empty());
        for width in [320., 1000., 480.] {
            window.resize(size(px(width), px(600.)));
            window.bounds_changed(cx);
            assert_fit(window, cx);
            let viewport = window.find("argo-execution-graph").bounds();
            assert!(viewport.right() <= px(width));
            assert_eq!(window.try_find("argo-minimap").is_some(), width >= 360.);
            assert!(window.find("argo-fit").bounds().right() <= viewport.right());
        }
        // Opting out of grouping still fits every node. Zooming mounts only
        // the visible cards, and Fit restores the full diagram.
        window.click("argo-group-nodes", cx);
        assert_fit(window, cx);
        assert_eq!(view.read(cx).scene.members.len(), 1202);
        let fitted = window.find("argo-graph-surface").bounds();
        window.click("argo-zoom-in", cx);
        assert!(window.find("argo-graph-surface").bounds().size.width > fitted.size.width);
        window.click("argo-fit", cx);
        assert_eq!(window.find("argo-graph-surface").bounds(), fitted);
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn search_reveals_folded_nodes_and_requests_nested_scopes_with_real_node_data() {
    let (mut cx, window, view) = fixture();
    let events = Rc::new(RefCell::new(Vec::new()));
    cx.update(|cx| {
        let events = events.clone();
        cx.subscribe(&view, move |_, event: &CanvasEvent, _| {
            events.borrow_mut().push(match event {
                CanvasEvent::Inspect(node) => ("inspect", node.clone()),
                CanvasEvent::Reveal(node) => ("reveal", node.clone()),
            });
        })
        .detach();
    });
    cx.update_window(window, |_, window, cx| {
        populate(&view, 1200, cx);
        assert_fit(window, cx);
        view.update(cx, |view, cx| {
            view.search.update(cx, |input, cx| input.set_value("task-999", window, cx));
            view.search_nodes(cx);
            cx.notify();
        });
        window.render_frame(cx);
        assert!(window.try_find("argo-search-results").is_some());
        window.click(("argo-search-result", 0usize), cx);
        assert!(window.try_find("argo-search-results").is_none());
        let state = view.read(cx);
        assert_eq!(state.pinned.as_deref(), Some("task-999"));
        let index = state.scene.members.iter().position(|m| m == &[999]).unwrap();
        let node = state.resolved().bounds(state.scene.node_bounds(index), state.bounds.get().size);
        assert!(node.center().x > px(0.) && node.center().x < state.bounds.get().size.width);
        click_node(index, &view, window, cx);
        view.update(cx, |view, cx| {
            Arc::make_mut(&mut view.nodes).insert("nested-child".into(), json!({
                "id":"nested-child", "displayName":"nested-child", "boundaryID":"inner-dag", "phase":"Running", "type":"Pod"
            }));
            view.search.update(cx, |input, cx| input.set_value("nested-child", window, cx));
            view.search_nodes(cx);
            cx.notify();
        });
        window.render_frame(cx);
        window.click(("argo-search-result", 0usize), cx);
    }).unwrap();
    cx.run_until_parked();
    let events = events.borrow();
    assert_eq!(events.len(), 3);
    assert_eq!(
        (events[0].0, events[0].1.id.as_str()),
        ("inspect", "task-999")
    );
    assert_eq!(events[1].1.id, "task-999");
    assert_eq!(events[2].0, "reveal");
    assert_eq!(events[2].1.data["boundaryID"], "inner-dag");
}

#[::core::prelude::v1::test]
fn dragging_stops_on_release_and_refresh_keeps_manual_camera_and_group_focus() {
    let (mut cx, window, view) = fixture();
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| view.loading(None, "workflow/".into(), cx));
        populate(&view, 1200, cx);
        assert_fit(window, cx);
        click_node(1, &view, window, cx);
        assert_fit(window, cx);
        let bounds = window.find("argo-execution-graph").bounds();
        let start = bounds.origin + point(px(8.), px(8.));
        window.drag(start, start + point(px(40.), px(30.)), cx);
        assert!(view.read(cx).drag.is_none());
        let camera = view.read(cx).camera;
        assert!(!camera.automatic);
        window.dispatch_event(
            MouseMoveEvent {
                position: start + point(px(60.), px(60.)),
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        assert_eq!(view.read(cx).camera.center, camera.center);
        view.update(cx, |view, cx| view.loading(None, "workflow/".into(), cx));
        populate(&view, 1201, cx);
        assert_eq!(view.read(cx).camera.center, camera.center);
        assert_eq!(view.read(cx).camera.scale, camera.scale);
        assert_eq!(view.read(cx).stack.len(), 1);
        assert_eq!(
            view.read(cx)
                .scene
                .members
                .iter()
                .map(Vec::len)
                .sum::<usize>(),
            1200
        );
        view.update(cx, |view, cx| {
            view.loading(None, "workflow/inner".into(), cx)
        });
        window.render_frame(cx);
        assert!(view.read(cx).camera.automatic);
        assert!(view.read(cx).stack.is_empty());
        assert!(window.try_find("argo-graph-loading").is_some());
        view.update(cx, |view, cx| {
            view.fail("Could not decode nodes".into(), cx)
        });
        window.render_frame(cx);
        assert!(window.try_find("argo-graph-surface").is_none());
        assert_eq!(
            window.find("argo-execution-graph").bounds().size.height,
            px(420.)
        );
    })
    .unwrap();
}
