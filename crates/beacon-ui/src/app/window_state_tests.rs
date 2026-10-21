use super::*;
use crate::feature_test_support as support;

#[::core::prelude::v1::test]
fn window_state_restores_maximization_and_saves_only_the_main_window_at_shutdown() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let path = directory.path().join("window-state.json");
    std::fs::write(&path, r#"{"mode":"maximized","width":1000,"height":700}"#).unwrap();
    cx.read(|cx| {
        let options = crate::window_state::main_window_options(cx);
        let WindowBounds::Maximized(bounds) = options.window_bounds.unwrap() else {
            panic!("a previously maximized window must reopen maximized");
        };
        assert_eq!(bounds.size, size(px(1000.), px(700.)));
    });
    let main = cx.update(|cx| {
        // TestWindow does not implement native maximization; use a windowed
        // window here to exercise real bounds observers and the quit handler.
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| BeaconApp::new(window, cx))
        })
        .unwrap()
        .0
    });
    let detached = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| BeaconApp::new_workspace(false, window, cx))
        })
        .unwrap()
        .0
    });
    cx.simulate_window_resize(main, size(px(1100.), px(750.)));
    cx.simulate_window_resize(detached, size(px(800.), px(600.)));
    cx.run_until_parked();
    cx.update(|cx| cx.shutdown());
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["mode"], "windowed");
    assert_eq!(saved["width"], 1100.);
    assert_eq!(saved["height"], 750.);
    assert!(!directory.path().join("settings.json").exists());
}
