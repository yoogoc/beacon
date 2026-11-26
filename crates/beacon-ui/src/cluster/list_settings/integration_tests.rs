use super::*;
use crate::data_table::TableDelegate as _;
use crate::feature_test_support as support;
use gpui_kit::test::TestWindowExt as _;
use serde_json::json;

fn window(
    cx: &mut TestAppContext,
    directory: &std::path::Path,
) -> (
    beacon_kube::test_support::Fixture,
    AnyWindowHandle,
    Entity<ClusterView>,
) {
    support::workspace(cx, directory);
    let (fixture, session) = support::fixture(cx, "saved-filters", ["api", "worker"].into_iter().map(|name| json!({
        "apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"default","uid":name,"resourceVersion":"1","labels":{"app":"web"}},
        "spec":{"containers":[{"name":"app","image":"busybox"}]},"status":{"phase":"Running","containerStatuses":[{"name":"app","ready":true,"restartCount":0,"state":{"running":{}}}]}
    })).collect());
    let mut view = None;
    let (window, _) = cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Point::default(),
                    size(px(1100.), px(720.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                window.set_view_retention(false);
                let entity = cx.new(|cx| {
                    ClusterView::new(
                        session,
                        Some("default".into()),
                        Some(Arc::new(support::kind("", "Pod", "pods"))),
                        None,
                        false,
                        window,
                        cx,
                    )
                });
                view = Some(entity.clone());
                entity
            },
        )
        .unwrap()
    });
    let view = view.unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, cx| !view.table.read(cx).delegate().is_loading())
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

#[::core::prelude::v1::test]
fn column_handles_reorder_hidden_and_filtered_choices_and_persist_the_table_layout() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    let (_fixture, window, view) = window(cx, directory.path());
    let choices = view.read_with(cx, |view, cx| {
        view.table.read(cx).delegate().column_choices()
    });
    let id = |name| choices.iter().find(|c| c.name == name).unwrap().id.clone();
    let name = id("Name");
    let containers = id("Containers");
    let ready = id("Ready");
    let restarts = id("Restarts");
    let sort = crate::table_preferences::ColumnSort {
        column: ready.clone(),
        descending: true,
    };
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.table.update(cx, |table, cx| {
                let mut layout = table.delegate().layout_snapshot();
                layout.hidden.insert(containers.clone());
                layout.widths.insert(ready.clone(), 123.);
                layout.sort = Some(sort.clone());
                table.delegate_mut().apply_layout(layout);
                table.refresh(cx);
            });
        });
        window.click("resource-columns-trigger", cx);
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.drag_to(
            format!("drag-column-{containers}"),
            format!("column-picker-row-{name}"),
            cx,
        );
    })
    .unwrap();
    draw(cx, window);
    let layout = view.read_with(cx, |view, cx| {
        assert!(
            view.list_settings.columns_open,
            "dropping keeps the picker open"
        );
        let table = view.table.read(cx).delegate();
        let layout = table.layout_snapshot();
        assert_eq!(&layout.order[1..3], &[containers.clone(), name.clone()]);
        assert!(
            layout.hidden.contains(&containers),
            "dragging must not toggle visibility"
        );
        assert_eq!(layout.widths[&ready], 123.);
        assert_eq!(layout.sort.as_ref(), Some(&sort));
        assert_eq!(table.column(1, cx).name.as_ref(), "Name");
        assert_eq!(view.resource_preferences(cx).columns, layout);
        layout
    });
    let saved: crate::settings::Preferences =
        serde_json::from_slice(&std::fs::read(directory.path().join("settings.json")).unwrap())
            .unwrap();
    assert!(
        saved
            .resources
            .values()
            .any(|prefs| prefs.columns == layout)
    );
    cx.update_window(window, |_, window, cx| {
        assert!(
            window
                .find(format!("column-picker-row-{containers}"))
                .bounds()
                .origin
                .y
                < window
                    .find(format!("column-picker-row-{name}"))
                    .bounds()
                    .origin
                    .y
        );
        window.click(format!("visible-column-{containers}"), cx);
    })
    .unwrap();
    draw(cx, window);
    view.read_with(cx, |view, cx| {
        assert_eq!(
            view.table.read(cx).delegate().column(1, cx).name.as_ref(),
            "Containers"
        );
    });
    cx.update_window(window, |_, window, cx| {
        window.click("resource-column-search", cx);
        window.input("re", cx);
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, cx| {
        assert!(
            window
                .try_find(format!("column-picker-row-{name}"))
                .is_none()
        );
        window.drag_to(
            format!("drag-column-{ready}"),
            format!("column-picker-row-{restarts}"),
            cx,
        );
    })
    .unwrap();
    draw(cx, window);
    view.read_with(cx, |view, cx| {
        let layout = view.table.read(cx).delegate().layout_snapshot();
        let at = |id: &String| layout.order.iter().position(|column| column == id).unwrap();
        assert_eq!(
            at(&ready),
            at(&restarts) + 1,
            "filtered results use stable column IDs"
        );
        assert_eq!(&layout.order[1..3], &[containers.clone(), name]);
        assert_eq!(view.resource_preferences(cx).columns, layout);
    });
    cx.update_window(window, |_, window, cx| {
        window.click("reset-resource-columns", cx)
    })
    .unwrap();
    draw(cx, window);
    view.read_with(cx, |view, cx| {
        assert_eq!(view.table.read(cx).delegate().column_choices(), choices);
        assert_eq!(
            view.resource_preferences(cx).columns,
            ColumnLayout::default()
        );
    });
}

#[::core::prelude::v1::test]
fn pinned_headers_and_cells_stay_at_both_edges_and_actions_can_be_unpinned() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    let (_fixture, window, view) = window(cx, directory.path());
    // A pre-pinning preferences file has no system columns or pin map.
    view.update(cx, |view, cx| {
        view.table.update(cx, |state, cx| {
            let mut old = serde_json::to_value(state.delegate().layout_snapshot()).unwrap();
            let old = old.as_object_mut().unwrap();
            old.remove("pins");
            old["order"]
                .as_array_mut()
                .unwrap()
                .retain(|id| id != "actions" && id != "select");
            let layout = serde_json::from_value(serde_json::Value::Object(old.clone())).unwrap();
            state.delegate_mut().apply_layout(layout);
            state.refresh(cx);
        });
    });
    let choices = view.read_with(cx, |view, cx| {
        view.table.read(cx).delegate().column_choices()
    });
    let column = |name| choices.iter().find(|col| col.name == name).unwrap();
    let name = &column("Name").id;
    let status = &column("Status").id;
    assert_eq!(column("Actions").pin, ColumnPin::Right);
    assert_eq!(column("Select rows").pin, ColumnPin::Left);
    cx.update_window(window, |_, window, cx| {
        window.click("resource-columns-trigger", cx)
    })
    .unwrap();
    draw(cx, window);
    for (side, id) in [("left", name), ("right", status)] {
        cx.update_window(window, |_, window, cx| {
            window.click(format!("pin-{side}-{id}"), cx)
        })
        .unwrap();
        draw(cx, window);
    }
    cx.update_window(window, |_, window, cx| {
        window.click("resource-column-search", cx);
        window.input("Actions", cx);
    })
    .unwrap();
    draw(cx, window);
    for (side, expected) in [
        ("left", ColumnPin::Left),
        ("left", ColumnPin::Unpinned),
        ("right", ColumnPin::Right),
    ] {
        cx.update_window(window, |_, window, cx| {
            window.click(format!("pin-{side}-actions"), cx)
        })
        .unwrap();
        draw(cx, window);
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.table.read(cx).delegate().column_pin("actions"),
                expected
            );
            assert_eq!(
                view.resource_preferences(cx).columns.pins["actions"],
                expected
            );
        });
    }
    for visible in [false, true] {
        cx.update_window(window, |_, window, cx| {
            window.click("visible-column-actions", cx)
        })
        .unwrap();
        draw(cx, window);
        view.read_with(cx, |view, cx| {
            let table = view.table.read(cx).delegate();
            assert_eq!(
                table
                    .column_choices()
                    .into_iter()
                    .find(|c| c.id == "actions")
                    .unwrap()
                    .visible,
                visible
            );
        });
    }
    cx.update_window(window, |_, window, cx| {
        window.click("resource-columns-trigger", cx)
    })
    .unwrap();
    draw(cx, window);
    let positions = |window: &Window, cx: &App| {
        let table = view.read(cx).table.read(cx).delegate();
        let count = table.columns_count(cx);
        assert_eq!(table.column(1, cx).key.as_ref(), name);
        assert_eq!(table.column(count - 2, cx).key.as_ref(), status);
        assert_eq!(table.column(count - 1, cx).key.as_ref(), "actions");
        [0, 1, count - 2, count - 1].map(|index| {
            let head = window.find(("col-header", index)).bounds();
            let cell = window.find(format!("table-cell:0:{index}")).bounds();
            assert!(head.size.width > px(0.) && cell.size.width > px(0.));
            assert!((head.left() - cell.left()).abs() < px(2.));
            (head.left(), cell.left())
        })
    };
    cx.update_window(window, |_, window, cx| {
        let before = positions(window, cx);
        let scroll = view
            .read(cx)
            .table
            .read(cx)
            .horizontal_scroll_handle
            .clone();
        assert!(scroll.max_offset().x > px(0.));
        scroll.set_offset(point(-scroll.max_offset().x, px(0.)));
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(scroll.offset().x < px(0.));
        assert_eq!(before, positions(window, cx));
    })
    .unwrap();
    let saved: crate::settings::Preferences =
        serde_json::from_slice(&std::fs::read(directory.path().join("settings.json")).unwrap())
            .unwrap();
    let saved = &saved.resources.values().next().unwrap().columns;
    assert_eq!(saved.pins[name], ColumnPin::Left);
    assert_eq!(saved.pins[status], ColumnPin::Right);
}

#[::core::prelude::v1::test]
fn first_filter_can_be_saved_searched_and_applied_without_empty_state_confusion() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    let (_fixture, window, view) = window(cx, directory.path());
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.table.update(cx, |table, _| {
                table.delegate_mut().set_filter("api");
                table.delegate_mut().set_label_filter("app=web").unwrap();
                table
                    .delegate_mut()
                    .set_field_filter(Field::PodStatus, Some("Running".into()));
            });
        });
        window.click("resource-saved-filters-trigger", cx);
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, cx| {
        assert!(window.find("saved-filter-empty").visible());
        assert!(window.try_find("saved-filter-search").is_none());
        assert!(window.find("saved-filter-menu").bounds().size.height < px(280.));
        window.click("saved-filter-name", cx);
        cx.write_to_clipboard(ClipboardItem::new_string(" ".into()));
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-v"
            } else {
                "ctrl-v"
            },
            cx,
        );
        window.press("enter", cx);
        window.click("save-resource-filter", cx);
    })
    .unwrap();
    view.read_with(cx, |view, cx| {
        assert!(view.resource_preferences(cx).filters.is_empty());
        assert!(view.list_settings.error.is_none());
    });
    cx.update_window(window, |_, window, cx| {
        window.click("saved-filter-name", cx);
        cx.write_to_clipboard(ClipboardItem::new_string("API pods".into()));
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-v"
            } else {
                "ctrl-v"
            },
            cx,
        );
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click("save-resource-filter", cx);
    })
    .unwrap();
    draw(cx, window);
    view.read_with(cx, |view, cx| {
        let preset = &view.resource_preferences(cx).filters["API pods"];
        assert_eq!(preset.namespaces, BTreeSet::from(["default".into()]));
        assert_eq!(preset.search, "api");
        assert_eq!(preset.labels, "app=web");
        assert_eq!(
            preset.fields.get(&Field::PodStatus).map(String::as_str),
            Some("Running")
        );
        assert!(view.list_settings.name.read(cx).value().is_empty());
    });
    cx.update_window(window, |_, window, cx| {
        assert!(window.find("saved-filter-current-API pods").visible());
        assert!(window.try_find("saved-filter-empty").is_none());
        window.click("saved-filter-search", cx);
        window.input("missing", cx);
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, cx| {
        assert!(window.find("saved-filter-no-results").visible());
        assert!(window.try_find("saved-filter-empty").is_none());
        window.press(
            if cfg!(target_os = "macos") {
                "cmd-a"
            } else {
                "ctrl-a"
            },
            cx,
        );
        window.press("backspace", cx);
        view.update(cx, |view, cx| {
            view.table.update(cx, |table, cx| {
                table.delegate_mut().set_filter("worker");
                cx.notify();
            })
        });
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, cx| {
        assert!(window.try_find("saved-filter-current-API pods").is_none());
        window.click("apply-filter-API pods", cx);
    })
    .unwrap();
    draw(cx, window);
    view.read_with(cx, |view, cx| {
        assert!(!view.list_settings.presets_open);
        assert_eq!(view.table.read(cx).delegate().filter(), "api");
        assert_eq!(view.row_search.read(cx).value().as_ref(), "api");
    });
}

#[::core::prelude::v1::test]
fn replace_delete_and_failed_save_keep_feedback_inside_the_menu() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    let (_fixture, window, view) = window(cx, directory.path());
    cx.update_window(window, |_, window, cx| {
        window.click("resource-saved-filters-trigger", cx);
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click("saved-filter-name", cx);
        window.input("Team", cx);
        window.press("enter", cx);
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, cx| {
        view.update(cx, |view, cx| {
            view.table.update(cx, |table, _| {
                table.delegate_mut().set_filter("worker");
            })
        });
        window.click("saved-filter-name", cx);
        window.input("Team", cx);
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, cx| {
        assert_eq!(
            window.find("save-resource-filter").label(),
            Some("Replace saved filter")
        );
        window.click("save-resource-filter", cx);
    })
    .unwrap();
    draw(cx, window);
    view.read_with(cx, |view, cx| {
        let preferences = view.resource_preferences(cx);
        assert_eq!(preferences.filters.len(), 1);
        assert_eq!(preferences.filters["Team"].search, "worker");
    });
    cx.update_window(window, |_, window, cx| {
        window.click("remove-filter-Team", cx);
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, _| {
        assert!(window.find("saved-filter-empty").visible());
        assert!(window.try_find("saved-filter-search").is_none());
    })
    .unwrap();
    let blocked = directory.path().join("not-a-directory");
    std::fs::write(&blocked, "blocked").unwrap();
    cx.update(|cx| crate::settings::store(cx).update(cx, |state, _| state.directory = blocked));
    cx.update_window(window, |_, window, cx| {
        window.click("saved-filter-name", cx);
        window.input("Unsaved", cx);
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, cx| {
        window.click("save-resource-filter", cx);
    })
    .unwrap();
    draw(cx, window);
    cx.update_window(window, |_, window, _| {
        assert!(window.find("saved-filter-error").visible())
    })
    .unwrap();
    view.read_with(cx, |view, cx| {
        assert!(view.list_settings.presets_open);
        assert!(view.resource_preferences(cx).filters.is_empty());
        assert_eq!(view.list_settings.name.read(cx).value().as_ref(), "Unsaved");
    });
}
