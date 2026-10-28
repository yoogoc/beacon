//! Two presentations of the same structured container configuration.
use super::*;
use gpui_kit::component::Icon;
mod model;
pub(super) use model::Configuration;
use model::{Category, image_parts};

#[derive(Default)]
pub(super) struct Browser {
    tabs: BTreeMap<String, Category>,
}

impl DetailView {
    pub(super) fn container_section(
        &self,
        containers: Vec<ContainerLine>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let layout = crate::settings::try_store(cx)
            .map(|settings| settings.read(cx).preferences.container_layout)
            .unwrap_or_default();
        v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(self.heading(
                        if self.kind.resource.kind == "Pod" {
                            "Containers"
                        } else {
                            "Pod template"
                        },
                        cx,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(containers.len().to_string()),
                    ),
            )
            .children(
                containers
                    .iter()
                    .enumerate()
                    .map(|(index, container)| self.container_card(container, index, layout, cx)),
            )
            .into_any_element()
    }

    fn container_card(
        &self,
        container: &ContainerLine,
        index: usize,
        layout: crate::settings::ContainerLayout,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = format!("container:{}{}", container.category, container.name);
        let default_open = index == 0 && container.category.is_empty();
        let expanded = self.expanded_sections.contains(&key) != default_open;
        let tone = if container.ready {
            Tone::Healthy
        } else {
            crate::status::tone(&container.state)
        };
        let toggle_key = key.clone();
        v_flex()
            .id(SharedString::from(key.clone()))
            .test_support()
            .aria_label(format!("{}{}", container.category, container.name))
            .w_full()
            .min_w_0()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .p_3()
                    .child(
                        Button::new("toggle-container")
                            .ghost()
                            .xsmall()
                            .toggled(expanded)
                            .icon(if expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .tooltip(crate::i18n::text(if expanded {
                                "Collapse container"
                            } else {
                                "Expand container"
                            }))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.toggle_container_section(&toggle_key, cx);
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(SelectableText::new(
                                "name",
                                format!(
                                    "{}{}",
                                    crate::i18n::text(container.category),
                                    container.name
                                ),
                            )),
                    )
                    .when(container.state != "Template", |this| {
                        this.child(
                            h_flex()
                                .items_center()
                                .gap_1()
                                .max_w(px(230.))
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .text_xs()
                                .bg(cx.theme().tone_surface(tone))
                                .text_color(cx.theme().tone(tone))
                                .when(container.ready, |this| {
                                    this.child(Icon::new(IconName::CircleCheck).size_3())
                                })
                                .child(SelectableText::new(
                                    "state",
                                    crate::i18n::text(&container.state),
                                )),
                        )
                    })
                    .when(container.restarts > 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(SelectableText::new(
                                    "restarts",
                                    crate::i18n::message(
                                        "messages.restarts",
                                        &[("value0", container.restarts.to_string())],
                                    )
                                    .to_string(),
                                )),
                        )
                    })
                    .when(container.stop_debug, |this| {
                        let name = container.name.clone();
                        this.child(
                            Button::new("stop-debug-container")
                                .xsmall()
                                .ghost()
                                .icon(IconName::CircleX)
                                .label(crate::i18n::text("Stop debug container"))
                                .disabled(!crate::actions::may_exec(self.rules.as_deref()))
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    crate::debug_stop::open(
                                        view.session.clone(),
                                        view.object.clone(),
                                        name.clone(),
                                        window,
                                        cx,
                                    );
                                })),
                        )
                    }),
            )
            .when(expanded, |this| {
                let (repository, version) = image_parts(&container.image);
                let mut body =
                    v_flex().w_full().min_w_0().px_3().pb_2().child(
                        h_flex()
                            .items_start()
                            .gap_2()
                            .pb_4()
                            .child(
                                Icon::new(IconName::Box)
                                    .size_4()
                                    .mt_0p5()
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(div().text_sm().whitespace_normal().child(
                                        copyable_text("container-image", repository.to_owned()),
                                    ))
                                    .when_some(version, |this, version| {
                                        this.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .whitespace_normal()
                                                .child(copyable_text(
                                                    "container-image-version",
                                                    version.to_owned(),
                                                )),
                                        )
                                    }),
                            ),
                    );
                match layout {
                    crate::settings::ContainerLayout::Workbench => {
                        let tab = self
                            .container_browser
                            .tabs
                            .get(&key)
                            .copied()
                            .unwrap_or_default();
                        body = body
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .flex_wrap()
                                    .gap_1()
                                    .p_1()
                                    .rounded_md()
                                    .bg(cx.theme().muted.opacity(0.5))
                                    .children(Category::ALL.into_iter().map(|category| {
                                        let key = key.clone();
                                        Button::new(category.id())
                                            .ghost()
                                            .small()
                                            .flex_1()
                                            .flex_basis(px(110.))
                                            .min_w_0()
                                            .selected(tab == category)
                                            .label(crate::i18n::text(category.label()))
                                            .on_click(cx.listener(move |view, _, _, cx| {
                                                view.container_browser
                                                    .tabs
                                                    .insert(key.clone(), category);
                                                cx.notify();
                                            }))
                                    })),
                            )
                            .child(
                                div()
                                    .id("container-workbench-panel")
                                    .test_support()
                                    .aria_label(crate::i18n::text(tab.label()))
                                    .w_full()
                                    .min_w_0()
                                    .pt_3()
                                    .child(self.container_category(container, tab, &key, cx)),
                            );
                    }
                    crate::settings::ContainerLayout::Grouped => {
                        body = body
                            .child(self.container_category(container, Category::Ports, &key, cx))
                            .child(self.container_category(container, Category::Health, &key, cx))
                            .child(self.container_fold(
                                "Environment variables",
                                container.configuration.environment.rows.len(),
                                &format!("{key}:environment"),
                                |cx| {
                                    self.container_category(
                                        container,
                                        Category::Environment,
                                        &key,
                                        cx,
                                    )
                                },
                                cx,
                            ))
                            .child(self.container_fold(
                                "Volume mounts",
                                container.configuration.mounts.rows.len(),
                                &format!("{key}:mounts"),
                                |cx| self.container_category(container, Category::Mounts, &key, cx),
                                cx,
                            ));
                    }
                }
                if !container.configuration.extra.is_empty() {
                    body = body.child(self.container_fold(
                        "Additional settings",
                        container.configuration.extra.len(),
                        &format!("{key}:additional"),
                        |cx| self.field_rows(&container.configuration.extra, cx),
                        cx,
                    ));
                }
                if !container.status_fields.is_empty() {
                    body = body.child(self.container_fold(
                        "Runtime details",
                        0,
                        &format!("{key}:runtime"),
                        |cx| self.field_rows(&container.status_fields, cx),
                        cx,
                    ));
                }
                this.child(body)
            })
            .into_any_element()
    }

    fn toggle_container_section(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.expanded_sections.remove(key) {
            self.expanded_sections.insert(key.to_owned());
        }
        cx.notify();
    }

    fn container_fold(
        &self,
        label: &'static str,
        count: usize,
        key: &str,
        content: impl FnOnce(&mut Context<Self>) -> AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self.expanded_sections.contains(key);
        let key = key.to_owned();
        v_flex()
            .id(SharedString::from(key.clone()))
            .test_support()
            .aria_label(crate::i18n::text(label))
            .w_full()
            .min_w_0()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                Button::new("toggle-container-group")
                    .ghost()
                    .small()
                    .w_full()
                    .h_auto()
                    .py_3()
                    .accessibility_label(crate::i18n::text(label))
                    .selected(expanded)
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .text_left()
                                    .text_sm()
                                    .child(crate::i18n::text(label)),
                            )
                            .when(count > 0, |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(count.to_string()),
                                )
                            })
                            .child(
                                Icon::new(if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size_3()
                                .text_color(cx.theme().muted_foreground),
                            ),
                    )
                    .on_click(
                        cx.listener(move |view, _, _, cx| view.toggle_container_section(&key, cx)),
                    ),
            )
            .when(expanded, |this| {
                this.child(div().w_full().min_w_0().pb_3().child(content(cx)))
            })
            .into_any_element()
    }

    fn container_category(
        &self,
        container: &ContainerLine,
        category: Category,
        key: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let configuration = &container.configuration;
        match category {
            Category::Ports => v_flex()
                .w_full()
                .min_w_0()
                .child(self.container_table_section(
                    Some("Ports"),
                    &configuration.ports,
                    "No container ports declared",
                    cx,
                ))
                .child(self.container_table_section(
                    Some("Resource requests & limits"),
                    &container.resources,
                    "Not configured",
                    cx,
                ))
                .into_any_element(),
            Category::Health => {
                let mut section = v_flex()
                    .w_full()
                    .min_w_0()
                    .py_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(self.container_label("Health checks", configuration.probes.len(), cx));
                if configuration.probes.is_empty() {
                    section =
                        section.child(self.container_empty("No health checks configured", cx));
                }
                for (index, probe) in configuration.probes.iter().enumerate() {
                    section = section.child(
                        v_flex()
                            .id(("container-probe", index))
                            .w_full()
                            .min_w_0()
                            .py_2()
                            .gap_1()
                            .when(index > 0, |this| {
                                this.border_t_1().border_color(cx.theme().border)
                            })
                            .child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .flex_wrap()
                                    .items_start()
                                    .gap_2()
                                    .child(
                                        div()
                                            .w(px(112.))
                                            .flex_shrink_0()
                                            .text_sm()
                                            .child(crate::i18n::text(probe.title)),
                                    )
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_1()
                                            .child(div().text_sm().whitespace_normal().child(
                                                copyable_text("probe-target", probe.target.clone()),
                                            ))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(crate::i18n::message(
                                                        "containers.probe_timing",
                                                        &[
                                                            ("period", probe.period.to_string()),
                                                            ("timeout", probe.timeout.to_string()),
                                                        ],
                                                    )),
                                            )
                                            .when_some(probe.port.as_ref(), |this, port| {
                                                this.child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(copyable_text(
                                                            "probe-port",
                                                            port.clone(),
                                                        )),
                                                )
                                            }),
                                    ),
                            )
                            .child(self.container_fold(
                                "Thresholds & timing",
                                0,
                                &format!("{key}:probe:{index}"),
                                |cx| self.field_rows(&probe.details, cx),
                                cx,
                            )),
                    );
                }
                section.into_any_element()
            }
            Category::Environment => v_flex()
                .w_full()
                .min_w_0()
                .when(
                    !configuration.environment.rows.is_empty()
                        || configuration.environment_sources.rows.is_empty(),
                    |this| {
                        this.child(self.container_table_section(
                            None,
                            &configuration.environment,
                            "No environment variables declared",
                            cx,
                        ))
                    },
                )
                .when(!configuration.environment_sources.rows.is_empty(), |this| {
                    this.child(self.container_table_section(
                        Some("Environment sources"),
                        &configuration.environment_sources,
                        "Not configured",
                        cx,
                    ))
                })
                .into_any_element(),
            Category::Mounts => {
                self.container_table_section(None, &configuration.mounts, "No volumes mounted", cx)
            }
        }
    }

    fn container_label(&self, label: &'static str, count: usize, cx: &App) -> AnyElement {
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .mb_2()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .child(crate::i18n::text(label))
            .when(count > 0, |this| {
                this.child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::NORMAL)
                        .text_color(cx.theme().muted_foreground)
                        .child(count.to_string()),
                )
            })
            .into_any_element()
    }

    fn container_empty(&self, label: &'static str, cx: &App) -> AnyElement {
        div()
            .py_1()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(crate::i18n::text(label))
            .into_any_element()
    }

    fn container_table_section(
        &self,
        title: Option<&'static str>,
        table: &crate::overview::Table,
        empty: &'static str,
        cx: &App,
    ) -> AnyElement {
        let translate_access = table.headers.get(2) == Some(&"Access");
        v_flex()
            .w_full()
            .min_w_0()
            .when_some(title, |this, title| {
                this.py_3()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(self.container_label(title, table.rows.len(), cx))
            })
            .when(table.rows.is_empty(), |this| {
                this.child(self.container_empty(empty, cx))
            })
            .when(!table.rows.is_empty(), |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .rounded_sm()
                        .bg(cx.theme().muted.opacity(0.5))
                        .children(table.headers.iter().map(|header| {
                            div()
                                .flex_1()
                                .min_w_0()
                                .px_2()
                                .py_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::i18n::text(header))
                        })),
                )
                .children(table.rows.iter().enumerate().map(|(index, row)| {
                    h_flex()
                        .id(("container-row", index))
                        .w_full()
                        .min_w_0()
                        .items_start()
                        .when(index > 0, |this| {
                            this.border_t_1().border_color(cx.theme().border)
                        })
                        .children(row.iter().enumerate().map(|(index, cell)| {
                            let value = cell.value.as_deref().unwrap_or("—");
                            let value = if (translate_access && index == 2)
                                || (table.headers.first() == Some(&"Resource") && index == 0)
                            {
                                crate::i18n::text(value)
                            } else {
                                value.to_owned().into()
                            };
                            div()
                                .id(("container-cell", index))
                                .flex_1()
                                .min_w_0()
                                .px_2()
                                .py_2()
                                .text_sm()
                                .whitespace_normal()
                                .child(copyable_text("value", value))
                        }))
                }))
            })
            .into_any_element()
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests;
