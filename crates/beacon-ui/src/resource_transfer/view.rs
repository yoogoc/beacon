use super::*;
use crate::copyable_text::copyable_text;
fn label(text: &str, cx: &App) -> AnyElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(tr(text))
        .into_any_element()
}
fn notice(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    h_flex()
        .gap_2()
        .p_3()
        .rounded_md()
        .bg(cx.theme().muted.opacity(0.5))
        .text_sm()
        .child(gpui_kit::component::Icon::new(IconName::Info).size_4())
        .child(div().min_w_0().child(text.into()))
        .into_any_element()
}
impl TransferView {
    fn render_target(&self, cx: &mut Context<Self>) -> AnyElement {
        let locked = self.busy || self.has_targets();
        let source_namespaces = self
            .rows
            .iter()
            .filter(|row| !row.selection.copy_only)
            .filter_map(|row| row.selection.reference.namespace.as_deref())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        let source = v_flex()
            .gap_1()
            .p_3()
            .rounded_md()
            .bg(cx.theme().muted.opacity(0.5))
            .child(label("Source", cx))
            .child(context_label(self.source.id(), cx))
            .when(self.namespaced, |view| view.child(source_namespaces));
        let cluster = v_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .child(label("Target cluster", cx))
            .child(
                Select::new(&self.cluster)
                    .id("transfer-cluster")
                    .w_full()
                    .search_placeholder(tr("Search clusters"))
                    .accessibility_label(tr("Target cluster"))
                    .disabled(locked),
            );
        let namespace_label = h_flex()
            .justify_between()
            .child(label(
                if self.namespaced {
                    "Target namespace"
                } else {
                    "Resource scope"
                },
                cx,
            ))
            .when(self.namespaced, |view| {
                view.child(
                    Button::new("transfer-manual-namespace")
                        .xsmall()
                        .ghost()
                        .icon(IconName::Pencil)
                        .tooltip(tr("Enter namespace manually"))
                        .disabled(locked)
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.manual_namespace = !view.manual_namespace;
                            cx.notify();
                        })),
                )
            });
        let namespace_input = if !self.namespaced {
            div()
                .p_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(tr("Cluster-scoped resource"))
                .into_any_element()
        } else if self.manual_namespace {
            Input::new(&self.namespace)
                .id("transfer-namespace-input")
                .disabled(locked)
                .into_any_element()
        } else {
            Select::new(&self.namespaces)
                .id("transfer-namespace")
                .w_full()
                .search_placeholder(tr("Search namespaces"))
                .accessibility_label(tr("Target namespace"))
                .disabled(locked || self.loading)
                .into_any_element()
        };
        let namespace = v_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .child(namespace_label)
            .child(namespace_input);
        let rows = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| self.render_selection_row(index, row, locked, cx));
        v_flex().gap_4().child(source)
            .child(h_flex().gap_3().items_start().child(cluster).child(namespace))
            .when(self.loading, |view| view.child(label("Connecting or loading namespaces…", cx)))
            .when(!self.loading && self.destination(cx).is_none(), |view| view.child(
                Button::new("transfer-retry-connection").small().outline()
                    .label(tr("Retry connection")).disabled(locked)
                    .on_click(cx.listener(|view, _, window, cx| view.retry_connection(window, cx)))))
            .when_some(self.namespace_note.as_ref(), |view, note| view.child(notice(
                format!("{}\n{note}", tr("Could not list namespaces. Enter an existing namespace manually.")), cx)))
            .child(h_flex().justify_between()
                .child(tr("Resources and target names"))
                .child(div().text_xs().text_color(cx.theme().muted_foreground)
                    .child(message("transfer.selected_count", self.rows.iter().filter(|row| row.included).count()))))
            .child(v_flex().border_1().border_color(cx.theme().border).rounded_md().children(rows))
            .child(notice(tr(if self.moving {
                "Create and verify the destination first. Source deletion requires a separate confirmation."
            } else { "Copy keeps all source resources." }), cx))
            .into_any_element()
    }
    fn render_selection_row(
        &self,
        index: usize,
        row: &Row,
        locked: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let kind = format!(
            "{}{}",
            row.selection.kind.resource.kind,
            if row.selection.copy_only {
                format!(" · {}", tr("Copy only"))
            } else {
                String::new()
            }
        );
        h_flex()
            .id(("transfer-row", index))
            .test_support()
            .gap_3()
            .items_center()
            .p_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                crate::selection::checkbox(("transfer-include", index))
                    .checked(row.included)
                    .disabled(locked)
                    .on_click(cx.listener(move |view, checked: &bool, _, cx| {
                        view.rows[index].included = *checked;
                        view.invalidate(cx);
                    })),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(copyable_text(
                        ("transfer-source-name", index),
                        row.selection.reference.to_string(),
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(kind),
                    ),
            )
            .child(
                gpui_kit::component::Icon::new(IconName::ArrowRight)
                    .size_4()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&row.name)
                        .id(("transfer-target-name", index))
                        .small()
                        .disabled(locked || !row.included),
                ),
            )
            .child(
                Button::new(("transfer-rename", index))
                    .xsmall()
                    .ghost()
                    .icon(IconName::Pencil)
                    .tooltip(tr("Add a -copy suffix"))
                    .disabled(locked || !row.included)
                    .on_click(
                        cx.listener(move |view, _, window, cx| view.rename(index, window, cx)),
                    ),
            )
            .into_any_element()
    }
    fn render_review(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(batch) = &self.batch else {
            return div().into_any_element();
        };
        let items = batch.items().to_vec();
        let body = if self.yaml {
            let manifests = items
                .iter()
                .filter_map(|item| item.preview())
                .filter_map(|value| serde_saphyr::to_string(&value).ok())
                .collect::<Vec<_>>()
                .join("---\n");
            div()
                .id("transfer-manifest")
                .test_support()
                .font_family("monospace")
                .text_xs()
                .p_3()
                .rounded_md()
                .bg(cx.theme().muted.opacity(0.4))
                .child(copyable_text("transfer-manifest-text", manifests))
                .into_any_element()
        } else {
            v_flex().gap_3()
                .children(items.iter().enumerate().map(|(index, item)| self.render_check_item(index, item, cx)))
                .children(batch.dependencies().iter().enumerate().map(|(index, dependency)| self.render_dependency(index, dependency, cx)))
                .children(batch.warnings.iter().map(|warning| notice(tr(warning), cx)))
                .child(notice(tr("Server identity, status, ownership and generated addresses are removed from the copy. Volume data and external systems are not transferred."), cx))
                .into_any_element()
        };
        v_flex()
            .gap_3()
            .child(self.render_route(cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("transfer-review-summary")
                            .small()
                            .ghost()
                            .selected(!self.yaml)
                            .label(tr("Check summary"))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.yaml = false;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("transfer-review-yaml")
                            .small()
                            .ghost()
                            .selected(self.yaml)
                            .label(tr("Target YAML"))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.yaml = true;
                                cx.notify();
                            })),
                    ),
            )
            .when(
                self.rows.iter().any(|row| {
                    row.included
                        && row.selection.kind.resource.group.is_empty()
                        && row.selection.kind.resource.kind == "Secret"
                }),
                |v| {
                    v.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                crate::selection::checkbox("transfer-secret-consent")
                                    .checked(self.secret_consent)
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|view, checked: &bool, window, cx| {
                                        view.secret_consent = *checked;
                                        view.check(window, cx);
                                    })),
                            )
                            .child(tr("Allow sending Secret contents to this destination.")),
                    )
                },
            )
            .child(body)
            .into_any_element()
    }
    fn render_check_item(
        &self,
        index: usize,
        item: &beacon_kube::transfer::Item,
        cx: &Context<Self>,
    ) -> AnyElement {
        let good = item.problem.is_none();
        let row_index = self.rows.iter().position(|row| {
            row.selection.reference == item.selection().reference
                && row.selection.kind.gvk() == item.selection().kind.gvk()
        });
        let heading = h_flex()
            .gap_2()
            .items_center()
            .child(
                gpui_kit::component::Icon::new(if good {
                    IconName::CircleCheck
                } else {
                    IconName::CircleAlert
                })
                .size_4()
                .text_color(if good {
                    cx.theme().success
                } else {
                    cx.theme().warning
                }),
            )
            .child(copyable_text(
                ("transfer-reviewed-name", index),
                format!(
                    "{}/{}",
                    item.selection().kind.resource.kind,
                    item.selection().name
                ),
            ));
        v_flex()
            .id(("transfer-check", index))
            .test_support()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .child(heading)
            .when_some(item.problem.as_ref(), |view, error| {
                view.child(copyable_text(
                    ("transfer-check-error", index),
                    error_text(error),
                ))
            })
            .when(good, |view| {
                view.child(label(
                    "Target name, permissions and server dry-run passed.",
                    cx,
                ))
            })
            .when(item.owner.is_some(), |view| {
                view.child(
                    Button::new(("transfer-use-owner", index))
                        .small()
                        .outline()
                        .label(tr("Use owning workload"))
                        .disabled(self.busy)
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.use_owner(index, window, cx)
                        })),
                )
            })
            .when(item.conflict, |view| {
                view.child(h_flex().gap_2().when_some(row_index, |view, row| {
                    view.child(
                        Button::new(("transfer-conflict-rename", index))
                            .small()
                            .outline()
                            .label(tr("Rename"))
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(move |view, _, window, cx| {
                                    view.rename(row, window, cx)
                                }),
                            ),
                    )
                    .child(
                        Button::new(("transfer-conflict-skip", index))
                            .small()
                            .ghost()
                            .label(tr("Skip"))
                            .disabled(self.busy)
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.rows[row].included = false;
                                view.invalidate(cx);
                                view.check(window, cx);
                            })),
                    )
                }))
            })
            .into_any_element()
    }
    fn render_dependency(
        &self,
        index: usize,
        dependency: &beacon_kube::transfer::Dependency,
        cx: &Context<Self>,
    ) -> AnyElement {
        let description = crate::i18n::message(
            "transfer.dependency_missing",
            &[
                ("kind", dependency.kind.resource.kind.clone()),
                ("name", dependency.reference.to_string()),
            ],
        );
        v_flex()
            .gap_2()
            .p_3()
            .bg(cx.theme().warning.opacity(0.08))
            .rounded_md()
            .child(copyable_text(("transfer-dependency", index), description))
            .child(
                Button::new(("transfer-add-dependency", index))
                    .small()
                    .outline()
                    .label(tr("Copy this dependency too"))
                    .disabled(dependency.uid.is_none() || self.busy)
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.add_dependency(index, window, cx)
                    })),
            )
            .into_any_element()
    }
    fn render_route(&self, cx: &App) -> AnyElement {
        h_flex()
            .gap_3()
            .p_3()
            .rounded_md()
            .bg(cx.theme().muted.opacity(0.4))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(label("Source", cx))
                    .child(self.source.id().display_name().to_owned()),
            )
            .child(gpui_kit::component::Icon::new(IconName::ArrowRight).size_4())
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(label("Destination", cx))
                    .child(
                        self.selected_id(cx)
                            .map(|id| context_label(&id, cx))
                            .unwrap_or_default(),
                    )
                    .when(self.namespaced, |v| {
                        v.child(self.namespace.read(cx).value().clone())
                    }),
            )
            .into_any_element()
    }
    fn render_result(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(batch) = &self.batch else {
            return div().into_any_element();
        };
        let created = batch
            .items()
            .iter()
            .filter(|item| matches!(item.target, TargetState::Created { .. }))
            .count();
        let deleted = batch.items().iter().filter(|item| item.deleted).count();
        let confirmation = h_flex()
            .gap_2()
            .items_start()
            .child(
                crate::selection::checkbox("transfer-cleanup-confirmation")
                    .checked(self.cleanup_confirmed)
                    .disabled(self.busy || !batch.can_cleanup())
                    .on_click(cx.listener(|view, checked: &bool, _, cx| {
                        view.cleanup_confirmed = *checked;
                        cx.notify();
                    })),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .gap_1()
                    .child(tr(
                        "I checked the destination. Delete the reviewed source resources from:",
                    ))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.source_location()),
                    ),
            );
        v_flex().gap_3().child(self.render_route(cx))
            .child(h_flex().gap_4().child(message("transfer.created_count", created))
                .child(message("transfer.deleted_count", deleted)))
            .children(batch.items().iter().enumerate().map(|(index, item)| self.render_result_item(index, item, cx)))
            .when(self.moving && !self.ended && !batch.complete(), |view| view
                .child(notice(tr("No source cleanup starts until every target is verified. Deletions check the reviewed source UID and version; finalizers are never removed."), cx))
                .child(confirmation))
            .when(self.ended, |view| view.child(notice(tr("Transfer ended. Any remaining source resources were retained."), cx)))
            .child(notice(tr("Workload readiness checks do not verify application traffic or external dependencies."), cx))
            .into_any_element()
    }
    fn render_result_item(
        &self,
        index: usize,
        item: &beacon_kube::transfer::Item,
        cx: &Context<Self>,
    ) -> AnyElement {
        let target_status = if item.verified {
            "Target verified"
        } else if matches!(item.target, TargetState::Created { .. }) {
            "Target created; verification pending"
        } else {
            "Target not confirmed"
        };
        let source_status = if item.deleted {
            "Source deleted"
        } else if item.delete_requested {
            "Source deletion requested"
        } else if item.selection().copy_only {
            "Source retained · dependency"
        } else {
            "Source retained"
        };
        let heading = h_flex()
            .min_w_0()
            .gap_2()
            .child(div().flex_1().min_w_0().child(copyable_text(
                ("transfer-result-name", index),
                format!(
                    "{}/{}",
                    item.selection().kind.resource.kind,
                    item.selection().name
                ),
            )))
            .child(
                Button::new(("transfer-open-target", index))
                    .xsmall()
                    .ghost()
                    .icon(IconName::ExternalLink)
                    .tooltip(tr("View target resource"))
                    .disabled(!matches!(item.target, TargetState::Created { .. }) || self.busy)
                    .on_click(
                        cx.listener(move |view, _, window, cx| view.open_target(index, window, cx)),
                    ),
            );
        v_flex()
            .id(("transfer-result", index))
            .test_support()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .child(heading)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(item.selection().reference.to_string()),
            )
            .child(
                h_flex().gap_3().flex_wrap().child(tr(target_status)).child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child(tr(source_status)),
                ),
            )
            .when_some(item.problem.as_ref(), |view, error| {
                view.child(copyable_text(
                    ("transfer-result-error", index),
                    error_text(error),
                ))
            })
            .into_any_element()
    }
    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy;
        let mut bar = h_flex()
            .min_w_0()
            .flex_wrap()
            .gap_2()
            .items_center()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(0.25));
        if busy {
            return bar.child(tr("Operation in progress…")).into_any_element();
        }
        match self.step {
            Step::Target => {
                bar = bar
                    .child(div().flex_1())
                    .child(
                        Button::new("transfer-cancel")
                            .small()
                            .ghost()
                            .label(tr("Cancel"))
                            .on_click(|_, window, _| window.remove_window()),
                    )
                    .child(
                        Button::new("transfer-check-target")
                            .small()
                            .primary()
                            .label(tr("Check target"))
                            .disabled(self.loading || self.destination(cx).is_none())
                            .on_click(cx.listener(|view, _, window, cx| view.check(window, cx))),
                    );
            }
            Step::Review => {
                bar = bar
                    .child(
                        Button::new("transfer-back")
                            .small()
                            .ghost()
                            .label(tr("Back"))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.step = Step::Target;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("transfer-create-target")
                            .small()
                            .primary()
                            .label(tr(if self.moving {
                                "Create target copies"
                            } else {
                                "Copy resources"
                            }))
                            .disabled(!self.batch.as_ref().is_some_and(Batch::ready))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.execute(Execution::Create, window, cx)
                            })),
                    );
            }
            Step::Result => {
                let batch = self.batch.as_ref();
                let finished = self.ended
                    || batch.is_some_and(|b| {
                        if self.moving {
                            b.complete()
                        } else {
                            b.items().iter().all(|item| item.verified)
                        }
                    });
                if finished {
                    bar = bar.child(div().flex_1()).child(
                        Button::new("transfer-done")
                            .small()
                            .primary()
                            .label(tr("Done"))
                            .on_click(|_, window, _| window.remove_window()),
                    );
                } else {
                    bar = bar
                        .child(
                            Button::new("transfer-retry-create")
                                .small()
                                .outline()
                                .label(tr("Retry failed copies"))
                                .disabled(!batch.is_some_and(Batch::ready))
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.execute(Execution::Create, window, cx)
                                })),
                        )
                        .child(
                            Button::new("transfer-verify")
                                .small()
                                .outline()
                                .label(tr("Verify again"))
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.execute(Execution::Verify, window, cx)
                                })),
                        )
                        .child(
                            Button::new("transfer-keep-sources")
                                .small()
                                .ghost()
                                .label(tr("Keep remaining sources"))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.ended = true;
                                    cx.notify();
                                })),
                        );
                    if self.moving {
                        bar = bar.child(
                            Button::new("transfer-delete-sources")
                                .small()
                                .danger()
                                .label(message(
                                    "transfer.delete_sources_count",
                                    batch.map_or(0, |batch| {
                                        batch
                                            .items()
                                            .iter()
                                            .filter(|item| {
                                                !item.selection().copy_only && !item.deleted
                                            })
                                            .count()
                                    }),
                                ))
                                .disabled(
                                    !self.cleanup_confirmed
                                        || !batch.is_some_and(Batch::can_cleanup),
                                )
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.execute(Execution::Cleanup, window, cx)
                                })),
                        );
                    }
                }
            }
        }
        bar.into_any_element()
    }
}
impl Render for TransferView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locked = self.busy || self.has_targets();
        let modes = h_flex()
            .gap_1()
            .p_1()
            .rounded_md()
            .bg(cx.theme().muted)
            .child(
                Button::new("transfer-mode-copy")
                    .small()
                    .ghost()
                    .selected(!self.moving)
                    .toggled(!self.moving)
                    .icon(IconName::Copy)
                    .label(tr("Copy"))
                    .disabled(locked)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.moving = false;
                        view.invalidate(cx);
                    })),
            )
            .child(
                Button::new("transfer-mode-move")
                    .small()
                    .ghost()
                    .selected(self.moving)
                    .toggled(self.moving)
                    .icon(IconName::ArrowRightLeft)
                    .label(tr("Move"))
                    .disabled(locked)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.moving = true;
                        view.invalidate(cx);
                    })),
            );
        let heading = h_flex()
            .justify_between()
            .items_center()
            .gap_3()
            .child(div().text_lg().font_weight(FontWeight::MEDIUM).child(tr(
                if self.step == Step::Result {
                    "Transfer results"
                } else if self.moving {
                    "Move resources"
                } else {
                    "Copy resources"
                },
            )))
            .child(modes);
        let steps = h_flex().gap_3().py_2().text_xs().children(
            [
                ("Target and names", Step::Target),
                ("Check and preview", Step::Review),
                ("Results", Step::Result),
            ]
            .into_iter()
            .enumerate()
            .map(|(index, (name, step))| {
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .size_5()
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(self.step == step, |v| {
                                v.bg(cx.theme().primary)
                                    .text_color(cx.theme().primary_foreground)
                            })
                            .when(self.step != step, |v| v.bg(cx.theme().muted))
                            .child((index + 1).to_string()),
                    )
                    .child(tr(name))
            }),
        );
        let body = match self.step {
            Step::Target => self.render_target(cx),
            Step::Review => self.render_review(cx),
            Step::Result => self.render_result(cx),
        };
        let error = self.error.as_ref().map(|error| {
            div()
                .mb_3()
                .text_color(cx.theme().danger)
                .child(copyable_text("transfer-error", error_text(error)))
        });
        v_flex()
            .id("resource-transfer-window")
            .test_support()
            .size_full()
            .min_size_0()
            .bg(cx.theme().background)
            .child(TitleBar::new().child(tr("Resource transfer")))
            .child(
                v_flex()
                    .px_4()
                    .pt_4()
                    .pb_3()
                    .gap_2()
                    .child(heading)
                    .child(steps),
            )
            .child(
                div()
                    .id("transfer-scroll")
                    .flex_1()
                    .min_size_0()
                    .overflow_y_scrollbar()
                    .px_4()
                    .pb_4()
                    .children(error)
                    .child(body),
            )
            .child(self.render_footer(cx))
    }
}
