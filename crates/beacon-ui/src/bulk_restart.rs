//! A fixed selection is reviewed once; every Deployment keeps its own result.
use crate::{bridge::Bridge, copyable_text::copyable_text};
use beacon_kube::{ClusterSession, DeleteTarget, Kind};
use gpui_kit::{
    component::{
        ActiveTheme as _, Disableable as _, Sizable as _,
        button::{Button, ButtonVariants as _},
        h_flex,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder as _,
    *,
};
use std::{collections::HashMap, sync::Arc};

const NAMESPACE_WIDTH: f32 = 180.;

#[derive(Clone)]
enum Status {
    Pending,
    Running,
    Restarted,
    Failed(String),
}
pub(crate) enum Event {
    Closed,
    Completed {
        restarted: Vec<DeleteTarget>,
        failed: usize,
    },
}
pub(crate) struct BulkRestart {
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    targets: Vec<DeleteTarget>,
    statuses: Vec<Status>,
    busy: bool,
    finished: bool,
    list: ListState,
    _task: Option<Task<()>>,
}
impl EventEmitter<Event> for BulkRestart {}
impl BulkRestart {
    pub fn new(session: Arc<ClusterSession>, kind: Arc<Kind>, targets: Vec<DeleteTarget>) -> Self {
        let count = targets.len();
        Self {
            session,
            kind,
            targets,
            statuses: vec![Status::Pending; count],
            busy: false,
            finished: false,
            list: ListState::new(count, ListAlignment::Top, px(56.)),
            _task: None,
        }
    }
    pub(crate) fn busy(&self) -> bool {
        self.busy
    }
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.finished || self.targets.is_empty() {
            return;
        }
        self.busy = true;
        self.statuses.fill(Status::Running);
        let session = self.session.clone();
        let resource = self.kind.resource.clone();
        let targets = self.targets.clone();
        let running = Bridge::global(cx)
            .run_cancellable(async move { session.restart_many(resource, targets).await });
        self._task = Some(cx.spawn_in(window, async move |view, cx| {
            let results = running.result().await;
            let _ = view.update(cx, |view, cx| {
                view.busy = false;
                view.finished = true;
                let mut restarted = Vec::new();
                let mut failed = 0;
                match results {
                    Ok(results) => {
                        let mut results: HashMap<_, _> = results
                            .into_iter()
                            .map(|(target, result)| ((target.reference, target.uid), result))
                            .collect();
                        for (index, target) in view.targets.iter().enumerate() {
                            if let Some(result) =
                                results.remove(&(target.reference.clone(), target.uid.clone()))
                            {
                                match result {
                                    Ok(_) => {
                                        view.statuses[index] = Status::Restarted;
                                        restarted.push(target.clone());
                                    }
                                    Err(error) => {
                                        view.statuses[index] = Status::Failed(error.user_message());
                                        failed += 1;
                                    }
                                }
                            } else {
                                view.statuses[index] = Status::Failed(
                                    "The request returned no result. Refresh before retrying."
                                        .into(),
                                );
                                failed += 1;
                            }
                        }
                    }
                    Err(error) => {
                        view.statuses.fill(Status::Failed(error.to_string()));
                        failed = view.targets.len();
                    }
                }
                view.list.reset(view.targets.len());
                cx.emit(Event::Completed { restarted, failed });
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn row(&self, index: usize, cx: &Context<Self>) -> AnyElement {
        let target = &self.targets[index].reference;
        let namespace = target.namespace.as_deref().unwrap_or("—").to_owned();
        let name = target.name.clone();
        let (status, color) = match &self.statuses[index] {
            Status::Pending => ("Ready".to_string(), cx.theme().muted_foreground),
            Status::Running => ("Restarting…".into(), cx.theme().muted_foreground),
            Status::Restarted => ("Restart requested".into(), cx.theme().success),
            Status::Failed(error) => (error.clone(), cx.theme().danger),
        };
        v_flex()
            .id(("bulk-restart-target", index))
            .role(Role::Row)
            .aria_label(format!(
                "{} / {}",
                target.namespace.as_deref().unwrap_or("—"),
                target.name
            ))
            .test_support()
            .w_full()
            .min_w_0()
            .p_3()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .id(("restart-namespace-cell", index))
                            .w(px(NAMESPACE_WIDTH))
                            .flex_shrink_0()
                            .text_sm()
                            .truncate()
                            .tooltip(move |window, cx| {
                                Tooltip::new(namespace.clone()).build(window, cx)
                            })
                            .child(copyable_text(
                                ("restart-namespace", index),
                                target.namespace.as_deref().unwrap_or("—").to_string(),
                            )),
                    )
                    .child(
                        div()
                            .id(("restart-name-cell", index))
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .tooltip(move |window, cx| Tooltip::new(name.clone()).build(window, cx))
                            .child(copyable_text(("restart-name", index), target.name.clone())),
                    ),
            )
            .child(
                div()
                    .id(("bulk-restart-status", index))
                    .aria_label(status.clone())
                    .test_support()
                    .text_xs()
                    .text_color(color)
                    .child(copyable_text(("restart-status", index), status.clone())),
            )
            .when(matches!(&self.statuses[index], Status::Failed(_)), |view| {
                view.children(crate::permissions::error_link(
                    ("restart-permissions", index),
                    self.session.clone(),
                    &status,
                    beacon_kube::rbac::Query {
                        verb: "patch".into(),
                        group: "apps".into(),
                        resource: "deployments".into(),
                        namespace: target.namespace.clone(),
                        name: Some(target.name.clone()),
                        ..Default::default()
                    },
                ))
            })
            .into_any_element()
    }
}
impl Render for BulkRestart {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let successful = self
            .statuses
            .iter()
            .filter(|status| matches!(status, Status::Restarted))
            .count();
        let failed = self
            .statuses
            .iter()
            .filter(|status| matches!(status, Status::Failed(_)))
            .count();
        let message = if self.finished {
            crate::i18n::message("messages.successful_requested_failed_failed_rollouts_continue_in_the_deployment_controller", &[("successful", (successful).to_string()), ("failed", (failed).to_string())]).to_string()
        } else if self.busy {
            crate::i18n::message(
                "messages.restarting_deployments",
                &[("value0", (self.targets.len()).to_string())],
            )
            .to_string()
        } else {
            "The Deployments below will restart their Pods using their current update strategy."
                .into()
        };
        let heading = if self.finished {
            "Restart results".to_string()
        } else {
            crate::i18n::message(
                "messages.restart_deployments",
                &[("value0", (self.targets.len()).to_string())],
            )
            .to_string()
        };
        let table = v_flex()
            .w_full()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().border)
            .overflow_hidden()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .px_3()
                    .py_2()
                    .gap_3()
                    .bg(cx.theme().muted)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        div()
                            .w(px(NAMESPACE_WIDTH))
                            .flex_shrink_0()
                            .whitespace_nowrap()
                            .child(crate::i18n::text("Namespace")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .child("Deployment"),
                    ),
            )
            .child(
                div()
                    .id("bulk-restart-target-table")
                    .test_support()
                    .w_full()
                    .h(px((self.targets.len() as f32 * 74.).clamp(74., 330.)))
                    .child(
                        list(
                            self.list.clone(),
                            cx.processor(|view, index, _, cx| view.row(index, cx)),
                        )
                        .size_full(),
                    ),
            );
        let footer = h_flex()
            .justify_end()
            .gap_2()
            .child(
                Button::new("bulk-restart-close")
                    .small()
                    .ghost()
                    .label(crate::i18n::text(if self.finished {
                        "Close"
                    } else {
                        "Cancel"
                    }))
                    .disabled(self.busy)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Closed))),
            )
            .when(!self.finished, |row| {
                row.child(
                    Button::new("bulk-restart-confirm")
                        .small()
                        .primary()
                        .label(crate::i18n::text(if self.busy {
                            "Restarting…"
                        } else {
                            "Restart Deployments"
                        }))
                        .disabled(self.busy || self.targets.is_empty())
                        .on_click(cx.listener(|view, _, window, cx| view.start(window, cx))),
                )
            });
        v_flex().id("bulk-restart-dialog").test_support()
            .w(px(620.)).max_w_full().max_h_full().overflow_y_scroll().p_4().gap_3()
            .rounded_lg().border_1().border_color(cx.theme().border)
            .bg(cx.theme().background).text_color(cx.theme().foreground).shadow_lg()
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(heading))
            .child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child(copyable_text("bulk-restart-summary", message)))
            .child(table)
            .when(self.finished && failed > 0, |view| view.child(div().text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(crate::i18n::text("Failed targets remain selected when still available. Review their errors before retrying."))))
            .child(footer)
    }
}
