//! Creates one resource from an editable YAML manifest.

use std::sync::Arc;

use beacon_kube::{ClusterSession, DynamicObject, Kind, ops};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use serde_json::{Value, json};

use crate::bridge::Bridge;
use crate::theme::{BeaconTheme as _, Tone};

pub(crate) enum CreateEvent {
    Created(Box<DynamicObject>),
    Cancelled,
}

enum Status {
    Idle,
    Reviewing,
    Running { dry_run: bool },
    Validated,
    Failed(String),
}

pub(crate) struct CreateView {
    session: Arc<ClusterSession>,
    kind: Arc<Kind>,
    editor: Entity<EditorState>,
    status: Status,
    _operation: Option<Task<()>>,
    _review: Option<Task<()>>,
    _changes: Subscription,
}

impl EventEmitter<CreateEvent> for CreateView {}

impl CreateView {
    pub fn new(
        session: Arc<ClusterSession>,
        kind: Arc<Kind>,
        namespace: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("yaml")
                .default_value(template(&kind, namespace))
        });
        editor.read(cx).focus_handle(cx).focus(window, cx);
        let changes = cx.subscribe(&editor, |view, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && !view.is_running() {
                view.status = Status::Idle;
                cx.notify();
            }
        });
        Self {
            session,
            kind,
            editor,
            status: Status::Idle,
            _operation: None,
            _review: None,
            _changes: changes,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status, Status::Running { .. } | Status::Reviewing)
    }

    fn manifest(&self, cx: &App) -> Result<Value, String> {
        parse(self.editor.read(cx).value().as_ref(), &self.kind)
    }

    fn submit(&mut self, dry_run: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running() {
            return;
        }
        let manifest = match self.manifest(cx) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.status = Status::Failed(error);
                cx.notify();
                return;
            }
        };
        if !dry_run {
            self.review(manifest, window, cx);
            return;
        }
        self.send(manifest, true, window, cx);
    }

    fn review(&mut self, manifest: Value, window: &mut Window, cx: &mut Context<Self>) {
        self.status = Status::Reviewing;
        cx.notify();
        let name = manifest
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "{} (generated name)",
                    manifest
                        .pointer("/metadata/generateName")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                )
            });
        let target = manifest
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            .map(|namespace| format!("{namespace}/{name}"))
            .unwrap_or(name);
        let context = format!(
            "{} · {} · {}",
            self.session.id().display_name(),
            self.kind.resource.kind,
            target
        );
        let preparing =
            Bridge::global(cx).run(async move { crate::yaml_review::Preview::create(manifest) });
        self._review = Some(cx.spawn_in(window, async move |this, cx| {
            let result = preparing.await;
            let _ = this.update_in(cx, |view, window, cx| {
                match result {
                    Ok(Ok(preview)) => {
                        let review =
                            crate::yaml_review::open(preview, context, true, false, window, cx);
                        cx.subscribe_in(
                            &review,
                            window,
                            |view, _, event: &crate::yaml_review::ReviewEvent, window, cx| {
                                view.status = Status::Idle;
                                if let crate::yaml_review::ReviewEvent::Confirmed(manifest) = event
                                {
                                    view.send((**manifest).clone(), false, window, cx);
                                }
                                cx.notify();
                            },
                        )
                        .detach();
                    }
                    Ok(Err(error)) => view.status = Status::Failed(error),
                    Err(error) => view.status = Status::Failed(error.to_string()),
                }
                cx.notify();
            });
        }));
    }

    fn send(
        &mut self,
        manifest: Value,
        dry_run: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.status = Status::Running { dry_run };
        cx.notify();
        let session = self.session.clone();
        let kind = (*self.kind).clone();
        let creating =
            Bridge::global(cx).run(async move { session.create(kind, manifest, dry_run).await });
        self._operation = Some(cx.spawn_in(window, async move |this, cx| {
            let result = creating.await;
            let _ = this.update(cx, |view, cx| {
                view.status = match result {
                    Ok(Ok(object)) if !dry_run => {
                        cx.emit(CreateEvent::Created(Box::new(object)));
                        Status::Idle
                    }
                    Ok(Ok(_)) => Status::Validated,
                    Ok(Err(error)) => Status::Failed(error.user_message()),
                    Err(error) => Status::Failed(error.to_string()),
                };
                cx.notify();
            });
        }));
    }

    fn format(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_running() {
            return;
        }
        let formatted = self.manifest(cx).and_then(|manifest| {
            serde_saphyr::to_string(&manifest).map_err(|error| error.to_string())
        });
        match formatted {
            Ok(yaml) => {
                self.editor
                    .update(cx, |editor, cx| editor.set_value(yaml, window, cx));
                self.status = Status::Idle;
            }
            Err(error) => self.status = Status::Failed(error),
        }
        cx.notify();
    }
}

impl Render for CreateView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.is_running();
        let (tone, message) = match &self.status {
            Status::Reviewing => (
                Tone::Progressing,
                "Preparing YAML changes for review…".into(),
            ),
            Status::Idle => (
                Tone::Unknown,
                "Edit one resource manifest. Existing names are refused.".to_string(),
            ),
            Status::Running { dry_run: true } => {
                (Tone::Progressing, "Validating with the API server…".into())
            }
            Status::Running { dry_run: false } => (Tone::Progressing, "Creating resource…".into()),
            Status::Validated => (
                Tone::Healthy,
                "Server validation passed. No resource was saved.".into(),
            ),
            Status::Failed(error) => (Tone::Critical, error.clone()),
        };
        let scope = self
            .manifest(cx)
            .ok()
            .map(|manifest| {
                manifest
                    .pointer("/metadata/namespace")
                    .and_then(Value::as_str)
                    .map(|namespace| format!("Namespace: {namespace}"))
                    .unwrap_or_else(|| "Cluster-scoped".into())
            })
            .unwrap_or_else(|| "Set the resource name and namespace in YAML".into());

        v_flex()
            .w(relative(0.95))
            .max_w(px(900.))
            .h(relative(0.9))
            .max_h(px(760.))
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_lg()
            .overflow_hidden()
            .child(
                v_flex()
                    .px_4()
                    .py_3()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Create {}", self.kind.resource.kind)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Cluster: {} · {} · {}",
                                self.session.id().display_name(),
                                scope,
                                self.kind.resource.api_version
                            )),
                    ),
            )
            .child(
                div().flex_1().min_h_0().overflow_hidden().p_2().child(
                    Editor::new(&self.editor)
                        .readonly(running)
                        .bordered(false)
                        .h(relative(1.)),
                ),
            )
            .child(
                div()
                    .id("create-resource-status")
                    .max_h(px(100.))
                    .overflow_y_scroll()
                    .px_4()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().tone(tone))
                    .child(crate::copyable_text::copyable_text(
                        "create-status-text",
                        message,
                    )),
            )
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .gap_2()
                    .justify_between()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("format-create-yaml")
                            .small()
                            .ghost()
                            .label("Format")
                            .tooltip("Format YAML; comments are not kept")
                            .disabled(running)
                            .on_click(cx.listener(|view, _, window, cx| view.format(window, cx))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("cancel-create-resource")
                                    .small()
                                    .ghost()
                                    .label("Cancel")
                                    .disabled(running)
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(CreateEvent::Cancelled)),
                                    ),
                            )
                            .child(
                                Button::new("validate-create-resource")
                                    .small()
                                    .label("Validate")
                                    .tooltip("Ask the API server to validate without saving")
                                    .disabled(running)
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.submit(true, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("confirm-create-resource")
                                    .small()
                                    .primary()
                                    .label(
                                        if matches!(self.status, Status::Running { dry_run: false })
                                        {
                                            "Creating…"
                                        } else {
                                            "Create"
                                        },
                                    )
                                    .disabled(running)
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.submit(false, window, cx)
                                    })),
                            ),
                    ),
            )
    }
}

fn parse(yaml: &str, kind: &Kind) -> Result<Value, String> {
    let manifest: Value =
        serde_saphyr::from_str(yaml).map_err(|error| format!("This is not valid YAML: {error}"))?;
    let object = ops::prepare_create(kind, manifest).map_err(|error| error.to_string())?;
    serde_json::to_value(object).map_err(|error| error.to_string())
}

fn template(kind: &Kind, namespace: Option<&str>) -> String {
    let name = format!("new-{}", kind.resource.kind.to_lowercase());
    let mut manifest = json!({
        "apiVersion": kind.resource.api_version,
        "kind": kind.resource.kind,
        "metadata": { "name": name },
    });
    if kind.namespaced {
        manifest["metadata"]["namespace"] = json!(namespace.unwrap_or("default"));
    }
    match (kind.resource.group.as_str(), kind.resource.kind.as_str()) {
        ("", "ConfigMap") => manifest["data"] = json!({}),
        ("", "Secret") => {
            manifest["type"] = json!("Opaque");
            manifest["stringData"] = json!({});
        }
        ("", "Pod") => {
            manifest["spec"] = json!({
                "containers": [{ "name": "app", "image": "your-image:tag" }],
            })
        }
        ("apps", "Deployment" | "StatefulSet" | "DaemonSet") => {
            manifest["spec"] = json!({
                "selector": { "matchLabels": { "app": name } },
                "template": {
                    "metadata": { "labels": { "app": name } },
                    "spec": { "containers": [{ "name": "app", "image": "your-image:tag" }] },
                },
            });
            if kind.resource.kind != "DaemonSet" {
                manifest["spec"]["replicas"] = json!(1);
            }
            if kind.resource.kind == "StatefulSet" {
                manifest["spec"]["serviceName"] = json!(name);
            }
        }
        ("", "Service") => {
            manifest["spec"] = json!({
                "selector": { "app": "your-app" },
                "ports": [{ "port": 80, "targetPort": 80 }],
            })
        }
        _ => {}
    }
    let yaml = serde_saphyr::to_string(&manifest).expect("resource template is serializable");
    format!("# Edit this template or paste a manifest for this resource type.\n{yaml}")
}

#[cfg(test)]
mod tests {
    use super::{parse, template};
    use beacon_kube::{ApiResource, Kind};
    use serde_json::Value;

    fn kind(group: &str, name: &str, namespaced: bool) -> Kind {
        Kind {
            resource: ApiResource {
                group: group.into(),
                version: "v1".into(),
                api_version: if group.is_empty() {
                    "v1".into()
                } else {
                    format!("{group}/v1")
                },
                kind: name.into(),
                plural: format!("{}s", name.to_lowercase()),
            },
            namespaced,
            verbs: vec!["create".into()],
        }
    }

    #[test]
    fn templates_follow_resource_identity_and_scope_including_custom_resources() {
        for kind in [
            kind("", "ConfigMap", true),
            kind("", "Secret", true),
            kind("apps", "Deployment", true),
            kind("example.io", "Widget", true),
            kind("", "Namespace", false),
        ] {
            let manifest = parse(&template(&kind, Some("qa")), &kind).unwrap();
            assert_eq!(manifest["apiVersion"], kind.resource.api_version);
            assert_eq!(manifest["kind"], kind.resource.kind);
            assert_eq!(
                manifest
                    .pointer("/metadata/namespace")
                    .and_then(Value::as_str),
                kind.namespaced.then_some("qa")
            );
        }
    }

    #[test]
    fn a_list_or_multiple_documents_cannot_be_submitted_as_one_resource() {
        let kind = kind("", "ConfigMap", true);
        let yaml = template(&kind, Some("default"));
        assert!(parse(&format!("{yaml}\n---\n{yaml}"), &kind).is_err());
        assert!(parse("[]", &kind).is_err());
    }
}
