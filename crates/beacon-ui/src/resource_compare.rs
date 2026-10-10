//! Read-only, normalized resource comparison between connected clusters.
use crate::{bridge::Bridge, connections::SharedConnections, copyable_text::copyable_text};
use beacon_kube::{ClusterSession, DynamicObject, Kind};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffFile, DiffMode, DiffState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::sync::Arc;

#[derive(Clone, Copy, Default)]
struct Options {
    status: bool,
    metadata: bool,
    reveal: bool,
}

fn normalized(mut value: Value, options: Options) -> Value {
    if !options.status {
        value.as_object_mut().map(|object| object.remove("status"));
    }
    if !options.metadata
        && let Some(metadata) = value.get_mut("metadata").and_then(Value::as_object_mut)
    {
        for key in [
            "uid",
            "resourceVersion",
            "generation",
            "creationTimestamp",
            "deletionTimestamp",
            "deletionGracePeriodSeconds",
            "managedFields",
            "selfLink",
            "namespace",
        ] {
            metadata.remove(key);
        }
        if let Some(annotations) = metadata
            .get_mut("annotations")
            .and_then(Value::as_object_mut)
        {
            annotations.remove("kubectl.kubernetes.io/last-applied-configuration");
        }
        // Owner identity differs across clusters even when the desired relationship is the same.
        if let Some(owners) = metadata
            .get_mut("ownerReferences")
            .and_then(Value::as_array_mut)
        {
            for owner in owners {
                if let Some(owner) = owner.as_object_mut() {
                    owner.remove("uid");
                }
            }
        }
    }
    if value["kind"] == "Secret" && !options.reveal {
        for key in ["data", "stringData"] {
            if let Some(data) = value.get_mut(key).and_then(Value::as_object_mut) {
                for value in data.values_mut() {
                    let digest = Sha256::digest(value.as_str().unwrap_or_default().as_bytes());
                    *value = Value::String(format!("[concealed · sha256:{digest:x}]"));
                }
            }
        }
        if let Some(annotations) = value
            .pointer_mut("/metadata/annotations")
            .and_then(Value::as_object_mut)
        {
            annotations.remove("kubectl.kubernetes.io/last-applied-configuration");
        }
    }
    value
}

pub(crate) struct CompareView {
    left: Arc<ClusterSession>,
    kind: Arc<Kind>,
    object: Arc<DynamicObject>,
    sessions: Vec<Arc<ClusterSession>>,
    cluster: Entity<SelectState<SearchableVec<String>>>,
    namespace: Entity<InputState>,
    name: Entity<InputState>,
    options: Options,
    snapshots: Option<(Value, Value)>,
    diff: Option<Entity<DiffState>>,
    loading: bool,
    message: Option<String>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

pub(crate) fn open(
    left: Arc<ClusterSession>,
    kind: Arc<Kind>,
    object: Arc<DynamicObject>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<CompareView> {
    let mut sessions: Vec<_> = cx
        .global::<SharedConnections>()
        .0
        .read(cx)
        .sessions
        .values()
        .cloned()
        .collect();
    sessions.sort_by(|a, b| a.id().cmp(b.id()));
    if !sessions.iter().any(|session| session.id() == left.id()) {
        sessions.push(left.clone());
    }
    let default = sessions
        .iter()
        .find(|session| session.id() != left.id())
        .unwrap_or(&left)
        .id()
        .to_string();
    let title = format!(
        "Compare {} · {}",
        kind.resource.kind,
        object.metadata.name.as_deref().unwrap_or("")
    );
    let view = cx.new(|cx: &mut Context<CompareView>| {
        let choices = sessions
            .iter()
            .map(|session| session.id().to_string())
            .collect::<Vec<_>>();
        let cluster = cx.new(|cx| {
            let mut state =
                SelectState::new(SearchableVec::new(choices), None, window, cx).searchable(true);
            state.set_selected_value(&default, window, cx);
            state
        });
        let subscription = cx.subscribe(
            &cluster,
            |view, _, event: &SelectEvent<SearchableVec<String>>, cx| {
                if matches!(event, SelectEvent::Confirm(_)) {
                    view._task = None;
                    view.loading = false;
                    view.snapshots = None;
                    view.diff = None;
                    view.message = None;
                    cx.notify();
                }
            },
        );
        let mut view = CompareView {
            left,
            kind,
            namespace: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(object.metadata.namespace.clone().unwrap_or_default())
                    .placeholder("Target namespace")
            }),
            name: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(object.metadata.name.clone().unwrap_or_default())
                    .placeholder("Target resource name")
            }),
            object,
            sessions,
            cluster,
            options: Options::default(),
            snapshots: None,
            diff: None,
            loading: false,
            message: None,
            _task: None,
            _subscriptions: vec![subscription],
        };
        for input in [view.namespace.clone(), view.name.clone()] {
            view._subscriptions
                .push(cx.subscribe(&input, |view, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        view._task = None;
                        view.loading = false;
                        view.snapshots = None;
                        view.diff = None;
                        view.message = None;
                        cx.notify();
                    }
                }));
        }
        view
    });
    let content = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        dialog
            .title(title.clone())
            .width(px(1400.))
            .margin_top(px(24.))
            .child(content.clone())
    });
    view
}

impl CompareView {
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.cluster.read(cx).selected_value().cloned() else {
            return;
        };
        let Some(right) = self
            .sessions
            .iter()
            .find(|session| session.id().as_str() == id)
            .cloned()
        else {
            return;
        };
        let Some(right_kind) = right
            .discovery()
            .kinds()
            .iter()
            .find(|kind| {
                kind.resource.group == self.kind.resource.group
                    && kind.resource.kind == self.kind.resource.kind
            })
            .cloned()
        else {
            self.message = Some("This resource type is not served by the selected cluster.".into());
            cx.notify();
            return;
        };
        let name = self.name.read(cx).value().trim().to_owned();
        let namespace = self.namespace.read(cx).value().trim().to_owned();
        if name.is_empty() || right_kind.namespaced && namespace.is_empty() {
            self.message = Some("Enter a resource name and namespace.".into());
            cx.notify();
            return;
        }
        let right_namespace = right_kind.namespaced.then_some(namespace);
        self.loading = true;
        self.message = None;
        self.diff = None;
        self.snapshots = None;
        let left = self.left.clone();
        let resource = self.kind.resource.clone();
        let object = self.object.clone();
        let fetching = Bridge::global(cx).run_cancellable(async move {
            let (a, b) = futures::join!(
                left.get_object(
                    resource,
                    object.metadata.namespace.clone(),
                    object.metadata.name.clone().unwrap_or_default()
                ),
                right.get_object(right_kind.resource, right_namespace, name)
            );
            let a = a.map_err(|error| format!("Source: {}", error.user_message()))?;
            let b = b.map_err(|error| format!("Target: {}", error.user_message()))?;
            Ok::<_, String>((
                serde_json::to_value(a).map_err(|error| error.to_string())?,
                serde_json::to_value(b).map_err(|error| error.to_string())?,
            ))
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = fetching.result().await;
            let _ = this.update(cx, |view, cx| {
                view.loading = false;
                match result {
                    Ok(Ok(snapshots)) => {
                        view.snapshots = Some(snapshots);
                        view.rebuild(cx);
                    }
                    Ok(Err(error)) => view.message = Some(error),
                    Err(error) => view.message = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let Some((a, b)) = &self.snapshots else {
            return;
        };
        let result = (|| {
            let before = crate::yaml_review::canonical_yaml(normalized(a.clone(), self.options))?;
            let after = crate::yaml_review::canonical_yaml(normalized(b.clone(), self.options))?;
            let files = if before == after {
                self.message = Some("The normalized resources are identical.".into());
                vec![DiffFile::unchanged("resource.yaml", &before).with_language("yaml")]
            } else {
                self.message = None;
                let patch = similar::TextDiff::from_lines(&before, &after)
                    .unified_diff()
                    .context_radius(before.lines().count().max(after.lines().count()))
                    .header("source.yaml", "target.yaml")
                    .to_string();
                DiffFile::parse(&patch)
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(|file| file.with_language("yaml"))
                    .collect()
            };
            Ok::<_, String>(files)
        })();
        match result {
            Ok(files) => {
                self.diff = Some(cx.new(|cx| DiffState::new(files, cx).with_mode(DiffMode::Split)))
            }
            Err(error) => self.message = Some(error),
        }
        cx.notify();
    }
}

impl Render for CompareView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .h((window.viewport_size().height * 0.78).min(px(760.)))
            .gap_3()
            .child(copyable_text(
                "comparison-source",
                format!(
                    "Source: {} · {}/{}",
                    self.left.id(),
                    self.object
                        .metadata
                        .namespace
                        .as_deref()
                        .unwrap_or("Cluster"),
                    self.object.metadata.name.as_deref().unwrap_or("")
                ),
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            Select::new(&self.cluster)
                                .small()
                                .title_prefix("Target cluster: ")
                                .disabled(self.loading),
                        ),
                    )
                    .when(self.kind.namespaced, |bar| {
                        bar.child(
                            div()
                                .w(px(180.))
                                .child(Input::new(&self.namespace).small().disabled(self.loading)),
                        )
                    })
                    .child(
                        div()
                            .w(px(230.))
                            .child(Input::new(&self.name).small().disabled(self.loading)),
                    )
                    .child(
                        Button::new("compare-resources")
                            .small()
                            .primary()
                            .label(if self.loading {
                                "Loading…"
                            } else {
                                "Compare"
                            })
                            .disabled(self.loading)
                            .on_click(cx.listener(|view, _, window, cx| view.load(window, cx))),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("compare-status")
                            .small()
                            .ghost()
                            .label(if self.options.status {
                                "Status: included"
                            } else {
                                "Include status"
                            })
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.options.status = !view.options.status;
                                view.rebuild(cx);
                            })),
                    )
                    .child(
                        Button::new("compare-metadata")
                            .small()
                            .ghost()
                            .label(if self.options.metadata {
                                "Server metadata: included"
                            } else {
                                "Include server metadata"
                            })
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.options.metadata = !view.options.metadata;
                                view.rebuild(cx);
                            })),
                    )
                    .when(
                        self.kind.resource.group.is_empty() && self.kind.resource.kind == "Secret",
                        |bar| {
                            bar.child(
                                Button::new("compare-reveal")
                                    .small()
                                    .ghost()
                                    .label(if self.options.reveal {
                                        "Conceal values"
                                    } else {
                                        "Reveal values"
                                    })
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.options.reveal = !view.options.reveal;
                                        view.rebuild(cx);
                                    })),
                            )
                        },
                    )
                    .when_some(self.diff.clone(), |bar, diff| {
                        bar.child(
                            Button::new("compare-mode")
                                .small()
                                .ghost()
                                .label("Split / unified")
                                .on_click(move |_, _, cx| {
                                    diff.update(cx, |state, cx| {
                                        let mode = if state.mode() == DiffMode::Split {
                                            DiffMode::Unified
                                        } else {
                                            DiffMode::Split
                                        };
                                        state.set_mode(mode, cx);
                                    });
                                }),
                        )
                    }),
            )
            .children(
                self.message
                    .as_ref()
                    .map(|message| copyable_text("comparison-message", message.clone())),
            )
            .when_some(self.diff.clone(), |view, diff| {
                view.child(div().flex_1().min_size_0().child(Diff::new(&diff)))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{Options, normalized};
    use serde_json::json;
    #[test]
    fn runtime_fields_are_ignored_but_desired_configuration_and_secret_differences_remain() {
        let a = json!({"kind":"Deployment","metadata":{"name":"web","namespace":"dev","uid":"one","resourceVersion":"1","creationTimestamp":"yesterday"},"spec":{"template":{"spec":{"containers":[{"image":"web:v1"}]}}},"status":{"readyReplicas":1}});
        let mut b = a.clone();
        b["metadata"]["namespace"] = json!("prod");
        b["metadata"]["uid"] = json!("two");
        b["status"]["readyReplicas"] = json!(3);
        assert_eq!(
            normalized(a.clone(), Options::default()),
            normalized(b.clone(), Options::default())
        );
        b["spec"]["template"]["spec"]["containers"][0]["image"] = json!("web:v2");
        assert_ne!(
            normalized(a, Options::default()),
            normalized(b, Options::default())
        );
        let secret = json!({"kind":"Secret","data":{"password":"c2VjcmV0"}});
        let masked = normalized(secret.clone(), Options::default());
        assert!(!masked.to_string().contains("c2VjcmV0"));
        assert_eq!(
            normalized(
                secret.clone(),
                Options {
                    reveal: true,
                    ..Options::default()
                }
            ),
            secret
        );
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod integration_tests {
    use super::*;
    use crate::connections::{Connections, SharedConnections};
    use crate::feature_test_support as support;
    use gpui_kit::test::TestWindowExt as _;
    use serde_json::json;
    #[::core::prelude::v1::test]
    fn comparison_fetches_both_clusters_and_never_writes() {
        let cx = &mut support::context();
        let object = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"default","uid":"one","resourceVersion":"1"},"spec":{"template":{"spec":{"containers":[{"name":"app","image":"web:v1"}]}}}});
        let mut other = object.clone();
        other["metadata"]["uid"] = json!("two");
        other["spec"]["template"]["spec"]["containers"][0]["image"] = json!("web:v2");
        let (left_api, left) = support::fixture(cx, "compare-left", vec![object.clone()]);
        let (right_api, right) = support::fixture(cx, "compare-right", vec![other]);
        cx.update(|cx| {
            let connections = cx.new(|_| Connections::default());
            connections.update(cx, |state, _| {
                state.sessions.insert(left.id().clone(), left.clone());
                state.sessions.insert(right.id().clone(), right);
            });
            cx.set_global(SharedConnections(connections));
        });
        let window = support::window(cx);
        let view = cx
            .update_window(window, |_, window, cx| {
                open(
                    left,
                    Arc::new(support::kind("apps", "Deployment", "deployments")),
                    Arc::new(serde_json::from_value(object).unwrap()),
                    window,
                    cx,
                )
            })
            .unwrap();
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("compare-resources", cx);
        })
        .unwrap();
        support::settle(cx, |cx| {
            view.read_with(cx, |view, _| {
                view.snapshots.is_some() && view.diff.is_some()
            })
        });
        view.read_with(cx, |view, _| {
            let (a, b) = view.snapshots.as_ref().unwrap();
            assert_ne!(
                a.pointer("/spec/template/spec/containers/0/image"),
                b.pointer("/spec/template/spec/containers/0/image")
            );
        });
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.click("compare-mode", cx);
            window.click("compare-status", cx);
            window.render_frame(cx);
        })
        .unwrap();
        assert!(
            left_api
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(request, _)| request.starts_with("GET"))
        );
        assert!(
            right_api
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(request, _)| request.starts_with("GET"))
        );
    }
}
