use super::*;
use crate::feature_test_support as support;
use gpui_kit::test::TestWindowExt as _;

fn tab(id: u64, kind: &str) -> Tab {
    Tab {
        id,
        cluster: ClusterId::new("tab-title-fixture"),
        namespace: None,
        initial_kind: Some(Arc::new(support::kind("", kind, "resources"))),
        initial_scope: None,
        initial_mode: Mode::Objects,
        state: TabState::Connecting,
        _navigation: None,
        _resource_requests: None,
        _files_requests: None,
        _overview_requests: None,
        files_object: None,
    }
}

#[::core::prelude::v1::test]
fn tab_titles_preserve_the_destination_through_connection_states() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (window, app) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| BeaconApp::new(window, cx))
        })
        .unwrap()
    });
    cx.update_window(window, |_, _, cx| {
        app.update(cx, |app, cx| {
            let mut tab = tab(0, "ValidatingAdmissionPolicyBinding");
            for state in [
                TabState::Connecting,
                TabState::Failed("Authentication failed".into()),
                TabState::Disconnected,
            ] {
                tab.state = state;
                assert_eq!(app.tab_title(&tab, cx), "ValidatingAdmissionPolicyBinding");
                tab.initial_kind = None;
                assert_eq!(app.tab_title(&tab, cx), "Cluster");
                for mode in [Mode::Overview, Mode::Releases, Mode::Forwards] {
                    tab.initial_mode = mode;
                    assert_eq!(app.tab_title(&tab, cx), mode.label());
                }
                tab.initial_mode = Mode::Objects;
                tab.initial_kind = Some(Arc::new(support::kind(
                    "admissionregistration.k8s.io",
                    "ValidatingAdmissionPolicyBinding",
                    "validatingadmissionpolicybindings",
                )));
            }
            tab.initial_kind = None;
            crate::i18n::apply(crate::i18n::Language::SimplifiedChinese, cx);
            assert_eq!(app.tab_title(&tab, cx), crate::i18n::text("Cluster"));
            tab.initial_mode = Mode::Forwards;
            assert_eq!(app.tab_title(&tab, cx), crate::i18n::text("Port Forwards"));
        });
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn tab_titles_expand_without_wrapping_and_overflow_stays_scrollable() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (window, app) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| {
                let mut app = BeaconApp::new(window, cx);
                app.contexts = Ok(Contexts::default());
                app.sidebar_collapsed = true;
                let mut files = tab(1, "Pod");
                files.files_object = Some(Arc::new(serde_json::from_value(serde_json::json!({
                    "apiVersion": "v1", "kind": "Pod",
                    "metadata": {"name": "dashboard-workflow-controller-very-long-resource-name-7fbf859bbf"}
                })).unwrap()));
                app.tabs = vec![tab(0, "ValidatingAdmissionPolicyBinding"), files, tab(2, "Pod")];
                for id in 0..3 {
                    app.layout.insert(id, app.layout.focused, None);
                }
                app.layout.select(0);
                app.sync_layout(window, cx);
                app
            })
        })
        .unwrap()
    });
    for width in [1100., 650., 420.] {
        cx.simulate_window_resize(window, size(px(width), px(600.)));
        cx.update_window(window, |_, window, cx| {
            assert_eq!(window.viewport_size().width, px(width));
            let scroll = app.read(cx).tab_scrolls[&0].clone();
            scroll.set_offset(Point::default());
            window.render_frame(cx);
            window.render_frame(cx);
            for id in 0..3usize {
                let tab = window.find(("resource-tab", id));
                let title = window.find(("resource-tab-title", id)).bounds();
                let close = window.find(format!("close-tab-{id}")).bounds();
                assert_eq!(tab.bounds().size.height, px(28.));
                assert!(title.size.height <= tab.bounds().size.height);
                assert!(title.left() > tab.bounds().left());
                assert!(title.right() <= close.left());
                assert!(close.right() <= tab.bounds().right());
                if id < 2 {
                    assert!(title.size.width > px(190.), "long title was constrained");
                }
            }
            let plus = window.find(("new-tab", 0usize)).bounds();
            let list = window.find(("tab-list", 0usize)).bounds();
            assert!(list.right() <= window.viewport_size().width);
            assert_eq!(plus.right(), list.left());
            if width <= 650. {
                assert!(scroll.max_offset().x > px(0.));
                scroll.set_offset(point(-scroll.max_offset().x, px(0.)));
                window.render_frame(cx);
                assert!(window.find(("resource-tab", 2usize)).visible());
                assert!(window.find("close-tab-2").bounds().right() <= plus.left());
            }
        })
        .unwrap();
    }
}
