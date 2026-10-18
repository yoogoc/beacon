//! Node maintenance is reviewed against a fixed Pod snapshot, with independent
//! eviction progress and explicit acknowledgement of emptyDir/unmanaged Pods.
use crate::{
    bridge::{Bridge, drain_into},
    copyable_text::copyable_text,
    selection::checkbox,
    theme::{BeaconTheme as _, Tone},
};
use beacon_kube::{
    ClusterSession, Kind, ObjectRef, Operation,
    node_maintenance::{
        self as maintenance, Cancellation, Options, Plan, PodState, Progress, Report,
    },
};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;

pub(crate) enum NodeEvent {
    Closed,
    Open { kind: Arc<Kind>, target: ObjectRef },
}
enum Completed {
    Scheduling(bool),
    Drain(Report),
}

pub(crate) struct NodeActions {
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    target: ObjectRef,
    uid: Option<String>,
    operation: Operation,
    plan: Option<Plan>,
    rows: Vec<PodState>,
    scroll: ScrollHandle,
    allow_empty: bool,
    force: bool,
    timeout: Entity<InputState>,
    grace: Entity<InputState>,
    cancellation: Cancellation,
    loading: bool,
    busy: bool,
    cordoned: bool,
    done: bool,
    report: Option<Report>,
    error: Option<String>,
    _task: Option<Task<()>>,
    _progress: Option<Task<()>>,
    _inputs: Vec<Subscription>,
}
impl EventEmitter<NodeEvent> for NodeActions {}
impl Drop for NodeActions {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl NodeActions {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        kind: Arc<Kind>,
        target: ObjectRef,
        uid: Option<String>,
        operation: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let timeout = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value("300")
                .placeholder("Timeout seconds")
        });
        let grace = cx.new(|cx| crate::i18n::input("Pod default", window, cx));
        let inputs = vec![
            cx.subscribe(&timeout, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&grace, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        let mut view = Self {
            session,
            kind,
            target,
            uid,
            operation,
            plan: None,
            rows: vec![],
            scroll: ScrollHandle::new(),
            allow_empty: false,
            force: false,
            timeout,
            grace,
            cancellation: Default::default(),
            loading: false,
            busy: false,
            cordoned: false,
            done: false,
            report: None,
            error: None,
            _task: None,
            _progress: None,
            _inputs: inputs,
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
        self.rows.clear();
        self.done = false;
        self.report = None;
        let (session, node, uid, drain) = (
            self.session.clone(),
            self.target.name.clone(),
            self.uid.clone(),
            self.operation == Operation::NodeDrain,
        );
        let loading = Bridge::global(cx).run_cancellable(async move {
            maintenance::prepare(session, node, uid, drain)
                .await
                .map_err(|error| error.user_message())
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = loading.result().await;
            let _ = this.update(cx, |view, cx| {
                view.loading = false;
                match result {
                    Ok(Ok(plan)) => {
                        view.cordoned = plan.was_cordoned;
                        view.rows = plan
                            .pods
                            .iter()
                            .map(|pod| {
                                pod.skipped.as_ref().map_or_else(|| {
                                    if !pod.allowed {PodState::Failed("Requires create on pods/eviction in this namespace".into())}
                                    else if pod.uid.is_empty() {PodState::Failed("Pod UID is missing; refresh the preview".into())}
                                    else {PodState::Pending}
                                }, |reason|PodState::Skipped(reason.clone()))
                            })
                            .collect();
                        view.plan = Some(plan);
                    }
                    Ok(Err(error)) => view.error = Some(error),
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn options(&self, cx: &App) -> Result<Options, String> {
        let timeout = self
            .timeout
            .read(cx)
            .value()
            .trim()
            .parse::<u64>()
            .map_err(|_| "Enter a timeout in seconds.".to_owned())?;
        if !(1..=86400).contains(&timeout) {
            return Err("Timeout must be between 1 and 86400 seconds.".into());
        }
        let grace = self.grace.read(cx).value();
        let grace_seconds = if grace.trim().is_empty() {
            None
        } else {
            Some(grace.trim().parse::<u32>().map_err(|_| {
                "Grace period must be zero or a positive number of seconds.".to_owned()
            })?)
        };
        Ok(Options {
            allow_empty_dir: self.allow_empty,
            force_unmanaged: self.force,
            timeout: std::time::Duration::from_secs(timeout),
            grace_seconds,
            ..Default::default()
        })
    }
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.loading || self.done {
            return;
        }
        let Some(plan) = self.plan.clone() else {
            return;
        };
        let options = match self.options(cx) {
            Ok(options) => options,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        if self.operation == Operation::NodeDrain
            && let Err(error) = plan.validate(&options)
        {
            self.error = Some(error.user_message());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        self.cancellation = Default::default();
        let (session, operation, cancellation) = (
            self.session.clone(),
            self.operation.clone(),
            self.cancellation.clone(),
        );
        let (sender, receiver) = futures::channel::mpsc::unbounded();
        self._progress = Some(drain_into(
            cx,
            receiver,
            |view, event, _, cx| {
                if view.done {
                    return;
                }
                match event {
                    Progress::Cordoned => view.cordoned = true,
                    Progress::Pod { index, state } => {
                        if let Some(row) = view.rows.get_mut(index) {
                            *row = state;
                        }
                    }
                }
                cx.notify();
            },
            window,
        ));
        let running = Bridge::global(cx).run(async move {
            match operation {
                Operation::NodeDrain => {
                    maintenance::drain(session, plan, options, cancellation, sender)
                        .await
                        .map(Completed::Drain)
                        .map_err(|error| error.user_message())
                }
                Operation::NodeCordon { unschedulable } => {
                    maintenance::cordon(session, &plan, unschedulable)
                        .await
                        .map(|_| Completed::Scheduling(unschedulable))
                        .map_err(|error| error.user_message())
                }
                _ => Err("Unsupported Node operation.".into()),
            }
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = running.await;
            let _ = this.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok(Ok(Completed::Scheduling(cordoned))) => {
                        view.cordoned = cordoned;
                        view.done = true;
                    }
                    Ok(Ok(Completed::Drain(report))) => {
                        view.cordoned = true;
                        view.rows = report.states.clone();
                        view.done = true;
                        view.report = Some(report);
                    }
                    Ok(Err(error)) => view.error = Some(error),
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn stop(&mut self, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        cx.notify();
    }
    fn open(&self, kind: Arc<Kind>, target: ObjectRef, cx: &mut Context<Self>) {
        if !self.busy {
            cx.emit(NodeEvent::Open { kind, target });
        }
    }
    fn render_pods(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let plan = self.plan.as_ref()?;
        if self.operation != Operation::NodeDrain {
            return None;
        }
        let mut header = h_flex()
            .w(px(1110.))
            .gap_3()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().table_header())
            .text_xs()
            .text_color(cx.theme().muted_foreground);
        for (name, width) in [
            ("Pod / namespace", 180.),
            ("Controller", 140.),
            ("Local storage", 250.),
            ("Disruption budgets", 230.),
            ("Result", 250.),
        ] {
            header = header.child(div().w(px(width)).flex_shrink_0().child(name));
        }
        let rows = plan.pods.iter().enumerate().map(|(index, pod)| {
            let state = self.rows.get(index).cloned().unwrap_or(PodState::Pending);
            let tone = match state {
                PodState::Evicted => Tone::Healthy,
                PodState::Failed(_) => Tone::Critical,
                PodState::Blocked(_) | PodState::Cancelled => Tone::Warning,
                PodState::Evicting | PodState::Terminating => Tone::Progressing,
                _ => Tone::Unknown,
            };
            let pod_kind = self
                .session
                .discovery()
                .kinds()
                .iter()
                .find(|kind| kind.resource.group.is_empty() && kind.resource.kind == "Pod")
                .cloned()
                .map(Arc::new);
            let target = pod.target.clone();
            let budgets = if pod.budgets.is_empty() {
                if pod.budgets_known {
                    "No matching PDB".into()
                } else {
                    "Budget information unavailable".into()
                }
            } else {
                pod.budgets
                    .iter()
                    .map(|budget| {
                        format!(
                            "{} · {}{}",
                            budget.target.name,
                            budget
                                .disruptions
                                .map_or("unknown allowance".into(), |count| format!(
                                    "{count} disruptions allowed"
                                )),
                            if budget.blocked {
                                " · currently blocked"
                            } else {
                                ""
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            h_flex()
                .id(("node-drain-pod", index))
                .test_support()
                .w(px(1110.))
                .gap_3()
                .px_3()
                .py_2()
                .items_start()
                .border_b_1()
                .border_color(cx.theme().border)
                .text_sm()
                .child(
                    v_flex()
                        .w(px(180.))
                        .flex_shrink_0()
                        .gap_1()
                        .child(
                            div()
                                .id(("node-drain-open-pod", index))
                                .cursor_pointer()
                                .text_color(cx.theme().resource_link())
                                .font_weight(FontWeight::MEDIUM)
                                .child(pod.target.name.clone())
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    if let Some(kind) = &pod_kind {
                                        view.open(kind.clone(), target.clone(), cx);
                                    }
                                })),
                        )
                        .child(copyable_text(
                            ("node-drain-namespace", index),
                            pod.target.namespace.clone().unwrap_or_default(),
                        )),
                )
                .child(div().w(px(140.)).flex_shrink_0().child(copyable_text(
                    ("node-drain-controller", index),
                    pod.owner.clone(),
                )))
                .child(
                    div()
                        .w(px(250.))
                        .flex_shrink_0()
                        .text_color(if pod.storage.is_empty() {
                            cx.theme().muted_foreground
                        } else {
                            cx.theme().warning
                        })
                        .child(copyable_text(
                            ("node-drain-storage", index),
                            if pod.storage.is_empty() {
                                "—".into()
                            } else {
                                pod.storage.join("\n")
                            },
                        )),
                )
                .child(
                    div()
                        .w(px(230.))
                        .flex_shrink_0()
                        .text_color(if pod.budgets.iter().any(|budget| budget.blocked) {
                            cx.theme().warning
                        } else {
                            cx.theme().muted_foreground
                        })
                        .child(copyable_text(("node-drain-budgets", index), budgets)),
                )
                .child(
                    div()
                        .w(px(250.))
                        .flex_shrink_0()
                        .text_color(cx.theme().tone(tone))
                        .child(copyable_text(("node-drain-result", index), state.label())),
                )
        });
        Some(
            v_flex()
                .w_full()
                .min_h_0()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(format!(
                            "{} Pods on this Node · DaemonSet and mirror Pods are kept",
                            plan.pods.len()
                        )),
                )
                .child(
                    div()
                        .id("node-drain-table")
                        .test_support()
                        .w_full()
                        .h(px(270.))
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded_md()
                        .relative()
                        .child(
                            div()
                                .id("node-drain-viewport")
                                .size_full()
                                .overflow_scroll()
                                .track_scroll(&self.scroll)
                                .child(v_flex().child(header).children(rows)),
                        )
                        .child(Scrollbar::horizontal(&self.scroll).mode(ScrollbarMode::Always))
                        .child(Scrollbar::vertical(&self.scroll)),
                )
                .when(plan.pods.is_empty(), |view| {
                    view.child(
                        div()
                            .text_sm()
                            .child(crate::i18n::text("No Pods are scheduled to this Node.")),
                    )
                })
                .into_any_element(),
        )
    }
}

impl Render for NodeActions {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let drain = self.operation == Operation::NodeDrain;
        let description = if drain {
            "Cordon this Node, then evict the reviewed Pods through the Eviction API. Budgets are enforced by the server. Cancellation leaves the Node cordoned and cannot undo submitted evictions."
        } else if self.operation
            == (Operation::NodeCordon {
                unschedulable: true,
            })
        {
            "Prevent new workloads from being scheduled here. Existing Pods keep running."
        } else {
            "Allow new workloads to be scheduled to this Node again."
        };
        let ready = self.plan.as_ref().is_some_and(|plan| {
            !drain
                || self
                    .options(cx)
                    .ok()
                    .is_some_and(|options| plan.validate(&options).is_ok())
        }) && !self.loading
            && !self.busy
            && !self.done;
        let local = self.plan.as_ref().is_some_and(|plan| {
            plan.pods
                .iter()
                .any(|pod| pod.skipped.is_none() && !pod.empty_dir.is_empty())
        });
        let unmanaged = self.plan.as_ref().is_some_and(|plan| {
            plan.pods
                .iter()
                .any(|pod| pod.skipped.is_none() && pod.unmanaged)
        });
        v_flex()
            .id("node-maintenance-dialog")
            .test_support()
            .w(px(if drain { 980. } else { 560. }))
            .max_w(relative(0.94))
            .max_h(relative(0.94))
            .min_h_0()
            .p_5()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_lg()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.operation.describe()),
            )
            .child(copyable_text(
                "node-maintenance-target",
                format!(
                    "{} · {}",
                    self.session.id().display_name(),
                    self.target.name
                ),
            ))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(description),
            )
            .children(self.loading.then(|| {
                div()
                    .id("node-maintenance-loading")
                    .test_support()
                    .text_sm()
                    .child(crate::i18n::text(
                        "Checking permissions, Pods, storage and disruption budgets…",
                    ))
            }))
            .children(self.render_pods(cx))
            .children(
                self.plan
                    .as_ref()
                    .map(|plan| {
                        plan.warnings
                            .iter()
                            .enumerate()
                            .map(|(index, warning)| {
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().warning)
                                    .child(copyable_text(
                                        ("node-maintenance-warning", index),
                                        warning.clone(),
                                    ))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
            )
            .children((drain && !self.done).then(|| {
                v_flex()
                    .gap_2()
                    .children(local.then(|| {
                        checkbox("node-drain-emptydir")
                            .checked(self.allow_empty)
                            .disabled(self.busy)
                            .label(crate::i18n::text(
                                "Allow deletion of the emptyDir data listed above",
                            ))
                            .on_click(cx.listener(|view, value, _, cx| {
                                view.allow_empty = *value;
                                cx.notify();
                            }))
                    }))
                    .children(unmanaged.then(|| {
                        checkbox("node-drain-unmanaged")
                            .checked(self.force)
                            .disabled(self.busy)
                            .label(crate::i18n::text(
                                "Evict Pods without a controller; they may not be recreated",
                            ))
                            .on_click(cx.listener(|view, value, _, cx| {
                                view.force = *value;
                                cx.notify();
                            }))
                    }))
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                div()
                                    .text_sm()
                                    .child(crate::i18n::text("Timeout (seconds)")),
                            )
                            .child(
                                Input::new(&self.timeout)
                                    .small()
                                    .w(px(90.))
                                    .disabled(self.busy),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .child(crate::i18n::text("Grace period (seconds)")),
                            )
                            .child(
                                Input::new(&self.grace)
                                    .small()
                                    .w(px(130.))
                                    .disabled(self.busy),
                            ),
                    )
            }))
            .children(self.cordoned.then(|| {
                div()
                    .id("node-maintenance-cordoned")
                    .test_support()
                    .text_sm()
                    .text_color(cx.theme().warning)
                    .child(crate::i18n::text(
                        "Node is cordoned. Uncordon it explicitly when maintenance is finished.",
                    ))
            }))
            .children(self.error.clone().map(|error| {
                div()
                    .id("node-maintenance-error")
                    .test_support()
                    .text_sm()
                    .text_color(cx.theme().danger)
                    .child(copyable_text("node-maintenance-error-value", error))
            }))
            .children(self.report.as_ref().map(|report| {
                div()
                    .id("node-drain-summary")
                    .test_support()
                    .text_sm()
                    .text_color(if report.complete() {
                        cx.theme().success
                    } else {
                        cx.theme().warning
                    })
                    .child(copyable_text("node-drain-summary-value", report.summary()))
            }))
            .children(
                (!self.loading && !self.busy && !self.done && drain)
                    .then(|| {
                        self.options(cx)
                            .and_then(|options| {
                                self.plan.as_ref().map_or(Ok(()), |plan| {
                                    plan.validate(&options)
                                        .map_err(|error| error.user_message())
                                })
                            })
                            .err()
                    })
                    .flatten()
                    .map(|message| {
                        div()
                            .id("node-drain-blocked")
                            .test_support()
                            .text_sm()
                            .text_color(cx.theme().warning)
                            .child(copyable_text("node-drain-blocked-value", message))
                    }),
            )
            .children(self.busy.then(|| {
                div()
                    .id("node-maintenance-running")
                    .test_support()
                    .text_sm()
                    .child(if drain {
                        "Drain is in progress. Each Pod result is shown above."
                    } else {
                        "Updating Node scheduling…"
                    })
            }))
            .children((self.done && !drain).then(|| {
                div()
                    .id("node-maintenance-result")
                    .test_support()
                    .text_sm()
                    .text_color(cx.theme().success)
                    .child(if self.cordoned {
                        "Node cordoned. Existing Pods keep running."
                    } else {
                        "Node uncordoned. Scheduling is enabled."
                    })
            }))
            .children((self.busy && self.cancellation.cancelled()).then(|| {
                div().text_sm().child(crate::i18n::text(
                    "Cancellation requested. Waiting for submitted requests to finish…",
                ))
            }))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .children((drain && self.busy).then(|| {
                        Button::new("node-drain-stop")
                            .small()
                            .label(crate::i18n::text("Stop draining"))
                            .disabled(self.cancellation.cancelled())
                            .on_click(cx.listener(|view, _, _, cx| view.stop(cx)))
                    }))
                    .children((self.error.is_some() || self.done).then(|| {
                        Button::new("node-maintenance-review")
                            .small()
                            .label(crate::i18n::text("Review again"))
                            .disabled(self.busy)
                            .on_click(cx.listener(|view, _, window, cx| view.load(window, cx)))
                    }))
                    .children(self.done.then(|| {
                        Button::new("node-maintenance-open")
                            .small()
                            .label(crate::i18n::text("Open Node"))
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.open(view.kind.clone(), view.target.clone(), cx)
                            }))
                    }))
                    .child(
                        Button::new("node-maintenance-close")
                            .ghost()
                            .small()
                            .label(crate::i18n::text(if self.done { "Done" } else { "Cancel" }))
                            .disabled(self.busy)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NodeEvent::Closed))),
                    )
                    .children((!self.done).then(|| {
                        Button::new("node-maintenance-confirm")
                            .primary()
                            .small()
                            .label(if drain {
                                "Cordon and drain"
                            } else if self.operation
                                == (Operation::NodeCordon {
                                    unschedulable: true,
                                })
                            {
                                "Cordon"
                            } else {
                                "Uncordon"
                            })
                            .disabled(!ready)
                            .on_click(cx.listener(|view, _, window, cx| view.confirm(window, cx)))
                    })),
            )
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    fn node() -> serde_json::Value {
        json!({"apiVersion":"v1","kind":"Node","metadata":{"name":"node-a","uid":"node-uid","resourceVersion":"1"},"spec":{"unschedulable":false},"status":{"capacity":{"cpu":"4"}}})
    }
    fn pod(name: &str) -> serde_json::Value {
        json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"default","uid":format!("uid-{name}"),"resourceVersion":"1","labels":{"app":"worker"},"ownerReferences":[{"apiVersion":"apps/v1","kind":"ReplicaSet","name":"worker","uid":"rs-uid","controller":true}]},"spec":{"nodeName":"node-a","containers":[{"name":"app","image":"busybox"}]},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}]}})
    }
    fn open(
        cx: &mut TestAppContext,
        session: Arc<ClusterSession>,
        operation: Operation,
    ) -> (AnyWindowHandle, Entity<NodeActions>) {
        cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    NodeActions::new(
                        session,
                        Arc::new(support::kind("", "Node", "nodes")),
                        ObjectRef::new(None, "node-a"),
                        Some("node-uid".into()),
                        operation,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        })
    }

    #[::core::prelude::v1::test]
    fn drain_reviews_storage_and_pdb_then_stops_without_bypassing_the_budget() {
        let cx = &mut support::context();
        let mut worker = pod("worker");
        worker["spec"]["volumes"] = json!([{"name":"cache","emptyDir":{}},{"name":"host","hostPath":{"path":"/var/cache"}}]);
        let mut daemon = pod("daemon");
        daemon["metadata"]["ownerReferences"][0]["kind"] = json!("DaemonSet");
        let budget = json!({"apiVersion":"policy/v1","kind":"PodDisruptionBudget","metadata":{"name":"worker-budget","namespace":"default","uid":"budget-uid","resourceVersion":"1"},"spec":{"selector":{"matchLabels":{"app":"worker"}}},"status":{"disruptionsAllowed":0}});
        let (fixture, session) = support::fixture(
            cx,
            "node-stop-ui",
            vec![node(), worker.clone(), daemon.clone(), budget],
        );
        let (window, view) = open(cx, session, Operation::NodeDrain);
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.plan.is_some()));
        view.read_with(cx, |view, _| {
            let plan = view.plan.as_ref().unwrap();
            assert_eq!(plan.pods.len(), 2);
            assert!(plan.pods.iter().any(|pod| {
                pod.storage
                    .iter()
                    .any(|storage| storage.contains("hostPath"))
            }));
            assert!(
                plan.pods
                    .iter()
                    .any(|pod| pod.budgets.iter().any(|budget| budget.blocked))
            );
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("node-drain-table").visible());
            assert!(window.find("node-drain-blocked").visible());
            window.click("node-maintenance-confirm", cx);
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
        cx.update_window(window, |_, window, cx| {
            window.click("node-drain-emptydir", cx);
            window.render_frame(cx);
            window.click("node-maintenance-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| {
                view.busy
                    && view
                        .rows
                        .iter()
                        .any(|state| matches!(state, PodState::Blocked(_)))
            })
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("node-maintenance-cordoned").visible());
            window.click("node-drain-stop", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| !view.busy && view.report.is_some())
        });
        assert!(view.read_with(cx, |view, _| {
            view.rows
                .iter()
                .any(|state| matches!(state, PodState::Cancelled))
        }));
        assert_eq!(fixture.object("Pod", "worker"), worker);
        assert_eq!(fixture.object("Pod", "daemon"), daemon);
        assert_eq!(
            fixture.object("Node", "node-a")["spec"]["unschedulable"],
            true
        );
        assert!(
            !fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.starts_with("DELETE"))
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("node-drain-summary").visible());
        })
        .unwrap();
    }

    #[::core::prelude::v1::test]
    fn scheduling_and_a_successful_drain_show_results_and_navigation() {
        let cx = &mut support::context();
        let (fixture, session) =
            support::fixture(cx, "node-scheduling-ui", vec![node(), pod("worker")]);
        for unschedulable in [true, false] {
            let (window, view) = open(cx, session.clone(), Operation::NodeCordon { unschedulable });
            support::settle(cx, |cx| view.read_with(cx, |view, _| view.plan.is_some()));
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                window.click("node-maintenance-confirm", cx);
            })
            .unwrap();
            support::settle(cx, |cx| view.read_with(cx, |view, _| view.done));
            assert_eq!(
                fixture.object("Node", "node-a")["spec"]["unschedulable"],
                unschedulable
            );
            assert_eq!(fixture.object("Pod", "worker"), pod("worker"));
            cx.update_window(window, |_, window, cx| {
                window.render_frame(cx);
                assert!(window.find("node-maintenance-result").visible());
            })
            .unwrap();
        }
        let (window, view) = open(cx, session, Operation::NodeDrain);
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.plan.is_some()));
        let navigation = Arc::new(std::sync::Mutex::new(None));
        let requested = navigation.clone();
        cx.update(|cx| {
            cx.subscribe(&view, move |_, event: &NodeEvent, _| {
                if let NodeEvent::Open { target, .. } = event {
                    *requested.lock().unwrap() = Some(target.clone());
                }
            })
            .detach()
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("node-maintenance-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| {
                view.report.as_ref().is_some_and(Report::complete)
            })
        });
        assert!(view.read_with(cx, |view, _| view.rows == vec![PodState::Evicted]));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("node-maintenance-open", cx);
        })
        .unwrap();
        assert_eq!(
            *navigation.lock().unwrap(),
            Some(ObjectRef::new(None, "node-a"))
        );
    }

    #[::core::prelude::v1::test]
    fn denied_permissions_and_partial_server_failures_remain_reviewable() {
        let cx = &mut support::context();
        let (fixture, session) =
            support::fixture(cx, "node-denied-ui", vec![node(), pod("worker")]);
        fixture.rules(json!([{"verbs":["patch"],"apiGroups":[""],"resources":["nodes"]}]));
        let (window, view) = open(cx, session, Operation::NodeDrain);
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.plan.is_some()));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("node-drain-blocked").visible());
            window.click("node-maintenance-confirm", cx);
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
        let (other, session) =
            support::fixture(cx, "node-partial-ui", vec![node(), pod("good"), pod("bad")]);
        other.deny("pods/bad/eviction");
        let (window, view) = open(cx, session, Operation::NodeDrain);
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.plan.is_some()));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("node-maintenance-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.report.is_some()));
        assert!(view.read_with(cx, |view, _| {
            !view.report.as_ref().unwrap().complete()
                && view.rows.iter().any(
                    |state| matches!(state,PodState::Failed(message)if message.contains("HTTP 403"))
                )
                && view
                    .rows
                    .iter()
                    .any(|state| matches!(state, PodState::Evicted))
        }));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("node-drain-summary").visible());
            assert!(window.find("node-maintenance-review").visible());
        })
        .unwrap();
    }
}
