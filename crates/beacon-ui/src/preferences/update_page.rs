//! Compact update status and preferences, using the application's theme.
use super::*;
use crate::scroll::OverflowScrollbars as _;
use crate::updates::{Status, Updater};
use beacon_updater::Channel;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::progress::Progress;

fn last_checked(updater: &Updater) -> String {
    let Some(checked) = updater.last_checked else {
        return crate::i18n::text("Not checked yet").to_string();
    };
    let seconds = checked.elapsed().map_or(0, |elapsed| elapsed.as_secs());
    let (count, unit) = if seconds < 60 {
        return crate::i18n::text("Last checked just now").to_string();
    } else if seconds < 3600 {
        (seconds / 60, "minute")
    } else if seconds < 86400 {
        (seconds / 3600, "hour")
    } else {
        (seconds / 86400, "day")
    };
    let unit = if count == 1 {
        unit.to_owned()
    } else {
        format!("{unit}s")
    };
    crate::i18n::message(
        "updates.last_checked_ago",
        &[
            ("count", count.to_string()),
            ("unit", crate::i18n::text(unit).to_string()),
        ],
    )
    .to_string()
}

fn update_actions(updater: &Updater) -> AnyElement {
    let check = || {
        Button::new("check-updates")
            .small()
            .outline()
            .icon(IconName::RefreshCw)
            .label(crate::i18n::text("Check for updates"))
            .on_click(|_, _, cx| crate::updates::store(cx).update(cx, |view, cx| view.check(cx)))
    };
    let action = match &updater.status {
        Status::Checking | Status::Downloading { .. } => Button::new("cancel-update")
            .small()
            .outline()
            .label(crate::i18n::text("Cancel"))
            .on_click(|_, _, cx| crate::updates::store(cx).update(cx, |view, cx| view.cancel(cx))),
        Status::Available(release)
        | Status::Failed {
            release: Some(release),
            ..
        } => {
            if updater.installation.can_install() {
                Button::new("download-update")
                    .small()
                    .primary()
                    .icon(IconName::Download)
                    .label(crate::i18n::text(
                        if matches!(updater.status, Status::Failed { .. }) {
                            "Retry download"
                        } else {
                            "Download update"
                        },
                    ))
                    .on_click(|_, _, cx| {
                        crate::updates::store(cx).update(cx, |view, cx| view.download(cx))
                    })
            } else {
                let url = release.asset.url.to_string();
                Button::new("get-update-package")
                    .small()
                    .primary()
                    .icon(IconName::Download)
                    .label(crate::i18n::text("Get update package"))
                    .on_click(move |_, _, cx| cx.open_url(&url))
            }
        }
        Status::Ready(_) => Button::new("install-update")
            .small()
            .primary()
            .icon(IconName::RefreshCw)
            .label(crate::i18n::text("Restart & install"))
            .on_click(|_, window, cx| {
                crate::updates::store(cx).update(cx, |view, cx| view.restart(window, cx))
            }),
        Status::Installing => Button::new("install-update")
            .small()
            .outline()
            .label(crate::i18n::text("Installing…"))
            .disabled(true),
        _ => check(),
    };
    h_flex()
        .gap_2()
        .flex_wrap()
        .child(action)
        .when(
            matches!(
                updater.status,
                Status::Available(_)
                    | Status::Failed {
                        release: Some(_),
                        ..
                    }
            ),
            |actions| {
                actions.child(
                    check()
                        .label("")
                        .accessibility_label(crate::i18n::text("Check for updates"))
                        .tooltip(crate::i18n::text("Check for updates")),
                )
            },
        )
        .into_any_element()
}

fn preference_row(
    label: &str,
    description: &str,
    control: impl IntoElement,
    last: bool,
    cx: &App,
) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .p_4()
        .when(!last, |row| {
            row.border_b_1().border_color(cx.theme().border)
        })
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child(crate::i18n::text(label)),
                )
                .child(hint(description, cx)),
        )
        .child(div().flex_shrink_0().child(control))
        .into_any_element()
}

impl PreferencesView {
    pub(super) fn updates_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(store) = crate::updates::maybe_store(cx) else {
            return div().into_any_element();
        };
        let updater = store.read(cx);
        let can_install = updater.installation.can_install();
        let installed = updater.receipt.as_deref() == Some("Update installed successfully.");
        let channel = if self.updates.channel == Channel::Stable {
            "Stable"
        } else {
            "Development"
        };
        let (title, description, icon, release) = match &updater.status {
            Status::Idle => (
                if installed {
                    "Update installed"
                } else {
                    "Check for a new version"
                }
                .to_owned(),
                if installed {
                    "Update installed successfully.".into()
                } else {
                    crate::i18n::message(
                        "updates.channel_latest",
                        &[("channel", crate::i18n::text(channel).to_string())],
                    )
                    .to_string()
                },
                if installed {
                    IconName::CircleCheck
                } else {
                    IconName::Info
                },
                None,
            ),
            Status::Checking => (
                "Checking for updates…".into(),
                "Looking for the latest release.".into(),
                IconName::RefreshCw,
                None,
            ),
            Status::Current => (
                "You're up to date".into(),
                if installed {
                    "Update installed successfully.".into()
                } else {
                    last_checked(updater)
                },
                IconName::CircleCheck,
                None,
            ),
            Status::Available(release) => (
                crate::i18n::message(
                    "updates.available",
                    &[("version", release.version.to_string())],
                )
                .to_string(),
                format!(
                    "{:.1} MB · {channel} channel",
                    release.asset.size as f64 / 1_000_000.
                ),
                IconName::Download,
                Some(release),
            ),
            Status::Downloading { release, .. } => (
                crate::i18n::message(
                    "updates.downloading",
                    &[("version", release.version.to_string())],
                )
                .to_string(),
                "You can keep using Beacon.".into(),
                IconName::Download,
                Some(release),
            ),
            Status::Ready(download) => (
                "Ready to install".into(),
                crate::i18n::message(
                    "updates.verified",
                    &[("version", download.release.version.to_string())],
                )
                .to_string(),
                IconName::CircleCheck,
                Some(&download.release),
            ),
            Status::Installing => (
                "Preparing update and restart…".into(),
                "Keep Beacon open while the installer is prepared.".into(),
                IconName::RefreshCw,
                None,
            ),
            Status::Failed { release, .. } => (
                if release.is_some() {
                    "Couldn't download the update"
                } else {
                    "Couldn't check for updates"
                }
                .into(),
                if release.is_some() {
                    "Try downloading the update again."
                } else {
                    "Check your network connection and try again."
                }
                .into(),
                IconName::CircleX,
                release.as_ref(),
            ),
        };
        let error = match &updater.status {
            Status::Failed { message, .. } => Some(message.as_str()),
            Status::Idle | Status::Current | Status::Ready(_) if !installed => {
                updater.receipt.as_deref()
            }
            _ => None,
        };
        let tone = if error.is_some() {
            cx.theme().danger
        } else if matches!(updater.status, Status::Current | Status::Ready(_))
            || (matches!(updater.status, Status::Idle) && installed)
        {
            cx.theme().success
        } else {
            cx.theme().foreground
        };
        let icon = if error.is_some() {
            IconName::CircleX
        } else {
            icon
        };
        let compact = self.updates_width.get() < px(540.);
        let state_copy = h_flex()
            .gap_3()
            .items_center()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .size(px(32.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(tone.opacity(0.08))
                    .text_color(tone)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(icon).size_4()),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().font_weight(FontWeight::MEDIUM).child(
                        crate::copyable_text::copyable_text(
                            "update-status",
                            crate::i18n::text(title),
                        ),
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(crate::copyable_text::copyable_text(
                                "update-status-description",
                                crate::i18n::text(description),
                            )),
                    ),
            )
            .into_any_element();
        let status = if compact {
            v_flex()
                .gap_3()
                .child(state_copy)
                .child(div().pl(px(44.)).child(update_actions(updater)))
                .into_any_element()
        } else {
            h_flex()
                .gap_4()
                .items_center()
                .child(state_copy)
                .child(div().flex_shrink_0().child(update_actions(updater)))
                .into_any_element()
        };
        let mut card = v_flex()
            .id("update-status-card")
            .test_support()
            .w_full()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .overflow_hidden()
            .child(
                h_flex()
                    .gap_2()
                    .p_3()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .child(Icon::new(IconName::RadioTower).size_4())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_weight(FontWeight::MEDIUM)
                            .child(format!("Beacon {}", env!("CARGO_PKG_VERSION"))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(crate::i18n::text("Installed version")),
                    ),
            )
            .child(
                div()
                    .id("update-status-row")
                    .test_support()
                    .p_4()
                    .child(status),
            );
        if let Status::Downloading {
            received, total, ..
        } = &updater.status
        {
            let percent = (*received as f64 / (*total).max(1) as f64 * 100.).clamp(0., 100.) as f32;
            card = card.child(
                v_flex()
                    .px_4()
                    .pb_4()
                    .pl(px(60.))
                    .gap_2()
                    .child(
                        h_flex()
                            .justify_between()
                            .gap_3()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{:.1} / {:.1} MB",
                                *received as f64 / 1_000_000.,
                                *total as f64 / 1_000_000.
                            ))
                            .child(format!("{percent:.0}%")),
                    )
                    .child(
                        Progress::new("update-download-progress")
                            .xsmall()
                            .w_full()
                            .value(percent)
                            .accessibility_label(crate::i18n::text("Downloading update")),
                    ),
            );
        }
        card =
            card.when_some(error.map(str::to_owned), |card, error| {
                card.child(
                    div()
                        .id("update-error-row")
                        .test_support()
                        .px_4()
                        .pb_4()
                        .pl(px(60.))
                        .child(
                            div()
                                .id("update-error-scroll")
                                .test_support()
                                .max_h(px(120.))
                                .overflow_y_scrollbar()
                                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .child(div().id("update-error-content").test_support().child(
                                    crate::copyable_text::copyable_text("update-error", error),
                                )),
                        ),
                )
            });
        if let Some(release) = release {
            let url = release.page.to_string();
            card = card.child(
                v_flex()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .px_3()
                            .py_1()
                            .justify_between()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new("update-notes-disclosure")
                                    .small()
                                    .ghost()
                                    .icon(if self.update_notes_expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .label(
                                        crate::i18n::message(
                                            "messages.what_s_new_in",
                                            &[("value0", (release.version).to_string())],
                                        )
                                        .to_string(),
                                    )
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.update_notes_expanded = !view.update_notes_expanded;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("update-release-page")
                                    .small()
                                    .ghost()
                                    .label(crate::i18n::text("View release"))
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            ),
                    )
                    .when(self.update_notes_expanded, |notes| {
                        notes.child(
                            div().px_4().pb_4().child(
                                div()
                                    .id("update-notes-scroll")
                                    .test_support()
                                    .max_h(px(220.))
                                    .overflow_y_scrollbar()
                                    .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                                    .child(div().id("update-notes-content").test_support().child(
                                        crate::copyable_text::copyable_text(
                                            "update-release-notes",
                                            if release.notes.trim().is_empty() {
                                                "No release notes provided.".into()
                                            } else {
                                                release.notes.chars().take(8192).collect::<String>()
                                            },
                                        ),
                                    )),
                            ),
                        )
                    }),
            );
        }
        if updater.last_checked.is_some()
            && !(matches!(updater.status, Status::Current) && !installed)
        {
            card = card.child(div().px_4().pb_3().child(hint(&last_checked(updater), cx)));
        }
        let channel_control = h_flex()
            .gap_1()
            .p_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().muted)
            .children(
                [
                    (Channel::Stable, "Stable"),
                    (Channel::Development, "Development"),
                ]
                .into_iter()
                .map(|(channel, label)| {
                    let selected = self.updates.channel == channel;
                    NavigationButton::new(label)
                        .selected(selected)
                        .disabled(matches!(updater.status, Status::Installing))
                        .h(px(28.))
                        .px_2()
                        .text_xs()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().transparent)
                        .bg(Background::from(if selected {
                            cx.theme().background
                        } else {
                            cx.theme().transparent
                        }))
                        .text_color(if selected {
                            cx.theme().foreground
                        } else {
                            cx.theme().muted_foreground
                        })
                        .focus_visible(|style| style.border_color(cx.theme().ring))
                        .child(crate::i18n::text(label))
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.updates.channel = channel;
                            view.update_notes_expanded = false;
                            view.save_updates(window, cx);
                        }))
                }),
            );
        let width = self.updates_width.clone();
        let entity = cx.entity().entity_id();
        let (proxy_label, proxy_hint) = match settings::store(cx).read(cx).preferences.proxy {
            Proxy::System => (
                "Checks and downloads use the system proxy.",
                "Change the proxy in Network settings.",
            ),
            Proxy::Direct => (
                "Checks and downloads use a direct connection.",
                "Change the proxy in Network settings.",
            ),
            Proxy::Custom(_) => (
                "Checks and downloads use the global proxy.",
                "Change the proxy in Network settings.",
            ),
        };
        v_flex().id("update-page").test_support().relative().w_full().min_w_0().text_sm().gap_5()
            .on_prepaint(move |bounds, _, cx| { if width.replace(bounds.size.width) != bounds.size.width { cx.notify(entity); } })
            .child(card)
            .child(v_flex().gap_3()
                .child(heading("Update preferences"))
                .child(v_flex().w_full().border_1().border_color(cx.theme().border).rounded_lg().overflow_hidden()
                    .child(preference_row("Release channel", if self.updates.channel == Channel::Stable { "Recommended for everyday use." } else { "Includes development releases; never downgrades." }, channel_control, false, cx))
                    .child(preference_row("Check automatically", "At startup, then every hour.",
                        Switch::new("auto-check-updates").accessibility_label(crate::i18n::text("Automatically check for updates")).checked(self.updates.auto_check)
                            .on_click(cx.listener(|view, checked, window, cx| { view.updates.auto_check = *checked; view.save_updates(window, cx); })), false, cx))
                    .child(preference_row("Download automatically", "Ask before restarting to install.",
                        Switch::new("auto-download-updates").accessibility_label(crate::i18n::text("Automatically download updates")).checked(self.updates.auto_download).disabled(!can_install)
                            .on_click(cx.listener(|view, checked, window, cx| { view.updates.auto_download = *checked; view.save_updates(window, cx); })), true, cx)))
                .child(h_flex().items_start().gap_2().text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::Network).size_4().flex_shrink_0())
                    .child(v_flex().min_w_0().gap_1().child(hint(proxy_label, cx)).child(hint(proxy_hint, cx))))
                .when(!can_install, |form| form.child(hint("This installation is managed externally. Install the downloaded package with your package manager. Automatic installation is available for macOS/Windows apps and Linux AppImage.", cx))))
            .into_any_element()
    }
}
