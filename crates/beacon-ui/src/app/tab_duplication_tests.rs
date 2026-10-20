use super::*;
use crate::feature_test_support as support;
use crate::filters::Field;
use crate::table::ResourceTable;
use crate::table_preferences::FilterPreset;
use beacon_columns::ColumnSet;
use gpui_kit::component::table::ColumnSort;
use gpui_kit::test::TestWindowExt as _;

fn preset(view: &ClusterView, cx: &App) -> FilterPreset {
    let mut table = ResourceTable::new(ColumnSet::for_kind("", "Pod", true));
    table.restore_filters(view.view_filters(cx));
    table.filter_preset(view.navigation().1)
}

#[::core::prelude::v1::test]
fn duplicate_tab_menu_copies_the_clicked_tab_next_to_it_with_independent_filters() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let objects = [("web-one", "default", "web"), ("web-two", "staging", "web"), ("db", "staging", "db")]
        .into_iter()
        .map(|(name, namespace, app)| serde_json::json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": name, "namespace": namespace, "uid": name, "resourceVersion": "1", "labels": {"app": app}},
            "status": {"phase": "Running"}
        })).collect();
    let (_fixture, session) = support::fixture(cx, "duplicate-tab", objects);
    let scope = BTreeSet::from(["default".into(), "staging".into()]);
    let mut table = ResourceTable::new(ColumnSet::for_kind("", "Pod", true));
    table.set_filter("web");
    table.set_label_filter("app=web").unwrap();
    table.set_field_filter(Field::PodStatus, Some("Running".into()));
    table.sort_by(0, ColumnSort::Descending);
    let expected = table.filter_preset(scope.clone());
    let (window, app) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                let mut app = BeaconApp::new(window, cx);
                app.connections.update(cx, |state, _| {
                    state.sessions.insert(session.id().clone(), session.clone());
                });
                app.open_kind(
                    session.id().clone(),
                    Some(Arc::new(support::kind("", "Pod", "pods"))),
                    Some(scope),
                    window,
                    cx,
                );
                app.cluster().unwrap().update(cx, |view, cx| {
                    view.restore_filters(table.view_filters(), window, cx)
                });
                app.open_kind(
                    session.id().clone(),
                    Some(Arc::new(support::kind("", "ConfigMap", "configmaps"))),
                    None,
                    window,
                    cx,
                );
                app
            })
        })
        .unwrap()
    });
    let (source, other, source_view) = app.read_with(cx, |app, _| {
        (app.tabs[0].id, app.tabs[1].id, app.view(0).unwrap())
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.right_click(("resource-tab", source as usize), cx);
    })
    .unwrap();
    cx.run_until_parked();
    support::render_until(cx, window, |window| {
        gpui_kit::base::test_support::snapshots(window)
            .iter()
            .any(|item| {
                item.role() == Some(Role::MenuItem) && item.label() == Some("Duplicate tab")
            })
    });
    cx.update_window(window, |_, window, cx| {
        let item = gpui_kit::base::test_support::snapshots(window)
            .into_iter()
            .find(|item| {
                item.role() == Some(Role::MenuItem) && item.label() == Some("Duplicate tab")
            })
            .unwrap();
        window.click(item.path().last().unwrap().clone(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    let duplicate_view = app.read_with(cx, |app, cx| {
        assert_eq!(app.tabs.len(), 3);
        let duplicate = app.tabs[app.active].id;
        assert_ne!(duplicate, source);
        assert_ne!(duplicate, other);
        assert_eq!(
            app.layout.pane(app.layout.focused).unwrap().tabs,
            vec![source, duplicate, other]
        );
        let view = app.cluster().unwrap();
        assert_ne!(view.entity_id(), source_view.entity_id());
        assert_eq!(view.read(cx).session().id(), session.id());
        assert_eq!(view.read(cx).kind().unwrap().resource.kind, "Pod");
        assert_eq!(preset(view.read(cx), cx), expected);
        assert_eq!(app.connections.read(cx).sessions.len(), 1);
        view
    });
    support::settle(cx, |cx| {
        duplicate_view.read_with(cx, |view, cx| view.counts(cx) == (2, 3))
            && source_view.read_with(cx, |view, cx| view.counts(cx) == (2, 3))
    });
    cx.update_window(window, |_, window, cx| {
        table.set_filter("one");
        duplicate_view.update(cx, |view, cx| {
            view.restore_filters(table.view_filters(), window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    duplicate_view.read_with(cx, |view, cx| assert_eq!(view.counts(cx), (1, 3)));
    source_view.read_with(cx, |view, cx| {
        assert_eq!(view.counts(cx), (2, 3));
        assert_eq!(preset(view, cx), expected);
    });
}

#[::core::prelude::v1::test]
fn duplicate_tab_stays_in_the_source_pane_and_keeps_overview_or_disconnected_state() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (_fixture, session) = support::fixture(cx, "duplicate-overview", vec![]);
    let (window, app) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                let mut app = BeaconApp::new(window, cx);
                app.connections.update(cx, |state, _| {
                    state.sessions.insert(session.id().clone(), session.clone());
                });
                app.go_to_overview(session.id().clone(), true, window, cx);
                app.split_tab(app.tabs[0].id, Direction::Right, window, cx);
                app
            })
        })
        .unwrap()
    });
    cx.update_window(window, |_, window, cx| {
        app.update(cx, |app, cx| {
            let source = app.tabs[0].id;
            let pane = app.layout.owner(source).unwrap();
            assert_ne!(app.layout.focused, pane);
            let duplicate = app.duplicate_tab(source, window, cx).unwrap();
            assert_eq!(app.layout.panes().len(), 2);
            assert_eq!(app.layout.owner(duplicate), Some(pane));
            assert_eq!(app.layout.pane(pane).unwrap().tabs, vec![source, duplicate]);
            assert_eq!(
                app.cluster().unwrap().read(cx).navigation().2,
                Mode::Overview
            );
            app.disconnect(session.id(), window, cx);
            let disconnected = app.duplicate_tab(source, window, cx).unwrap();
            let tab = &app.tabs[app.index_of(disconnected).unwrap()];
            assert!(matches!(tab.state, TabState::Disconnected));
            assert_eq!(tab.initial_mode, Mode::Overview);
            assert!(app.connections.read(cx).sessions.is_empty());
            assert!(!app.connections.read(cx).connecting(session.id()));
            assert_eq!(app.layout.panes().len(), 2);
        });
    })
    .unwrap();
}
