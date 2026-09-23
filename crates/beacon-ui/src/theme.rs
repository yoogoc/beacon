//! Beacon's semantic colours.
//!
//! gpui-component supplies the base palette (background, border, muted,
//! primary...). This module adds the vocabulary Beacon actually reasons in --
//! the health of a workload and the health of a connection -- so that no view
//! picks a literal colour. Changing how "degraded" looks is a change here and
//! nowhere else.

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::*;

/// The state of a thing we are showing, independent of the resource kind.
///
/// Pods, Deployments, Nodes and connections all collapse onto these five.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Running, Ready, Bound, Active.
    Healthy,
    /// Pending, ContainerCreating, Terminating, rolling out.
    Progressing,
    /// Reachable but not right: NotReady nodes, restart loops, evictions.
    Warning,
    /// Failed, CrashLoopBackOff, Error, disconnected.
    Critical,
    /// No information yet.
    Unknown,
}

pub trait BeaconTheme {
    /// Foreground colour for a status label.
    fn tone(&self, tone: Tone) -> Hsla;
    /// Background for a status pill, at the low contrast a dense table wants.
    fn tone_surface(&self, tone: Tone) -> Hsla;
    /// Background of the resource table's header row.
    fn table_header(&self) -> Hsla;
    /// Background of alternating rows.
    fn table_stripe(&self) -> Hsla;
}

impl BeaconTheme for Theme {
    fn tone(&self, tone: Tone) -> Hsla {
        match tone {
            Tone::Healthy => self.success,
            Tone::Progressing => self.primary,
            Tone::Warning => self.warning,
            Tone::Critical => self.danger,
            Tone::Unknown => self.muted_foreground,
        }
    }

    fn tone_surface(&self, tone: Tone) -> Hsla {
        // A status pill sits inside a row that is already coloured, so the fill
        // has to stay far quieter than the text on top of it.
        let alpha = if self.is_dark() { 0.18 } else { 0.12 };
        self.tone(tone).opacity(alpha)
    }

    fn table_header(&self) -> Hsla {
        self.muted
    }

    fn table_stripe(&self) -> Hsla {
        self.muted.opacity(if self.is_dark() { 0.4 } else { 0.5 })
    }
}

/// Flips between light and dark, following whatever the window currently uses.
pub fn toggle_mode(window: &mut Window, cx: &mut App) {
    let next = if cx.theme().is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(next, Some(window), cx);
}
