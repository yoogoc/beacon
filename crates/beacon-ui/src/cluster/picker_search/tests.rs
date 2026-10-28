use super::*;
use crate::feature_test_support as support;
use gpui_kit::test::TestWindowExt as _;
use serde_json::json;

fn open(
    cx: &mut TestAppContext,
    directory: &std::path::Path,
) -> (
    beacon_kube::test_support::Fixture,
    AnyWindowHandle,
    Entity<ClusterView>,
) {
    support::workspace(cx, directory);
    let objects = ["default", "dashboard-demo-sandbox", "dashboard-demo-staging"].into_iter()
        .map(|name| json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":name,"uid":name,"resourceVersion":"1"}}))
        .chain([("api", "Running"), ("worker", "Pending")].into_iter().map(|(name, phase)| json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"default","uid":name,"resourceVersion":"1","labels":{"app":name,"tier":"backend"}},"spec":{"containers":[{"name":"app"}]},"status":{"phase":phase}}))).collect();
    let (fixture, session) = support::fixture(cx, "picker-enter", objects);
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Point::default(),
                    size(px(1440.), px(900.)),
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
                        Some(Arc::new(support::kind("", "Pod", "pods"))),
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
            view.namespace_names.len() == 3 && !view.table.read(cx).delegate().is_loading()
        })
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

fn click(cx: &mut TestAppContext, window: AnyWindowHandle, id: &'static str) {
    cx.update_window(window, |_, window, cx| window.click(id, cx))
        .unwrap();
    draw(cx, window);
}

fn enter(cx: &mut TestAppContext, window: AnyWindowHandle, input: &'static str, query: &str) {
    cx.update_window(window, |_, window, cx| {
        window.click(input, cx);
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            },
            cx,
        );
        window.press("backspace", cx);
        window.input(query, cx);
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, cx| window.press("enter", cx))
        .unwrap();
    draw(cx, window);
}

#[::core::prelude::v1::test]
fn namespace_enter_picks_the_first_match_not_all_and_no_results_keep_the_scope() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    let (_fixture, window, view) = open(cx, directory.path());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.toggle_namespace("dashboard-demo-staging", window, cx)
        })
    })
    .unwrap();
    draw(cx, window);
    click(cx, window, "namespace-picker-trigger");
    enter(cx, window, "namespace-search", "DEMO-S");
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.scoped_to,
            BTreeSet::from(["dashboard-demo-sandbox".into()])
        );
        assert!(
            !view.namespace_menu_open,
            "Enter must not reopen the popover"
        );
    });
    click(cx, window, "namespace-picker-trigger");
    for query in ["missing-namespace", "   "] {
        enter(cx, window, "namespace-search", query);
        view.read_with(cx, |view, _| {
            assert!(view.namespace_menu_open);
            assert_eq!(
                view.scoped_to,
                BTreeSet::from(["dashboard-demo-sandbox".into()])
            );
        });
    }
}

#[::core::prelude::v1::test]
fn field_enter_selects_the_first_matching_value_and_keeps_an_unmatched_filter() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    let (_fixture, window, view) = open(cx, directory.path());
    click(cx, window, "filter-trigger-PodStatus");
    enter(cx, window, "filter-search-PodStatus", "ing");
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.table
                .read(cx)
                .delegate()
                .field_filter(Field::PodStatus),
            Some("Pending")
        );
        assert_eq!(view.filter_menu_open, None);
    });
    click(cx, window, "filter-trigger-PodStatus");
    enter(cx, window, "filter-search-PodStatus", "missing-status");
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.table
                .read(cx)
                .delegate()
                .field_filter(Field::PodStatus),
            Some("Pending")
        );
        assert_eq!(view.filter_menu_open, Some(Field::PodStatus));
    });
}

#[::core::prelude::v1::test]
fn label_enter_adds_the_first_result_and_repeated_enter_does_not_uncheck_it() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    let (_fixture, window, view) = open(cx, directory.path());
    click(cx, window, "label-filter-trigger");
    for query in ["app=", "app=", "missing-label"] {
        enter(cx, window, "label-options-search", query);
        view.read_with(cx, |view, cx| {
            assert!(view.label_menu_open);
            assert_eq!(view.table.read(cx).delegate().label_filter(), "app=api");
            assert!(view.label_error.is_none());
        });
    }
}

#[::core::prelude::v1::test]
fn column_enter_supports_translated_search_and_never_hides_an_already_visible_column() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    let (_fixture, window, view) = open(cx, directory.path());
    let memory = view.read_with(cx, |view, cx| {
        view.table
            .read(cx)
            .delegate()
            .column_choices()
            .into_iter()
            .find(|(_, name, _, _)| name == "Memory")
            .unwrap()
            .0
    });
    cx.update_window(window, |_, _, cx| {
        view.update(cx, |view, cx| {
            view.table.update(cx, |table, cx| {
                let mut layout = table.delegate().layout_snapshot();
                layout.hidden.insert(memory.clone());
                table.delegate_mut().apply_layout(layout);
                table.refresh(cx);
            })
        });
        crate::i18n::apply(crate::i18n::Language::SimplifiedChinese, cx);
    })
    .unwrap();
    draw(cx, window);
    click(cx, window, "resource-columns-trigger");
    for query in ["内存", "内存", "missing-column", "Name"] {
        enter(cx, window, "resource-column-search", query);
        view.read_with(cx, |view, cx| {
            assert!(view.list_settings.is_open());
            assert!(
                !view
                    .table
                    .read(cx)
                    .delegate()
                    .layout_snapshot()
                    .hidden
                    .contains(&memory)
            );
            assert!(
                !view
                    .resource_preferences(cx)
                    .columns
                    .hidden
                    .contains(&memory)
            );
        });
    }
    cx.update(|cx| crate::i18n::apply(crate::i18n::Language::English, cx));
}

#[::core::prelude::v1::test]
fn saved_filter_enter_applies_the_first_result_and_never_saves_a_new_filter() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    let (_fixture, window, view) = open(cx, directory.path());
    let (key, mut preferences) = view.read_with(cx, |view, cx| {
        (
            crate::table_preferences::key(view.session.id(), view.kind.as_ref().unwrap()),
            view.resource_preferences(cx),
        )
    });
    for (name, search) in [("Alpha team", "worker"), ("Beta team", "api")] {
        preferences.filters.insert(
            name.into(),
            crate::table_preferences::FilterPreset {
                namespaces: BTreeSet::from(["default".into()]),
                search: search.into(),
                ..Default::default()
            },
        );
    }
    cx.update(|cx| {
        crate::settings::store(cx).update(cx, |settings, _| {
            settings.preferences.resources.insert(key, preferences);
        })
    });
    draw(cx, window);
    click(cx, window, "resource-saved-filters-trigger");
    enter(cx, window, "saved-filter-search", "team");
    view.read_with(cx, |view, cx| {
        assert!(!view.list_settings.is_open());
        assert_eq!(
            view.table
                .read(cx)
                .delegate()
                .filter_preset(view.scoped_to.clone())
                .search,
            "worker"
        );
        assert_eq!(view.resource_preferences(cx).filters.len(), 2);
    });
    click(cx, window, "resource-saved-filters-trigger");
    enter(cx, window, "saved-filter-search", "missing-preset");
    view.read_with(cx, |view, cx| {
        assert!(view.list_settings.is_open());
        assert_eq!(
            view.table
                .read(cx)
                .delegate()
                .filter_preset(view.scoped_to.clone())
                .search,
            "worker"
        );
        assert_eq!(view.resource_preferences(cx).filters.len(), 2);
    });
}
