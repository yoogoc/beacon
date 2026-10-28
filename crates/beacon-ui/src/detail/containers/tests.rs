use super::*;
use crate::feature_test_support as support;
use crate::settings::{self, ContainerLayout};
use gpui_kit::test::TestWindowExt as _;
use serde_json::json;

fn object() -> Value {
    json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"container-layout","namespace":"default","uid":"containers","resourceVersion":"1"},"spec":{"containers":[{"name":"main","image":"registry:5000/team/app:v1","ports":[{"name":"http","containerPort":4000}],"livenessProbe":{"httpGet":{"path":"/health","port":"http"}},"env":[{"name":"APP_ENV","value":"staging"}],"volumeMounts":[{"name":"config","mountPath":"/app/config","readOnly":true}]},{"name":"sidecar","image":"sidecar:latest"}],"initContainers":[{"name":"setup","image":"setup:v1"}]},"status":{"phase":"Running","containerStatuses":[{"name":"main","ready":true,"restartCount":2,"state":{"running":{}}}]}})
}

fn open(
    cx: &mut TestAppContext,
    session: Arc<ClusterSession>,
) -> (AnyWindowHandle, Entity<DetailView>) {
    cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    Point::default(),
                    size(px(1000.), px(1800.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    DetailView::new(
                        session,
                        Arc::new(support::kind("", "Pod", "pods")),
                        Arc::new(serde_json::from_value(object()).unwrap()),
                        None,
                        window,
                        cx,
                    )
                })
            },
        )
        .unwrap()
    })
}

#[::core::prelude::v1::test]
fn container_workbench_tabs_are_independent_and_survive_layout_changes() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    support::workspace(cx, directory.path());
    let (_fixture, session) = support::fixture(cx, "container-layout", vec![object()]);
    let (window, view) = open(cx, session.clone());
    let (other_window, _) = open(cx, session);
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            settings::store(cx).read(cx).preferences.container_layout,
            ContainerLayout::Workbench
        );
        assert_eq!(
            window.find("container-workbench-panel").label(),
            Some("Ports & resources")
        );
        window
            .within("container:main")
            .click("container-tab-health", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("container-workbench-panel").label(),
            Some("Health checks")
        );
        window
            .within("container:sidecar")
            .click("toggle-container", cx);
        window.render_frame(cx);
        window
            .within("container:sidecar")
            .click("container-tab-environment", cx);
        window.render_frame(cx);
        assert_eq!(
            window
                .within("container:main")
                .find("container-workbench-panel")
                .label(),
            Some("Health checks")
        );
        assert_eq!(
            window
                .within("container:sidecar")
                .find("container-workbench-panel")
                .label(),
            Some("Environment variables")
        );
        settings::save_container_layout(ContainerLayout::Grouped, cx).unwrap();
        window.render_frame(cx);
        assert!(window.try_find("container-tab-health").is_none());
        assert!(
            !view
                .read(cx)
                .expanded_sections
                .contains("container:main:environment")
        );
        window
            .within("container:main:environment")
            .click("toggle-container-group", cx);
        assert!(
            view.read(cx)
                .expanded_sections
                .contains("container:main:environment")
        );
        settings::save_container_layout(ContainerLayout::Workbench, cx).unwrap();
        window.render_frame(cx);
        assert_eq!(
            window
                .within("container:main")
                .find("container-workbench-panel")
                .label(),
            Some("Health checks")
        );
        assert_eq!(
            window
                .within("container:sidecar")
                .find("container-workbench-panel")
                .label(),
            Some("Environment variables")
        );
        settings::save_container_layout(ContainerLayout::Grouped, cx).unwrap();
        window.render_frame(cx);
        assert!(
            view.read(cx)
                .expanded_sections
                .contains("container:main:environment")
        );
        let persisted: settings::Preferences =
            serde_json::from_slice(&std::fs::read(directory.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(persisted.container_layout, ContainerLayout::Grouped);
        window.remove_window();
    })
    .unwrap();
    cx.update_window(other_window, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("container-tab-health").is_none());
        assert!(window.find("container:main:environment").visible());
        window.remove_window();
    })
    .unwrap();
}

#[::core::prelude::v1::test]
fn container_cards_fit_a_narrow_pane_in_both_layouts_and_languages() {
    let directory = tempfile::tempdir().unwrap();
    let cx = &mut support::context();
    support::workspace(cx, directory.path());
    let (_fixture, session) = support::fixture(cx, "container-narrow", vec![object()]);
    let (window, _) = open(cx, session);
    for language in [
        crate::i18n::Language::English,
        crate::i18n::Language::SimplifiedChinese,
    ] {
        for layout in [ContainerLayout::Workbench, ContainerLayout::Grouped] {
            for width in [360., 680.] {
                cx.simulate_window_resize(window, size(px(width), px(1800.)));
                cx.update_window(window, |_, window, cx| {
                    crate::i18n::apply(language, cx);
                    settings::save_container_layout(layout, cx).unwrap();
                    window.render_frame(cx);
                    window.render_frame(cx);
                    let card = window.find("container:main");
                    assert!(card.visible());
                    assert!(
                        card.bounds().right() <= window.bounds().size.width,
                        "{layout:?} at {width}: {:?}",
                        card.bounds()
                    );
                    assert!(card.bounds().left() >= px(0.));
                    if layout == ContainerLayout::Workbench {
                        for category in Category::ALL {
                            let button = window.within("container:main").find(category.id());
                            assert!(button.visible());
                            assert!(button.bounds().right() <= window.bounds().size.width);
                            window.within("container:main").click(category.id(), cx);
                            window.render_frame(cx);
                        }
                    }
                })
                .unwrap();
            }
        }
    }
    cx.update_window(window, |_, window, _| window.remove_window())
        .unwrap();
}
