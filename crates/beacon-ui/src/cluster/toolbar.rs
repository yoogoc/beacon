//! A single resource toolbar, measured within its own workspace pane.
use super::*;
use gpui_kit::assets::IconName as Glyph;
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::button::ButtonCustomVariant;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Layout {
    Full,
    Compact,
    Minimal,
    #[default]
    Overflow,
}

impl Layout {
    fn for_width(width: Pixels, fields: usize, selected: bool) -> Self {
        // PVC has several facets; selection also adds two bulk actions. Keep
        // those reachable in a menu rather than squeezing the name search.
        let extra = fields.saturating_sub(1) as f32;
        let bulk = if selected { 220. } else { 0. };
        let width = f32::from(width) - bulk;
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

    pub(super) fn text_actions(self) -> bool {
        matches!(self, Self::Full)
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
        .label(label)
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

impl ClusterView {
    pub(super) fn render_create_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.kind
            .as_ref()
            .filter(|_| self.mode == Mode::Objects)
            .map(|kind| {
                let allowed = kind.supports("create");
                div()
                    .absolute()
                    .bottom(px(12.))
                    .right(px(16.))
                    .child(
                        Button::new("create-resource")
                            .primary()
                            .size(px(48.))
                            .rounded(px(24.))
                            .shadow_md()
                            .child(Icon::new(IconName::Plus).size(px(22.)))
                            .disabled(!allowed)
                            .accessibility_label(format!("Create {}", kind.resource.kind))
                            .tooltip(if allowed {
                                format!("Create {} from YAML", kind.resource.kind)
                            } else {
                                format!("{} does not support creation", kind.resource.kind)
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
                            .child(self.render_saved_filters(layout, cx))
                            .child(self.render_column_picker(layout, cx)),
                    )
            })
            .children(
                self.kind
                    .as_ref()
                    .filter(|kind| kind.namespaced)
                    .map(|_| self.render_namespace_picker(layout, cx)),
            )
            .child(self.render_bulk_and_logs(layout, false, cx))
            .into_any_element()
    }

    /// Overflow uses the same controlled pickers, including their option search.
    /// Each gets its own line inside the popup; the resource toolbar never wraps.
    fn render_toolbar_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let opening = cx.entity().downgrade();
        let content = opening.clone();
        Popover::new("resource-toolbar-more")
            .open(self.toolbar_open)
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
                    .accessibility_label("Filters and display settings")
                    .tooltip("Filters and display settings"),
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
                                    .child("Filters and display"),
                            )
                            .when(view.mode == Mode::Objects && view.kind.is_some(), |menu| {
                                menu.child(view.render_label_picker(Layout::Full, cx))
                                    .children(fields.into_iter().map(|field| {
                                        view.render_field_picker(field, Layout::Full, cx)
                                            .into_any_element()
                                    }))
                                    .child(view.render_saved_filters(Layout::Full, cx))
                                    .child(view.render_column_picker(Layout::Full, cx))
                            })
                            .children(
                                view.kind
                                    .as_ref()
                                    .filter(|kind| kind.namespaced)
                                    .map(|_| view.render_namespace_picker(Layout::Full, cx)),
                            )
                            .child(view.render_bulk_and_logs(Layout::Full, true, cx))
                            .into_any_element()
                    })
                    .unwrap_or_else(|_| div().into_any_element())
            })
    }

    fn render_bulk_and_logs(
        &self,
        layout: Layout,
        vertical: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Reuse the toolbar's action handling without mounting a second set
        // of filter triggers (which would share popover IDs).
        let selected = self.table.read(cx).delegate().selected_count();
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
                            .when(layout.text_actions(), |button| {
                                button.label("Aggregated logs…")
                            })
                            .accessibility_label("Aggregated logs")
                            .tooltip("Aggregate logs from the filtered Pods")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.toolbar_open = false;
                                view.aggregate_filtered(window, cx);
                                cx.notify();
                            })),
                    )
                },
            )
            .when(self.mode == Mode::Objects && selected > 0, |menu| {
                menu.child(div().text_xs().child(format!("{selected} selected")))
                    .child(
                        Button::new("clear-selected")
                            .small()
                            .ghost()
                            .icon(IconName::Close)
                            .when(vertical, |button| button.label("Clear selection"))
                            .accessibility_label("Clear selection")
                            .tooltip("Clear selection")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.table.update(cx, |table, cx| {
                                    table.delegate_mut().clear_selected();
                                    cx.notify();
                                });
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("delete-selected")
                            .small()
                            .danger()
                            .icon(Icon::new(Glyph::Trash))
                            .label("Delete selected")
                            .disabled(!self.may_delete() || self.bulk_deleting)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.toolbar_open = false;
                                view.start_delete_selected(window, cx);
                            })),
                    )
            })
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
                })
                .into_any_element()
        })
    }

    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (shown, total) = self.counts(cx);
        let title = match self.mode {
            Mode::Objects => self
                .kind
                .as_ref()
                .map(|kind| kind.resource.kind.clone())
                .unwrap_or_else(|| "Nothing selected".into()),
            other => other.label().to_string(),
        };
        let count = match self.mode {
            Mode::Objects if self.table.read(cx).delegate().is_loading() => String::new(),
            Mode::Objects if shown == total => total.to_string(),
            Mode::Objects => format!("{shown} of {total}"),
            Mode::Releases => match &self.releases {
                Releases::Ready(items) => items.len().to_string(),
                _ => String::new(),
            },
            Mode::Forwards => self.session.forwards().len().to_string(),
        };
        let layout = self.toolbar_layout.get();
        let measured = self.toolbar_layout.clone();
        let entity = cx.entity_id();
        let fields = self
            .kind
            .as_ref()
            .map(|kind| Field::for_kind(kind).len())
            .unwrap_or(0);
        let selected =
            self.mode == Mode::Objects && self.table.read(cx).delegate().selected_count() > 0;
        h_flex()
            .id("resource-toolbar")
            .role(Role::Toolbar)
            .aria_label("Resource toolbar")
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
                let next = Layout::for_width(bounds.size.width, fields, selected);
                if measured.replace(next) != next {
                    cx.notify(entity);
                }
            })
            .child(
                h_flex()
                    .min_w_0()
                    .max_w(px(200.))
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .min_w_0()
                            .max_w(px(145.))
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_sm()
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .flex_shrink_0()
                            .whitespace_nowrap()
                            .text_color(cx.theme().muted_foreground)
                            .rounded_md()
                            .bg(cx.theme().muted)
                            .px_1p5()
                            .child(count),
                    ),
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
}
