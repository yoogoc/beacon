//! Theme-aware release identity, revision history and read-only document review.
use super::*;
fn muted(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
        .into_any_element()
}
fn field(id: &'static str, label: &str, value: String, cx: &App) -> AnyElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(muted(crate::i18n::text(label), cx))
        .child(div().text_sm().child(copyable_text(
            id,
            if value.is_empty() {
                "—".into()
            } else {
                value
            },
        )))
        .into_any_element()
}
impl HelmDetails {
    fn header(&self, cx: &Context<Self>) -> AnyElement {
        let selected = self.selected();
        let status = selected.map_or("Unknown", |revision| revision.summary.status.as_str());
        let color = match status {
            "deployed" => cx.theme().success,
            "failed" | "uninstalled" => cx.theme().danger,
            status if status.starts_with("pending") => cx.theme().warning,
            _ => cx.theme().muted_foreground,
        };
        v_flex()
            .gap_3()
            .flex_shrink_0()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(copyable_text("helm-name", self.identity.name.clone())),
                            )
                            .child(muted(
                                format!(
                                    "{} · {}",
                                    self.identity.namespace,
                                    self.session.id().display_name()
                                ),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(color.opacity(0.1))
                            .text_color(color)
                            .text_sm()
                            .child(crate::i18n::text(status)),
                    )
                    .child(
                        Button::new("helm-refresh")
                            .small()
                            .outline()
                            .icon(IconName::RefreshCw)
                            .label(crate::i18n::text("Refresh"))
                            .disabled(self.loading)
                            .on_click(cx.listener(|view, _, window, cx| view.load(window, cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("helm-upgrade")
                            .small()
                            .outline()
                            .label(crate::i18n::text("Upgrade"))
                            .disabled(
                                self.loading
                                    || self
                                        .history
                                        .as_ref()
                                        .and_then(|history| history.revisions.first())
                                        .is_none_or(|record| record.revision.is_none()),
                            )
                            .on_click(cx.listener(|view, _, window, cx| {
                                if let Some(revision) = view
                                    .history
                                    .as_ref()
                                    .and_then(|history| history.revisions.first())
                                    .and_then(|record| record.revision.as_ref())
                                {
                                    view.manage(
                                        beacon_kube::helm::lifecycle::Change::Upgrade(
                                            beacon_kube::helm::lifecycle::Chart {
                                                reference: revision.chart_name.clone(),
                                                version: revision.chart_version.clone(),
                                                repository: String::new(),
                                            },
                                        ),
                                        window,
                                        cx,
                                    );
                                }
                            })),
                    )
                    .child(
                        Button::new("helm-rollback")
                            .small()
                            .outline()
                            .label(crate::i18n::text("Rollback to this revision"))
                            .disabled(
                                self.loading
                                    || self.selected().is_none()
                                    || self
                                        .history
                                        .as_ref()
                                        .and_then(|history| history.revisions.first())
                                        .is_some_and(|record| {
                                            Some(record.number)
                                                == self
                                                    .selected()
                                                    .map(|revision| revision.summary.revision)
                                        }),
                            )
                            .on_click(cx.listener(|view, _, window, cx| {
                                if let Some(revision) = view.selected() {
                                    view.manage(
                                        beacon_kube::helm::lifecycle::Change::Rollback(
                                            revision.summary.revision,
                                        ),
                                        window,
                                        cx,
                                    );
                                }
                            })),
                    )
                    .child(
                        Button::new("helm-uninstall")
                            .small()
                            .outline()
                            .label(crate::i18n::text("Uninstall"))
                            .disabled(
                                self.loading
                                    || self
                                        .history
                                        .as_ref()
                                        .is_none_or(|history| history.revisions.is_empty()),
                            )
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.manage(
                                    beacon_kube::helm::lifecycle::Change::Uninstall,
                                    window,
                                    cx,
                                )
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(field(
                        "helm-chart",
                        "Chart",
                        selected
                            .map(|revision| revision.summary.chart.clone())
                            .unwrap_or_default(),
                        cx,
                    ))
                    .child(field(
                        "helm-app-version",
                        "App version",
                        selected
                            .map(|revision| revision.summary.app_version.clone())
                            .unwrap_or_default(),
                        cx,
                    ))
                    .child(field(
                        "helm-updated",
                        "Deployed",
                        selected
                            .and_then(|revision| revision.summary.updated.clone())
                            .unwrap_or_default(),
                        cx,
                    )),
            )
            .into_any_element()
    }
    fn tabs(&self, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .pb_2()
            .children(
                [Page::Values, Page::Manifest, Page::History, Page::Compare]
                    .into_iter()
                    .map(|page| {
                        Button::new(format!("helm-tab-{}", page.label()))
                            .small()
                            .ghost()
                            .selected(self.page == page)
                            .label(crate::i18n::text(page.label()))
                            .on_click(
                                cx.listener(move |view, _, window, cx| {
                                    view.select(page, window, cx)
                                }),
                            )
                    }),
            )
            .into_any_element()
    }
    fn controls(&self, cx: &Context<Self>) -> AnyElement {
        let compare = self.page == Page::Compare;
        h_flex().items_center().gap_2().flex_shrink_0().flex_wrap()
            .when(compare, |row| row
                .child(muted("From", cx))
                .child(Select::new(&self.from_picker).small().w(px(250.)).search_placeholder(crate::i18n::text("Search revisions")).accessibility_label(crate::i18n::text("Original Helm revision")))
                .child(muted("To", cx))
                .child(Select::new(&self.to_picker).small().w(px(250.)).search_placeholder(crate::i18n::text("Search revisions")).accessibility_label(crate::i18n::text("Modified Helm revision"))))
            .when(!compare, |row| row.child(Select::new(&self.revision_picker).small().w(px(340.)).search_placeholder(crate::i18n::text("Search revisions")).accessibility_label(crate::i18n::text("Helm revision"))))
            .child(div().flex_1())
            .child(Button::new("helm-reveal").small().outline().label(crate::i18n::text(if self.reveal { "Hide values" } else { "Reveal values" }))
                .tooltip(crate::i18n::text("Helm values can contain credentials. Secret data in manifests is also concealed until revealed."))
                .on_click(cx.listener(|view, _, window, cx| { view.reveal = !view.reveal; view.prepare(window, cx); })))
            .child(Button::new("helm-wrap").small().ghost().selected(self.wrap).label(crate::i18n::text("Wrap"))
                .on_click(cx.listener(|view, _, window, cx| { view.wrap = !view.wrap; view.editor.update(cx, |state, cx| state.set_soft_wrap(view.wrap, window, cx)); cx.notify(); })))
            .into_any_element()
    }
    fn compare_controls(&self, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .gap_2()
            .items_center()
            .flex_shrink_0()
            .children(
                [
                    (Document::Values, "Values"),
                    (Document::Manifest, "Manifest"),
                ]
                .into_iter()
                .map(|(document, label)| {
                    Button::new(format!("helm-compare-{label}"))
                        .small()
                        .outline()
                        .selected(self.document == document)
                        .label(crate::i18n::text(label))
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.document = document;
                            view.prepare(window, cx);
                        }))
                }),
            )
            .child(div().flex_1())
            .children(
                [
                    (DiffMode::Split, "Side by side"),
                    (DiffMode::Unified, "Unified"),
                ]
                .into_iter()
                .map(|(mode, label)| {
                    Button::new(format!("helm-mode-{label}"))
                        .small()
                        .ghost()
                        .selected(self.mode == mode)
                        .label(crate::i18n::text(label))
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.mode = mode;
                            if let Some(diff) = &view.diff {
                                diff.update(cx, |state, cx| state.set_mode(mode, cx));
                            }
                            cx.notify();
                        }))
                }),
            )
            .into_any_element()
    }
    fn history_view(&self, cx: &Context<Self>) -> AnyElement {
        let Some(history) = &self.history else {
            return div().into_any_element();
        };
        if history.revisions.is_empty() {
            return muted(
                "No stored revisions were found. This release may have been removed.",
                cx,
            );
        }
        v_flex().flex_1().min_size_0().gap_2()
            .child(muted("Select a revision to inspect its values. Helm may prune older revisions; gaps are shown as stored.", cx))
            .child(div().id("helm-history").test_support().flex_1().overflow_y_scroll()
                .child(v_flex().w_full()
                    .child(h_flex().px_3().py_2().gap_3().bg(cx.theme().muted).text_xs()
                        .child(div().w(px(74.)).child(crate::i18n::text("Revision")))
                        .child(div().w(px(140.)).child(crate::i18n::text("Status")))
                        .child(div().flex_1().child(crate::i18n::text("Chart version")))
                        .child(div().w(px(110.)).child(crate::i18n::text("App version")))
                        .child(div().w(px(220.)).child(crate::i18n::text("Deployed"))))
                    .children(history.revisions.iter().map(|record| {
                        let number = record.number;
                        let revision = record.revision.as_ref();
                        v_flex().id(format!("helm-revision-{number}")).test_support().px_3().py_3().gap_2()
                            .border_b_1().border_color(cx.theme().border)
                            .when(revision.is_some(), |row| row.cursor_pointer().hover(|row| row.bg(cx.theme().muted.opacity(0.5)))
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.revision = number;
                                    view.revision_picker.update(cx, |picker, cx| picker.set_selected_value(&number, window, cx));
                                    view.select(Page::Values, window, cx);
                                })))
                            .child(h_flex().gap_3().text_sm()
                                .child(div().w(px(74.)).child(format!("#{number}")))
                                .child(div().w(px(140.)).child(revision.map_or("Unreadable".into(), |revision| revision.summary.status.clone())))
                                .child(div().flex_1().min_w_0().child(revision.map_or("—".into(), |revision| revision.summary.chart.clone())))
                                .child(div().w(px(110.)).child(revision.map_or("—".into(), |revision| revision.summary.app_version.clone())))
                                .child(div().w(px(220.)).child(revision.and_then(|revision| revision.summary.updated.clone()).unwrap_or_else(|| "—".into()))))
                            .when_some(record.error.clone(), |row, error| row.child(div().id(format!("helm-history-error-{number}")).test_support().text_sm().text_color(cx.theme().danger)
                                .child(copyable_text(format!("helm-history-error-text-{number}"), format!("{} · {error}", record.secret_name)))))
                    }))))
            .into_any_element()
    }
}
impl Render for HelmDetails {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = v_flex().size_full().min_size_0().gap_3()
            .child(self.header(cx)).child(self.tabs(cx))
            .when(self.history.is_some() && self.page != Page::History, |view| view.child(self.controls(cx)))
            .when(self.history.is_some() && self.page == Page::Compare, |view| view.child(self.compare_controls(cx)))
            .when_some(self.error.clone().or(self.content_error.clone()), |view, error| view.child(div().id("helm-error").test_support().text_sm().text_color(cx.theme().danger).child(copyable_text("helm-error-text", error))))
            .when(self.loading || self.preparing, |view| view.child(muted(if self.loading { "Reading release history…" } else { "Preparing revision…" }, cx)))
            .when(!self.loading && !self.preparing && self.history.is_some(), |view| {
                if self.page == Page::History { return view.child(self.history_view(cx)); }
                let hint = if self.reveal {
                    "Stored release content · read only. Values and Secret data are visible."
                } else {
                    "Values and Secret data are concealed. Reveal values to read their contents; concealed differences remain detectable."
                };
                view.child(muted(crate::i18n::text(hint), cx))
                    .when(self.content_error.is_none() && self.page != Page::Compare, |view| view.child(div().id("helm-document").test_support().flex_1().min_size_0().overflow_hidden().rounded_md().border_1().border_color(cx.theme().border)
                        .child(Editor::new(&self.editor).h_full().readonly(true).aria_label(crate::i18n::text(if self.page == Page::Values { "Helm values" } else { "Helm rendered manifest" })))))
                    .when_some(self.diff.clone(), |view, diff| view.child(div().id("helm-diff").test_support().flex_1().min_size_0().overflow_hidden().border_1().border_color(cx.theme().border).rounded_md()
                        .child(Diff::new(&diff).header_visible(false).soft_wrap(self.wrap))))
            });
        v_flex()
            .id("helm-details-window")
            .test_support()
            .size_full()
            .track_focus(&self.focus)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(|_: &crate::app::CloseTab, window, cx| {
                window.defer(cx, |window, _| window.remove_window())
            })
            .child(
                TitleBar::new().child(
                    div()
                        .flex_1()
                        .truncate()
                        .child(format!("Helm · {}", self.identity.name)),
                ),
            )
            .child(div().flex_1().min_size_0().p_4().child(content))
    }
}
