//! Resource-specific reviewed runs, with live Job progress and related Pods.
use beacon_kube::{
    ClusterSession, DynamicObject, Kind, ObjectRef, Operation,
    jobs::{self, Plan},
};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use std::sync::Arc;

use crate::{bridge::Bridge, copyable_text::copyable_text, detail::OwnerRequested};

pub(crate) enum JobActionEvent {
    Closed,
    Open { kind: Arc<Kind>, target: ObjectRef },
}

pub(crate) struct JobActions {
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    target: ObjectRef,
    uid: Option<String>,
    operation: Operation,
    plan: Option<Plan>,
    loading: bool,
    busy: bool,
    error: Option<String>,
    result: Option<Arc<DynamicObject>>,
    related: Option<Entity<crate::related::RelatedView>>,
    _task: Option<Task<()>>,
    _poll: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<JobActionEvent> for JobActions {}

impl JobActions {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        kind: Arc<Kind>,
        target: ObjectRef,
        uid: Option<String>,
        operation: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            session,
            kind,
            target,
            uid,
            operation,
            plan: None,
            loading: false,
            busy: false,
            error: None,
            result: None,
            related: None,
            _task: None,
            _poll: None,
            _subscriptions: vec![],
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
        let (session, kind, target, uid, operation) = (
            self.session.clone(),
            (*self.kind).clone(),
            self.target.clone(),
            self.uid.clone(),
            self.operation.clone(),
        );
        let preparing = Bridge::global(cx).run_cancellable(async move {
            jobs::prepare(session, kind, target, uid, operation)
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
        if self.busy || self.result.is_some() {
            return;
        }
        let Some(plan) = self.plan.take() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let session = self.session.clone();
        // Once confirmed a write is allowed to finish; closing a window must
        // not abort an ambiguous POST and invite an accidental second run.
        let running = Bridge::global(cx).run(async move {
            jobs::execute(session, &plan)
                .await
                .map_err(|error| error.user_message())
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = running.await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.busy = false;
                match result {
                    Ok(Ok(object)) => {
                        let object = Arc::new(object);
                        view.result = Some(object.clone());
                        if view.operation == Operation::RunJob {
                            view.track(object, window, cx);
                        }
                    }
                    Ok(Err(error)) => view.error = Some(error),
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn track(&mut self, object: Arc<DynamicObject>, window: &mut Window, cx: &mut Context<Self>) {
        let relationship =
            beacon_kube::relationships::Relationship::for_kind("batch", "Job").unwrap();
        let related = cx.new(|cx| {
            crate::related::RelatedView::new(
                self.session.clone(),
                object.clone(),
                relationship,
                window,
                cx,
            )
        });
        self._subscriptions
            .push(cx.subscribe(&related, |_, _, event: &OwnerRequested, cx| {
                cx.emit(JobActionEvent::Open {
                    kind: event.kind.clone(),
                    target: event.target.clone(),
                });
            }));
        self.related = Some(related);
        let session = self.session.clone();
        let resource = beacon_kube::ApiResource::from_gvk(&beacon_kube::GroupVersionKind::gvk(
            "batch", "v1", "Job",
        ));
        let target = ObjectRef::of(&object);
        let uid = object.metadata.uid.clone();
        self._poll = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
                let (session, resource, target, uid) = (
                    session.clone(),
                    resource.clone(),
                    target.clone(),
                    uid.clone(),
                );
                let polling = this.update(cx, |_, cx| {
                    Bridge::global(cx).run_cancellable(async move {
                        let current = session
                            .get_object(resource, target.namespace, target.name)
                            .await
                            .map_err(|error| error.user_message())?;
                        if current.metadata.uid != uid {
                            return Err(
                                "The Job was replaced. Open Jobs to inspect the new resource."
                                    .to_owned(),
                            );
                        }
                        Ok(current)
                    })
                });
                let Ok(polling) = polling else {
                    break;
                };
                let result = polling.result().await;
                if this
                    .update(cx, |view, cx| {
                        match result {
                            Ok(Ok(object)) => {
                                let object = Arc::new(object);
                                if let Some(related) = &view.related {
                                    related.update(cx, |view, cx| view.refresh(object.clone(), cx));
                                }
                                view.result = Some(object);
                                view.error = None;
                            }
                            Ok(Err(error)) => {
                                view.error = Some(
                                    crate::i18n::message(
                                        "messages.job_status_error",
                                        &[("error", (error).to_string())],
                                    )
                                    .to_string(),
                                )
                            }
                            Err(error) => {
                                view.error = Some(
                                    crate::i18n::message(
                                        "messages.job_status_error",
                                        &[("error", (error).to_string())],
                                    )
                                    .to_string(),
                                )
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn open_result(&self, cx: &mut Context<Self>) {
        let Some(result) = &self.result else {
            return;
        };
        let kind = if self.operation == Operation::RunJob {
            self.session
                .discovery()
                .kinds()
                .iter()
                .find(|kind| kind.resource.group == "batch" && kind.resource.kind == "Job")
                .cloned()
                .map(Arc::new)
        } else {
            Some(self.kind.clone())
        };
        if let Some(kind) = kind {
            cx.emit(JobActionEvent::Open {
                kind,
                target: ObjectRef::of(result),
            });
        }
    }

    fn open_logs(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(result) = &self.result else {
            return;
        };
        crate::aggregate_logs::open(
            self.session.clone(),
            vec![result.metadata.namespace.clone()],
            jobs::pod_selector(result),
            window,
            cx,
        );
    }

    fn description(&self) -> &'static str {
        match self.operation {
            Operation::RunJob if self.kind.resource.kind == "CronJob" => {
                "Create a Job from the current template. The schedule and existing Jobs are unchanged. This manual run can overlap scheduled executions."
            }
            Operation::RunJob => {
                "Create a new Job with fresh Pods. The original Job, its Pods and execution history are kept."
            }
            Operation::CronJobSuspend { suspend: true } => {
                "Pause future scheduled executions. Jobs already running will continue."
            }
            Operation::CronJobSuspend { suspend: false } => {
                "Resume scheduled executions. Missed schedules may start immediately, depending on the starting deadline."
            }
            _ => "",
        }
    }
}

impl Render for JobActions {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_job = self.result.is_some() && self.operation == Operation::RunJob;
        let complete = self.result.is_some();
        let title = if has_job {
            "Job created".to_owned()
        } else if complete {
            "Schedule updated".to_owned()
        } else if self.operation == Operation::RunJob && self.kind.resource.kind == "Job" {
            "Rerun as a new Job".to_owned()
        } else if self.operation == Operation::RunJob {
            "Run CronJob now".to_owned()
        } else {
            self.operation.describe()
        };
        let confirm = match self.operation {
            Operation::RunJob => "Create Job",
            Operation::CronJobSuspend { suspend: true } => "Suspend schedule",
            _ => "Resume schedule",
        };
        v_flex().id("job-action-dialog").test_support().w(px(if has_job { 880. } else { 580. })).max_h(relative(0.9)).min_h_0().p_5().gap_3().rounded_lg().border_1().border_color(cx.theme().border).bg(cx.theme().background).shadow_lg()
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(crate::i18n::text(title)))
            .child(copyable_text("job-action-source", format!("{} · {}/{}", self.session.id().display_name(), self.target.namespace.as_deref().unwrap_or("default"), self.target.name)))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(self.description()))
            .children(self.loading.then(|| div().id("job-action-preparing").test_support().text_sm().child(crate::i18n::text("Checking permissions and validating the operation…"))))
            .children(self.plan.as_ref().and_then(|plan| plan.manifest.as_ref()).map(|manifest| {
                let containers = manifest.pointer("/spec/template/spec/containers").and_then(serde_json::Value::as_array).map(|containers| containers.iter().filter_map(|container| container.get("image").and_then(serde_json::Value::as_str)).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                v_flex().id("job-action-preview").test_support().p_3().gap_2().border_1().border_color(cx.theme().border).rounded_md().bg(cx.theme().muted.opacity(0.3))
                    .child(copyable_text("job-action-new-name", crate::i18n::message("messages.new_job_generated", &[("value0", (manifest.pointer("/metadata/generateName").and_then(serde_json::Value::as_str).unwrap_or("")).to_string())]).to_string()))
                    .child(copyable_text("job-action-images", format!("Images: {containers}")))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(crate::i18n::text("Server validation passed. Review the source's YAML for the full template.")))
            }))
            .children(self.error.clone().map(|error| div().id("job-action-failed").test_support().text_sm().text_color(cx.theme().danger).child(copyable_text("job-action-error", error))))
            .children(self.busy.then(|| div().id("job-action-running").test_support().text_sm().child(crate::i18n::text("Applying the confirmed operation…"))))
            .children(self.result.as_ref().map(|result| {
                v_flex().gap_2().p_3().border_1().border_color(cx.theme().border).rounded_md()
                    .child(copyable_text("job-action-result", ObjectRef::of(result).to_string()))
                    .child(div().id("job-action-status").test_support().text_sm().child(copyable_text("job-action-status-value", if has_job { jobs::status(result) } else if result.data.pointer("/spec/suspend").and_then(serde_json::Value::as_bool).unwrap_or(false) { "Schedule suspended. Existing Jobs continue.".into() } else { "Schedule resumed.".into() })))
                    .child(h_flex().gap_2()
                        .child(Button::new("job-action-open").small().label(crate::i18n::text(if has_job { "Open Job" } else { "Open CronJob" })).on_click(cx.listener(|view,_,_,cx| view.open_result(cx))))
                        .children(has_job.then(|| Button::new("job-action-logs").small().label(crate::i18n::text("View logs")).on_click(cx.listener(|view,_,window,cx| view.open_logs(window,cx))))))
            }))
            .children(self.related.clone().map(|related| div().h(px(280.)).min_h_0().overflow_hidden().border_1().border_color(cx.theme().border).rounded_md().child(related)))
            .child(h_flex().gap_2().justify_end()
                .children((self.error.is_some() && !complete).then(|| Button::new("job-action-retry").small().label(crate::i18n::text("Review again")).disabled(self.busy || self.loading).on_click(cx.listener(|view,_,window,cx| view.load(window,cx)))))
                .child(Button::new("job-action-cancel").ghost().small().label(crate::i18n::text(if complete { "Done" } else { "Cancel" })).disabled(self.busy).on_click(cx.listener(|_,_,_,cx| cx.emit(JobActionEvent::Closed))))
                .children((!complete).then(|| Button::new("job-action-confirm").primary().small().label(confirm).disabled(self.plan.is_none() || self.busy || self.loading).on_click(cx.listener(|view,_,window,cx| view.confirm(window,cx))))))
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;

    fn cron() -> serde_json::Value {
        json!({"apiVersion":"batch/v1","kind":"CronJob","metadata":{"name":"schedule","namespace":"default","uid":"cron-uid","resourceVersion":"1"},"spec":{"schedule":"* * * * *","jobTemplate":{"spec":{"template":{"spec":{"restartPolicy":"Never","containers":[{"name":"app","image":"busybox"}]}}}}}})
    }
    fn open(
        cx: &mut TestAppContext,
        session: Arc<ClusterSession>,
        operation: Operation,
    ) -> (AnyWindowHandle, Entity<JobActions>) {
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    JobActions::new(
                        session,
                        Arc::new(support::kind("batch", "CronJob", "cronjobs")),
                        ObjectRef::new(Some("default".into()), "schedule"),
                        Some("cron-uid".into()),
                        operation,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        (window, view)
    }

    #[::core::prelude::v1::test]
    fn manual_job_is_reviewed_tracks_status_and_pods_and_opens_its_logs() {
        let cx = &mut support::context();
        let (fixture, session) = support::fixture(cx, "manual-job-ui", vec![cron()]);
        let (window, view) = open(cx, session, Operation::RunJob);
        support::settle(cx, |cx| view.read_with(cx, |view, _| !view.loading));
        assert!(view.read_with(cx, |view, _| view.plan.is_some() && view.error.is_none()));
        assert_eq!(fixture.object("CronJob", "schedule"), cron());
        assert_eq!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(
                    |(line, _)| line.starts_with("POST /apis/batch/v1/namespaces/default/jobs")
                        && !line.contains("dryRun")
                )
                .count(),
            0
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("job-action-preview").visible());
            window.click("job-action-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.result.is_some()));
        support::settle(cx, |_| fixture.watchers() > 0);
        let mut created = view.read_with(cx, |view, _| {
            serde_json::to_value(&*view.result.as_ref().unwrap().clone()).unwrap()
        });
        let name = created["metadata"]["name"].as_str().unwrap().to_owned();
        let uid = created["metadata"]["uid"].as_str().unwrap().to_owned();
        let pod = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"manual-pod","namespace":"default","uid":"pod-uid","resourceVersion":"1","labels":{"batch.kubernetes.io/controller-uid":uid,"batch.kubernetes.io/job-name":name},"ownerReferences":[{"apiVersion":"batch/v1","kind":"Job","name":name,"uid":uid,"controller":true}]},"spec":{"containers":[{"name":"app","image":"busybox"}]},"status":{"phase":"Running","containerStatuses":[{"name":"app","restartCount":0,"state":{"running":{}}}]}});
        fixture.put(pod);
        created["metadata"]["resourceVersion"] = json!("2");
        created["status"] = json!({"active":1});
        fixture.put(created);
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| {
                view.result
                    .as_ref()
                    .is_some_and(|object| jobs::status(object).starts_with("Running"))
            })
        });
        support::render_until(cx, window, |window| {
            window
                .try_find(("related-table", 0usize))
                .is_some_and(|item| item.visible())
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("job-action-status").visible());
            assert!(window.find("job-action-open").visible());
            window.click("job-action-logs", cx);
        })
        .unwrap();
        support::settle(cx, |_| {
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.contains("/pods/manual-pod/log"))
        });
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.contains("labelSelector")
                    && line.contains("batch.kubernetes.io%2Fcontroller-uid"))
        );
        assert_eq!(fixture.object("CronJob", "schedule"), cron());
    }

    #[::core::prelude::v1::test]
    fn rerunning_a_completed_job_creates_fresh_pods_and_preserves_its_history() {
        let cx = &mut support::context();
        let original = json!({"apiVersion":"batch/v1","kind":"Job","metadata":{"name":"completed","namespace":"default","uid":"old-uid","resourceVersion":"3","labels":{"batch.kubernetes.io/job-name":"completed"}},"spec":{"selector":{"matchLabels":{"batch.kubernetes.io/controller-uid":"old-uid"}},"template":{"metadata":{"labels":{"batch.kubernetes.io/controller-uid":"old-uid","app":"worker"}},"spec":{"restartPolicy":"Never","containers":[{"name":"app","image":"busybox"}]}}},"status":{"succeeded":1,"conditions":[{"type":"Complete","status":"True"}]}});
        let (fixture, session) = support::fixture(cx, "rerun-job-ui", vec![original.clone()]);
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    JobActions::new(
                        session,
                        Arc::new(support::kind("batch", "Job", "jobs")),
                        ObjectRef::new(Some("default".into()), "completed"),
                        Some("old-uid".into()),
                        Operation::RunJob,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.plan.is_some()));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("job-action-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| view.read_with(cx, |view, _| view.result.is_some()));
        assert_eq!(fixture.object("Job", "completed"), original);
        view.read_with(cx, |view, _| {
            let rerun = view.result.as_ref().unwrap();
            assert!(
                rerun
                    .metadata
                    .name
                    .as_deref()
                    .unwrap()
                    .starts_with("completed-rerun-")
            );
            assert_ne!(rerun.metadata.uid.as_deref(), Some("old-uid"));
            assert_eq!(
                rerun.data.pointer("/spec/template/metadata/labels/app"),
                Some(&json!("worker"))
            );
            assert_ne!(
                rerun
                    .data
                    .pointer("/spec/selector/matchLabels/batch.kubernetes.io~1controller-uid"),
                Some(&json!("old-uid"))
            );
        });
        assert!(
            !fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.starts_with("DELETE") || line.starts_with("PATCH"))
        );
    }

    #[::core::prelude::v1::test]
    fn permissions_failures_cancellation_and_schedule_confirmation_are_visible() {
        let cx = &mut support::context();
        let (fixture, session) = support::fixture(cx, "denied-job-ui", vec![cron()]);
        fixture.rules(json!([{"verbs":["patch"],"apiGroups":["batch"],"resources":["cronjobs"]}]));
        let (window, view) = open(cx, session.clone(), Operation::RunJob);
        support::settle(cx, |cx| view.read_with(cx, |view, _| !view.loading));
        assert!(view.read_with(cx, |view, _| {
            view.error
                .as_ref()
                .unwrap()
                .contains("create on batch/jobs")
        }));
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("job-action-failed").visible());
            window.click("job-action-confirm", cx);
        })
        .unwrap();
        assert!(
            !fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.starts_with("POST /apis/batch/"))
        );
        let (schedule_window, schedule) =
            open(cx, session, Operation::CronJobSuspend { suspend: true });
        support::settle(cx, |cx| {
            schedule.read_with(cx, |view, _| view.plan.is_some())
        });
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = closed.clone();
        cx.update(|cx| {
            cx.subscribe(&schedule, move |_, event, cx| {
                let _ = cx;
                if matches!(event, JobActionEvent::Closed) {
                    observed.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            })
            .detach()
        });
        cx.update_window(schedule_window, |_, window, cx| {
            window.render_frame(cx);
            window.click("job-action-cancel", cx);
        })
        .unwrap();
        assert!(closed.load(std::sync::atomic::Ordering::Relaxed));
        assert!(
            !fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.starts_with("PATCH"))
        );
        cx.update_window(schedule_window, |_, window, cx| {
            window.render_frame(cx);
            window.click("job-action-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            schedule.read_with(cx, |view, _| view.result.is_some())
        });
        assert_eq!(
            fixture.object("CronJob", "schedule")["spec"]["suspend"],
            true
        );
        assert!(schedule.read_with(cx, |view, _| view.plan.is_none() && !view.busy));

        let (other, other_session) = support::fixture(cx, "job-write-failure", vec![cron()]);
        let (other_window, other_view) = open(cx, other_session, Operation::RunJob);
        support::settle(cx, |cx| {
            other_view.read_with(cx, |view, _| view.plan.is_some())
        });
        other.deny("jobs");
        cx.update_window(other_window, |_, window, cx| {
            window.render_frame(cx);
            window.click("job-action-confirm", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            other_view.read_with(cx, |view, _| view.error.is_some() && !view.busy)
        });
        assert!(other_view.read_with(cx, |view, _| view.result.is_none()
            && view.error.as_ref().unwrap().contains("HTTP 403")));
        cx.update_window(other_window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("job-action-failed").visible());
            assert!(window.find("job-action-retry").visible());
        })
        .unwrap();
    }
}
