//! Revision history, reviewed rollback, and live Deployment progress.
use crate::{
    bridge::Bridge,
    copyable_text::copyable_text,
    theme::{BeaconTheme as _, Tone},
};
use beacon_kube::{
    ClusterSession, DynamicObject,
    rollout::{Revision, Rollback},
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::sync::Arc;

pub(crate) struct RolloutView {
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    revisions: Vec<Revision>,
    loading: bool,
    busy: bool,
    may_patch: bool,
    message: Option<String>,
    _task: Option<Task<()>>,
}

impl RolloutView {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        object: Arc<DynamicObject>,
        may_patch: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            session,
            object,
            revisions: vec![],
            loading: false,
            busy: false,
            may_patch,
            message: None,
            _task: None,
        };
        view.load(window, cx);
        view
    }
    pub(crate) fn busy(&self) -> bool {
        self.busy
    }
    pub(crate) fn refresh(
        &mut self,
        object: Arc<DynamicObject>,
        may_patch: bool,
        cx: &mut Context<Self>,
    ) {
        self.object = object;
        self.may_patch = may_patch;
        self.revisions = beacon_kube::rollout::revisions(
            &self.object,
            &self
                .revisions
                .iter()
                .map(|revision| revision.replica_set.clone())
                .collect::<Vec<_>>(),
        );
        cx.notify();
    }
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.loading = true;
        self.message = None;
        let session = self.session.clone();
        let object = self.object.clone();
        let fetching = Bridge::global(cx).run_cancellable(async move {
            beacon_kube::rollout::history(session, &object)
                .await
                .map_err(|error| error.user_message())
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = fetching.result().await;
            let _ = this.update(cx, |view, cx| {
                view.loading = false;
                match result {
                    Ok(Ok(revisions)) => {
                        view.revisions = beacon_kube::rollout::revisions(
                            &view.object,
                            &revisions
                                .into_iter()
                                .map(|revision| revision.replica_set)
                                .collect::<Vec<_>>(),
                        );
                    }
                    Ok(Err(error)) => view.message = Some(error),
                    Err(error) => view.message = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn review(&mut self, revision: Revision, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.message = None;
        let session = self.session.clone();
        let uid = self.object.metadata.uid.clone();
        let target = beacon_kube::ObjectRef::of(&self.object);
        let resource = beacon_kube::ApiResource::from_gvk(&beacon_kube::GroupVersionKind::gvk(
            "apps",
            "v1",
            "Deployment",
        ));
        let fetching = Bridge::global(cx).run_cancellable(async move {
            let current = session
                .get_object(resource, target.namespace, target.name)
                .await
                .map_err(|error| error.user_message())?;
            if current.metadata.uid != uid {
                return Err(
                    "This Deployment was replaced. Refresh its details before rolling back."
                        .to_string(),
                );
            }
            let plan =
                Rollback::prepare(&current, &revision).map_err(|error| error.user_message())?;
            let proposed = plan
                .proposed(&current)
                .map_err(|error| error.user_message())?;
            let before = serde_json::to_value(&current).map_err(|error| error.to_string())?;
            let preview = crate::yaml_review::Preview::apply(before, proposed)?;
            Ok::<_, String>((plan, preview))
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = fetching.result().await;
            let _ = this.update_in(cx, |view, window, cx| {
                match result {
                    Ok(Ok((plan, preview))) => {
                        let context = format!(
                            "{} · {}/{} · Roll back to revision {}",
                            view.session.id().display_name(),
                            plan.target.namespace.as_deref().unwrap_or(""),
                            plan.target.name,
                            plan.revision
                        );
                        let review =
                            crate::yaml_review::open(preview, context, false, false, window, cx);
                        review.update(cx, |review, _| {
                            review.confirmation_label("Confirm and roll back");
                            if !view.may_patch {
                                review.read_only();
                            }
                        });
                        cx.subscribe_in(
                            &review,
                            window,
                            move |view, _, event: &crate::yaml_review::ReviewEvent, window, cx| {
                                view.busy = false;
                                if matches!(event, crate::yaml_review::ReviewEvent::Confirmed(_)) {
                                    view.execute(plan.clone(), window, cx);
                                }
                                cx.notify();
                            },
                        )
                        .detach();
                    }
                    Ok(Err(error)) => {
                        view.busy = false;
                        view.message = Some(error);
                    }
                    Err(error) => {
                        view.busy = false;
                        view.message = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn execute(&mut self, plan: Rollback, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = true;
        let session = self.session.clone();
        let saving = Bridge::global(cx).run(async move {
            plan.execute(session)
                .await
                .map_err(|error| error.user_message())
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = saving.await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.busy = false;
                match result {
                    Ok(Ok(object)) => {
                        view.object = Arc::new(object);
                        view.load(window, cx);
                        view.message =
                            Some("Rollback submitted. Follow rollout progress below.".into());
                    }
                    Ok(Err(error)) => view.message = Some(error),
                    Err(error) => view.message = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Render for RolloutView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let data = &self.object.data;
        let desired = data
            .pointer("/spec/replicas")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1);
        let count = |field: &str| {
            data.pointer(&format!("/status/{field}"))
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        let observed = data
            .pointer("/status/observedGeneration")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let converged = observed >= self.object.metadata.generation.unwrap_or(0)
            && count("updatedReplicas") == desired
            && count("availableReplicas") == desired
            && count("replicas") == desired;
        v_flex()
            .id("deployment-history")
            .size_full()
            .overflow_y_scroll()
            .p_3()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Deployment history"),
                    )
                    .child(
                        Button::new("refresh-rollout-history")
                            .small()
                            .ghost()
                            .label("Refresh")
                            .disabled(self.loading || self.busy)
                            .on_click(cx.listener(|view, _, window, cx| view.load(window, cx))),
                    ),
            )
            .child(copyable_text(
                "rollout-progress",
                format!(
                    "{} · Updated {}/{} · Ready {}/{} · Available {}/{}",
                    if converged { "Complete" } else { "Rolling out" },
                    count("updatedReplicas"),
                    desired,
                    count("readyReplicas"),
                    desired,
                    count("availableReplicas"),
                    desired
                ),
            ))
            .children(
                data.pointer("/status/conditions")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(|(i, condition)| {
                        copyable_text(
                            ("rollout-condition", i),
                            format!(
                                "{}: {} · {}",
                                condition["type"].as_str().unwrap_or("Condition"),
                                condition["status"].as_str().unwrap_or("Unknown"),
                                condition["message"].as_str().unwrap_or("")
                            ),
                        )
                    }),
            )
            .children(
                self.message
                    .as_ref()
                    .map(|message| copyable_text("rollout-message", message.clone())),
            )
            .when(self.loading, |view| view.child("Loading revisions…"))
            .when(!self.loading && self.revisions.is_empty(), |view| {
                view.child(
                    "No retained revisions. Older history may have been pruned by the controller.",
                )
            })
            .children(self.revisions.iter().map(|revision| {
                let selected = revision.clone();
                v_flex()
                    .p_3()
                    .gap_2()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_md()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!(
                                        "Revision {}{}",
                                        revision.number,
                                        if revision.current { " · Current" } else { "" }
                                    )),
                            )
                            .child(
                                Button::new(("review-revision", revision.number as usize))
                                    .small()
                                    .ghost()
                                    .label(if self.may_patch {
                                        "Compare / roll back…"
                                    } else {
                                        "Compare revision…"
                                    })
                                    .disabled(self.busy || revision.current)
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.review(selected.clone(), window, cx)
                                    })),
                            ),
                    )
                    .child(copyable_text(
                        ("revision-name", revision.number as usize),
                        revision
                            .replica_set
                            .metadata
                            .name
                            .clone()
                            .unwrap_or_default(),
                    ))
                    .child(copyable_text(
                        ("revision-created", revision.number as usize),
                        revision
                            .replica_set
                            .metadata
                            .creation_timestamp
                            .as_ref()
                            .map(|time| time.0.to_string())
                            .unwrap_or_else(|| "Creation time unavailable".into()),
                    ))
                    .child(copyable_text(
                        ("revision-images", revision.number as usize),
                        revision.images.join("\n"),
                    ))
                    .when(revision.current, |view| {
                        view.text_color(cx.theme().tone(Tone::Healthy))
                    })
            }))
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    #[::core::prelude::v1::test]
    fn a_revision_is_reviewed_before_any_patch_is_sent() {
        let cx = &mut support::context();
        let deployment = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"default","uid":"web-uid","resourceVersion":"42"},"spec":{"replicas":2,"template":{"metadata":{"labels":{"app":"web"}},"spec":{"containers":[{"name":"app","image":"web:new"}]}}},"status":{"updatedReplicas":2,"readyReplicas":2,"availableReplicas":2}});
        let mut set = deployment.clone();
        set["kind"] = json!("ReplicaSet");
        set["metadata"]["name"] = json!("web-old");
        set["metadata"]["uid"] = json!("rs-uid");
        set["metadata"]["annotations"] = json!({"deployment.kubernetes.io/revision":"1"});
        set["metadata"]["ownerReferences"] = json!([{"apiVersion":"apps/v1","kind":"Deployment","name":"web","uid":"web-uid","controller":true}]);
        set["spec"]["template"]["spec"]["containers"][0]["image"] = json!("web:old");
        let (fixture, session) =
            support::fixture(cx, "rollout-fixture", vec![deployment.clone(), set]);
        let (window, view) = cx.update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    RolloutView::new(
                        session,
                        Arc::new(serde_json::from_value(deployment).unwrap()),
                        true,
                        window,
                        cx,
                    )
                })
            })
            .unwrap()
        });
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| !view.loading && view.revisions.len() == 1)
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click(("review-revision", 1usize), cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            fixture.requests.lock().unwrap().iter().any(|(line, _)| {
                line.starts_with("GET /apis/apps/v1/namespaces/default/deployments/web")
            }) && view.read_with(cx, |view, _| view.busy)
        });
        support::render_until(cx, window, |window| {
            window
                .try_find("confirm-yaml-review")
                .is_some_and(|button| button.visible())
        });
        assert!(
            !fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(line, _)| line.starts_with("PATCH"))
        );
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("confirm-yaml-review").visible());
            window.click("confirm-yaml-review", cx);
        })
        .unwrap();
        support::settle(cx, |_| {
            fixture.object("Deployment", "web")["spec"]["template"]["spec"]["containers"][0]["image"]
                == "web:old"
        });
        assert_eq!(fixture.object("Deployment", "web")["spec"]["replicas"], 2);
    }
}
