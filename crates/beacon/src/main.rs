// A GUI binary must not open a console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod logging;

use beacon_ui::{BeaconApp, Bridge};
use gpui_kit::component::{Root, TitleBar};
use gpui_kit::*;

fn main() -> anyhow::Result<()> {
    if let Some(result) = beacon_updater::helper_main() {
        return result;
    }
    // GPUI Fast 0.1.5 can underflow while rebasing retained paint ranges.
    // Use its supported fallback for every window until that path is fixed.
    // An explicit value remains available for renderer diagnostics.
    if std::env::var_os("GPUI_VIEW_RETENTION").is_none() {
        // SAFETY: main has not started the logging worker, GPUI or Tokio yet.
        unsafe { std::env::set_var("GPUI_VIEW_RETENTION", "0") };
    }

    // PATH also has to be updated before the logging worker starts, and
    // before kubeconfig exec credential plugins are looked up on PATH.
    beacon_kube::shell_env::merge_login_shell_path();
    let logging = logging::init()?;
    let log_directory = logging.directory.clone();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        target = std::env::consts::OS,
        retained_views = std::env::var("GPUI_VIEW_RETENTION").map_or(true, |value| value != "0"),
        "starting beacon"
    );

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            beacon_ui::app::init(log_directory, cx);

            if let Err(err) = Bridge::init(cx) {
                // Without a runtime there is nothing to show, and a window that
                // can never connect is worse than a clear failure.
                tracing::error!(%err, "could not start the network runtime");
                cx.quit();
                return;
            }
            beacon_ui::updates::init(cx);

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
