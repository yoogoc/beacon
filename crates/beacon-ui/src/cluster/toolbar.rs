//! Resource filters and an independent checked-selection action row.
use super::*;
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::button::ButtonCustomVariant;
use gpui_kit::component::tooltip::Tooltip;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Layout {
    Full,
    Compact,
    Minimal,
    #[default]
    Overflow,
}

impl Layout {
    fn for_width(width: Pixels, fields: usize) -> Self {
        // PVC has several facets. Bulk actions live on their own row, so
        // checking resources does not squeeze the name search or filters.
        let extra = fields.saturating_sub(1) as f32;
        let width = f32::from(width);
        if width >= 1200. + extra * 140. {
            Self::Full
        } else if width >= 880. + extra * 140. {
            Self::Compact
        } else if width >= 640. + extra * 100. {
            Self::Minimal
        } else {
            Self::Overflow
        }
    }
}

pub(super) fn filter_button(
    id: impl Into<ElementId>,
    glyph: Glyph,
    label: String,
    active: bool,
    cx: &App,
) -> Button {
    Button::new(id)
        .small()
        .outline()
        .icon(Icon::new(glyph))
        .label(crate::i18n::filter_text(&label))
        .dropdown_caret(true)
        .max_w(px(140.))
        .when(active, |button| {
            button.custom(
                ButtonCustomVariant::new(cx)
                    .color(cx.theme().link.opacity(0.12))
                    .foreground(cx.theme().link)
                    .hover(cx.theme().link.opacity(0.2))
                    .active(cx.theme().link.opacity(0.25)),
            )
        })
}

pub(super) fn field_icon(field: Field) -> Glyph {
    match field {
        Field::SecretType | Field::ServiceType | Field::VolumeMode => Glyph::Shapes,
        Field::PodStatus | Field::DeploymentStatus | Field::ClaimStatus => Glyph::Activity,
        Field::IngressClass | Field::Scope => Glyph::Layers,
        Field::Volume | Field::StorageClass => Glyph::Database,
        Field::AccessMode => Glyph::ShieldCheck,
    }
}

/// Search is a compact menu header, separated from the choices by a fine line.
pub(super) fn picker_search(
    id: impl Into<ElementId>,
    input: &Entity<InputState>,
    cx: &App,
) -> impl IntoElement {
    div()
        .id(("picker-search-frame", input.entity_id()))
        // Input emits PressEnter and then propagates the action. The search
        // owns that Enter; it must not also activate a surrounding trigger.
        .on_action(|_: &gpui_kit::component::input::Enter, _, cx| cx.stop_propagation())
        .px_1()
        .pb_1()
        .mb_1()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(
            Input::new(input)
                .id(id)
                .small()
                .text_size(cx.theme().font_size * 0.8125)
                .appearance(false)
                .focus_bordered(false)
                .cleanable(true)
                .prefix(Icon::new(IconName::Search).size(px(14.))),
        )
}

pub(super) fn picker_choice(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    cx: &App,
) -> impl IntoElement + StatefulInteractiveElement + 'static {
    let label = crate::i18n::text(label.into());
    h_flex()
        .id(id)
        .role(Role::ListBoxOption)
        .aria_label(label.clone())
        .aria_selected(selected)
        .test_support()
        .w_full()
        .h(picker_row_height(cx))
        .flex_shrink_0()
        .px_2()
        .gap_2()
        .rounded(px(4.))
        .cursor_pointer()
        .text_size(cx.theme().font_size * 0.8125)
        .line_height(relative(1.))
        .text_color(cx.theme().foreground)
        .font_weight(if selected {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        })
        .when(selected, |row| row.bg(cx.theme().muted.opacity(0.65)))
        .hover(|row| row.bg(cx.theme().muted))
        .child(div().flex_1().min_w_0().truncate().child(label))
        .child(
            div()
                .w_4()
                .flex_shrink_0()
                .children(selected.then(|| Icon::new(IconName::Check).size(px(12.)))),
        )
}

pub(super) fn picker_row_height(cx: &App) -> Pixels {
    (cx.theme().font_size * 1.75).max(px(28.))
}

impl ClusterView {
    pub(super) fn render_create_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.kind
            .as_ref()
            .filter(|_| self.mode == Mode::Objects)
            .map(|kind| {
                let allowed = kind.supports("create");
                div()
                    .id("create-resource-overlay")
                    .test_support()
                    .absolute()
                    .bottom(px(20.))
                    .right(px(24.))
                    .occlude()
                    .child(
                        Button::new("create-resource")
                            .primary()
                            .size(px(48.))
                            .rounded(px(24.))
                            .shadow_md()
                            .child(Icon::new(IconName::Plus).size(px(22.)))
                            .disabled(!allowed)
                            .accessibility_label(
                                crate::i18n::message(
                                    "messages.create",
                                    &[("value0", (kind.resource.kind).to_string())],
                                )
                                .to_string(),
                            )
                            .tooltip(if allowed {
                                crate::i18n::message(
                                    "messages.create_from_yaml",
                                    &[("value0", (kind.resource.kind).to_string())],
                                )
                                .to_string()
                            } else {
                                crate::i18n::message(
                                    "messages.does_not_support_creation",
                                    &[("value0", (kind.resource.kind).to_string())],
                                )
                                .to_string()
                            })
                            .on_click(
                                cx.listener(|view, _, window, cx| view.start_create(window, cx)),
                            ),
                    )
                    .into_any_element()
            })
    }

    fn render_toolbar_controls(&self, layout: Layout, cx: &mut Context<Self>) -> AnyElement {
        let objects = self.mode == Mode::Objects && self.kind.is_some();
        h_flex()
            .gap_2()
            .items_center()
            .flex_shrink_0()
            .when(objects, |bar| {
                bar.child(self.render_label_picker(layout, cx))
                    .children(
                        self.kind
                            .as_ref()
                            .into_iter()
                            .flat_map(|kind| Field::for_kind(kind))
                            .map(|field| {
                                self.render_field_picker(field, layout, cx)
                                    .into_any_element()
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .pl_2()
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .child(self.render_saved_filters(cx))
                            .child(self.render_column_picker(cx)),
                    )
            })
            .child(self.render_log_action(false, cx))
            .into_any_element()
    }

    /// Overflow uses the same controlled pickers, including their option search.
    /// Each gets its own line inside the popup; the resource toolbar never wraps.
    fn render_toolbar_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let opening = cx.entity().downgrade();
        let content = opening.clone();
        Popover::new("resource-toolbar-more")
            .open(self.toolbar_open)
            // Nested popups extend beyond this surface. Let the child handle
            // outside clicks while it is open, so its rows remain clickable.
            .overlay_closable(
                !self.namespace_menu_open
                    && !self.label_menu_open
                    && self.filter_menu_open.is_none()
                    && !self.list_settings.is_open(),
            )
            .on_open_change(move |open, _, cx| {
                let _ = opening.update(cx, |view, cx| {
                    view.toolbar_open = *open;
                    if !open {
                        view.namespace_menu_open = false;
                        view.label_menu_open = false;
                        view.filter_menu_open = None;
                        view.list_settings.close();
                    }
                    cx.notify();
                });
            })
            .trigger(
                Button::new("resource-toolbar-more-trigger")
                    .small()
                    .ghost()
                    .icon(IconName::Ellipsis)
                    .accessibility_label(crate::i18n::text("Filters and display settings"))
                    .tooltip(crate::i18n::text("Filters and display settings")),
            )
            .content(move |_, _, cx| {
                content
                    .update(cx, |view, cx| {
                        let fields = view
                            .kind
                            .as_ref()
                            .map(|kind| Field::for_kind(kind))
                            .unwrap_or_default();
                        v_flex()
                            .w(px(260.))
                            .gap_2()
                            .text_sm()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(crate::i18n::text("Filters and display")),
                            )
                            .when(view.mode == Mode::Objects && view.kind.is_some(), |menu| {
                                menu.child(view.render_label_picker(Layout::Full, cx))
                                    .children(fields.into_iter().map(|field| {
                                        view.render_field_picker(field, Layout::Full, cx)
                                            .into_any_element()
                                    }))
                                    .child(view.render_saved_filters(cx))
                                    .child(view.render_column_picker(cx))
                            })
                            .child(view.render_log_action(true, cx))
                            .into_any_element()
                    })
                    .unwrap_or_else(|_| div().into_any_element())
            })
    }

    fn render_log_action(&self, vertical: bool, cx: &mut Context<Self>) -> impl IntoElement {
        // Reuse the toolbar's action handling without mounting a second set
        // of filter triggers (which would share popover IDs).
        h_flex()
            .gap_2()
            .items_center()
            .when(vertical, |actions| actions.flex_col().items_stretch())
            .when(
                self.mode == Mode::Objects
                    && self.kind.as_ref().is_some_and(|kind| {
                        kind.resource.group.is_empty() && kind.resource.kind == "Pod"
                    }),
                |menu| {
                    menu.child(
                        Button::new("aggregate-pod-logs")
                            .small()
                            .ghost()
                            .icon(Icon::new(Glyph::Logs))
                            .accessibility_label(crate::i18n::text("Aggregated logs"))
                            .tooltip(crate::i18n::text("Aggregate logs from the filtered Pods"))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.toolbar_open = false;
                                view.aggregate_filtered(window, cx);
                                cx.notify();
                            })),
                    )
                },
            )
    }

    pub(super) fn render_selection_toolbar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let selected = self.table.read(cx).delegate().selected_count();
        if self.mode != Mode::Objects || selected == 0 {
            return None;
        }
        let compact = matches!(
            self.toolbar_layout.get(),
            Layout::Minimal | Layout::Overflow
        );
        Some(
            h_flex()
                .id("resource-selection-toolbar")
                .role(Role::Toolbar)
                .aria_label(crate::i18n::text("Selected resource actions"))
                .test_support()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .px_3()
                .py_1p5()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.35))
                .child(
                    div()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .child(crate::i18n::message(
                            "resources.selected",
                            &[("count", selected.to_string())],
                        )),
                )
                .child(
                    Button::new("clear-selected")
                        .small()
                        .ghost()
                        .icon(IconName::Close)
                        .accessibility_label(crate::i18n::text("Clear selection"))
                        .tooltip(crate::i18n::text("Clear selection"))
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.table.update(cx, |table, cx| {
                                table.delegate_mut().clear_selected();
                                cx.notify();
                            });
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("transfer-selected")
                        .small()
                        .outline()
                        .icon(Glyph::ArrowRightLeft)
                        .when(!compact, |button| {
                            button.label(crate::i18n::text("Copy / move"))
                        })
                        .accessibility_label(crate::i18n::text("Copy or move selected resources"))
                        .tooltip(crate::i18n::text("Copy or move selected resources"))
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.transfer_resources(None, window, cx)
                        })),
                )
                .child(
                    Button::new("export-selected")
                        .small()
                        .outline()
                        .icon(Glyph::Download)
                        .loading(self.resource_exporting)
                        .when(!compact, |button| button.label(crate::i18n::text("Export")))
                        .accessibility_label(crate::i18n::text("Export selected resources"))
                        .tooltip(crate::i18n::text(if self.kind.as_ref().is_some_and(|kind| kind.resource.group.is_empty() && kind.resource.kind == "Secret") {
                            "Export full Secret YAML, including Secret values. Multiple resources are saved as a ZIP."
                        } else {
                            "Export one resource as YAML, or multiple YAML files in a ZIP."
                        }))
                        .disabled(self.resource_exporting || !self.kind.as_ref().is_some_and(|kind| kind.supports("get")))
                        .on_click(cx.listener(|view, _, window, cx| view.export_selected(window, cx))),
                )
                .when(selected == 2, |bar| {
                    bar.child(
                        Button::new("compare-selected")
                            .small()
                            .outline()
                            .icon(Icon::new(Glyph::Columns3))
                            .when(!compact, |button| {
                                button.label(crate::i18n::text("Compare"))
                            })
                            .accessibility_label(crate::i18n::text("Compare selected resources"))
                            .tooltip(crate::i18n::text("Compare the two selected resources"))
                            .on_click(
                                cx.listener(|view, _, window, cx| {
                                    view.compare_selected(window, cx)
                                }),
                            ),
                    )
                })
                .when(
                    self.kind.as_ref().is_some_and(|kind| {
                        kind.resource.group == "apps" && kind.resource.kind == "Deployment"
                    }),
                    |bar| {
                        bar.child(
                            Button::new("restart-selected")
                                .small()
                                .outline()
                                .icon(IconName::RefreshCw)
                                .when(!compact, |button| {
                                    button.label(crate::i18n::text("Restart"))
                                })
                                .accessibility_label(crate::i18n::text(
                                    "Restart selected Deployments",
                                ))
                                .tooltip(crate::i18n::text(
                                    "Restart Pods using each Deployment's update strategy",
                                ))
                                .disabled(
                                    !self.may_restart_selected(cx)
                                        || self.bulk_deleting
                                        || self.bulk_restart.is_some(),
                                )
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.start_restart_selected(window, cx)
                                })),
                        )
                    },
                )
                .child(
                    Button::new("delete-selected")
                        .small()
                        .danger()
                        .icon(Icon::new(Glyph::Trash))
                        .when(!compact, |button| {
                            button.label(crate::i18n::text("Delete selected"))
                        })
                        .accessibility_label(crate::i18n::text("Delete selected resources"))
                        .tooltip(crate::i18n::text("Delete selected resources"))
                        .disabled(
                            !self.may_delete() || self.bulk_deleting || self.bulk_restart.is_some(),
                        )
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.start_delete_selected(window, cx)
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_toolbar_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notice = self
            .list_settings
            .error
            .as_ref()
            .map(|error| (Tone::Critical, error.clone()))
            .or_else(|| {
                self.outcome.as_ref().map(|outcome| match outcome {
                    Outcome::Running(what) => (Tone::Progressing, format!("{what}…")),
                    Outcome::Done(what) => (Tone::Healthy, format!("{what} — done")),
                    Outcome::Failed(why) => (Tone::Critical, why.clone()),
                })
            });
        notice.map(|(tone, text)| {
            let session = self.session.clone();
            let query = beacon_kube::rbac::Query {
                verb: "get".into(),
                group: self
                    .kind
                    .as_ref()
                    .map(|kind| kind.resource.group.clone())
                    .unwrap_or_default(),
                resource: self
                    .kind
                    .as_ref()
                    .map(|kind| kind.resource.plural.clone())
                    .unwrap_or_default(),
                namespace: self.scoped_to.iter().next().cloned(),
                ..Default::default()
            };
            let glyph = match tone {
                Tone::Healthy => Glyph::CircleCheck,
                Tone::Critical => Glyph::CircleX,
                _ => Glyph::Activity,
            };
            Popover::new("resource-operation-notice")
                .trigger(
                    Button::new("operation-outcome-trigger")
                        .small()
                        .ghost()
                        .icon(Icon::new(glyph).text_color(cx.theme().tone(tone)))
                        .accessibility_label(text.clone())
                        .tooltip(text.clone()),
                )
                .content(move |_, _, cx| {
                    div()
                        .w(px(320.))
                        .text_sm()
                        .text_color(cx.theme().tone(tone))
                        .child(crate::copyable_text::copyable_text(
                            "operation-outcome",
                            text.clone(),
                        ))
                        .children(crate::permissions::error_link(
                            "operation-permissions",
                            session.clone(),
                            &text,
                            query.clone(),
                        ))
                })
                .into_any_element()
        })
    }

    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (shown, total) = self.counts(cx);
        let loading = match self.mode {
            Mode::Objects => self.table.read(cx).delegate().is_loading(),
            Mode::Releases => matches!(self.releases, Releases::Unopened | Releases::Loading),
            _ => false,
        };
        let count = match self.mode {
            Mode::Overview => String::new(),
            Mode::Objects if shown == total => total.to_string(),
            Mode::Objects => crate::i18n::message(
                "messages.shown_of_total",
                &[
                    ("shown", (shown).to_string()),
                    ("total", (total).to_string()),
                ],
            )
            .to_string(),
            Mode::Releases => match &self.releases {
                Releases::Ready(items) => items.len().to_string(),
                _ => String::new(),
            },
            Mode::Forwards => self.session.forwards().len().to_string(),
        };
        let count_label = if loading {
            if self.mode == Mode::Objects && total > 0 {
                crate::i18n::message(
                    "messages.total_resources_loaded_loading_more",
                    &[("total", (total).to_string())],
                )
                .to_string()
            } else {
                crate::i18n::text("Loading resources").to_string()
            }
        } else if count == "1" {
            crate::i18n::text("1 resource").to_string()
        } else {
            crate::i18n::message(
                "messages.count_resources",
                &[("count", (count).to_string())],
            )
            .to_string()
        };
        let count_tooltip = count_label.clone();
        let layout = self.toolbar_layout.get();
        let measured = self.toolbar_layout.clone();
        let entity = cx.entity_id();
        let fields = self
            .kind
            .as_ref()
            .map(|kind| Field::for_kind(kind).len())
            .unwrap_or(0);
        h_flex()
            .id("resource-toolbar")
            .role(Role::Toolbar)
            .aria_label(crate::i18n::text("Resource toolbar"))
            .test_support()
            .relative()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .on_prepaint(move |bounds, _, cx| {
                let next = Layout::for_width(bounds.size.width, fields);
                if measured.replace(next) != next {
                    cx.notify(entity);
                }
            })
            .child(
                h_flex()
                    .min_w_0()
                    .flex_shrink_0()
                    .gap_2()
                    .items_center()
                    .when(self.mode != Mode::Objects, |header| {
                        header.child(
                            div()
                                .min_w_0()
                                .max_w(px(145.))
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .text_sm()
                                .child(crate::i18n::text(self.mode.label())),
                        )
                    })
                    .children(self.kind.as_ref().filter(|kind| kind.namespaced).map(|_| {
                        self.render_namespace_picker(
                            if layout == Layout::Overflow {
                                Layout::Minimal
                            } else {
                                Layout::Compact
                            },
                            cx,
                        )
                    }))
                    .when(loading || !count.is_empty(), |header| {
                        header.child(
                            h_flex()
                                .id("resource-count")
                                .role(Role::Status)
                                .test_support()
                                .aria_label(count_label)
                                .tooltip(move |window, cx| {
                                    Tooltip::new(count_tooltip.clone()).build(window, cx)
                                })
                                .text_xs()
                                .flex_shrink_0()
                                .h(px(24.))
                                .min_w(px(26.))
                                .max_w(px(100.))
                                .items_center()
                                .justify_center()
                                .gap_1p5()
                                .text_color(cx.theme().muted_foreground)
                                .rounded_md()
                                .bg(cx.theme().muted)
                                .px_1p5()
                                .children(loading.then(|| {
                                    div()
                                        .id("resource-count-loading")
                                        .role(Role::ProgressIndicator)
                                        .aria_label(crate::i18n::text("Loading resources"))
                                        .test_support()
                                        .child(
                                            Spinner::new()
                                                .xsmall()
                                                .color(cx.theme().muted_foreground),
                                        )
                                }))
                                .children(
                                    (!loading || self.mode == Mode::Objects && total > 0).then(
                                        || {
                                            div()
                                                .id("resource-count-value")
                                                .test_support()
                                                .min_w_0()
                                                .text_ellipsis()
                                                .child(count)
                                        },
                                    ),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .id("resource-search")
                    .test_support()
                    .flex_1()
                    .min_w(px(80.))
                    .child(
                        Input::new(&self.row_search)
                            .small()
                            .prefix(Icon::new(IconName::Search).small())
                            .cleanable(true),
                    ),
            )
            .children(self.render_toolbar_notice(cx))
            .child(if layout == Layout::Overflow {
                self.render_toolbar_menu(cx).into_any_element()
            } else {
                self.render_toolbar_controls(layout, cx)
            })
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod bulk_restart_tests;

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;

    struct Pane {
        view: Entity<ClusterView>,
        width: Pixels,
    }
    impl Render for Pane {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(div().w(self.width).h_full().child(self.view.clone()))
        }
    }
    fn draw(cx: &mut TestAppContext, window: AnyWindowHandle) {
        for _ in 0..3 {
            cx.run_until_parked();
            cx.update_window(window, |_, window, cx| window.render_frame(cx))
                .unwrap();
        }
    }

    #[::core::prelude::v1::test]
    fn first_page_is_visible_and_searchable_while_later_pages_are_loading() {
        let directory = tempfile::tempdir().unwrap();
        let cx = &mut support::context();
        support::workspace(cx, directory.path());
        let (fixture, session) = support::fixture(cx, "progressive-list", (0..3).map(|i| json!({
            "apiVersion":"v1","kind":"Pod", "metadata":{"name":format!("page-pod-{i}"),"namespace":"default","uid":format!("pod-{i}"),"resourceVersion":"1"},
            "spec":{"containers":[{"name":"app","image":"busybox"}]},"status":{"phase":"Running"}
        })).collect());
        let gate = fixture.paginate("pods", 2);
        let (window, pane) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        Point::default(),
                        size(px(1000.), px(720.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    let view = cx.new(|cx| {
                        ClusterView::new(
                            session,
                            Some("default".into()),
                            Some(Arc::new(support::kind("", "Pod", "pods"))),
                            None,
                            false,
                            window,
                            cx,
                        )
                    });
                    cx.new(|_| Pane {
                        view,
                        width: px(1000.),
                    })
                },
            )
            .unwrap()
        });
        let view = pane.read_with(cx, |pane, _| pane.view.clone());
        support::settle(cx, |cx| {
            view.read_with(cx, |view, cx| view.table.read(cx).delegate().total() == 2)
        });
        draw(cx, window);
        cx.update_window(window, |_, window, cx| {
            assert!(window.find("resource-count-loading").visible());
            assert!(window.find("resource-count-value").visible());
            assert_eq!(
                window.find("resource-count").label(),
                Some("2 resources loaded; loading more")
            );
            assert!(
                window.find("select-default-page-pod-0").visible(),
                "the table must show data instead of skeleton rows"
            );
            view.update(cx, |view, cx| {
                view.table.update(cx, |table, cx| {
                    assert!(table.delegate().is_loading());
                    table.delegate_mut().set_filter("page-pod-2");
                    cx.notify();
                })
            });
        })
        .unwrap();
        assert!(view.read_with(cx, |view, cx| view.table.read(cx).delegate().is_empty()));
        gate.release();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, cx| {
                let table = view.table.read(cx).delegate();
                !table.is_loading() && table.total() == 3 && table.len() == 1
            })
        });
        draw(cx, window);
        cx.update_window(window, |_, window, _| {
            assert!(window.try_find("resource-count-loading").is_none());
            assert_eq!(
                window.find("resource-count").label(),
                Some("1 of 3 resources")
            );
            assert!(window.find("select-default-page-pod-2").visible());
        })
        .unwrap();
    }

    #[::core::prelude::v1::test]
    fn create_button_floats_over_rows_and_stays_fixed_when_the_table_scrolls() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let (_fixture, session) = support::fixture(
            cx,
            "floating-create",
            (0..80).map(|index| json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":format!("demo-{index:02}"),"namespace":"default","uid":index.to_string(),"resourceVersion":"1"},"type":"Opaque"})).collect(),
        );
        let mut kind = support::kind("", "Secret", "secrets");
        kind.verbs.push("create".into());
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(1200.), px(500.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|cx| {
                        ClusterView::new(
                            session,
                            Some("default".into()),
                            Some(Arc::new(kind)),
                            None,
                            false,
                            window,
                            cx,
                        )
                    })
                },
            )
            .unwrap()
        });
        support::settle(cx, |cx| {
            view.read_with(cx, |view, cx| view.counts(cx).0 == 80)
        });
        draw(cx, window);
        let before = cx
            .update_window(window, |_, window, _| {
                let button = window.find("create-resource").bounds();
                let pane = window.find("resource-list-pane").bounds();
                assert_eq!(pane.bottom() - button.bottom(), px(20.));
                assert_eq!(pane.right() - button.right(), px(24.));
                assert!(
                    gpui_kit::base::test_support::snapshots(window)
                        .into_iter()
                        .any(|row| row.role() == Some(Role::Row)
                            && row.bounds().contains(&button.center())),
                    "FAB reserved blank space below the table instead of overlaying a row"
                );
                button
            })
            .unwrap();
        view.update(cx, |view, cx| {
            view.table
                .update(cx, |table, cx| table.scroll_to_row(45, cx))
        });
        draw(cx, window);
        cx.update_window(window, |_, window, cx| {
            assert_eq!(window.find("create-resource").bounds(), before);
            window.click("create-resource", cx);
        })
        .unwrap();
        assert!(view.read_with(cx, |view, _| view.creation.is_some()
            && view.detail.is_none()));
    }

    #[::core::prelude::v1::test]
    fn toolbar_stays_in_one_row_within_resized_panes_and_create_opens_the_same_dialog() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let (fixture, session) = support::fixture(
            cx,
            "toolbar-fixture",
            vec![
                json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"demo-tls","namespace":"default","uid":"one","resourceVersion":"1"},"type":"kubernetes.io/tls"}),
                json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"demo-pod","namespace":"default","uid":"pod-one","resourceVersion":"1"},"spec":{"containers":[{"name":"app","image":"busybox"}]},"status":{"phase":"Running"}}),
            ],
        );
        let mut kind = support::kind("", "Secret", "secrets");
        kind.verbs.push("create".into());
        let (window, pane) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(1900.), px(720.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    let view = cx.new(|cx| {
                        ClusterView::new(
                            session,
                            Some("default".into()),
                            Some(Arc::new(kind)),
                            None,
                            false,
                            window,
                            cx,
                        )
                    });
                    cx.new(|_| Pane {
                        view,
                        width: px(1360.),
                    })
                },
            )
            .unwrap()
        });
        let view = pane.read_with(cx, |pane, _| pane.view.clone());
        support::settle(cx, |cx| {
            view.read_with(cx, |view, cx| view.counts(cx).0 == 1)
        });
        for width in [1360., 1200., 960., 880., 700., 640., 440., 280.] {
            pane.update(cx, |pane, cx| {
                pane.width = px(width);
                cx.notify();
            });
            draw(cx, window);
            cx.update_window(window, |_, window, _| {
                let bar = window.find("resource-toolbar").bounds();
                assert_eq!(bar.size.width, px(width));
                assert!(
                    bar.size.height <= px(44.),
                    "toolbar wrapped at {width}: {bar:?}"
                );
                let namespace = window.find("namespace-picker-trigger").bounds();
                let count = window.find("resource-count").bounds();
                let search = window.find("resource-search").bounds();
                assert!(namespace.right() <= count.origin.x);
                assert!(count.right() <= search.origin.x);
                assert_eq!(window.find("resource-count").label(), Some("1 resource"));
                assert!(window.try_find("resource-count-loading").is_none());
                let controls = gpui_kit::base::test_support::snapshots(window)
                    .into_iter()
                    .filter(|element| {
                        element
                            .path()
                            .contains(&ElementId::from("resource-toolbar"))
                            && element.role() == Some(Role::Button)
                    });
                for control in controls {
                    let bounds = control.bounds();
                    assert!(
                        bounds.origin.x >= bar.origin.x && bounds.right() <= bar.right(),
                        "control escaped pane at {width}: {control:?}"
                    );
                    assert!((f32::from(bounds.center().y - bar.center().y)).abs() <= 1.);
                }
                let create = window.find("create-resource");
                assert_eq!(create.label(), Some("Create Secret"));
                assert_eq!(create.bounds().size, size(px(48.), px(48.)));
                assert!(create.bounds().origin.y > bar.bottom());
                assert!(create.bounds().right() <= px(width));
            })
            .unwrap();
        }
        cx.update_window(window, |_, window, cx| window.click("create-resource", cx))
            .unwrap();
        assert!(view.read_with(cx, |view, _| view.creation.is_some()));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("cancel-create-resource", cx);
        })
        .unwrap();
        assert!(view.read_with(cx, |view, _| view.creation.is_none()));
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.show(Arc::new(support::kind("", "Pod", "pods")), window, cx);
            });
            window.render_frame(cx);
            assert_eq!(
                window.find("resource-count-loading").role(),
                Some(Role::ProgressIndicator)
            );
            assert!(window.try_find("resource-count-value").is_none());
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, cx| view.counts(cx).0 == 1)
        });
        for selected in [false, true] {
            if selected {
                view.update(cx, |view, cx| {
                    view.table.update(cx, |table, cx| {
                        table.delegate_mut().toggle_all_visible();
                        cx.notify();
                    });
                    cx.notify();
                });
            }
            for width in [1420., 1200., 1100., 880., 860., 640., 280.] {
                pane.update(cx, |pane, cx| {
                    pane.width = px(width);
                    cx.notify();
                });
                draw(cx, window);
                cx.update_window(window, |_, window, _| {
                    let bar = window.find("resource-toolbar").bounds();
                    assert!(bar.size.height <= px(44.));
                    assert!(window.find("resource-search").bounds().size.width >= px(80.));
                    for control in gpui_kit::base::test_support::snapshots(window)
                        .into_iter()
                        .filter(|element| {
                            element
                                .path()
                                .contains(&ElementId::from("resource-toolbar"))
                                && element.role() == Some(Role::Button)
                        })
                    {
                        assert!(
                            control.bounds().right() <= bar.right(),
                            "Pod control escaped at {width}, selected={selected}: {control:?}"
                        );
                    }
                })
                .unwrap();
            }
        }
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.show(
                    Arc::new(support::kind(
                        "",
                        "PersistentVolumeClaim",
                        "persistentvolumeclaims",
                    )),
                    window,
                    cx,
                );
                view.table.update(cx, |table, cx| {
                    table.delegate_mut().set_field_filter(
                        Field::Volume,
                        Some("pvc-a-long-volume-name-for-layout-validation".into()),
                    );
                    table.delegate_mut().set_field_filter(
                        Field::StorageClass,
                        Some("a-long-storage-class-name-for-layout-validation".into()),
                    );
                    cx.notify();
                });
            });
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, cx| !view.table.read(cx).delegate().is_loading())
        });
        draw(cx, window);
        cx.update_window(window, |_, window, _| {
            assert_eq!(window.find("resource-count").label(), Some("0 resources"));
            assert!(window.try_find("resource-count-loading").is_none());
        })
        .unwrap();
        for width in [1900., 1440., 1100., 740., 280.] {
            pane.update(cx, |pane, cx| {
                pane.width = px(width);
                cx.notify();
            });
            draw(cx, window);
            cx.update_window(window, |_, window, _| {
                let bar = window.find("resource-toolbar").bounds();
                assert!(bar.size.height <= px(44.));
                for control in gpui_kit::base::test_support::snapshots(window)
                    .into_iter()
                    .filter(|element| {
                        element
                            .path()
                            .contains(&ElementId::from("resource-toolbar"))
                            && element.role() == Some(Role::Button)
                    })
                {
                    assert!(
                        control.bounds().right() <= bar.right(),
                        "PVC control escaped at {width}: {control:?}"
                    );
                }
            })
            .unwrap();
        }
        assert!(
            !fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(
                    |(request, _)| request.starts_with("POST /api/v1/namespaces/default/secrets")
                        || request.starts_with("PATCH")
                        || request.starts_with("DELETE")
                )
        );
    }

    #[::core::prelude::v1::test]
    fn narrow_toolbar_keeps_searchable_nested_filters_and_column_settings_usable() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let (_fixture, session) = support::fixture(
            cx,
            "toolbar-filters",
            vec![
                json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"one","namespace":"default","uid":"one","resourceVersion":"1"},"type":"Opaque"}),
                json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"two","namespace":"default","uid":"two","resourceVersion":"1"},"type":"kubernetes.io/tls"}),
            ],
        );
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(420.), px(720.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|cx| {
                        ClusterView::new(
                            session,
                            Some("default".into()),
                            Some(Arc::new(support::kind("", "Secret", "secrets"))),
                            None,
                            false,
                            window,
                            cx,
                        )
                    })
                },
            )
            .unwrap()
        });
        support::settle(cx, |cx| {
            view.read_with(cx, |view, cx| view.counts(cx).0 == 2)
        });
        draw(cx, window);
        cx.update_window(window, |_, window, cx| {
            window.click("resource-toolbar-more-trigger", cx);
            window.render_frame(cx);
            window.click("filter-trigger-SecretType", cx);
            window.render_frame(cx);
            let all = window.find("filter-SecretType-All");
            assert_eq!(all.role(), Some(Role::ListBoxOption));
            assert_eq!(all.selected(), Some(true));
            assert_eq!(
                window
                    .find("filter-SecretType-kubernetes.io/tls")
                    .selected(),
                Some(false)
            );
        })
        .unwrap();
        assert!(view.read_with(cx, |view, _| view.toolbar_open
            && view.filter_menu_open == Some(Field::SecretType)));
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.picker_search
                    .update(cx, |input, cx| input.set_value("tls", window, cx))
            });
            window.render_frame(cx);
            let choices = gpui_kit::base::test_support::snapshots(window);
            assert!(
                choices
                    .iter()
                    .any(|element| element.label() == Some("kubernetes.io/tls"))
            );
            assert!(
                !choices
                    .iter()
                    .any(|element| element.label() == Some("Opaque"))
            );
            window.click("filter-SecretType-kubernetes.io/tls", cx);
        })
        .unwrap();
        assert_eq!(view.read_with(cx, |view, cx| view.counts(cx)), (1, 2));
        draw(cx, window);
        cx.update_window(window, |_, window, cx| {
            window.click("filter-trigger-SecretType", cx);
            window.render_frame(cx);
            assert_eq!(
                window
                    .find("filter-SecretType-kubernetes.io/tls")
                    .selected(),
                Some(true)
            );
            assert_eq!(window.find("filter-SecretType-All").selected(), Some(false));
            window.click("filter-SecretType-kubernetes.io/tls", cx);
        })
        .unwrap();
        assert_eq!(
            view.read_with(cx, |view, _| (view.toolbar_open, view.filter_menu_open)),
            (true, None)
        );
        cx.update_window(window, |_, window, cx| {
            window.click("resource-columns-trigger", cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, _| {
            assert!(window.find("reset-resource-columns").visible());
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.click("reset-resource-columns", cx)
        })
        .unwrap();
    }

    #[::core::prelude::v1::test]
    fn namespace_menu_keeps_single_and_multiple_selection_distinct_and_searchable() {
        let cx = &mut support::context();
        let directory = tempfile::tempdir().unwrap();
        support::workspace(cx, directory.path());
        let (fixture, session) = support::fixture(
            cx,
            "namespace-menu",
            vec![
                json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"default","uid":"default","resourceVersion":"1"}}),
                json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":"argo","uid":"argo","resourceVersion":"1"}}),
                json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"demo","namespace":"default","uid":"demo","resourceVersion":"1"},"type":"Opaque"}),
            ],
        );
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: Default::default(),
                        size: size(px(1200.), px(720.)),
                    })),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    cx.new(|cx| {
                        ClusterView::new(
                            session,
                            Some("default".into()),
                            Some(Arc::new(support::kind("", "Secret", "secrets"))),
                            None,
                            false,
                            window,
                            cx,
                        )
                    })
                },
            )
            .unwrap()
        });
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| view.namespace_names.len() == 2)
        });
        draw(cx, window);
        cx.update_window(window, |_, window, cx| {
            window.click("namespace-picker-trigger", cx);
            assert_eq!(window.find("ns-tick-default").checked(), Some(true));
            assert_eq!(window.find("ns-all").selected(), Some(false));
            window.click("ns-tick-argo", cx);
            assert_eq!(window.find("ns-tick-argo").checked(), Some(true));
            assert_eq!(window.find("ns-tick-default").checked(), Some(true));
        })
        .unwrap();
        assert!(view.read_with(cx, |view, _| view.namespace_menu_open
            && view.scoped_to.len() == 2));
        cx.update_window(window, |_, window, cx| {
            window.click("ns-only-default", cx);
        })
        .unwrap();
        assert!(view.read_with(cx, |view, _| !view.namespace_menu_open
            && view.scoped_to == std::collections::BTreeSet::from(["default".into()])));
        cx.update_window(window, |_, window, cx| {
            window.click("namespace-picker-trigger", cx);
            window.click("ns-all", cx);
        })
        .unwrap();
        assert!(view.read_with(cx, |view, _| view.scoped_to.is_empty()
            && !view.namespace_menu_open));
        cx.update_window(window, |_, window, cx| {
            window.click("namespace-picker-trigger", cx);
            view.update(cx, |view, cx| {
                view.picker_search
                    .update(cx, |input, cx| input.set_value("argo", window, cx));
            });
            window.render_frame(cx);
            assert!(window.find("ns-only-argo").visible());
            assert!(window.try_find("ns-only-default").is_none());
            assert_eq!(window.find("ns-all").selected(), Some(true));
        })
        .unwrap();
        assert!(fixture.requests.lock().unwrap().iter().all(|(request, _)| {
            !request.starts_with("POST /api/v1/")
                && !request.starts_with("PATCH")
                && !request.starts_with("DELETE")
        }));
    }
}
