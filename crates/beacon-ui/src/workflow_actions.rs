//! Argo actions are configured, validated and reviewed before any write.
use crate::scroll::OverflowScrollbars as _;
use crate::{bridge::Bridge, copyable_text::copyable_text};
use beacon_kube::{
    ClusterSession, DynamicObject, Kind, ObjectRef,
    workflow_ops::{self, Action, Inputs, Parameter, Plan},
};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;

pub(crate) enum WorkflowEvent {
    Closed,
    Open { kind: Arc<Kind>, target: ObjectRef },
}
struct Field {
    definition: Parameter,
    input: Entity<InputState>,
    override_runtime: bool,
    allow_empty: bool,
}
pub(crate) struct WorkflowActions {
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    target: ObjectRef,
    uid: Option<String>,
    action: Action,
    source: Option<Arc<DynamicObject>>,
    fields: Vec<Field>,
    namespace: Entity<InputState>,
    entrypoint: Option<Entity<SelectState<SearchableVec<String>>>>,
    plan: Option<Plan>,
    loading: bool,
    busy: bool,
    error: Option<String>,
    scroll: ScrollHandle,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    _field_subscriptions: Vec<Subscription>,
}
impl EventEmitter<WorkflowEvent> for WorkflowActions {}
impl WorkflowActions {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        kind: Arc<Kind>,
        target: ObjectRef,
        uid: Option<String>,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let namespace = cx.new(|cx| {
            crate::i18n::input("Target namespace", window, cx)
                .default_value(target.namespace.as_deref().unwrap_or("default"))
        });
        let subscriptions = vec![cx.subscribe(&namespace, |_, _, _: &InputEvent, cx| cx.notify())];
        let mut view = Self {
            session,
            kind,
            target,
            uid,
            action,
            source: None,
            fields: vec![],
            namespace,
            entrypoint: None,
            plan: None,
            loading: false,
            busy: false,
            error: None,
            scroll: ScrollHandle::new(),
            _task: None,
            _subscriptions: subscriptions,
            _field_subscriptions: vec![],
        };
        view.load(window, cx);
        view
    }
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.loading = true;
        self.error = None;
        self.plan = None;
        self.source = None;
        self.fields.clear();
        self._field_subscriptions.clear();
        self.entrypoint = None;
        let (session, kind, target, uid, action) = (
            self.session.clone(),
            (*self.kind).clone(),
            self.target.clone(),
            self.uid.clone(),
            self.action,
        );
        let loading = Bridge::global(cx).run_cancellable(async move {
            let source = workflow_ops::load(session, kind.clone(), target, uid, action)
                .await
                .map_err(|error| error.user_message())?;
            let definitions = workflow_ops::parameters(&kind.resource.kind, &source, action)
                .map_err(|error| error.user_message())?;
            Ok::<_, String>((source, definitions))
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = loading.result().await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.loading = false;
                match result {
                    Ok(Ok((source, definitions))) => {
                        view.set_fields(definitions, window, cx);
                        let entries = workflow_ops::entrypoints(&view.kind.resource.kind, &source);
                        if view.action.creates() && !entries.is_empty() {
                            let entry =
                                workflow_ops::default_entrypoint(&view.kind.resource.kind, &source);
                            let picker = cx.new(|cx| {
                                let mut state =
                                    SelectState::new(SearchableVec::new(entries), None, window, cx)
                                        .searchable(true);
                                if !entry.is_empty() {
                                    state.set_selected_value(&entry, window, cx);
                                }
                                state
                            });
                            view._subscriptions.push(cx.subscribe_in(
                                &picker,
                                window,
                                |view,
                                 _,
                                 event: &SelectEvent<SearchableVec<String>>,
                                 window,
                                 cx| {
                                    if let SelectEvent::Confirm(Some(entrypoint)) = event
                                        && let Some(source) = &view.source
                                    {
                                        match workflow_ops::parameters_for_entrypoint(
                                            &view.kind.resource.kind,
                                            source,
                                            view.action,
                                            entrypoint,
                                        ) {
                                            Ok(definitions) => {
                                                view.set_fields(definitions, window, cx);
                                                view.error = None;
                                            }
                                            Err(error) => {
                                                view.fields.clear();
                                                view.error = Some(error.user_message());
                                            }
                                        }
                                    }
                                    cx.notify();
                                },
                            ));
                            view.entrypoint = Some(picker);
                        }
                        view.source = Some(source);
                    }
                    Ok(Err(error)) => view.error = Some(error),
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn set_fields(
        &mut self,
        definitions: Vec<Parameter>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous: std::collections::BTreeMap<_, _> = self
            .fields
            .iter()
            .filter(|field| {
                field.input.read(cx).value().as_ref()
                    != field.definition.value.as_deref().unwrap_or("")
                    || field.override_runtime
                    || field.allow_empty
            })
            .map(|field| {
                (
                    field.definition.key.clone(),
                    (
                        field.input.read(cx).value().to_string(),
                        field.override_runtime,
                        field.allow_empty,
                    ),
                )
            })
            .collect();
        self.fields.clear();
        self._field_subscriptions.clear();
        for definition in definitions {
            let old = previous.get(&definition.key);
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(
                        old.map(|old| old.0.as_str())
                            .unwrap_or(definition.value.as_deref().unwrap_or("")),
                    )
                    .placeholder(if definition.required {
                        "Required value"
                    } else {
                        "Value"
                    })
            });
            self._field_subscriptions
                .push(cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify()));
            self.fields.push(Field {
                definition,
                input,
                override_runtime: old.is_some_and(|old| old.1),
                allow_empty: old.is_some_and(|old| old.2),
            });
        }
    }
    fn inputs(&self, cx: &App) -> Inputs {
        Inputs {
            namespace: self.namespace.read(cx).value().trim().into(),
            entrypoint: self
                .entrypoint
                .as_ref()
                .and_then(|state| state.read(cx).selected_value())
                .cloned()
                .unwrap_or_default(),
            values: self
                .fields
                .iter()
                .filter(|field| !field.definition.runtime || field.override_runtime)
                .map(|field| {
                    (
                        field.definition.key.clone(),
                        field.input.read(cx).value().to_string(),
                    )
                })
                .collect(),
        }
    }
    fn review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let Some(source) = self.source.clone() else {
            return;
        };
        if let Some(field) = self.fields.iter().find(|field| {
            field.definition.required
                && field.input.read(cx).value().is_empty()
                && !field.allow_empty
        }) {
            self.error = Some(
                crate::i18n::message(
                    "messages.provide_a_value_for_before_reviewing",
                    &[("value0", (field.definition.name).to_string())],
                )
                .to_string(),
            );
            cx.notify();
            return;
        }
        let inputs = self.inputs(cx);
        self.loading = true;
        self.error = None;
        let (session, kind, action) = (self.session.clone(), (*self.kind).clone(), self.action);
        let preparing = Bridge::global(cx).run_cancellable(async move {
            workflow_ops::prepare(session, kind, source, action, inputs)
                .await
                .map_err(|error| error.user_message())
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = preparing.result().await;
            let _ = this.update(cx, |view, cx| {
                view.loading = false;
                match result {
                    Ok(Ok(plan)) => view.plan = Some(plan),
                    Ok(Err(error)) => view.error = Some(error),
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let Some(plan) = self.plan.take() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let session = self.session.clone();
        let kind = Arc::new(plan.workflow_kind.clone());
        // An accepted create/patch is allowed to finish even if its owner is
        // removed. Aborting a POST invites an accidental duplicate execution.
        let running = Bridge::global(cx).run(async move {
            workflow_ops::execute(session, &plan)
                .await
                .map_err(|error| error.user_message())
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = running.await;
            let _ = this.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok(Ok(object)) => cx.emit(WorkflowEvent::Open {
                        kind,
                        target: ObjectRef::of(&object),
                    }),
                    Ok(Err(error)) => view.error = Some(error),
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn description(&self) -> &'static str {
        match self.action {
            Action::Submit => {
                "Create a Workflow from this template. Review its entrypoint and runtime parameters before submitting."
            }
            Action::Resubmit => {
                "Create a fresh execution with fresh Pods. The original Workflow and its history are kept."
            }
            Action::Retry => {
                "Retry failed steps in this Workflow. Successful steps are kept; failed Pods and exit-handler Pods listed below are deleted before resetting the failed branches."
            }
            Action::Suspend => {
                "Pause scheduling new steps. Pods already running continue. Use Resume to allow the Workflow to proceed."
            }
            Action::Resume => {
                "Continue this Workflow and complete its active suspend steps. Supply any required outputs before resuming."
            }
            Action::Stop => {
                "Stop running tasks gracefully and run exit handlers. The Workflow may take time to stop."
            }
            Action::Terminate => {
                "Terminate immediately, without running exit handlers. Running Pods will be stopped by the controller."
            }
        }
    }
    fn yaml(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = self.plan.as_ref().filter(|plan| plan.action.creates()) else {
            return;
        };
        if let Ok(preview) = crate::yaml_review::Preview::create(plan.proposed.clone()) {
            crate::yaml_review::open(
                preview,
                format!("{} · Workflow", self.session.id().display_name()),
                true,
                false,
                window,
                cx,
            )
            .update(cx, |review, _| review.read_only());
        }
    }
}
impl Render for WorkflowActions {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let reviewed = self.plan.is_some();
        let disabled = self.loading || self.busy;
        let mut content = v_flex().gap_4().p_4();
        if let Some(plan) = &self.plan {
            content = content.child(
                div()
                    .id("workflow-reviewed")
                    .test_support()
                    .text_sm()
                    .text_color(cx.theme().success)
                    .child(crate::i18n::text(
                        "Permissions checked · Server validation passed",
                    )),
            );
            if plan.action.creates() {
                content = content.child(copyable_text(
                    "workflow-created-target",
                    crate::i18n::message(
                        "messages.new_workflow_generated",
                        &[
                            (
                                "value0",
                                (plan.proposed["metadata"]["namespace"]
                                    .as_str()
                                    .unwrap_or(""))
                                .to_string(),
                            ),
                            (
                                "value1",
                                (plan.proposed["metadata"]["generateName"]
                                    .as_str()
                                    .unwrap_or(""))
                                .to_string(),
                            ),
                        ],
                    )
                    .to_string(),
                ));
                content = content.child(copyable_text(
                    "workflow-reviewed-entrypoint",
                    crate::i18n::message(
                        "messages.entrypoint",
                        &[(
                            "value0",
                            (plan.proposed["spec"]["entrypoint"]
                                .as_str()
                                .unwrap_or("Template default"))
                            .to_string(),
                        )],
                    )
                    .to_string(),
                ));
            }
            for (index, field) in self.fields.iter().enumerate() {
                let value = plan
                    .inputs
                    .values
                    .get(&field.definition.key)
                    .cloned()
                    .unwrap_or_else(|| "Keep runtime reference".into());
                content = content.child(
                    h_flex()
                        .items_start()
                        .gap_4()
                        .child(
                            div()
                                .w(px(200.))
                                .flex_shrink_0()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(field.definition.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(copyable_text(("workflow-reviewed-value", index), value)),
                        ),
                );
            }
            if self.action == Action::Retry {
                content = content.child(
                    div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                        crate::i18n::message(
                            "messages.pods_to_delete",
                            &[("value0", (plan.pods.len()).to_string())],
                        )
                        .to_string(),
                    ),
                );
                if plan.pods.is_empty() {
                    content = content.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(crate::i18n::text("No retained Pods need deleting.")),
                    );
                }
                for (index, pod) in plan.pods.iter().enumerate() {
                    content = content.child(
                        h_flex()
                            .py_2()
                            .gap_4()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(div().w(px(160.)).child(copyable_text(
                                ("workflow-retry-namespace", index),
                                pod.reference.namespace.clone().unwrap_or_default(),
                            )))
                            .child(
                                div()
                                    .id(("workflow-retry-pod", index))
                                    .test_support()
                                    .flex_1()
                                    .child(copyable_text(
                                        ("workflow-retry-pod-value", index),
                                        pod.reference.name.clone(),
                                    )),
                            ),
                    );
                }
                content = content.child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text("This is a retry of the same Workflow. Deletion uses the reviewed Pod UIDs. If a later update fails, partial deletions are reported; refresh before retrying.")));
            }
            if self.action.creates() {
                content = content.child(
                    Button::new("workflow-preview-yaml")
                        .small()
                        .label(crate::i18n::text("View proposed YAML"))
                        .on_click(cx.listener(|view, _, window, cx| view.yaml(window, cx))),
                );
            }
        } else if self.source.is_some() && !self.busy {
            if self.kind.resource.kind == "ClusterWorkflowTemplate" {
                content = content.child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(crate::i18n::text("Target namespace")),
                        )
                        .child(Input::new(&self.namespace).small().disabled(disabled)),
                );
            }
            if let Some(entrypoint) = &self.entrypoint {
                content = content.child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(crate::i18n::text("Entrypoint")),
                        )
                        .child(Select::new(entrypoint).small().disabled(disabled)),
                );
            }
            if !self.fields.is_empty() {
                content = content.child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                    crate::i18n::text(if self.action == Action::Resume {
                        "Suspend step outputs"
                    } else {
                        "Runtime parameters"
                    }),
                ));
            }
            for (index, field) in self.fields.iter().enumerate() {
                let is_runtime = field.definition.runtime;
                let mut row = v_flex().gap_2().child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(field.definition.name.clone()),
                        )
                        .when(field.definition.required, |row| {
                            row.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().danger)
                                    .child(crate::i18n::text("Required")),
                            )
                        }),
                );
                if !field.definition.description.is_empty() {
                    row = row.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(field.definition.description.clone()),
                    );
                }
                if is_runtime {
                    row = row.child(
                        crate::selection::checkbox(("workflow-parameter-override", index))
                            .checked(field.override_runtime)
                            .disabled(disabled)
                            .label(crate::i18n::text("Override runtime value"))
                            .on_click(cx.listener(move |view, value, _, cx| {
                                view.fields[index].override_runtime = *value;
                                cx.notify();
                            })),
                    );
                }
                row = row.child(
                    div()
                        .id(("workflow-parameter", index))
                        .test_support()
                        .child(
                            Input::new(&field.input)
                                .small()
                                .disabled(disabled || (is_runtime && !field.override_runtime)),
                        ),
                );
                if field.definition.required && field.input.read(cx).value().is_empty() {
                    row = row.child(
                        crate::selection::checkbox(("workflow-parameter-empty", index))
                            .checked(field.allow_empty)
                            .disabled(disabled)
                            .label(crate::i18n::text("Supply an empty string"))
                            .on_click(cx.listener(move |view, value, _, cx| {
                                view.fields[index].allow_empty = *value;
                                cx.notify();
                            })),
                    );
                }
                if !field.definition.choices.is_empty() {
                    row = row.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                crate::i18n::message(
                                    "messages.allowed",
                                    &[(
                                        "value0",
                                        (field.definition.choices.join(", ")).to_string(),
                                    )],
                                )
                                .to_string(),
                            ),
                    );
                }
                content = content.child(row);
            }
            if self.fields.is_empty() && self.action.creates() {
                content = content.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::text(
                            "This template has no runtime parameters.",
                        )),
                );
            }
        }
        v_flex()
            .id("workflow-action-dialog")
            .test_support()
            .w(px(760.))
            .max_w(relative(0.94))
            .max_h(relative(0.88))
            .min_h_0()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_lg()
            .overflow_hidden()
            .child(
                v_flex()
                    .p_4()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(crate::i18n::text(self.action.label())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if reviewed {
                                        "2 · Review & confirm"
                                    } else {
                                        "1 · Configure"
                                    }),
                            ),
                    )
                    .child(copyable_text(
                        "workflow-action-source",
                        format!(
                            "{} · {} · {}",
                            self.session.id().display_name(),
                            self.kind.resource.kind,
                            self.target
                        ),
                    ))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(crate::i18n::text(self.description())),
                    ),
            )
            .child(
                div()
                    .relative()
                    .min_h_0()
                    .flex_1()
                    .child(
                        div()
                            .id("workflow-action-scroll")
                            .overflow_y_scrollbar()
                            .track_scroll(&self.scroll)
                            .child(content),
                    )
                    .child(Scrollbar::vertical(&self.scroll).mode(ScrollbarMode::Scrolling)),
            )
            .children(self.loading.then(|| {
                div()
                    .id("workflow-loading")
                    .test_support()
                    .px_4()
                    .py_2()
                    .text_sm()
                    .child(crate::i18n::text(
                        "Checking current state and validating permissions…",
                    ))
            }))
            .children(self.error.clone().map(|error| {
                div()
                    .id("workflow-failed")
                    .test_support()
                    .p_4()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(copyable_text("workflow-action-error", error))
            }))
            .children(self.busy.then(|| {
                div()
                    .id("workflow-running")
                    .test_support()
                    .p_4()
                    .text_sm()
                    .child(crate::i18n::text(if self.action == Action::Retry {
                        "Deleting reviewed failed Pods, then resetting Workflow state…"
                    } else {
                        "Applying the confirmed operation…"
                    }))
            }))
            .child(
                h_flex()
                    .p_4()
                    .gap_2()
                    .justify_end()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .children((!reviewed && self.error.is_some()).then(|| {
                        Button::new("workflow-reload")
                            .small()
                            .label(crate::i18n::text("Reload source"))
                            .disabled(disabled)
                            .on_click(cx.listener(|view, _, window, cx| view.load(window, cx)))
                    }))
                    .children(reviewed.then(|| {
                        Button::new("workflow-back")
                            .small()
                            .label(crate::i18n::text("Back"))
                            .disabled(disabled)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.plan = None;
                                cx.notify();
                            }))
                    }))
                    .child(
                        Button::new("workflow-cancel")
                            .ghost()
                            .small()
                            .label(crate::i18n::text("Cancel"))
                            .disabled(self.busy)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(WorkflowEvent::Closed))),
                    )
                    .child(
                        Button::new("workflow-confirm")
                            .primary()
                            .small()
                            .label(crate::i18n::text(if reviewed {
                                self.action.label()
                            } else {
                                "Review operation"
                            }))
                            .disabled(disabled || self.source.is_none())
                            .on_click(cx.listener(|view, _, window, cx| {
                                if view.plan.is_some() {
                                    view.confirm(window, cx)
                                } else {
                                    view.review(window, cx)
                                }
                            })),
                    ),
            )
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    use std::sync::Mutex;
    fn template() -> serde_json::Value {
        json!({"apiVersion":"argoproj.io/v1alpha1","kind":"WorkflowTemplate","metadata":{"name":"pipeline","namespace":"default","uid":"template-uid","resourceVersion":"1"},"spec":{"entrypoint":"main","arguments":{"parameters":[{"name":"message","value":"hello","description":"Message printed by the step"},{"name":"token","valueFrom":{"secretKeyRef":{"name":"token","key":"value"}}}]},"templates":[{"name":"main","container":{"image":"busybox","command":["echo"],"args":["{{workflow.parameters.message}}"]}}]}})
    }
    fn workflow(phase: &str) -> serde_json::Value {
        json!({"apiVersion":"argoproj.io/v1alpha1","kind":"Workflow","metadata":{"name":"run","namespace":"default","uid":"workflow-uid","resourceVersion":"1"},"spec":{"entrypoint":"main","templates":[{"name":"main","container":{"image":"busybox"}}]},"status":{"phase":phase,"nodes":{"run":{"id":"run","name":"run","type":"Pod","phase":phase,"templateName":"main"}}}})
    }
    fn open(
        cx: &mut TestAppContext,
        session: Arc<ClusterSession>,
        kind: &str,
        target: &str,
        uid: &str,
        action: Action,
    ) -> (AnyWindowHandle, Entity<WorkflowActions>) {
        cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    WorkflowActions::new(
                        session,
                        Arc::new(support::kind(
                            "argoproj.io",
                            kind,
                            if kind == "WorkflowTemplate" {
                                "workflowtemplates"
                            } else {
                                "workflows"
                            },
                        )),
                        ObjectRef::new(Some("default".into()), target),
                        Some(uid.into()),
                        action,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        })
    }
    #[::core::prelude::v1::test]
    fn template_parameters_are_frozen_reviewed_cancelled_and_submitted_with_navigation() {
        let cx = &mut support::context();
        let (fixture, session) = support::fixture(cx, "argo-template-ui", vec![template()]);
        let (window, view) = open(
            cx,
            session.clone(),
            "WorkflowTemplate",
            "pipeline",
            "template-uid",
            Action::Submit,
        );
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.source.is_some()));
        cx.update_window(window, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.fields[0]
                    .input
                    .update(cx, |input, cx| input.set_value("reviewed", window, cx))
            });
            window.render_frame(cx);
            window.click("workflow-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.plan.is_some()));
        view.read_with(cx, |view, _| {
            let proposed = &view.plan.as_ref().unwrap().proposed;
            assert_eq!(
                proposed["spec"]["arguments"]["parameters"][0]["value"],
                "reviewed"
            );
            assert!(proposed["spec"]["arguments"]["parameters"][1]["valueFrom"].is_object());
            assert_eq!(proposed["spec"]["workflowTemplateRef"]["name"], "pipeline");
        });
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = closed.clone();
        cx.update(|cx| {
            cx.subscribe(&view, move |_, event, _| {
                if matches!(event, WorkflowEvent::Closed) {
                    seen.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            })
            .detach()
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("workflow-reviewed").visible());
            window.click("workflow-cancel", cx);
        })
        .unwrap();
        assert!(closed.load(std::sync::atomic::Ordering::Relaxed));
        assert!(!fixture.requests.lock().unwrap().iter().any(|(line, _)| {
            line.starts_with("POST /apis/argoproj.io/") && !line.contains("dryRun")
        }));
        let (window, view) = open(
            cx,
            session,
            "WorkflowTemplate",
            "pipeline",
            "template-uid",
            Action::Submit,
        );
        let navigation = Arc::new(Mutex::new(None));
        let observed = navigation.clone();
        cx.update(|cx| {
            cx.subscribe(&view, move |_, event, _| {
                if let WorkflowEvent::Open { kind, target } = event {
                    *observed.lock().unwrap() = Some((kind.clone(), target.clone()));
                }
            })
            .detach()
        });
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.source.is_some()));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("workflow-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.plan.is_some()));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("workflow-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |_| navigation.lock().unwrap().is_some());
        let navigation = navigation.lock().unwrap();
        let (kind, target) = navigation.as_ref().unwrap();
        assert_eq!(kind.resource.kind, "Workflow");
        assert!(target.name.starts_with("pipeline-"));
        assert_eq!(
            fixture.object("Workflow", &target.name)["status"]["phase"],
            "Pending"
        );
        assert_eq!(fixture.object("WorkflowTemplate", "pipeline"), template());
    }
    #[::core::prelude::v1::test]
    fn retry_review_lists_only_failed_pods_and_opens_same_workflow() {
        let cx = &mut support::context();
        let mut run = workflow("Failed");
        run["metadata"]["annotations"] = json!({"workflows.argoproj.io/pod-name-format":"v1"});
        run["status"]["nodes"] = json!({"run":{"id":"run","name":"run","type":"DAG","phase":"Failed","children":["good","bad"]},"good":{"id":"good","name":"run.good","type":"Pod","phase":"Succeeded","boundaryID":"run","outputs":{"result":"kept"}},"bad":{"id":"bad","name":"run.bad","type":"Pod","phase":"Failed","boundaryID":"run"}});
        let pod = |name: &str| json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"default","uid":format!("{name}-uid"),"resourceVersion":"1","labels":{"workflows.argoproj.io/workflow":"run"},"ownerReferences":[{"apiVersion":"argoproj.io/v1alpha1","kind":"Workflow","name":"run","uid":"workflow-uid","controller":true}]},"spec":{"containers":[{"name":"app","image":"busybox"}]}});
        let (fixture, session) =
            support::fixture(cx, "argo-retry-ui", vec![run, pod("good"), pod("bad")]);
        let (window, view) = open(
            cx,
            session,
            "Workflow",
            "run",
            "workflow-uid",
            Action::Retry,
        );
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.source.is_some()));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("workflow-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.plan.is_some()));
        assert_eq!(
            view.read_with(cx, |view, _| view
                .plan
                .as_ref()
                .unwrap()
                .pods
                .iter()
                .map(|pod| pod.reference.name.clone())
                .collect::<Vec<_>>()),
            vec!["bad"]
        );
        let opened = Arc::new(Mutex::new(None));
        let seen = opened.clone();
        cx.update(|cx| {
            cx.subscribe(&view, move |_, event, _| {
                if let WorkflowEvent::Open { target, .. } = event {
                    *seen.lock().unwrap() = Some(target.clone());
                }
            })
            .detach()
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find(("workflow-retry-pod", 0usize)).visible());
            window.click("workflow-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |_| opened.lock().unwrap().is_some());
        assert_eq!(opened.lock().unwrap().as_ref().unwrap().name, "run");
        assert_eq!(
            fixture.object("Workflow", "run")["status"]["nodes"]["good"]["outputs"]["result"],
            "kept"
        );
        assert!(
            !fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.starts_with("DELETE") && line.contains("/pods/good"))
        );
    }
    #[::core::prelude::v1::test]
    fn denied_controls_and_server_failure_have_copyable_errors_without_false_success() {
        let cx = &mut support::context();
        let (fixture, session) = support::fixture(cx, "argo-denied-ui", vec![workflow("Running")]);
        fixture.rules(
            json!([{"apiGroups":["argoproj.io"],"resources":["workflows"],"verbs":["get","list"]}]),
        );
        let (window, view) = open(cx, session, "Workflow", "run", "workflow-uid", Action::Stop);
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.source.is_some()));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("workflow-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.error.is_some()));
        assert!(view.read_with(cx, |view, _| view.error.as_ref().unwrap().contains("patch")));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("workflow-failed").visible());
        })
        .unwrap();
        assert!(
            !fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.starts_with("PATCH"))
        );
        let (fixture, session) = support::fixture(cx, "argo-failure-ui", vec![workflow("Running")]);
        let (window, view) = open(
            cx,
            session,
            "Workflow",
            "run",
            "workflow-uid",
            Action::Terminate,
        );
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.source.is_some()));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("workflow-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.plan.is_some()));
        fixture.deny("workflows");
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("workflow-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.error.is_some()));
        assert!(!view.read_with(cx, |view, _| view.busy));
        assert!(view.read_with(cx, |view, _| {
            view.error.as_ref().unwrap().contains("HTTP 403")
        }));
        assert!(fixture.object("Workflow", "run")["spec"]["shutdown"].is_null());
    }
    #[::core::prelude::v1::test]
    fn entrypoint_changes_rebuild_inputs_and_allow_explicit_empty_values() {
        let cx = &mut support::context();
        let mut source = template();
        source["spec"].as_object_mut().unwrap().remove("arguments");
        source["spec"]["templates"] = json!([
            {"name":"main","inputs":{"parameters":[{"name":"message"},{"name":"retry-count","default":"2"}]},"container":{"image":"busybox"}},
            {"name":"other","inputs":{"parameters":[{"name":"destination","default":"prod"},{"name":"retry-count","default":"5"}]},"container":{"image":"alpine"}}
        ]);
        let (fixture, session) = support::fixture(cx, "argo-entry-inputs", vec![source]);
        let (window, view) = open(
            cx,
            session,
            "WorkflowTemplate",
            "pipeline",
            "template-uid",
            Action::Submit,
        );
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.source.is_some()));
        assert_eq!(
            view.read_with(cx, |view, _| view
                .fields
                .iter()
                .map(|field| field.definition.key.clone())
                .collect::<Vec<_>>()),
            vec!["message", "retry-count"]
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("workflow-confirm", cx);
        })
        .unwrap();
        assert!(view.read_with(cx, |view, _| {
            view.error
                .as_ref()
                .is_some_and(|error| error.contains("message"))
        }));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(("workflow-parameter-empty", 0usize), cx);
            window.render_frame(cx);
            window.click("workflow-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.plan.is_some()));
        assert_eq!(
            view.read_with(cx, |view, _| {
                view.plan.as_ref().unwrap().proposed["spec"]["arguments"]["parameters"][0]["value"]
                    .clone()
            }),
            ""
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("workflow-back", cx);
            let picker = view.read(cx).entrypoint.as_ref().unwrap().clone();
            picker.update(cx, |picker, cx| {
                picker.set_selected_value(&"other".to_owned(), window, cx);
                cx.emit(SelectEvent::Confirm(Some("other".to_owned())));
            });
        })
        .unwrap();
        assert_eq!(
            view.read_with(cx, |view, _| view
                .fields
                .iter()
                .map(|field| field.definition.key.clone())
                .collect::<Vec<_>>()),
            vec!["destination", "retry-count"]
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("workflow-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.plan.is_some()));
        view.read_with(cx, |view, _| {
            let spec = &view.plan.as_ref().unwrap().proposed["spec"];
            assert_eq!(spec["entrypoint"], "other");
            assert_eq!(
                spec["arguments"]["parameters"],
                json!([{"name":"destination","value":"prod"},{"name":"retry-count","value":"5"}])
            );
        });
        assert!(!fixture.requests.lock().unwrap().iter().any(|(line, _)| {
            line.starts_with("POST /apis/argoproj.io/") && !line.contains("dryRun")
        }));
    }
}
