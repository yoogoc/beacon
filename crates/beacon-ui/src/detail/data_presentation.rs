use super::*;
use gpui_kit::assets::IconName;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::Icon;
use gpui_kit::component::input::Input;

fn bytes(count: usize) -> String {
    if count < 1024 {
        format!("{count} B")
    } else {
        format!("{:.1} KiB", count as f64 / 1024.)
    }
}

impl DetailView {
    fn render_data_keys(&self, cx: &mut Context<Self>) -> AnyElement {
        let Data::Ready(keys) = &self.data else {
            unreachable!()
        };
        let visible = self.data_browser.visible.clone();
        let view = cx.weak_entity();
        let rows = uniform_list("data-key-list", visible.len(), move |range, _, cx| {
            let Some(view) = view.upgrade() else {
                return vec![];
            };
            let state = view.read(cx);
            let Data::Ready(keys) = &state.data else {
                return vec![];
            };
            range
                .filter_map(|row| {
                    let index = *visible.get(row)?;
                    let key = keys.get(index)?;
                    let active = state.data_browser.selected == Some(index);
                    let deleted = state.data_browser.deleted.contains(&index);
                    let modified = state.data_browser.modified.contains(&index);
                    let role = state.service_account_key(&key.entry.key);
                    let pick = view.downgrade();
                    let color = if deleted {
                        cx.theme().danger
                    } else {
                        cx.theme().foreground
                    };
                    Some(
                        h_flex()
                            .id(SharedString::from(format!(
                                "data-key-{}",
                                data_editors::id(&key.entry)
                            )))
                            .test_support()
                            .role(Role::Button)
                            .focusable()
                            .aria_label(
                                crate::i18n::message(
                                    "messages.open",
                                    &[
                                        ("value0", (key.entry.key).to_string()),
                                        (
                                            "value1",
                                            (if deleted { ", marked for deletion" } else { "" })
                                                .to_string(),
                                        ),
                                    ],
                                )
                                .to_string(),
                            )
                            .w_full()
                            .h(px(46.))
                            .min_w_0()
                            .px_3()
                            .gap_2()
                            .cursor_pointer()
                            .border_l_2()
                            .border_color(if active {
                                cx.theme().primary
                            } else {
                                transparent_black()
                            })
                            .when(active, |row| row.bg(cx.theme().tokens.table_active))
                            .hover(|row| {
                                if active {
                                    row
                                } else {
                                    row.bg(cx.theme().tokens.table_hover)
                                }
                            })
                            .child(
                                Icon::new(if deleted {
                                    IconName::Trash
                                } else if role == Some(crate::service_account_secret::Key::Token) {
                                    IconName::FileKey
                                } else if role
                                    == Some(crate::service_account_secret::Key::Certificate)
                                {
                                    IconName::ShieldCheck
                                } else if role
                                    == Some(crate::service_account_secret::Key::Namespace)
                                {
                                    IconName::Layers
                                } else if key.entry.text.is_none() {
                                    IconName::FileArchive
                                } else {
                                    IconName::FileCode
                                })
                                .size(px(15.))
                                .text_color(if deleted {
                                    color
                                } else {
                                    cx.theme().muted_foreground
                                }),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_color(color)
                                            .text_ellipsis()
                                            .child(key.entry.key.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(if deleted {
                                                "Pending deletion".into()
                                            } else {
                                                format!(
                                                    "{} · {}",
                                                    role.map_or(key.entry.field.name(), |role| {
                                                        role.label()
                                                    }),
                                                    bytes(key.entry.bytes)
                                                )
                                            }),
                                    ),
                            )
                            .when(modified && !deleted, |row| {
                                row.child(div().size(px(6.)).rounded_full().bg(cx.theme().warning))
                            })
                            .on_click(move |_, window, cx| {
                                let _ = pick
                                    .update(cx, |view, cx| view.select_data_key(index, window, cx));
                            }),
                    )
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.data_browser.scroll)
        .size_full();

        v_flex()
            .size_full()
            .min_size_0()
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(crate::i18n::text("KEYS")),
                            )
                            .child(format!(
                                "{} / {}",
                                self.data_browser.visible.len(),
                                keys.len()
                            )),
                    )
                    .child(
                        Input::new(self.data_browser.search.as_ref().unwrap())
                            .small()
                            .prefix(Icon::new(IconName::Search).size(px(14.))),
                    ),
            )
            .child(div().flex_1().min_h_0().overflow_hidden().child(
                if self.data_browser.visible.is_empty() {
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::text(if keys.is_empty() {
                            "No data keys"
                        } else {
                            "No matching keys"
                        }))
                        .into_any_element()
                } else {
                    rows.into_any_element()
                },
            ))
            .into_any_element()
    }

    fn render_data_value(&self, cx: &mut Context<Self>) -> AnyElement {
        let Data::Ready(keys) = &self.data else {
            unreachable!()
        };
        let Some(index) = self.data_browser.selected else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .p_4()
                .child(
                    Icon::new(IconName::FileCode)
                        .size(px(28.))
                        .text_color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::text(if keys.is_empty() {
                            "This resource has no data."
                        } else {
                            "Search or select a key to view its value."
                        })),
                )
                .into_any_element();
        };
        let key = &keys[index];
        let id = data_editors::id(&key.entry);
        let height = self.data_heights.get(&id).copied().unwrap_or(px(240.));
        let deleted = self.data_browser.deleted.contains(&index);
        let covered = self.kind.resource.kind == "Secret" && !self.revealed;
        let may_apply = crate::actions::may_apply(&self.kind, self.rules.as_deref());
        let busy = self.busy();
        let modified = self.data_browser.modified.contains(&index);
        let role = self.service_account_key(&key.entry.key);
        let details = role.is_some() && self.data_browser.details && key.editor.is_some();
        let format = if key.entry.text.is_none() {
            "Binary"
        } else if covered {
            "Hidden"
        } else {
            key.syntax.format.label()
        };
        let content = if deleted {
            v_flex()
                .w_full()
                .p_5()
                .gap_3()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().danger.opacity(0.25))
                .bg(cx.theme().danger.opacity(0.04))
                .child(
                    h_flex()
                        .gap_2()
                        .text_color(cx.theme().danger)
                        .child(Icon::new(IconName::Trash).size_4())
                        .child(crate::i18n::text("Key marked for deletion")),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::text(
                            "Save to review and delete this key/value pair.",
                        )),
                )
                .child(
                    Button::new("restore-data-key")
                        .small()
                        .outline()
                        .icon(IconName::Undo2)
                        .label(crate::i18n::text("Restore key"))
                        .disabled(busy)
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.delete_data_key(index, window, cx)
                        })),
                )
                .into_any_element()
        } else if covered {
            v_flex()
                .w_full()
                .p_5()
                .gap_3()
                .items_start()
                .rounded_lg()
                .bg(cx.theme().muted.opacity(0.45))
                .child(
                    Icon::new(IconName::Lock)
                        .size_5()
                        .text_color(cx.theme().muted_foreground),
                )
                .child(
                    div()
                        .text_sm()
                        .child(crate::i18n::text("Secret values are hidden")),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::text(
                            "Reveal values to read, edit or delete a key.",
                        )),
                )
                .into_any_element()
        } else if details {
            self.render_service_account_value(key, cx)
        } else if let Some(editor) = &key.editor {
            v_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().border)
                .overflow_hidden()
                .child(
                    div()
                        .id(SharedString::from(format!("data-editor-{id}")))
                        .test_support()
                        .w_full()
                        .h(height)
                        .overflow_hidden()
                        .child(
                            Editor::new(editor)
                                .h(relative(1.))
                                .bordered(false)
                                .aria_label(format!("{} contents", key.entry.key))
                                .readonly(!may_apply || busy),
                        ),
                )
                .child(
                    h_flex()
                        .id(SharedString::from(format!("resize-data-{id}")))
                        .test_support()
                        .w_full()
                        .h(px(12.))
                        .flex_shrink_0()
                        .justify_center()
                        .items_center()
                        .cursor_row_resize()
                        .border_t_1()
                        .border_color(cx.theme().border.opacity(0.5))
                        .bg(cx.theme().muted.opacity(0.2))
                        .hover(|this| this.bg(cx.theme().muted))
                        .child(
                            div()
                                .w(px(30.))
                                .h(px(3.))
                                .rounded_full()
                                .bg(cx.theme().border),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                                TextSelection::end(window, cx);
                                cx.stop_propagation();
                                view.data_resize = Some(data_editors::HeightDrag {
                                    id: id.clone(),
                                    height,
                                    y: event.position.y,
                                });
                                cx.notify();
                            }),
                        ),
                )
                .into_any_element()
        } else {
            v_flex().w_full().p_5().gap_2().rounded_lg().bg(cx.theme().muted.opacity(0.45))
                .child(Icon::new(IconName::FileArchive).size_5().text_color(cx.theme().muted_foreground))
                .child(div().text_sm().child(crate::i18n::text("Binary value")))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::text("This value cannot be edited as text. You can delete the entire key/value pair.")))
                .into_any_element()
        };
        v_flex()
            .size_full()
            .min_size_0()
            .child(
                v_flex()
                    .p_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .flex_shrink_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_sm()
                                    .text_ellipsis()
                                    .child(key.entry.key.clone()),
                            )
                            .when(!deleted, |row| {
                                row.child(
                                    Button::new("delete-data-key")
                                        .small()
                                        .ghost()
                                        .icon(
                                            Icon::new(IconName::Trash)
                                                .text_color(cx.theme().danger),
                                        )
                                        .label(crate::i18n::text("Delete key"))
                                        .disabled(busy || !may_apply || covered)
                                        .on_click(cx.listener(move |view, _, window, cx| {
                                            view.delete_data_key(index, window, cx)
                                        })),
                                )
                            })
                            .when(deleted, |row| {
                                row.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().danger)
                                        .child(crate::i18n::text("Deleted")),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .flex_wrap()
                            .child(format!(
                                "{} · {} · {format}",
                                key.entry.field.name(),
                                bytes(key.entry.bytes)
                            ))
                            .when(modified && !deleted, |row| {
                                row.child(div().text_color(cx.theme().warning).child(crate::i18n::text("Modified")))
                            })
                            .when(!may_apply, |row| row.child(crate::i18n::text("Read only")))
                            .when(role.is_some() && key.editor.is_some() && !deleted && !covered, |row| {
                                row.child(h_flex().gap_0p5().rounded_md().bg(cx.theme().muted)
                                    .child(Button::new("account-value-details").small().ghost().selected(self.data_browser.details).label(crate::i18n::text("Details"))
                                        .on_click(cx.listener(|view, _, _, cx| { view.data_browser.details = true; cx.notify(); })))
                                    .child(Button::new("account-value-raw").small().ghost().selected(!self.data_browser.details).label(crate::i18n::text("Raw value"))
                                        .on_click(cx.listener(|view, _, _, cx| { view.data_browser.details = false; cx.notify(); }))))
                                    .child(Button::new("account-copy-value").small().ghost().label(crate::i18n::text(if role == Some(crate::service_account_secret::Key::Token) { "Copy token" } else { "Copy value" }))
                                        .on_click(cx.listener(move |view, _, _, cx| view.copy_data_value(index, cx))))
                            })
                            .when(key.editor.is_some() && !deleted && !covered && !details, |row| {
                                row.child(Button::new("data-wrap-lines").small().ghost()
                                    .selected(self.data_browser.wrap).label(crate::i18n::text("Wrap lines"))
                                    .tooltip(crate::i18n::text("Wrap long lines to fit the editor. When off, scroll horizontally."))
                                    .on_click(cx.listener(|view, _, window, cx| view.toggle_data_wrap(window, cx))))
                            }),
                    ),
            )
            .child(
                div()
                    .id("data-value-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .child(content),
            )
            .into_any_element()
    }

    pub(super) fn render_data(&self, cx: &mut Context<Self>) -> AnyElement {
        if !matches!(self.data, Data::Ready(_)) {
            return self.notice("Reading the data…", Tone::Progressing, cx);
        }
        let wide = self.data_browser.width.get() >= px(680.);
        let width = self.data_browser.width.clone();
        let entity = cx.entity_id();
        let keys = self.render_data_keys(cx);
        let value = self.render_data_value(cx);
        let body = if wide {
            h_flex()
                .size_full()
                .min_size_0()
                .items_stretch()
                .child(
                    div()
                        .w(px(230.))
                        .h_full()
                        .flex_shrink_0()
                        .border_r_1()
                        .border_color(cx.theme().border)
                        .child(keys),
                )
                .child(div().flex_1().min_w_0().h_full().child(value))
                .into_any_element()
        } else {
            v_flex()
                .size_full()
                .min_size_0()
                .child(
                    div()
                        .w_full()
                        .h(px(206.))
                        .flex_shrink_0()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(keys),
                )
                .child(div().w_full().flex_1().min_h_0().child(value))
                .into_any_element()
        };
        v_flex()
            .id("data-browser")
            .test_support()
            .relative()
            .size_full()
            .min_size_0()
            .overflow_hidden()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_prepaint(move |bounds, _, cx| {
                if width.replace(bounds.size.width) != bounds.size.width {
                    cx.notify(entity);
                }
            })
            .when(self.data_resize.is_some(), |this| {
                this.child(self.data_resize_listener(cx))
            })
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_data_bar(
                self.kind.resource.kind == "Secret",
                crate::actions::may_apply(&self.kind, self.rules.as_deref()),
                cx,
            ))
            .into_any_element()
    }
}
