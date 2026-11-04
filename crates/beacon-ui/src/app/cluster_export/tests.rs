use super::*;
use crate::feature_test_support as support;
use beacon_kube::sources::{self, Source, SourceKind};
use gpui_kit::test::TestWindowExt as _;

fn setup(
    cx: &mut TestAppContext,
    directory: &std::path::Path,
) -> (AnyWindowHandle, Entity<BeaconApp>, ClusterId) {
    support::workspace(cx, directory);
    let mut preferences = sources::Preferences {
        system_enabled: false,
        sources: vec![],
    };
    for name in ["first", "second"] {
        let path = directory.join(format!("{name}.yaml"));
        std::fs::write(&path, format!(
            "apiVersion: v1\nkind: Config\nclusters:\n- name: cluster\n  cluster:\n    server: https://{name}.invalid\ncontexts:\n- name: {name}-context\n  context:\n    cluster: cluster\n    user: user\n    namespace: team-{name}\nusers:\n- name: user\n  user:\n    token: {name}-token\n"
        )).unwrap();
        preferences.sources.push(Source {
            id: name.into(),
            name: name.into(),
            kind: SourceKind::File { path },
            enabled: true,
            excluded: Default::default(),
        });
    }
    let catalog = sources::load(&preferences, directory, &[]);
    let id = catalog.contexts.entries()[1].id.clone();
    cx.update(|cx| {
        crate::sources::store(cx).update(cx, |state, _| {
            state.labels = catalog.labels();
            state.catalog = Arc::new(catalog);
        })
    });
    let (window, app) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                let mut app = BeaconApp::new(window, cx);
                app.preferences.context_display = crate::settings::ContextDisplay::SourceLabels;
                app
            })
        })
        .unwrap()
    });
    (window, app, id)
}

fn menu_item(window: &Window, label: &str) -> gpui_kit::base::test_support::ElementSnapshot {
    gpui_kit::base::test_support::snapshots(window)
        .into_iter()
        .find(|item| item.role() == Some(Role::MenuItem) && item.label() == Some(label))
        .unwrap_or_else(|| panic!("missing menu item: {label}"))
}

fn open_menu(cx: &mut TestAppContext, window: AnyWindowHandle) {
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.within("cluster-tree").right_click("0-1", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| window.render_frame(cx))
        .unwrap();
}

#[::core::prelude::v1::test]
fn cluster_export_menu_exports_the_clicked_offline_source_without_connecting() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    let (window, app, id) = setup(cx, directory.path());
    open_menu(cx, window);
    cx.update_window(window, |_, window, cx| {
        let item = menu_item(window, "Export cluster…");
        assert_ne!(item.disabled(), Some(true));
        window.click(item.path().last().unwrap().clone(), cx);
        assert!(app.read(cx).cluster_exports.contains(&id));
    })
    .unwrap();
    // The selected config is frozen even if sources change while choosing a path.
    cx.update(|cx| {
        crate::sources::store(cx).update(cx, |state, _| {
            state.catalog = Arc::default();
        })
    });
    let destination = directory.path().join("export.yaml");
    let selected = destination.clone();
    cx.simulate_new_path_selection(move |_| Some(selected));
    support::settle(cx, |cx| {
        app.read_with(cx, |app, _| app.cluster_exports.is_empty())
    });
    let value: serde_json::Value =
        serde_saphyr::from_str(&std::fs::read_to_string(destination).unwrap()).unwrap();
    assert_eq!(value["current-context"], "second-context");
    assert_eq!(value["contexts"][0]["context"]["namespace"], "team-second");
    assert_eq!(
        value["clusters"][0]["cluster"]["server"],
        "https://second.invalid"
    );
    assert_eq!(value["users"][0]["user"]["token"], "second-token");
    assert!(!value.to_string().contains("first-token"));
    app.read_with(cx, |app, cx| {
        assert!(app.tabs.is_empty());
        assert!(app.connections.read(cx).sessions.is_empty());
        assert!(!app.connections.read(cx).connecting(&id));
    });
}

#[::core::prelude::v1::test]
fn cluster_export_cancel_failure_and_missing_source_do_not_change_files_or_connections() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    let (window, app, id) = setup(cx, directory.path());
    cx.update_window(window, |_, window, cx| {
        app.update(cx, |app, cx| {
            app.export_cluster(id.clone(), window, cx);
            app.export_cluster(id.clone(), window, cx);
        });
    })
    .unwrap();
    cx.simulate_new_path_selection(|_| None);
    support::settle(cx, |cx| {
        app.read_with(cx, |app, _| app.cluster_exports.is_empty())
    });
    cx.update(|cx| {
        crate::sources::store(cx).update(cx, |state, _| {
            let target = Arc::make_mut(&mut state.catalog)
                .targets
                .get_mut(&id)
                .unwrap();
            Arc::make_mut(&mut target.config).auth_infos[0]
                .auth_info
                .as_mut()
                .unwrap()
                .client_key = Some(
                directory
                    .path()
                    .join("missing-key")
                    .to_string_lossy()
                    .into_owned(),
            );
        })
    });
    let path = directory.path().join("previous.yaml");
    std::fs::write(&path, b"keep previous export").unwrap();
    cx.update_window(window, |_, window, cx| {
        app.update(cx, |app, cx| app.export_cluster(id.clone(), window, cx))
    })
    .unwrap();
    let selected = path.clone();
    cx.simulate_new_path_selection(move |_| Some(selected));
    support::settle(cx, |cx| {
        app.read_with(cx, |app, _| app.cluster_exports.is_empty())
    });
    assert_eq!(std::fs::read(path).unwrap(), b"keep previous export");
    cx.update(|cx| {
        crate::sources::store(cx).update(cx, |state, _| {
            Arc::make_mut(&mut state.catalog).targets.remove(&id);
        })
    });
    open_menu(cx, window);
    cx.update_window(window, |_, window, cx| {
        let item = menu_item(window, "Export cluster…");
        window.click(item.path().last().unwrap().clone(), cx);
        assert!(app.read(cx).cluster_exports.is_empty());
        window.press("escape", cx);
        crate::i18n::apply(crate::i18n::Language::SimplifiedChinese, cx);
        assert_eq!(crate::i18n::text("Export cluster…"), "导出集群…");
        assert!(export_error(Failure::Context).contains("缺少"));
        app.read_with(cx, |app, cx| {
            assert!(app.cluster_exports.is_empty());
            assert!(app.tabs.is_empty());
            assert!(!app.connections.read(cx).connecting(&id));
        });
    })
    .unwrap();
}
