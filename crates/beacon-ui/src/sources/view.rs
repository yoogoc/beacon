use super::*;
use crate::{copyable_text::copyable_text, settings};
use beacon_kube::sources::{SourceKind, SourceStatus};
use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;

pub(crate) struct SourcesView {
    selected: String,
    search: Entity<InputState>,
    message: Option<(bool, String)>,
    _subscriptions: Vec<Subscription>,
}
pub(crate) fn add(window: &mut Window, cx: &mut App) {
    editor::open(None, false, window, cx);
}
impl SourcesView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| crate::i18n::input("Search sources or contexts", window, cx));
        let source_store = store(cx);
        let subscriptions = vec![
            cx.observe(&source_store, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        Self {
            selected: "system".into(),
            search,
            message: None,
            _subscriptions: subscriptions,
        }
    }
    fn toggle(&mut self, id: String, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut preferences = settings::store(cx).read(cx).preferences.clone();
        if id == "system" {
            preferences.kubeconfigs.system_enabled = enabled;
        } else if let Some(source) = preferences
            .kubeconfigs
            .sources
            .iter_mut()
            .find(|s| s.id == id)
        {
            source.enabled = enabled;
        }
        self.message = settings::save(preferences, window, cx)
            .err()
            .map(|error| (true, error));
        cx.notify();
    }
    fn remove(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let preferences = settings::store(cx).read(cx).preferences.clone();
        let Some(source) = preferences
            .kubeconfigs
            .sources
            .iter()
            .find(|s| s.id == id)
            .cloned()
        else {
            return;
        };
        let directory = settings::store(cx).read(cx).directory.clone();
        let imported = matches!(source.kind, SourceKind::Imported);
        let name = source.name.clone();
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let view = weak.clone();
            let id = id.clone();
            let directory = directory.clone();
            dialog.title(crate::i18n::message("messages.remove_name", &[("name", (name).to_string())]).to_string())
                .button_props(gpui_kit::component::dialog::DialogButtonProps::default().cancel_text("Cancel").show_cancel(true).ok_text("Remove source"))
                .child(div().text_sm().child(crate::i18n::text(if imported { "Remove this source and Beacon’s saved kubeconfig copy. Existing connections stay open until you disconnect." } else { "Remove this source from Beacon. Original files and existing connections stay in place." })))
                .on_ok(move |_, window, cx| {
                    let mut preferences = settings::store(cx).read(cx).preferences.clone();
                    preferences.kubeconfigs.sources.retain(|s| s.id != id);
                    match settings::save(preferences, window, cx) {
                        Ok(()) => {
                            if imported {
                                let path = sources::imported_path(&directory, &id);
                                let view = view.clone();
                                cx.spawn(async move |cx| {
                                    let result = cx.background_executor().spawn(async move {
                                        match std::fs::remove_file(path) { Ok(()) => Ok(()), Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()), Err(error) => Err(error.to_string()) }
                                    }).await;
                                    if let Err(error) = result { let _ = view.update(cx, |view, cx| { view.message = Some((true, crate::i18n::message("messages.source_removed_but_its_saved_copy_could_not_be_deleted_error", &[("error", (error).to_string())]).to_string())); cx.notify(); }); }
                                }).detach();
                            }
                            true
                        }
                        Err(error) => { let _ = view.update(cx, |view, cx| { view.message = Some((true, error)); cx.notify(); }); false }
                    }
                })
        });
    }
    fn detail(&self, status: &SourceStatus, cx: &mut Context<Self>) -> AnyElement {
        let id = status.id.clone();
        let source = settings::store(cx)
            .read(cx)
            .preferences
            .kubeconfigs
            .sources
            .iter()
            .find(|s| s.id == id)
            .cloned();
        let label = match source.as_ref().map(|s| &s.kind) {
            None => "KUBECONFIG / default path",
            Some(SourceKind::File { .. }) => "Linked file",
            Some(SourceKind::Directory { .. }) => "Watched folder",
            Some(SourceKind::Imported) => "Beacon-managed copy",
        };
        let reload = status.id.clone();
        let toggle_id = status.id.clone();
        v_flex().flex_1().min_w_0().gap_3()
            .child(h_flex().gap_3().justify_between().items_start()
                .child(v_flex().flex_1().min_w_0().gap_1().child(div().font_weight(FontWeight::MEDIUM).child(status.name.clone())).child(hint(label, cx)))
                .child(Switch::new("source-enabled").label(crate::i18n::text("Enabled")).checked(status.enabled)
                    .accessibility_label(crate::i18n::message("messages.enable", &[("value0", (status.name).to_string())]).to_string()).on_click(cx.listener(move |view, enabled, window, cx| view.toggle(toggle_id.clone(), *enabled, window, cx)))))
            .child(h_flex().gap_3().items_start().child(div().w(px(76.)).text_sm().text_color(cx.theme().muted_foreground).child(crate::i18n::text("Location")))
                .child(div().flex_1().min_w_0().text_sm().child(copyable_text("source-location", if status.location.is_empty() { "No system kubeconfig path found".into() } else { status.location.clone() }))))
            .when_some(source.clone(), |column, source| {
                column.when(matches!(source.kind, SourceKind::Directory { .. }), |column| {
                    let SourceKind::Directory { recursive, patterns, .. } = &source.kind else { unreachable!() };
                    column.child(hint(crate::i18n::message("messages.files", &[("value0", (status.files).to_string()), ("value1", (if *recursive { "Including subfolders" } else { "This folder only" }).to_string()), ("value2", (patterns).to_string())]).as_ref(), cx))
                })
            })
            .child(h_flex().gap_2().flex_wrap()
                .child(Button::new("source-reload").small().ghost().icon(IconName::RefreshCw).label(crate::i18n::text("Reload")).on_click(move |_, _, cx| { let _ = &reload; store(cx).update(cx, |state, cx| state.reload(true, cx)); }))
                .when_some(source, |row, source| {
                    let rename = source.clone(); let edit = source.clone(); let id = source.id;
                    row.child(Button::new("source-rename").small().ghost().icon(IconName::Pencil).label(crate::i18n::text("Rename")).on_click(move |_, window, cx| editor::open(Some(rename.clone()), true, window, cx)))
                        .child(Button::new("source-edit").small().ghost().icon(IconName::FolderOpen).label(crate::i18n::text(if matches!(edit.kind, SourceKind::Imported) { "Replace YAML" } else { "Change location" })).on_click(move |_, window, cx| editor::open(Some(edit.clone()), false, window, cx)))
                        .child(Button::new("source-remove").small().ghost().icon(IconName::Unlink).label(crate::i18n::text("Remove")).on_click(cx.listener(move |view, _, window, cx| view.remove(id.clone(), window, cx))))
                }))
            .children(status.issues.iter().enumerate().map(|(index, issue)| {
                v_flex().gap_1().p_3().rounded(cx.theme().radius).bg(cx.theme().warning.opacity(0.08)).text_sm()
                    .when(!issue.file.as_os_str().is_empty(), |v| v.child(div().text_color(cx.theme().muted_foreground).child(copyable_text(("source-issue-path", index), issue.file.display().to_string()))))
                    .child(div().text_color(cx.theme().warning).child(copyable_text(("source-issue", index), issue.message.clone())))
            }))
            .child(h_flex().gap_2().py_2().border_b_1().border_color(cx.theme().border).text_sm().font_weight(FontWeight::MEDIUM).child(crate::i18n::text("Contexts")).child(format!("{}", status.contexts.len())))
            .children(status.contexts.iter().enumerate().map(|(index, context)| {
                v_flex().gap_1().py_2().border_b_1().border_color(cx.theme().border)
                    .child(h_flex().gap_2().child(div().flex_1().min_w_0().text_sm().child(copyable_text(("source-context", index), context.entry.id.context_name().to_string())))
                        .child(div().text_xs().text_color(if context.included { cx.theme().success } else { cx.theme().muted_foreground }).child(crate::i18n::text(if context.included { "Available" } else { "Excluded" }))))
                    .child(hint(&format!("{} · {}{}", context.entry.initial_namespace(), crate::i18n::text(&context.authentication),
                        context.file.strip_prefix(std::path::Path::new(&status.location)).ok().filter(|path| !path.as_os_str().is_empty()).map_or(String::new(), |path| format!(" · {}", path.display()))), cx))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(copyable_text(("source-context-server", index), context.server.clone())))
            }))
            .when(status.contexts.is_empty() && status.issues.is_empty(), |v| v.child(hint(if status.enabled { "No matching kubeconfigs found." } else { "Enable this source to load its contexts." }, cx)))
            .child(hint("Each source keeps its own context identities. Importing and reloading never connects a cluster or changes an active session.", cx))
            .into_any_element()
    }
}
fn hint(text: &str, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(crate::i18n::text(text))
        .into_any_element()
}
impl Render for SourcesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = store(cx);
        let catalog = state.read(cx).catalog.clone();
        let loading = state.read(cx).loading;
        let query = self.search.read(cx).value().to_lowercase();
        let statuses = catalog
            .statuses
            .iter()
            .filter(|status| {
                format!(
                    "{} {} {}",
                    status.name,
                    status.location,
                    status
                        .contexts
                        .iter()
                        .map(|c| c.entry.id.context_name())
                        .collect::<Vec<_>>()
                        .join(" ")
                )
                .to_lowercase()
                .contains(&query)
            })
            .collect::<Vec<_>>();
        let selected = catalog
            .statuses
            .iter()
            .find(|s| s.id == self.selected)
            .or_else(|| catalog.statuses.first());
        let narrow = window.bounds().size.width < px(860.);
        let list = v_flex()
            .gap_1()
            .min_w_0()
            .when(!narrow, |v| v.w(px(196.)).flex_shrink_0())
            .children(statuses.iter().map(|status| {
                let id = status.id.clone();
                gpui_kit::base::Button::new(SharedString::from(format!("source-row-{id}")))
                    .w_full()
                    .justify_start()
                    .selected(selected.is_some_and(|selected| selected.id == id))
                    .bg(if selected.is_some_and(|selected| selected.id == id) {
                        Background::from(cx.theme().secondary)
                    } else {
                        Background::from(cx.theme().transparent)
                    })
                    .rounded(cx.theme().radius)
                    .p_3()
                    .accessibility_label(
                        crate::i18n::message(
                            "messages.source",
                            &[("value0", (status.name).to_string())],
                        )
                        .to_string(),
                    )
                    .child(
                        v_flex()
                            .w_full()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(status.name.clone()),
                            )
                            .child(hint(
                                &format!(
                                    "{} contexts · {}",
                                    status.contexts.iter().filter(|c| c.included).count(),
                                    if !status.enabled {
                                        "Disabled"
                                    } else if status.issues.is_empty() {
                                        "Loaded"
                                    } else {
                                        "Needs attention"
                                    }
                                ),
                                cx,
                            )),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.selected = id.clone();
                        view.message = None;
                        cx.notify();
                    }))
            }));
        let detail = selected.map(|status| self.detail(status, cx));
        v_flex()
            .id("kubeconfig-sources")
            .test_support()
            .w_full()
            .gap_4()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                crate::i18n::message(
                                    "messages.sources_available_contexts",
                                    &[
                                        ("value0", (catalog.statuses.len()).to_string()),
                                        ("value1", (catalog.contexts.entries().len()).to_string()),
                                    ],
                                )
                                .to_string(),
                            ),
                    )
                    .child(
                        Button::new("reload-kubeconfigs")
                            .small()
                            .ghost()
                            .icon(IconName::RefreshCw)
                            .label(crate::i18n::text(if loading {
                                "Loading…"
                            } else {
                                "Reload all"
                            }))
                            .disabled(loading)
                            .on_click(|_, _, cx| {
                                store(cx).update(cx, |state, cx| state.reload(true, cx))
                            }),
                    )
                    .child(
                        Button::new("add-kubeconfig-source")
                            .small()
                            .primary()
                            .icon(IconName::Plus)
                            .label(crate::i18n::text("Add source"))
                            .on_click(|_, window, cx| add(window, cx)),
                    ),
            )
            .child(Input::new(&self.search).small())
            .when_some(self.message.clone(), |v, (error, message)| {
                v.child(
                    div()
                        .text_sm()
                        .text_color(if error {
                            cx.theme().danger
                        } else {
                            cx.theme().success
                        })
                        .child(copyable_text("source-message", message)),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_4()
                    .items_start()
                    .when(narrow, |v| v.flex_col())
                    .child(list)
                    .when_some(detail, |v, detail| v.child(detail)),
            )
    }
}
