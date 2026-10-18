//! The source, target and results remain visible while browsing differences.
use super::*;

impl CompareView {
    fn source_card(&self, cx: &Context<Self>) -> AnyElement {
        let source = ObjectRef::of(&self.object);
        v_flex()
            .flex_1()
            .min_w_0()
            .p_3()
            .gap_2()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .child(label("1 · Source resource", cx))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(crate::icons::kubernetes().size_4())
                    .child(div().flex_1().min_w_0().text_sm().child(copyable_text(
                        "comparison-source-cluster",
                        cluster_name(&self.left, cx),
                    ))),
            )
            .child(copyable_text(
                "comparison-source",
                format!(
                    "{} · {}{}",
                    self.kind.resource.kind,
                    source
                        .namespace
                        .as_ref()
                        .map_or(String::new(), |namespace| format!("{namespace}/")),
                    source.name
                ),
            ))
            .child(label(
                "The resource you opened. Comparison is read only.",
                cx,
            ))
            .into_any_element()
    }
    fn comparison_options(&self, busy: bool, cx: &Context<Self>) -> AnyElement {
        let status = checkbox("compare-status")
            .label(crate::i18n::text("Include status"))
            .checked(self.options.status)
            .on_click(cx.listener(|view, value, _, cx| {
                view.options.status = *value;
                view.rebuild(cx);
            }));
        let metadata = checkbox("compare-metadata")
            .label(crate::i18n::text("Include server metadata"))
            .checked(self.options.metadata)
            .on_click(cx.listener(|view, value, _, cx| {
                view.options.metadata = *value;
                view.rebuild(cx);
            }));
        h_flex().gap_3().flex_wrap().items_center()
            .child(status).child(metadata)
            .when(self.kind.resource.group.is_empty() && self.kind.resource.kind == "Secret", |row| {
                row.child(checkbox("compare-reveal")
                    .label(crate::i18n::text("Show Secret values")).checked(self.options.reveal)
                    .tooltip(crate::i18n::text("Secret values are hidden by default. Revealed data is shown as stored, including base64 encoding."))
                    .on_click(cx.listener(|view, value, _, cx| {
                        view.options.reveal = *value;
                        view.rebuild(cx);
                    })))
            })
            .child(div().flex_1())
            .when(busy, |row| row.child(Button::new("compare-cancel")
                .small().outline().label(crate::i18n::text("Cancel"))
                .on_click(cx.listener(|view, _, _, cx| view.cancel(cx)))))
            .child(Button::new("compare-resources")
                .small().primary().icon(IconName::RefreshCw)
                .label(crate::i18n::text(if self.snapshots.is_some() { "Refresh" } else { "Compare" }))
                .disabled(busy || self.name.read(cx).value().trim().is_empty())
                .on_click(cx.listener(|view, _, window, cx| view.load(window, cx))))
            .into_any_element()
    }
    fn result_header(&self, cx: &Context<Self>) -> AnyElement {
        h_flex()
            .gap_2()
            .flex_wrap()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .pb_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!(
                        "3 · {}",
                        if self.changes.is_empty() {
                            "No differences".into()
                        } else {
                            crate::i18n::message(
                                "messages.changed_fields",
                                &[("value0", (self.changes.len()).to_string())],
                            )
                            .to_string()
                        }
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().success)
                    .child(format!("+{}", self.added)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().danger)
                    .child(format!("−{} lines", self.removed)),
            )
            .child(div().flex_1())
            .child(
                Button::new("compare-changes")
                    .small()
                    .ghost()
                    .selected(self.page == Page::Changes)
                    .label(crate::i18n::text("Changes"))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.page = Page::Changes;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("compare-yaml")
                    .small()
                    .ghost()
                    .selected(self.page == Page::Yaml)
                    .label(crate::i18n::text("YAML diff"))
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.page = Page::Yaml;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }
    fn yaml_view(&self, cx: &Context<Self>) -> AnyElement {
        let diff = self.diff.as_ref().unwrap().clone();
        let modes = [
            ("compare-split", "Side by side", DiffMode::Split),
            ("compare-unified", "Unified", DiffMode::Unified),
        ]
        .into_iter()
        .map(|(id, title, mode)| {
            Button::new(id)
                .small()
                .outline()
                .selected(self.mode == mode)
                .label(title)
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.mode = mode;
                    if let Some(diff) = &view.diff {
                        diff.update(cx, |state, cx| state.set_mode(mode, cx));
                    }
                    cx.notify();
                }))
        });
        let controls = h_flex()
            .gap_2()
            .items_center()
            .flex_wrap()
            .children(modes)
            .child(
                checkbox("compare-collapse")
                    .label(crate::i18n::text("Collapse unchanged"))
                    .checked(self.collapse)
                    .on_click(cx.listener(|view, value, _, cx| {
                        view.collapse = *value;
                        if let Some(diff) = &view.diff {
                            diff.update(cx, |state, cx| {
                                state.set_context_lines(if *value { Some(3) } else { None }, cx)
                            });
                        }
                        cx.notify();
                    })),
            )
            .child(
                checkbox("compare-wrap")
                    .label(crate::i18n::text("Wrap lines"))
                    .checked(self.wrap)
                    .on_click(cx.listener(|view, value, _, cx| {
                        view.wrap = *value;
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("compare-previous")
                    .small()
                    .ghost()
                    .icon(IconName::ChevronUp)
                    .accessibility_label(crate::i18n::text("Previous change"))
                    .tooltip(crate::i18n::text("Previous change"))
                    .on_click({
                        let diff = diff.clone();
                        move |_, _, cx| diff.update(cx, |state, cx| state.previous_change(cx))
                    }),
            )
            .child(
                Button::new("compare-next")
                    .small()
                    .ghost()
                    .icon(IconName::ChevronDown)
                    .accessibility_label(crate::i18n::text("Next change"))
                    .tooltip(crate::i18n::text("Next change"))
                    .on_click({
                        let diff = diff.clone();
                        move |_, _, cx| diff.update(cx, |state, cx| state.next_change(cx))
                    }),
            );
        v_flex()
            .flex_1()
            .min_size_0()
            .gap_2()
            .child(controls)
            .child(
                h_flex()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(div().flex_1().child(if self.mode == DiffMode::Split {
                        "Source"
                    } else {
                        "Source → Target"
                    }))
                    .when(self.mode == DiffMode::Split, |row| {
                        row.child(div().flex_1().child(crate::i18n::text("Target")))
                    }),
            )
            .child(
                div()
                    .id("comparison-yaml")
                    .test_support()
                    .flex_1()
                    .min_size_0()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_md()
                    .overflow_hidden()
                    .child(Diff::new(&diff).header_visible(false).soft_wrap(self.wrap)),
            )
            .into_any_element()
    }
}

impl Render for CompareView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.catalog_loading || self.loading || self.preparing;
        let ready = self.diff.is_some() && !busy;
        let normalization_hint = if self.options.metadata || self.options.status {
            "Choose which fields to include above. Changes are never applied to either resource."
        } else {
            "Status, namespace and server-generated metadata are ignored. Labels, annotations and configuration are compared."
        };
        let progress = if self.catalog_loading {
            "Loading target resources…"
        } else if self.loading {
            "Reading both resources…"
        } else if self.preparing {
            "Preparing differences…"
        } else {
            "Choose a target to compare with this resource"
        };
        let empty = v_flex().flex_1().items_center().justify_center().gap_3()
            .child(Icon::new(IconName::Search).size_6().text_color(cx.theme().muted_foreground))
            .child(progress)
            .child(label(if self.manual {
                "Enter the target namespace and name, then click Compare."
            } else {
                "Select a cluster, namespace and resource above. A same-name match is compared automatically."
            }, cx));
        let content = v_flex().id("resource-comparison").test_support().size_full().min_size_0()
            .gap_3().text_color(cx.theme().foreground)
            .child(h_flex().gap_3().items_stretch().child(self.source_card(cx)).child(self.target_form(cx)))
            .when_some(self.catalog_note.clone().filter(|_| !ready), |view, note| {
                view.child(div().text_sm().text_color(cx.theme().muted_foreground)
                    .child(copyable_text("comparison-target-note", note)))
            })
            .child(self.comparison_options(busy, cx))
            .child(label(normalization_hint, cx))
            .when_some(self.message.clone(), |view, message| {
                view.child(div().text_sm().text_color(cx.theme().danger)
                    .child(copyable_text("comparison-message", message)))
            })
            .when(ready, |view| view.child(self.result_header(cx)))
            .when(ready && self.page == Page::Changes && !self.changes.is_empty(), |view| {
                view.child(Input::new(&self.search).small()).child(self.changes_view(cx))
            })
            .when(ready && self.page == Page::Changes && self.changes.is_empty(), |view| {
                view.child(v_flex().flex_1().items_center().justify_center().gap_3()
                    .child(Icon::new(IconName::Check).size_6().text_color(cx.theme().success))
                    .child(crate::i18n::text("These resources match with the selected comparison options."))
                    .child(label("Enable status or server metadata if you want to inspect runtime differences.", cx)))
            })
            .when(ready && self.page == Page::Changes && !self.changes.is_empty() && self.visible.is_empty(), |view| {
                view.child(label("No changed fields match this filter.", cx))
            })
            .when(ready && self.page == Page::Yaml, |view| view.child(self.yaml_view(cx)))
            .when(!ready, |view| view.child(empty));
        v_flex()
            .id("resource-comparison-window")
            .test_support()
            .size_full()
            .track_focus(&self.focus)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(|_: &crate::app::CloseTab, window, cx| {
                window.defer(cx, |window, _| window.remove_window());
            })
            .child(
                TitleBar::new().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(crate::i18n::title_text(&self.title)),
                ),
            )
            .child(div().flex_1().min_size_0().p_4().child(content))
    }
}
