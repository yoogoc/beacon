//! A separate operation window keeps the editable draft, frozen review and progress distinct.
use super::*;
use beacon_kube::helm::lifecycle::{self, Change, Chart, Plan, Request};
use futures::{StreamExt as _, channel::mpsc};
use gpui_kit::component::input::{Input, InputState};
use std::{cell::RefCell, time::Duration};

thread_local! { static WINDOWS: RefCell<Vec<WeakEntity<OperationView>>> = const { RefCell::new(Vec::new()) }; }
pub(crate) fn pending(cx: &App) -> Vec<String> {
    WINDOWS.with(|windows| {
        windows
            .borrow()
            .iter()
            .filter_map(WeakEntity::upgrade)
            .filter(|view| view.read(cx).stage == Stage::Running)
            .map(|view| {
                let view = view.read(cx);
                format!(
                    "Helm {}: {}/{}",
                    view.change.label(),
                    view.identity.namespace,
                    view.identity.name
                )
            })
            .collect()
    })
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Draft,
    Preparing,
    Review,
    Running,
    Finished,
}
pub(crate) fn open(
    session: Arc<ClusterSession>,
    identity: Identity,
    change: Change,
    values: serde_json::Value,
    window: &mut Window,
    cx: &mut App,
    on_done: impl FnMut(&mut App) + 'static,
) {
    let bounds = Bounds::centered(
        window.display(cx).map(|display| display.id()),
        size(px(1080.), px(760.)),
        cx,
    );
    cx.defer(move |cx| {
        if let Err(error) = operation_window(
            session,
            identity,
            change,
            values,
            bounds,
            Box::new(on_done),
            cx,
        ) {
            tracing::error!(%error,"could not open Helm operation window");
        }
    });
}
fn operation_window(
    session: Arc<ClusterSession>,
    identity: Identity,
    change: Change,
    values: serde_json::Value,
    bounds: Bounds<Pixels>,
    on_done: Box<dyn FnMut(&mut App)>,
    cx: &mut App,
) -> anyhow::Result<(AnyWindowHandle, Entity<OperationView>)> {
    gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(740.), px(560.))),
            ..TitleBar::window_options()
        },
        cx,
        move |window, cx| {
            #[cfg(all(test, feature = "ui-tests"))]
            window.set_view_retention(false);
            crate::i18n::window_title(format!("Beacon — Helm {}", change.label()), window, cx);
            window.activate_window();
            let view = cx.new(|cx| {
                OperationView::new(session, identity, change, values, on_done, window, cx)
            });
            WINDOWS.with(|windows| windows.borrow_mut().push(view.downgrade()));
            let weak = view.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                if let Some(view) = weak.upgrade() {
                    view.update(cx, |view, cx| view.close(window, cx));
                } else {
                    window.defer(cx, |window, _| window.remove_window());
                }
                false
            });
            view
        },
    )
}
pub(crate) struct OperationView {
    session: Arc<ClusterSession>,
    identity: Identity,
    change: Change,
    focus: FocusHandle,
    stage: Stage,
    name: Entity<InputState>,
    namespace: Entity<InputState>,
    chart: Entity<InputState>,
    repository: Entity<InputState>,
    version: Entity<InputState>,
    versions: Entity<SelectState<SearchableVec<String>>>,
    values: Entity<EditorState>,
    draft_values: serde_json::Value,
    values_visible: bool,
    plan: Option<Arc<Plan>>,
    document: Document,
    reveal: bool,
    diff: Option<Entity<DiffState>>,
    message: Option<String>,
    started: Option<std::time::Instant>,
    searching: bool,
    on_done: Box<dyn FnMut(&mut App)>,
    _task: Option<Task<()>>,
    _search: Option<Task<()>>,
    _clock: Option<Task<()>>,
    _events: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}
impl Focusable for OperationView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl OperationView {
    fn new(
        session: Arc<ClusterSession>,
        identity: Identity,
        change: Change,
        draft_values: serde_json::Value,
        on_done: Box<dyn FnMut(&mut App)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| {
            crate::i18n::input("Release name", window, cx).default_value(identity.name.clone())
        });
        let namespace = cx.new(|cx| {
            crate::i18n::input("Namespace", window, cx).default_value(identity.namespace.clone())
        });
        let spec = match &change {
            Change::Install(spec) | Change::Upgrade(spec) => Some(spec.clone()),
            _ => None,
        };
        let chart = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(
                    spec.as_ref()
                        .map_or("".into(), |spec| spec.reference.clone()),
                )
                .placeholder("repository/chart or oci://registry/chart")
        });
        let repository = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(
                    spec.as_ref()
                        .map_or("".into(), |spec| spec.repository.clone()),
                )
                .placeholder(crate::i18n::text("Optional repository URL"))
        });
        let version = cx.new(|cx| {
            crate::i18n::input("Exact chart version", window, cx)
                .default_value(spec.as_ref().map_or("".into(), |spec| spec.version.clone()))
        });
        let versions = cx.new(|cx| {
            SelectState::new(SearchableVec::new(vec![]), None, window, cx).searchable(true)
        });
        let values_visible = matches!(change, Change::Install(_));
        let text = if values_visible {
            crate::yaml_review::canonical_yaml(draft_values.clone()).unwrap_or_default()
        } else {
            String::new()
        };
        let values = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("yaml")
                .default_value(text)
                .folding(true)
        });
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut view = Self {
            session,
            identity,
            change,
            focus,
            stage: Stage::Draft,
            name,
            namespace,
            chart,
            repository,
            version,
            versions,
            values,
            draft_values,
            values_visible,
            plan: None,
            document: Document::Manifest,
            reveal: false,
            diff: None,
            message: None,
            started: None,
            searching: false,
            on_done,
            _task: None,
            _search: None,
            _clock: None,
            _events: None,
            _subscriptions: vec![],
        };
        view._subscriptions.push(cx.subscribe_in(
            &view.versions,
            window,
            |view, _, event: &SelectEvent<SearchableVec<String>>, window, cx| {
                if let SelectEvent::Confirm(Some(version)) = event {
                    view.version
                        .update(cx, |field, cx| field.set_value(version.clone(), window, cx));
                }
            },
        ));
        view
    }
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.stage == Stage::Running {
            self.message =
                Some("Wait for the Helm operation to finish before closing this window.".into());
            cx.notify();
        } else {
            window.defer(cx, |window, _| window.remove_window());
        }
    }
    fn chart_operation(&self) -> bool {
        matches!(self.change, Change::Install(_) | Change::Upgrade(_))
    }
    fn values(&self, cx: &App) -> Result<serde_json::Value, String> {
        if !self.values_visible {
            return Ok(self.draft_values.clone());
        }
        let text = self.values.read(cx).value();
        let value: serde_json::Value = if text.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_saphyr::from_str(text.as_ref()).map_err(|_| {
                "Values contain invalid YAML. Correct the draft before preparing.".to_string()
            })?
        };
        if !value.is_object() {
            return Err("Values must be a YAML mapping.".into());
        }
        Ok(value)
    }
    fn toggle_values(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.values_visible {
            match self.values(cx) {
                Ok(value) => self.draft_values = value,
                Err(error) => {
                    self.message = Some(error);
                    cx.notify();
                    return;
                }
            }
            self.values
                .update(cx, |editor, cx| editor.set_value("", window, cx));
        } else {
            let text =
                crate::yaml_review::canonical_yaml(self.draft_values.clone()).unwrap_or_default();
            self.values
                .update(cx, |editor, cx| editor.set_value(text, window, cx));
        }
        self.values_visible = !self.values_visible;
        cx.notify();
    }
    fn search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let namespace = self.namespace.read(cx).value().to_string();
        let reference = self.chart.read(cx).value().trim().to_string();
        self.searching = true;
        self.message = None;
        let search = Bridge::global(cx).run_cancellable(async move {
            lifecycle::versions(session, namespace, reference).await
        });
        self._search=Some(cx.spawn_in(window,async move |view,cx| {
            let result=search.result().await;
            let _=view.update_in(cx,|view,window,cx| {view.searching=false;match result {
                Ok(Ok(versions))=> {let versions:Vec<_>=versions.into_iter().map(|version|version.version).collect();
                    if versions.is_empty() {view.message=Some("No repository versions matched. Enter repository/chart, or supply an exact version for an OCI/local chart.".into());}
                    else {view.version.update(cx,|field,cx|field.set_value(versions[0].clone(),window,cx));}
                    view.versions.update(cx,|picker,cx|picker.set_items(SearchableVec::new(versions),window,cx));
                }
                Ok(Err(error))=>view.message=Some(error.user_message()),Err(error)=>view.message=Some(error.to_string()),
            }cx.notify();});
        }));
        cx.notify();
    }
    fn prepare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.stage != Stage::Draft {
            return;
        }
        let values = match self.values(cx) {
            Ok(values) => values,
            Err(error) => {
                self.message = Some(error);
                cx.notify();
                return;
            }
        };
        let identity = if matches!(self.change, Change::Install(_)) {
            Identity {
                name: self.name.read(cx).value().trim().into(),
                namespace: self.namespace.read(cx).value().trim().into(),
            }
        } else {
            self.identity.clone()
        };
        let spec = Chart {
            reference: self.chart.read(cx).value().trim().into(),
            version: self.version.read(cx).value().trim().into(),
            repository: self.repository.read(cx).value().trim().into(),
        };
        let change = match &self.change {
            Change::Install(_) => Change::Install(spec),
            Change::Upgrade(_) => Change::Upgrade(spec),
            change => change.clone(),
        };
        self.identity = identity.clone();
        self.message = None;
        self.stage = Stage::Preparing;
        self.diff = None;
        let session = self.session.clone();
        let task = Bridge::global(cx).run_cancellable(async move {
            lifecycle::prepare(
                session,
                Request {
                    identity,
                    change,
                    values,
                },
            )
            .await
        });
        self._task = Some(cx.spawn_in(window, async move |view, cx| {
            let result = task.result().await;
            let _ = view.update_in(cx, |view, window, cx| {
                match result {
                    Ok(Ok(plan)) => {
                        view.plan = Some(Arc::new(plan));
                        view.stage = Stage::Review;
                        view.build_diff(window, cx);
                    }
                    Ok(Err(error)) => {
                        view.stage = Stage::Draft;
                        view.message = Some(error.user_message());
                    }
                    Err(error) => {
                        view.stage = Stage::Draft;
                        view.message = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn build_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let document = self.document;
        let reveal = self.reveal;
        let preparing = cx.background_executor().spawn(async move {
            let before = review_text(
                plan.before_values.clone(),
                &plan.before_manifest,
                document,
                reveal,
            )?;
            let after = review_text(
                plan.after_values.clone(),
                &plan.after_manifest,
                document,
                reveal,
            )?;
            let preview = crate::yaml_review::Preview::text(&before, after)?;
            Ok::<_, String>(
                preview
                    .files()
                    .iter()
                    .cloned()
                    .map(|file| file.with_language("yaml"))
                    .collect::<Vec<_>>(),
            )
        });
        self.diff = None;
        self._task = Some(cx.spawn_in(window, async move |view, cx| {
            let result = preparing.await;
            let _ = view.update(cx, |view, cx| {
                match result {
                    Ok(files) => {
                        view.diff = Some(cx.new(|cx| {
                            DiffState::new(files, cx)
                                .with_mode(DiffMode::Split)
                                .with_context_lines(Some(3))
                        }))
                    }
                    Err(error) => view.message = Some(error),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn execute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.stage != Stage::Review || self.diff.is_none() {
            return;
        }
        let Some(plan) = self.plan.clone() else {
            return;
        };
        self.stage = Stage::Running;
        self.started = Some(std::time::Instant::now());
        self.message = Some("Starting Helm operation…".into());
        let (sender, mut events) = mpsc::unbounded();
        self._events = Some(cx.spawn_in(window, async move |view, cx| {
            while let Some(event) = events.next().await {
                let _ = view.update(cx, |view, cx| {
                    if view.stage == Stage::Running {
                        view.message = Some(event);
                        cx.notify();
                    }
                });
            }
        }));
        self._clock = Some(cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if !view
                    .update(cx, |view, cx| {
                        cx.notify();
                        view.stage == Stage::Running
                    })
                    .unwrap_or(false)
                {
                    break;
                }
            }
        }));
        let session = self.session.clone();
        let running =
            Bridge::global(cx).run(async move { lifecycle::execute(session, plan, sender).await });
        self._task = Some(cx.spawn_in(window, async move |view, cx| {
            let result = running.await;
            let _ = view.update(cx, |view, cx| {
                view.stage = Stage::Finished;
                view._clock = None;
                view._events = None;
                view.message = Some(match result {
                    Ok(Ok(message)) => message,
                    Ok(Err(error)) => error.user_message(),
                    Err(error) => error.to_string(),
                });
                super::changed(&view.session, cx);
                (view.on_done)(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }
}
impl Render for OperationView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = self.stage == Stage::Draft;
        let chart = self.chart_operation();
        let field = |label: &str, input: &Entity<InputState>| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::text(label)),
                )
                .child(Input::new(input).small().disabled(!editing))
        };
        let header = v_flex()
            .gap_2()
            .flex_shrink_0()
            .child(
                div().text_lg().font_weight(FontWeight::SEMIBOLD).child(
                    crate::i18n::message(
                        "messages.helm_release",
                        &[("value0", (self.change.label()).to_string())],
                    )
                    .to_string(),
                ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} · {} / {}",
                        self.session.id().display_name(),
                        self.identity.namespace,
                        self.identity.name
                    )),
            )
            .child(
                h_flex().gap_2().children(
                    ["1 · Configure", "2 · Review", "3 · Apply"]
                        .into_iter()
                        .map(|step| {
                            div()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(cx.theme().muted)
                                .text_xs()
                                .child(step)
                        }),
                ),
            );
        let mut body = v_flex().size_full().min_size_0().gap_3().child(header);
        if matches!(self.stage, Stage::Draft | Stage::Preparing) {
            if matches!(self.change, Change::Install(_)) {
                body = body.child(
                    h_flex()
                        .gap_3()
                        .child(field("Release name", &self.name))
                        .child(field("Namespace", &self.namespace)),
                );
            }
            if chart {
                body=body.child(h_flex().gap_3().child(field("Chart",&self.chart)).child(field("Repository URL (optional)",&self.repository)))
                    .child(h_flex().gap_2().items_end().child(field("Exact version",&self.version))
                        .child(Select::new(&self.versions).small().w(px(260.)).placeholder(crate::i18n::text("Choose a chart version")).search_placeholder(crate::i18n::text("Search chart versions")).accessibility_label(crate::i18n::text("Chart version")).disabled(!editing))
                        .child(Button::new("helm-find-versions").small().outline().label(crate::i18n::text(if self.searching {"Loading…"} else {"Find versions"})).disabled(!editing||self.searching).on_click(cx.listener(|view,_,window,cx|view.search(window,cx)))))
                    .child(h_flex().gap_2().items_center().child(div().flex_1().text_sm().child(crate::i18n::text("Values overrides")))
                        .child(Button::new("helm-edit-reveal").small().ghost().label(crate::i18n::text(if self.values_visible {"Hide values"} else {"Reveal to edit"})).disabled(!editing).on_click(cx.listener(|view,_,window,cx|view.toggle_values(window,cx)))))
                    .child(if self.values_visible {div().id("helm-operation-values").test_support().flex_1().min_size_0().overflow_hidden().border_1().border_color(cx.theme().border).child(Editor::new(&self.values).h_full().readonly(!editing)).into_any_element()} else {div().flex_1().text_sm().text_color(cx.theme().muted_foreground).child(crate::i18n::text("Stored values are hidden. The current values will be reused unless you reveal and edit them.")).into_any_element()});
            } else {
                body=body.child(div().flex_1().text_sm().child(match self.change {Change::Rollback(number)=>crate::i18n::message("messages.review_a_rollback_to_stored_revision_number_this_creates_a_new_revision_existing_history_is_preserved", &[("number", (number).to_string())]).to_string(),_=>"Review removal of this release's resources. Revision history, CRDs and resources marked keep are retained.".into()}));
            }
        } else if self.stage == Stage::Review {
            body=body.child(h_flex().gap_2().items_center()
                .children([(Document::Values,"Values"),(Document::Manifest,"Manifest")].into_iter().map(|(document,label)|Button::new(format!("helm-review-{label}")).small().outline().selected(self.document==document).label(crate::i18n::text(label)).on_click(cx.listener(move |view,_,window,cx|{view.document=document;view.build_diff(window,cx); }))))
                .child(div().flex_1())
                .child(Button::new("helm-review-reveal").small().ghost().label(crate::i18n::text(if self.reveal {"Hide values"} else {"Reveal values"})).on_click(cx.listener(|view,_,window,cx|{view.reveal = !view.reveal;view.build_diff(window,cx);}))))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text("The chart archive and values are fixed for this review. Helm re-renders templates when applying. Secrets remain concealed until revealed.")))
                .when_some(self.diff.clone(),|body,diff|body.child(div().id("helm-operation-diff").test_support().flex_1().min_size_0().overflow_hidden().border_1().border_color(cx.theme().border).child(Diff::new(&diff).header_visible(false))))
                .when(self.diff.is_none(),|body|body.child(div().flex_1().child(crate::i18n::text("Preparing comparison…"))));
        } else {
            body = body.child(
                div()
                    .flex_1()
                    .text_sm()
                    .child(if self.stage == Stage::Running {
                        crate::i18n::message(
                            "messages.running_for_seconds",
                            &[(
                                "value0",
                                (self.started.map_or(0, |time| time.elapsed().as_secs()))
                                    .to_string(),
                            )],
                        )
                        .to_string()
                    } else {
                        "Operation finished. Release details and history have been refreshed."
                            .into()
                    }),
            );
        }
        body = body
            .when_some(self.message.clone(), |body, message| {
                body.child(
                    div()
                        .id("helm-operation-message")
                        .test_support()
                        .text_sm()
                        .child(copyable_text("helm-operation-message-text", message)),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .when(self.stage == Stage::Preparing, |row| {
                        row.child(
                            Button::new("helm-cancel-prepare")
                                .small()
                                .outline()
                                .label(crate::i18n::text("Cancel"))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view._task = None;
                                    view.stage = Stage::Draft;
                                    view.plan = None;
                                    cx.notify();
                                })),
                        )
                    })
                    .when(editing, |row| {
                        row.child(
                            Button::new("helm-prepare-operation")
                                .small()
                                .primary()
                                .label(crate::i18n::text("Prepare & review"))
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.prepare(window, cx)),
                                ),
                        )
                    })
                    .when(self.stage == Stage::Review, |row| {
                        row.child(
                            Button::new("helm-review-back")
                                .small()
                                .outline()
                                .label(crate::i18n::text("Back to configuration"))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view._task = None;
                                    view.plan = None;
                                    view.diff = None;
                                    view.stage = Stage::Draft;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("helm-confirm-operation")
                                .small()
                                .primary()
                                .label(
                                    crate::i18n::message(
                                        "messages.confirm",
                                        &[("value0", (self.change.label()).to_string())],
                                    )
                                    .to_string(),
                                )
                                .disabled(self.diff.is_none())
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.execute(window, cx)),
                                ),
                        )
                    })
                    .when(self.stage == Stage::Finished, |row| {
                        row.child(
                            Button::new("helm-operation-close")
                                .small()
                                .outline()
                                .label(crate::i18n::text("Close"))
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.close(window, cx)),
                                ),
                        )
                    }),
            );
        v_flex()
            .id("helm-operation-window")
            .test_support()
            .size_full()
            .track_focus(&self.focus)
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(
                cx.listener(|view, _: &crate::app::CloseTab, window, cx| view.close(window, cx)),
            )
            .child(TitleBar::new().child(format!("Helm {}", self.change.label())))
            .child(div().flex_1().min_size_0().p_4().child(body))
    }
}

fn review_text(
    value: serde_json::Value,
    manifest: &str,
    document: Document,
    reveal: bool,
) -> Result<String, String> {
    match document {
        Document::Values => {
            let mut value = value;
            if !reveal {
                super::content::conceal(&mut value);
            }
            crate::yaml_review::canonical_yaml(value)
        }
        Document::Manifest => {
            if reveal || manifest.trim().is_empty() {
                return Ok(manifest.into());
            }
            let mut docs: Vec<serde_json::Value> =
                serde_saphyr::from_multiple(manifest).map_err(|_| {
                    "Manifest cannot be safely concealed. Reveal values to inspect it.".to_string()
                })?;
            for doc in &mut docs {
                super::content::secret_documents(doc);
            }
            docs.into_iter()
                .filter(|doc| !doc.is_null())
                .map(crate::yaml_review::canonical_yaml)
                .collect::<Result<Vec<_>, _>>()
                .map(|docs| docs.join("---\n"))
        }
    }
}
#[cfg(all(test, feature = "ui-tests", unix))]
mod tests;
