// A GUI binary must not open a console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod logging;

use beacon_ui::{BeaconApp, Bridge};
use gpui_kit::component::{Root, TitleBar};
use gpui_kit::*;

fn main() -> anyhow::Result<()> {
    // Order matters here, and both of these run before any thread is started:
    //
    // 1. `merge_login_shell_path` mutates the process environment, which is
    //    only sound while we are still single-threaded.
    // 2. It has to happen before any cluster connection, because kubeconfig
    //    `exec` credential plugins are looked up on PATH.
    let _guard = logging::init()?;
    beacon_kube::shell_env::merge_login_shell_path();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        target = std::env::consts::OS,
        "starting beacon"
    );

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);

            if let Err(err) = Bridge::init(cx) {
                // Without a runtime there is nothing to show, and a window that
                // can never connect is worse than a clear failure.
                tracing::error!(%err, "could not start the network runtime");
                cx.quit();
                return;
            }

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1280.), px(800.)),
                    cx,
                ))),
                window_min_size: Some(size(px(720.), px(480.))),
                ..TitleBar::window_options()
            };

            cx.spawn(async move |cx| {
                let window = cx.open_window(options, |window, cx| {
                    let view = cx.new(|cx| BeaconApp::new(window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                });

                if let Err(err) = window {
                    tracing::error!(%err, "could not open the main window");
                }
            })
            .detach();
        });

    Ok(())
}
