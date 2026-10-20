//! Beacon's semantic colours.
//!
//! gpui-component supplies the base palette (background, border, muted,
//! primary...). This module adds the vocabulary Beacon actually reasons in --
//! the health of a workload and the health of a connection -- so that no view
//! picks a literal colour. Changing how "degraded" looks is a change here and
//! nowhere else.

use gpui_kit::component::Theme;
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
    /// Foreground for links that navigate to another resource.
    fn resource_link(&self) -> Hsla;
    /// Browser links use a separate hue from internal resource navigation.
    fn external_link(&self) -> Hsla;
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
    fn resource_link(&self) -> Hsla {
        self.link
    }

    fn external_link(&self) -> Hsla {
        // A restrained blue keeps browser links distinct from the resource
        // accent without inheriting its bright saturation or shifting purple.
        if self.is_dark() {
            rgb(0x8cabd1).into()
        } else {
            rgb(0x426b9a).into()
        }
    }

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

/// Cluster identity colours are independent of health/status colours. Golden
/// angle spacing spreads successive clusters across the hue wheel without a
/// short repeating palette; equal luminance keeps every hue readable.
pub(crate) fn cluster_color(index: usize, dark: bool) -> Hsla {
    let hue = ((7. / 12. + index as f64 * 0.381_966_011_250_105_1).fract()) as f32;
    let target = if dark { 0.45 } else { 0.17 };
    let (mut low, mut high) = (0., 1.);
    for _ in 0..14 {
        let lightness = (low + high) / 2.;
        let color = hsla(hue, 0.66, lightness, 1.).to_rgb();
        let linear = |channel: f32| {
            if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };
        let luminance =
            0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b);
        if luminance < target {
            low = lightness;
        } else {
            high = lightness;
        }
    }
    hsla(hue, 0.66, (low + high) / 2., 1.)
}

/// Flips between light and dark, following whatever the window currently uses.
pub fn toggle_mode(window: &mut Window, cx: &mut App) {
    crate::settings::toggle(window, cx);
}
