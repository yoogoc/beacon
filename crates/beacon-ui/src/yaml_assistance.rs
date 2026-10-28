//! Schema assistance shared by existing-resource and creation YAML editors.
use crate::scroll::OverflowScrollbars as _;
use crate::{
    bridge::Bridge,
    copyable_text::copyable_text,
    yaml_language::{self, Problem, Provider},
};
use beacon_kube::{ApiResource, ClusterSession, schema::Schema};
use gpui_kit::assets::IconName;
use gpui_kit::base::{
    TestSupportExt as _,
    input::{Diagnostic, DiagnosticSeverity},
};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{EditorState, InputEvent},
    menu::{DropdownMenu as _, PopupMenuItem},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::{rc::Rc, sync::Arc, time::Duration};

enum Status {
    Loading,
    Ready(Arc<Schema>),
    Unavailable(String),
}
pub(crate) struct YamlAssistance {
    session: Arc<ClusterSession>,
    resource: ApiResource,
    editor: Entity<EditorState>,
    status: Status,
    problems: Vec<Problem>,
    checking: bool,
    validation_enabled: bool,
    expanded: bool,
    wrap: bool,
    show_schema: bool,
    help: Option<String>,
    _load: Option<Task<()>>,
    _analysis: Option<Task<()>>,
    _changes: Subscription,
}
impl YamlAssistance {
    pub fn new(
        session: Arc<ClusterSession>,
        resource: ApiResource,
        editor: Entity<EditorState>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let changes = cx.subscribe_in(
                &editor,
                window,
                |view: &mut Self, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        view.help = None;
                        view.schedule(window, cx);
                    }
                },
            );
            let mut view = Self {
                session,
                resource,
                editor,
                status: Status::Loading,
                problems: vec![],
                checking: false,
                validation_enabled: false,
                expanded: false,
                wrap: false,
                show_schema: false,
                help: None,
                _load: None,
                _analysis: None,
                _changes: changes,
            };
            view.load(false, window, cx);
            view
        })
    }
    fn toggle_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.wrap = !self.wrap;
        self.editor.update(cx, |editor, cx| {
            editor.set_soft_wrap(self.wrap, window, cx);
        });
        cx.notify();
    }
    fn field_help(&mut self, cx: &mut Context<Self>) {
        if let Status::Ready(schema) = &self.status {
            let editor = self.editor.read(cx);
            let text = editor.value();
            self.help =
                yaml_language::hover(schema, &text, editor.selected_range().end.saturating_sub(1))
                    .and_then(|hover| match hover.contents {
                        lsp_types::HoverContents::Markup(content) => Some(content.value),
                        _ => None,
                    })
                    .or_else(|| {
                        Some(
                            "Place the caret on a field or value to see its schema description."
                                .into(),
                        )
                    });
            cx.notify();
        }
    }
    fn clear_analysis(&mut self, cx: &mut Context<Self>) {
        self._analysis = None;
        self.checking = false;
        self.problems.clear();
        self.expanded = false;
        self.editor.update(cx, |editor, cx| {
            let text = editor.text().clone();
            if let Some(diagnostics) = editor.diagnostics_mut() {
                diagnostics.reset(&text);
            }
            cx.notify();
        });
    }
    fn toggle_validation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.validation_enabled = !self.validation_enabled;
        if self.validation_enabled {
            self.schedule(window, cx);
        } else {
            self.clear_analysis(cx);
        }
        cx.notify();
    }
    fn load(&mut self, reload: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_analysis(cx);
        self.status = Status::Loading;
        let session = self.session.clone();
        let resource = self.resource.clone();
        let loading = Bridge::global(cx)
            .run_cancellable(async move { session.schema(&resource, reload).await });
        self._load = Some(cx.spawn_in(window, async move |view, cx| {
            let result = loading.result().await;
            let _ = view.update_in(cx, |view, window, cx| {
                match result {
                    Ok(Ok(schema)) => {
                        view.install(schema, window, cx);
                    }
                    Ok(Err(error)) => view.status = Status::Unavailable(error),
                    Err(error) => view.status = Status::Unavailable(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn install(&mut self, schema: Arc<Schema>, window: &mut Window, cx: &mut Context<Self>) {
        let provider = Rc::new(Provider(schema.clone()));
        self.editor.update(cx, |editor, cx| {
            editor.lsp_mut().completion_provider = Some(provider.clone());
            editor.lsp_mut().hover_provider = Some(provider);
            editor.refresh(cx);
        });
        self.status = Status::Ready(schema);
        self.schedule(window, cx);
    }
    fn schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.validation_enabled {
            return;
        }
        let Status::Ready(schema) = &self.status else {
            return;
        };
        let schema = schema.clone();
        let text = self.editor.read(cx).value().to_string();
        self.checking = true;
        self._analysis = Some(cx.spawn_in(window, async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let draft = text.clone();
            let problems = cx
                .background_executor()
                .spawn(async move { yaml_language::analyze(&schema, &draft) })
                .await;
            let _ = view.update(cx, |view, cx| {
                if !view.validation_enabled || view.editor.read(cx).value().as_ref() != text {
                    return;
                }
                view.editor.update(cx, |editor, cx| {
                    let rope = editor.text().clone();
                    if let Some(diagnostics) = editor.diagnostics_mut() {
                        diagnostics.reset(&rope);
                        diagnostics.extend(problems.iter().map(|problem| {
                            Diagnostic::new(
                                yaml_language::position(&text, problem.range.start)
                                    ..yaml_language::position(&text, problem.range.end),
                                problem.issue.message.clone(),
                            )
                            .with_severity(if problem.syntax_error {
                                DiagnosticSeverity::Error
                            } else {
                                DiagnosticSeverity::Warning
                            })
                            .with_source("Local YAML check")
                        }));
                    }
                    cx.notify();
                });
                view.problems = problems;
                view.checking = false;
                cx.notify();
            });
        }));
        cx.notify();
    }
}
impl Render for YamlAssistance {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = matches!(self.status, Status::Ready(_));
        let (summary, description, icon, color) = match &self.status {
            Status::Loading => (
                "Loading schema…".into(),
                "Loading cluster schema…".into(),
                IconName::RefreshCw,
                cx.theme().muted_foreground,
            ),
            Status::Unavailable(error) => (
                "Schema unavailable".into(),
                crate::i18n::message(
                    "messages.server_validation_remains_available_error",
                    &[("error", (error).to_string())],
                )
                .to_string(),
                IconName::Info,
                cx.theme().muted_foreground,
            ),
            Status::Ready(schema) => {
                let syntax_error = self.problems.iter().any(|problem| problem.syntax_error);
                let summary: String = if !self.validation_enabled {
                    "Schema ready".into()
                } else if self.checking {
                    "Checking YAML…".into()
                } else if syntax_error {
                    "YAML syntax error".into()
                } else if self.problems.is_empty() {
                    "Local checks passed".into()
                } else {
                    crate::i18n::message(
                        "messages.local_warnings",
                        &[("value0", (self.problems.len()).to_string())],
                    )
                    .to_string()
                };
                let description = format!(
                    "{} · {} · {}",
                    schema.source,
                    schema.version,
                    if self.validation_enabled {
                        crate::i18n::message(
                            "messages.summary_advisory_only_the_api_server_validates_saves",
                            &[("summary", crate::i18n::text(&summary).to_string())],
                        )
                        .to_string()
                    } else {
                        crate::i18n::text(
                            "Local validation is off. Field completion and help remain available.",
                        )
                        .to_string()
                    }
                );
                let color =
                    if self.validation_enabled && !self.checking && !self.problems.is_empty() {
                        if syntax_error {
                            cx.theme().danger
                        } else {
                            cx.theme().warning
                        }
                    } else {
                        cx.theme().muted_foreground
                    };
                (
                    summary,
                    description,
                    if self.validation_enabled && !self.checking && self.problems.is_empty() {
                        IconName::CircleCheck
                    } else {
                        IconName::Info
                    },
                    color,
                )
            }
        };
        let tooltip = description.clone();
        let owner = cx.weak_entity();
        let show_schema = self.show_schema;
        let validation_enabled = self.validation_enabled;
        v_flex()
            .id("yaml-assistance")
            .test_support()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .text_xs()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .id("yaml-toolbar")
                    .test_support()
                    .w_full()
                    .h(px(36.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .child(
                        h_flex()
                            .id("yaml-schema-status")
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .gap_1p5()
                            .text_color(color)
                            .tooltip(move |window, cx| {
                                Tooltip::new(tooltip.clone()).build(window, cx)
                            })
                            .child(Icon::new(icon).size(px(13.)).flex_shrink_0())
                            .child(
                                div()
                                    .min_w_0()
                                    .text_ellipsis()
                                    .child(crate::i18n::text(summary)),
                            ),
                    )
                    .child(
                        Button::new("yaml-wrap-lines")
                            .xsmall()
                            .ghost()
                            .selected(self.wrap)
                            .label(crate::i18n::text("Wrap lines"))
                            .tooltip(crate::i18n::text(
                                "Wrap long lines to fit the editor. When off, scroll horizontally.",
                            ))
                            .on_click(
                                cx.listener(|view, _, window, cx| view.toggle_wrap(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("yaml-tools")
                            .xsmall()
                            .ghost()
                            .icon(IconName::Ellipsis)
                            .accessibility_label(crate::i18n::text("YAML tools"))
                            .tooltip(crate::i18n::text("YAML tools"))
                            .dropdown_menu(move |menu, _, _| {
                                let suggest = owner.clone();
                                let help = owner.clone();
                                let reload = owner.clone();
                                let details = owner.clone();
                                let validation = owner.clone();
                                menu.item(
                                    PopupMenuItem::new(crate::i18n::text("Suggest fields"))
                                        .disabled(!ready)
                                        .on_click(move |_, window, cx| {
                                            let _ = suggest.update(cx, |view, cx| {
                                                view.editor
                                                    .read(cx)
                                                    .focus_handle(cx)
                                                    .focus(window, cx);
                                                view.editor.update(cx, |editor, cx| {
                                                    editor.show_completions(window, cx)
                                                })
                                            });
                                        }),
                                )
                                .item(
                                    PopupMenuItem::new(crate::i18n::text("Field help"))
                                        .disabled(!ready)
                                        .on_click(move |_, _, cx| {
                                            let _ = help.update(cx, |view, cx| view.field_help(cx));
                                        }),
                                )
                                .item(
                                    PopupMenuItem::new(crate::i18n::text(if ready {
                                        "Reload schema"
                                    } else {
                                        "Retry schema"
                                    }))
                                    .on_click(
                                        move |_, window, cx| {
                                            let _ = reload
                                                .update(cx, |view, cx| view.load(true, window, cx));
                                        },
                                    ),
                                )
                                .separator()
                                .item(
                                    PopupMenuItem::new(crate::i18n::text("Local validation"))
                                        .checked(validation_enabled)
                                        .on_click(move |_, window, cx| {
                                            let _ = validation.update(cx, |view, cx| {
                                                view.toggle_validation(window, cx)
                                            });
                                        }),
                                )
                                .item(
                                    PopupMenuItem::new(crate::i18n::text("Schema details"))
                                        .checked(show_schema)
                                        .on_click(move |_, _, cx| {
                                            let _ = details.update(cx, |view, cx| {
                                                view.show_schema = !view.show_schema;
                                                cx.notify();
                                            });
                                        }),
                                )
                            }),
                    ),
            )
            .when(self.show_schema, |panel| {
                panel.child(
                    div()
                        .px_3()
                        .pb_2()
                        .text_color(cx.theme().muted_foreground)
                        .child(copyable_text("yaml-schema-details", description)),
                )
            })
            .when(!self.problems.is_empty(), |panel| {
                panel.child(
                    v_flex()
                        .id("yaml-schema-problems")
                        .test_support()
                        .max_h(px(160.))
                        .overflow_y_scrollbar()
                        .px_3()
                        .pb_1()
                        .children(
                            self.problems
                                .iter()
                                .take(if self.expanded { 200 } else { 3 })
                                .enumerate()
                                .map(|(index, problem)| {
                                    let editor = self.editor.clone();
                                    let position = yaml_language::position(
                                        &self.editor.read(cx).value(),
                                        problem.range.start,
                                    );
                                    h_flex()
                                        .items_start()
                                        .gap_2()
                                        .child(
                                            Button::new(("yaml-schema-error", index))
                                                .xsmall()
                                                .ghost()
                                                .label(
                                                    crate::i18n::message(
                                                        "messages.line",
                                                        &[(
                                                            "value0",
                                                            (position.line + 1).to_string(),
                                                        )],
                                                    )
                                                    .to_string(),
                                                )
                                                .on_click(move |_, window, cx| {
                                                    editor.update(cx, |editor, cx| {
                                                        editor.unfold_at(position, cx);
                                                        editor.set_cursor_position(
                                                            position, window, cx,
                                                        );
                                                    })
                                                }),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .text_color(if problem.syntax_error {
                                                    cx.theme().danger
                                                } else {
                                                    cx.theme().warning
                                                })
                                                .child(copyable_text(
                                                    ("yaml-schema-error-message", index),
                                                    format!(
                                                        "{}{}",
                                                        problem.issue.pointer,
                                                        if problem.issue.pointer.is_empty() {
                                                            problem.issue.message.clone()
                                                        } else {
                                                            format!(" · {}", problem.issue.message)
                                                        }
                                                    ),
                                                )),
                                        )
                                }),
                        )
                        .when(self.problems.len() > 3, |panel| {
                            panel.child(
                                Button::new("yaml-schema-more-errors")
                                    .xsmall()
                                    .ghost()
                                    .label(if self.expanded {
                                        "Show fewer messages".into()
                                    } else {
                                        crate::i18n::message(
                                            "messages.show_all_messages",
                                            &[("value0", (self.problems.len()).to_string())],
                                        )
                                        .to_string()
                                    })
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.expanded = !view.expanded;
                                        cx.notify();
                                    })),
                            )
                        }),
                )
            })
            .children(self.help.as_ref().map(|help| {
                v_flex()
                    .id("yaml-field-documentation")
                    .test_support()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .max_h(px(160.))
                    .overflow_y_scrollbar()
                    .bg(cx.theme().muted.opacity(0.3))
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(crate::i18n::text("Field help")),
                            )
                            .child(
                                Button::new("yaml-close-help")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::X)
                                    .accessibility_label(crate::i18n::text("Close field help"))
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.help = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(copyable_text("yaml-field-help-text", help.clone()))
            }))
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::component::input::Editor;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    struct Pane {
        editor: Entity<EditorState>,
        assistance: Entity<YamlAssistance>,
    }
    impl Render for Pane {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            v_flex()
                .size_full()
                .child(self.assistance.clone())
                .child(Editor::new(&self.editor).h(relative(1.)).bordered(false))
        }
    }
    #[::core::prelude::v1::test]
    fn native_schema_hints_inline_errors_and_navigation_preserve_the_draft_and_undo() {
        let cx = &mut support::context();
        let (_fixture, session) = support::fixture(cx, "schema-ui", vec![]);
        let resource = support::kind("apps", "Deployment", "deployments").resource;
        let (window, pane) = cx.update(|cx| {
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(640.), px(760.)),
                    ))),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    window.set_view_retention(false);
                    let editor = cx.new(|cx| {
                        EditorState::new(window, cx)
                            .language("yaml")
                            .default_value("spec:\n  replicas: wrong\n  mode: Other\n")
                    });
                    let assistance =
                        YamlAssistance::new(session, resource, editor.clone(), window, cx);
                    cx.new(|_| Pane { editor, assistance })
                },
            )
            .unwrap()
        });
        let assistance = pane.read_with(cx, |pane, _| pane.assistance.clone());
        support::settle(cx, |cx| {
            assistance.read_with(cx, |view, _| matches!(view.status, Status::Unavailable(_)))
        });
        let schema = Arc::new(Schema {
            root: json!({"type":"object","properties":{"spec":{"type":"object","required":["replicas"],"properties":{"replicas":{"type":"integer","description":"Desired replicas."},"mode":{"type":"string","enum":["Rolling","Recreate"]}}}}}),
            definitions: Default::default(),
            source: "Fixture OpenAPI v3".into(),
            version: "v1.35.0".into(),
        });
        cx.update_window(window, |_, window, cx| {
            assistance.update(cx, |view, cx| view.install(schema, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(250));
        support::settle(cx, |cx| assistance.read_with(cx, |view, _| !view.checking));
        assert!(assistance.read_with(cx, |view, _| {
            !view.validation_enabled && view.problems.is_empty() && view._analysis.is_none()
        }));
        let editor = pane.read_with(cx, |pane, _| pane.editor.clone());
        assert!(
            editor.read_with(cx, |state, _| state.lsp().completion_provider.is_some()
                && state.lsp().hover_provider.is_some())
        );
        assert_eq!(
            editor.read_with(cx, |state, _| state.value().to_string()),
            "spec:\n  replicas: wrong\n  mode: Other\n"
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("yaml-schema-problems").is_none());
            window.click("yaml-tools", cx);
            window.within("popup-menu").click(4usize, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(250));
        support::settle(cx, |cx| assistance.read_with(cx, |view, _| !view.checking));
        assert_eq!(assistance.read_with(cx, |view, _| view.problems.len()), 2);
        assert!(editor.read_with(cx, |state, _| {
            let diagnostics = state.diagnostics().unwrap();
            diagnostics.len() == 2
                && diagnostics
                    .iter()
                    .all(|entry| entry.diagnostic.severity == DiagnosticSeverity::Warning)
        }));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("yaml-toolbar").bounds().size.height, px(36.));
            assert!(window.find("yaml-tools").bounds().right() <= px(640.));
            assert!(window.try_find("yaml-schema-details").is_none());
            assert!(window.find("yaml-schema-problems").visible());
            window.click(("yaml-schema-error", 0usize), cx);
        })
        .unwrap();
        assert_eq!(
            editor.read_with(cx, |state, _| state.cursor_position().line),
            1
        );
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |state, cx| {
                state.set_cursor_position(lsp_types::Position::new(1, 3), window, cx)
            });
            window.render_frame(cx);
            window.click("yaml-tools", cx);
            window.within("popup-menu").click(1usize, cx);
        })
        .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("yaml-field-documentation").visible());
        })
        .unwrap();
        assert!(assistance.read_with(cx, |view, _| {
            view.help.as_ref().unwrap().contains("Desired replicas")
        }));
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |state, cx| state.focus_handle(cx).focus(window, cx));
            window.press("cmd-a", cx);
            cx.write_to_clipboard(ClipboardItem::new_string(
                "spec:\n  replicas: 2\n  mode: Rolling\n".into(),
            ));
            window.press(
                if cfg!(target_os = "macos") {
                    "cmd-v"
                } else {
                    "ctrl-v"
                },
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(250));
        support::settle(cx, |cx| assistance.read_with(cx, |view, _| !view.checking));
        assert!(assistance.read_with(cx, |view, _| view.problems.is_empty()));
        cx.update_window(window, |_, window, cx| window.press("cmd-z", cx))
            .unwrap();
        assert!(editor.read_with(cx, |state, _| state.value().contains("wrong")));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(250));
        support::settle(cx, |cx| assistance.read_with(cx, |view, _| !view.checking));
        assert_eq!(assistance.read_with(cx, |view, _| view.problems.len()), 2);
        cx.update_window(window, |_, window, cx| {
            // A pending pass must not restore warnings after disabling checks.
            assistance.update(cx, |view, cx| view.schedule(window, cx));
            window.click("yaml-tools", cx);
            window.within("popup-menu").click(4usize, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(250));
        assert!(assistance.read_with(cx, |view, _| {
            !view.validation_enabled
                && !view.checking
                && view.problems.is_empty()
                && view._analysis.is_none()
        }));
        assert!(editor.read_with(cx, |state, _| state.diagnostics().unwrap().is_empty()));
        assert!(editor.read_with(cx, |state, _| state.value().contains("wrong")));
        cx.update_window(window, |_, window, cx| {
            editor.update(cx, |state, cx| {
                state.set_value("spec:\n  re", window, cx);
                state.set_cursor_position(lsp_types::Position::new(1, 4), window, cx);
            });
            window.render_frame(cx);
            window.click("yaml-tools", cx);
            window.within("popup-menu").click(0usize, cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(editor.read_with(cx, |state, _| state.completion_menu_state().open));
        assert!(assistance.read_with(cx, |view, _| view._analysis.is_none()));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            editor.read_with(cx, |state, _| state.value().to_string()),
            "spec:\n  replicas: "
        );
    }
}
