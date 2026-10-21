//! Main-window placement, kept separate from editable application preferences.
use gpui_kit::{component::TitleBar, *};
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, path::Path, rc::Rc};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Placement {
    mode: Mode,
    /// Keep the ordinary window's size even while maximized or fullscreen.
    width: f32,
    height: f32,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            mode: Mode::Windowed,
            width: 1280.,
            height: 800.,
        }
    }
}

impl Placement {
    fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .filter(|state| {
                state.width.is_finite()
                    && state.height.is_finite()
                    && state.width > 0.
                    && state.height > 0.
            })
            .unwrap_or_default()
    }

    fn record(&mut self, reported: WindowBounds, maximized: bool, fullscreen: bool) {
        let next = if fullscreen || matches!(reported, WindowBounds::Fullscreen(_)) {
            Mode::Fullscreen
        } else if maximized || matches!(reported, WindowBounds::Maximized(_)) {
            Mode::Maximized
        } else {
            Mode::Windowed
        };
        // macOS reports Windowed bounds while zoomed. Do not mistake those
        // enlarged bounds for the size to restore when leaving maximization.
        let restore = match reported {
            WindowBounds::Windowed(bounds) if next == Mode::Windowed => Some(bounds),
            WindowBounds::Maximized(bounds) => Some(bounds),
            WindowBounds::Fullscreen(bounds) if self.mode == Mode::Windowed => Some(bounds),
            _ => None,
        };
        if let Some(bounds) = restore {
            self.width = bounds.size.width.as_f32();
            self.height = bounds.size.height.as_f32();
        }
        self.mode = next;
    }

    fn bounds(&self, cx: &App) -> WindowBounds {
        let display_size = cx
            .primary_display()
            .map(|display| display.bounds().size)
            .unwrap_or(size(px(1280.), px(800.)));
        // A previously attached, larger monitor may no longer be available.
        let restored_size = size(
            px(self.width.max(720.)).min(display_size.width),
            px(self.height.max(480.)).min(display_size.height),
        );
        let bounds = Bounds::centered(None, restored_size, cx);
        match self.mode {
            Mode::Windowed => WindowBounds::Windowed(bounds),
            Mode::Maximized => WindowBounds::Maximized(bounds),
            Mode::Fullscreen => WindowBounds::Fullscreen(bounds),
        }
    }

    fn save(&self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(path.parent().ok_or("No window state directory.")?)
            .map_err(|error| error.to_string())?;
        crate::settings::atomic_write(path, &bytes)
    }
}

/// Restore the main window's last mode and its ordinary size.
pub fn main_window_options(cx: &App) -> WindowOptions {
    let path = crate::settings::store(cx)
        .read(cx)
        .directory
        .join("window-state.json");
    WindowOptions {
        window_bounds: Some(Placement::load(&path).bounds(cx)),
        window_min_size: Some(size(px(720.), px(480.))),
        ..TitleBar::window_options()
    }
}

/// Only the main workspace registers these observers. Detached resource and
/// settings windows must never overwrite its placement.
pub(crate) fn track<T: 'static>(window: &mut Window, cx: &mut Context<T>) -> Vec<Subscription> {
    let path = crate::settings::store(cx)
        .read(cx)
        .directory
        .join("window-state.json");
    let placement = Rc::new(RefCell::new(Placement::load(&path)));
    let observed = placement.clone();
    let bounds = cx.observe_window_bounds(window, move |_, window, _| {
        observed.borrow_mut().record(
            window.window_bounds(),
            window.is_maximized(),
            window.is_fullscreen(),
        );
    });
    let handle = window.window_handle();
    let quit = cx.on_app_quit(move |_, cx| {
        // Read once more before GPUI removes the windows. This also covers
        // quitting immediately after a resize or an updater-triggered restart.
        let _ = handle.update(cx, |_, window, _| {
            placement.borrow_mut().record(
                window.window_bounds(),
                window.is_maximized(),
                window.is_fullscreen(),
            );
        });
        let placement = *placement.borrow();
        let path = path.clone();
        cx.background_executor().spawn(async move {
            if let Err(error) = placement.save(&path) {
                tracing::warn!(%error, "could not save main window state");
            }
        })
    });
    vec![bounds, quit]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(20.), px(40.)), size(px(width), px(height)))
    }

    #[::core::prelude::v1::test]
    fn macos_zoom_keeps_ordinary_size_and_unmaximize_resets_mode() {
        let mut state = Placement::default();
        state.record(WindowBounds::Windowed(bounds(1000., 700.)), false, false);
        state.record(WindowBounds::Windowed(bounds(1920., 1080.)), true, false);
        assert_eq!(state.mode, Mode::Maximized);
        assert_eq!((state.width, state.height), (1000., 700.));
        state.record(WindowBounds::Windowed(bounds(1000., 700.)), false, false);
        assert_eq!(state.mode, Mode::Windowed);
    }

    #[::core::prelude::v1::test]
    fn native_restore_bounds_and_fullscreen_preserve_windowed_size() {
        let mut state = Placement::default();
        state.record(WindowBounds::Maximized(bounds(1100., 750.)), true, false);
        state.record(WindowBounds::Fullscreen(bounds(1920., 1080.)), false, true);
        assert_eq!(state.mode, Mode::Fullscreen);
        assert_eq!((state.width, state.height), (1100., 750.));
        state.record(WindowBounds::Maximized(bounds(1100., 750.)), true, false);
        assert_eq!(state.mode, Mode::Maximized);
    }

    #[::core::prelude::v1::test]
    fn placement_round_trips_and_invalid_files_fall_back() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/window-state.json");
        assert_eq!(Placement::load(&path), Placement::default());
        let state = Placement {
            mode: Mode::Maximized,
            width: 1100.,
            height: 750.,
        };
        state.save(&path).unwrap();
        assert_eq!(Placement::load(&path), state);
        for invalid in ["{", r#"{"mode":"maximized","width":0,"height":800}"#] {
            std::fs::write(&path, invalid).unwrap();
            assert_eq!(Placement::load(&path), Placement::default());
        }
    }
}
