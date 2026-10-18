use super::*;
use crate::{feature_test_support as support, i18n::Language};
use gpui_kit::test::TestWindowExt as _;

fn window(
    cx: &mut TestAppContext,
    target: Option<ClusterId>,
) -> (AnyWindowHandle, Entity<PreferencesView>) {
    cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Point::default(),
                    size(px(880.), px(760.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                window.set_view_retention(false);
                let view = cx.new(|cx| PreferencesView::new(target, window, cx));
                crate::i18n::window_title(view.read(cx).title(), window, cx);
                view
            },
        )
        .unwrap()
    })
}

fn draw(cx: &mut TestAppContext, windows: &[AnyWindowHandle]) {
    for _ in 0..3 {
        cx.run_until_parked();
        for window in windows {
            cx.update_window(*window, |_, window, cx| window.render_frame(cx))
                .unwrap();
        }
    }
}

#[::core::prelude::v1::test]
fn language_menu_updates_existing_windows_and_keeps_drafts_connections_and_layouts() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    support::workspace(cx, directory.path());
    let (fixture, session) = support::fixture(cx, "language-test", vec![]);
    let active = session.clone();
    let (main, cluster) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                crate::cluster::ClusterView::new(
                    session,
                    Some("default".into()),
                    Some(std::sync::Arc::new(support::kind("", "Pod", "pods"))),
                    None,
                    false,
                    window,
                    cx,
                )
            })
        })
        .unwrap()
    });
    support::settle(cx, |_| active.active_watches() > 0);
    let watches = active.active_watches();
    let (settings_window, settings) = window(cx, None);
    let (cluster_window, cluster_settings) = window(cx, Some(ClusterId::new("language-test")));
    let windows = [main, settings_window, cluster_window];
    draw(cx, &windows);
    cx.update_window(settings_window, |_, window, cx| {
        settings.update(cx, |view, cx| {
            view.proxy.url.update(cx, |input, cx| {
                input.set_value("http://unsaved.example:8080", window, cx)
            });
            view.dirty = true;
        });
        window.click("settings-language", cx);
    })
    .unwrap();
    draw(cx, &windows);
    cx.update_window(settings_window, |_, window, cx| {
        assert_eq!(
            window.within("popup-menu").find(2usize).label(),
            Some("简体中文")
        );
        window.within("popup-menu").click(2usize, cx);
    })
    .unwrap();
    draw(cx, &windows);
    cx.update_window(settings_window, |_, window, cx| {
        assert_eq!(window.find("settings-language").label(), Some("简体中文"));
        assert_eq!(
            window.find("settings-window-title").label(),
            Some("应用设置")
        );
        settings.read_with(cx, |view, cx| {
            assert!(view.dirty);
            assert_eq!(value(&view.proxy.url, cx), "http://unsaved.example:8080");
        });
    })
    .unwrap();
    cx.update_window(cluster_window, |_, window, cx| {
        assert_eq!(
            window.find("settings-window-title").label(),
            Some("集群设置")
        );
        assert!(!cluster_settings.read(cx).dirty);
    })
    .unwrap();
    cx.update_window(main, |_, window, _| {
        assert_eq!(window.find("resource-columns-trigger").label(), Some("列"));
    })
    .unwrap();
    assert_eq!(active.active_watches(), watches);
    assert_eq!(
        cluster.read_with(cx, |view, _| view.session().id().to_string()),
        "language-test"
    );
    let saved: crate::settings::Preferences =
        serde_json::from_slice(&std::fs::read(directory.path().join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(saved.language, Language::SimplifiedChinese);
    assert_eq!(
        saved.proxy,
        beacon_kube::connection::Proxy::System,
        "unsaved proxy stays a draft"
    );
    cx.update_window(settings_window, |_, window, cx| {
        window.click("settings-language", cx)
    })
    .unwrap();
    draw(cx, &windows);
    cx.update_window(settings_window, |_, window, cx| {
        window.within("popup-menu").click(1usize, cx)
    })
    .unwrap();
    draw(cx, &windows);
    cx.update_window(main, |_, window, _| {
        assert_eq!(
            window.find("resource-columns-trigger").label(),
            Some("Columns")
        )
    })
    .unwrap();
    cx.update_window(settings_window, |_, window, _| {
        assert_eq!(
            window.find("settings-window-title").label(),
            Some("Application settings")
        )
    })
    .unwrap();
    assert_eq!(active.active_watches(), watches);
    drop(fixture);
}

#[::core::prelude::v1::test]
fn failed_language_save_keeps_the_current_language_and_existing_preferences() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    support::workspace(cx, directory.path());
    let (handle, _) = window(cx, None);
    let blocked = directory.path().join("blocked");
    std::fs::write(&blocked, "file").unwrap();
    cx.update(|cx| settings::store(cx).update(cx, |state, _| state.directory = blocked));
    cx.update_window(handle, |_, _, cx| {
        assert!(settings::save_language(Language::SimplifiedChinese, cx).is_err());
        assert_eq!(
            settings::store(cx).read(cx).preferences.language,
            Language::English
        );
        assert_eq!(crate::i18n::text("Save").as_ref(), "Save");
    })
    .unwrap();
}
