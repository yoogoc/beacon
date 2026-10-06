//! Argo node details live in a modal sheet, never in the resource overview.
use std::sync::Arc;

use beacon_columns::Timestamp;
use beacon_kube::{
    ClusterSession, DynamicObject, ObjectRef,
    argo::{self, Nodes},
};
use gpui_kit::base::{Link, SelectableText, TextSelection};
use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    tab::{Tab, TabBar},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use serde_json::Value;

use crate::{
    argo::{GraphNode, array, text},
    bridge::Bridge,
    copyable_text::copyable_text,
    detail::OwnerRequested,
    overview::{Cell, Group, Table},
    theme::{BeaconTheme as _, Tone},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum TabKind {
    Summary,
    Containers,
    InputsOutputs,
    Yaml,
}
const TABS: [TabKind; 4] = [
    TabKind::Summary,
    TabKind::Containers,
    TabKind::InputsOutputs,
    TabKind::Yaml,
];
impl TabKind {
    fn label(self, runtime: bool) -> &'static str {
        match self {
            Self::Summary => "Summary",
            Self::Containers => "Containers",
            Self::InputsOutputs => "Inputs / Outputs",
            Self::Yaml => {
                if runtime {
                    "Node YAML"
                } else {
                    "Template YAML"
                }
            }
        }
    }
}

pub(crate) struct ArgoNodeView {
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    pub(crate) node: GraphNode,
    nodes: Arc<Nodes>,
    tab: TabKind,
    container: String,
    template: Option<Value>,
    template_error: Option<String>,
    pod: Option<DynamicObject>,
    pod_error: Option<String>,
    loading: bool,
    yaml: String,
    _load: Option<Task<()>>,
    _template_load: Option<Task<()>>,
}
impl EventEmitter<OwnerRequested> for ArgoNodeView {}
pub(crate) struct SubgraphRequested(pub String);
impl EventEmitter<SubgraphRequested> for ArgoNodeView {}

impl ArgoNodeView {
    pub(crate) fn new(
        session: Arc<ClusterSession>,
        object: Arc<DynamicObject>,
        node: GraphNode,
        nodes: Arc<Nodes>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let template = resolve(&object, &node);
        let mut this = Self {
            session,
            object,
            node,
            nodes,
            tab: TabKind::Summary,
            container: "main".into(),
            template,
            template_error: None,
            pod: None,
            pod_error: None,
            loading: false,
            yaml: String::new(),
            _load: None,
            _template_load: None,
        };
        this.update_yaml();
        this.load(window, cx);
        this
    }

    pub(crate) fn refresh(
        &mut self,
        object: Arc<DynamicObject>,
        nodes: Arc<Nodes>,
        cx: &mut Context<Self>,
    ) {
        if self.node.runtime {
            if let Some(node) = nodes.get(&self.node.id) {
                self.node.data = node.clone();
            } else {
                self.node.data = Value::Null;
            }
        }
        if !self.node.runtime
            && let Some(parent) = self.node.template_context.as_deref()
        {
            if let Some(node) = crate::argo::Graph::template(&object.data["spec"], parent)
                .nodes
                .into_iter()
                .find(|node| node.id == self.node.id)
            {
                self.node = node;
            } else {
                self.node.data = Value::Null;
                self.template = None;
            }
        }
        self.object = object;
        self.nodes = nodes;
        if let Some(template) = resolve(&self.object, &self.node) {
            self.template = Some(template);
        }
        self.update_yaml();
        cx.notify();
    }
    fn update_yaml(&mut self) {
        let source = if self.node.runtime {
            &self.node.data
        } else {
            self.template.as_ref().unwrap_or(&self.node.data)
        };
        self.yaml = serde_saphyr::to_string(source).unwrap_or_else(|error| error.to_string());
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = false;
        if self.node.runtime
            && let Some(name) = argo::pod_name(&self.object, &self.node.data)
            && !matches!(
                self.node.data["phase"].as_str(),
                Some("Skipped" | "Omitted")
            )
        {
            let session = self.session.clone();
            let namespace = self.object.metadata.namespace.clone();
            let id = self.node.id.clone();
            let bridge = Bridge::global(cx).clone();
            self.loading = true;
            self._load = Some(cx.spawn_in(window, async move |this, cx| {
                loop {
                    let session = session.clone();
                    let namespace = namespace.clone();
                    let name = name.clone();
                    let result = bridge
                        .run_cancellable(async move {
                            session
                                .get_object(beacon_kube::resources::pod(), namespace, name)
                                .await
                        })
                        .result()
                        .await;
                    let mut finished = true;
                    if this
                        .update(cx, |view, cx| {
                            if view.node.id != id {
                                return;
                            }
                            view.loading = false;
                            match result {
                                Ok(Ok(pod)) => {
                                    view.pod = Some(pod);
                                    view.pod_error = None;
                                }
                                Ok(Err(error)) => {
                                    view.pod_error = Some(error.user_message());
                                }
                                Err(error) => {
                                    view.pod_error = Some(error.to_string());
                                }
                            }
                            finished = matches!(
                                view.node.data["phase"].as_str(),
                                Some("Succeeded" | "Failed" | "Error" | "Skipped" | "Omitted")
                            );
                            cx.notify();
                        })
                        .is_err()
                        || finished
                    {
                        break;
                    }
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(5))
                        .await;
                }
            }));
        }
        if self.template.is_none()
            && let Some((kind, name, template)) =
                template_target(&self.session, &self.object, &self.node)
        {
            let session = self.session.clone();
            let namespace = if kind.namespaced {
                self.object.metadata.namespace.clone()
            } else {
                None
            };
            let resource = kind.resource.clone();
            let id = self.node.id.clone();
            let fetching = Bridge::global(cx).run_cancellable(async move {
                session.get_object(resource, namespace, name).await
            });
            self._template_load = Some(cx.spawn_in(window, async move |this, cx| {
                let result = fetching.result().await;
                let _ = this.update(cx, |view, cx| {
                    if view.node.id != id {
                        return;
                    }
                    match result {
                        Ok(Ok(object)) => {
                            let holder = serde_json::json!({"templateName":template});
                            view.template = argo::resolved_template(&object.data, &holder);
                            if view.template.is_none() {
                                view.template_error = Some(format!(
                                    "Template {template} was not found in the referenced resource."
                                ));
                            }
                        }
                        Ok(Err(error)) => view.template_error = Some(error.user_message()),
                        Err(error) => view.template_error = Some(error.to_string()),
                    }
                    view.update_yaml();
                    cx.notify();
                });
            }));
        }
    }

    fn node_summary(&self) -> Vec<Group> {
        let node = &self.node.data;
        let mut groups = Vec::new();
        let source_link =
            template_target(&self.session, &self.object, &self.node).map(|(kind, name, _)| {
                Cell::text(name).link(
                    argo::GROUP,
                    if kind.namespaced {
                        "WorkflowTemplate"
                    } else {
                        "ClusterWorkflowTemplate"
                    },
                )
            });
        if !self.node.runtime {
            let template = self.template.as_ref().unwrap_or(node);
            groups.push(Group::new(
                "Template",
                vec![
                    ("Name", Cell::of(template.get("name"))),
                    ("Type", Cell::text(crate::argo::template_type(template))),
                    ("Template reference", Cell::of(node.get("templateRef"))),
                    ("Arguments", Cell::of(node.get("arguments"))),
                    ("When", Cell::of(node.get("when"))),
                    (
                        "Depends",
                        Cell::of(node.get("depends").or_else(|| node.get("dependencies"))),
                    ),
                    (
                        "With items / parameter",
                        Cell::of(
                            node.get("withItems")
                                .or_else(|| node.get("withParam"))
                                .or_else(|| node.get("withSequence")),
                        ),
                    ),
                ],
            ));
            if let Some(source) = source_link {
                groups[0].fields.push(("Source resource".into(), source));
            }
            let mut execution = Group::new("Execution policy", vec![]);
            execution.fields = crate::overview::rows(template)
                .into_iter()
                .filter(|(key, _)| {
                    [
                        "retryStrategy",
                        "activeDeadlineSeconds",
                        "memoize",
                        "synchronization",
                        "timeout",
                    ]
                    .iter()
                    .any(|prefix| key.starts_with(prefix))
                })
                .map(|(key, value)| {
                    (
                        key,
                        Cell {
                            value,
                            reference: None,
                        },
                    )
                })
                .collect();
            if !execution.fields.is_empty() {
                groups.push(execution);
            }
            return groups;
        }
        groups.push(Group::new(
            "Identity",
            vec![
                ("Node name", Cell::of(node.get("name"))),
                ("Node ID", Cell::text(self.node.id.clone())),
                ("Node type", Cell::of(node.get("type"))),
                (
                    "Template",
                    Cell::of(
                        node.get("templateName")
                            .or_else(|| node.pointer("/templateRef/template")),
                    ),
                ),
                ("Template scope", Cell::of(node.get("templateScope"))),
                ("Template reference", Cell::of(node.get("templateRef"))),
            ],
        ));
        if let Some(source) = source_link {
            groups[0].fields.push(("Source resource".into(), source));
        }
        if let Some(pod) = argo::pod_name(&self.object, node) {
            groups[0]
                .fields
                .push(("Pod name".into(), Cell::text(pod).link("", "Pod")));
            groups[0].fields.push((
                "Host node".into(),
                Cell::of(node.get("hostNodeName").or_else(|| {
                    self.pod
                        .as_ref()
                        .and_then(|p| p.data.pointer("/spec/nodeName"))
                }))
                .link("", "Node"),
            ));
        }
        groups.push(Group::new(
            "Execution",
            vec![
                ("Phase", Cell::of(node.get("phase"))),
                ("Started", crate::argo::timestamp(node.get("startedAt"))),
                ("Finished", crate::argo::timestamp(node.get("finishedAt"))),
                (
                    "Duration",
                    Cell {
                        value: crate::argo::duration(node, Timestamp::now()),
                        reference: None,
                    },
                ),
                (
                    "Estimated duration (s)",
                    Cell::of(node.get("estimatedDuration")),
                ),
                ("Progress", Cell::of(node.get("progress"))),
                ("Exit code", Cell::of(node.pointer("/outputs/exitCode"))),
                ("Message", Cell::of(node.get("message"))),
            ],
        ));
        let mut resources = Group::new("Resources & cache", vec![]);
        for (name, duration) in node["resourcesDuration"].as_object().into_iter().flatten() {
            let unit = match name.as_str() {
                "memory" => "100Mi",
                "storage" | "ephemeral-storage" => "10Gi",
                _ => "1",
            };
            resources.fields.push((
                format!("{name} resource duration"),
                Cell::text(format!(
                    "{} seconds × ({unit} {name})",
                    text(duration).unwrap_or_default()
                )),
            ));
        }
        for (key, value) in node
            .get("memoizationStatus")
            .filter(|value| !value.is_null())
            .map(crate::overview::rows)
            .unwrap_or_default()
        {
            resources.fields.push((
                if key.is_empty() {
                    "Memoization".into()
                } else {
                    format!("Memoization / {key}")
                },
                Cell {
                    value,
                    reference: None,
                },
            ));
        }
        if node.get("memoizationStatus").is_none_or(Value::is_null) {
            resources
                .fields
                .push(("Memoization".into(), Cell::text("Not configured")));
        }
        groups.push(resources);
        if node["type"] == "Retry" {
            let mut hosts = std::collections::BTreeSet::new();
            let mut queue = vec![self.node.id.clone()];
            let mut visited = std::collections::BTreeSet::new();
            while let Some(id) = queue.pop() {
                if !visited.insert(id.clone()) {
                    continue;
                }
                if let Some(child) = self.nodes.get(&id) {
                    if matches!(child["phase"].as_str(), Some("Failed" | "Error"))
                        && let Some(host) = child["hostNodeName"].as_str()
                    {
                        hosts.insert(host.to_string());
                    }
                    queue.extend(
                        array(child, "/children")
                            .filter_map(Value::as_str)
                            .map(str::to_string),
                    );
                }
            }
            groups.push(Group::new(
                "Retry",
                vec![(
                    "Failed hosts",
                    Cell::text(hosts.into_iter().collect::<Vec<_>>().join("\n")),
                )],
            ));
        }
        groups
    }

    fn container_lines(&self) -> Vec<(String, Value, Option<Value>)> {
        if let Some(pod) = &self.pod {
            return [
                ("containers", "containerStatuses"),
                ("initContainers", "initContainerStatuses"),
                ("ephemeralContainers", "ephemeralContainerStatuses"),
            ]
            .into_iter()
            .flat_map(|(key, status)| {
                array(&pod.data, &format!("/spec/{key}")).map(move |container| {
                    let name = container["name"].as_str().unwrap_or("Unnamed");
                    let status = array(&pod.data, &format!("/status/{status}"))
                        .find(|status| status["name"] == name)
                        .cloned();
                    (name.to_string(), container.clone(), status)
                })
            })
            .collect();
        }
        let Some(template) = &self.template else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for key in ["container", "script"] {
            if let Some(container) = template.get(key) {
                result.push(("main".into(), container.clone(), None));
            }
        }
        for (path, prefix) in [
            ("/containerSet/containers", ""),
            ("/sidecars", ""),
            ("/initContainers", "init / "),
        ] {
            for container in array(template, path) {
                result.push((
                    format!(
                        "{prefix}{}",
                        container["name"].as_str().unwrap_or("Unnamed")
                    ),
                    container.clone(),
                    None,
                ));
            }
        }
        result
    }

    fn render_containers(&self, cx: &mut Context<Self>) -> AnyElement {
        let containers = self.container_lines();
        if containers.is_empty() {
            return self.notice(
                if self.template.is_some() {
                    "This node has no containers."
                } else {
                    "Container configuration is not available yet."
                },
                cx,
            );
        }
        let mut view = v_flex().w_full().gap_3();
        if self.pod.is_none() && self.node.runtime {
            view = view.child(self.notice(
                "Showing the resolved template; Pod configuration is unavailable.",
                cx,
            ));
        }
        let selected = containers
            .iter()
            .find(|(name, _, _)| *name == self.container)
            .unwrap_or(&containers[0]);
        view = view.child(
            h_flex()
                .w_full()
                .flex_wrap()
                .gap_1()
                .children(containers.iter().map(|(name, _, _)| {
                    let name = name.clone();
                    Button::new(SharedString::from(name.clone()))
                        .small()
                        .ghost()
                        .toggled(name == selected.0)
                        .label(name.clone())
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.container = name.clone();
                            cx.notify();
                        }))
                })),
        );
        if let Some(status) = &selected.2 {
            let mut group = Group::new(
                "Container status",
                vec![
                    ("Ready", Cell::of(status.get("ready"))),
                    ("Restart count", Cell::of(status.get("restartCount"))),
                    ("Image ID", Cell::of(status.get("imageID"))),
                    ("Container ID", Cell::of(status.get("containerID"))),
                ],
            );
            for (key, label) in [("state", "State"), ("lastState", "Last state")] {
                group
                    .fields
                    .extend(
                        crate::overview::rows(&status[key])
                            .into_iter()
                            .map(|(key, value)| {
                                (
                                    if key.is_empty() {
                                        label.to_string()
                                    } else {
                                        format!("{label} / {key}")
                                    },
                                    Cell {
                                        value,
                                        reference: None,
                                    },
                                )
                            }),
                    );
            }
            view = view.child(self.render_group(&group, cx));
        }
        let container = &selected.1;
        view = view.child(self.render_group(
            &Group::new(
                "Container configuration",
                vec![
                        ("Image", Cell::of(container.get("image"))),
                        (
                            "Image pull policy",
                            Cell::of(container.get("imagePullPolicy")),
                        ),
                        ("Command", Cell::of(container.get("command"))),
                        ("Arguments", Cell::of(container.get("args"))),
                        ("Working directory", Cell::of(container.get("workingDir"))),
                        ("Source", Cell::of(container.get("source"))),
                        (
                            "Ports",
                            Cell::text(
                                array(container, "/ports")
                                    .map(|port| {
                                        format!(
                                            "{}{} / {}",
                                            text(&port["name"])
                                                .map(|name| format!("{name}: "))
                                                .unwrap_or_default(),
                                            text(&port["containerPort"])
                                                .unwrap_or_else(|| "?".into()),
                                            port["protocol"].as_str().unwrap_or("TCP")
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            ),
                        ),
                        (
                            "Security context",
                            Cell::of(container.get("securityContext")),
                        ),
                    ],
            ),
            cx,
        ));
        let resources = crate::overview::resource_table(
            &container["resources"]["requests"],
            &container["resources"]["limits"],
            ["Resource", "Requests", "Limits"],
        );
        view = view
            .child(self.heading("Resources", cx))
            .child(self.render_table(&resources, cx));
        let env = Table {
            headers: vec!["Name", "Value / reference"],
            rows: array(container, "/env")
                .map(|env| vec![Cell::of(env.get("name")), environment_value(env)])
                .collect(),
        };
        view = view
            .child(self.heading("Environment", cx))
            .child(self.render_table(&env, cx));
        if let Some(env) = container.get("envFrom") {
            view = view.child(
                self.fields(
                    &crate::overview::rows(env)
                        .into_iter()
                        .map(|(k, v)| {
                            (
                                k,
                                Cell {
                                    value: v,
                                    reference: None,
                                },
                            )
                        })
                        .collect::<Vec<_>>(),
                    cx,
                ),
            );
        }
        let mounts = Table {
            headers: vec!["Volume", "Mount path", "Read only", "Subpath"],
            rows: array(container, "/volumeMounts")
                .map(|mount| {
                    vec![
                        Cell::of(mount.get("name")),
                        Cell::of(mount.get("mountPath")),
                        Cell::of(mount.get("readOnly")),
                        Cell::of(mount.get("subPath").or_else(|| mount.get("subPathExpr"))),
                    ]
                })
                .collect(),
        };
        view = view
            .child(self.heading("Volume mounts", cx))
            .child(self.render_table(&mounts, cx));
        // All remaining container fields, including probes, lifecycle and device mounts.
        let mut additional = container.clone();
        if let Some(map) = additional.as_object_mut() {
            for key in [
                "image",
                "imagePullPolicy",
                "command",
                "args",
                "workingDir",
                "source",
                "ports",
                "securityContext",
                "resources",
                "env",
                "envFrom",
                "volumeMounts",
            ] {
                map.remove(key);
            }
        }
        let extra = crate::overview::rows(&additional)
            .into_iter()
            .map(|(k, value)| {
                (
                    k,
                    Cell {
                        value,
                        reference: None,
                    },
                )
            })
            .collect::<Vec<_>>();
        view.when(!extra.is_empty(), |view| {
            view.child(self.heading("Additional container fields", cx))
                .child(self.fields(&extra, cx))
        })
        .into_any_element()
    }

    fn render_inputs_outputs(&self, cx: &mut Context<Self>) -> AnyElement {
        let source = if self.node.runtime {
            &self.node.data
        } else {
            self.template.as_ref().unwrap_or(&self.node.data)
        };
        let mut view = v_flex().w_full().gap_3();
        for (key, title) in [("inputs", "Inputs"), ("outputs", "Outputs")] {
            let io = &source[key];
            let mut section = v_flex()
                .id(SharedString::from(format!("node-io-{key}")))
                .w_full()
                .gap_3()
                .child(self.heading(title, cx));
            if io.is_null() {
                section = section.child(self.notice(
                    if key == "inputs" {
                        "No inputs"
                    } else if self.node.runtime
                        && !matches!(
                            source["phase"].as_str(),
                            Some("Succeeded" | "Failed" | "Error" | "Skipped" | "Omitted")
                        )
                    {
                        "Outputs will appear after this node completes."
                    } else {
                        "No outputs"
                    },
                    cx,
                ));
                view = view.child(section);
                continue;
            }
            if key == "outputs" {
                section = section.child(self.fields(
                    &[
                        ("Result".into(), Cell::of(io.get("result"))),
                        ("Exit code".into(), Cell::of(io.get("exitCode"))),
                    ],
                    cx,
                ));
            }
            let parameters = Table {
                headers: vec!["Parameter", "Value / default / source"],
                rows: array(io, "/parameters")
                    .map(|p| {
                        vec![
                            Cell::of(p.get("name")),
                            Cell::of(
                                p.get("value")
                                    .or_else(|| p.get("default"))
                                    .or_else(|| p.get("valueFrom")),
                            ),
                        ]
                    })
                    .collect(),
            };
            section = section.child(self.render_table(&parameters, cx));
            for (index, artifact) in array(io, "/artifacts").enumerate() {
                section = section.child(
                    v_flex()
                        .id(SharedString::from(format!("{key}-artifact-{index}")))
                        .w_full()
                        .gap_2()
                        .p_3()
                        .rounded_md()
                        .bg(cx.theme().muted.opacity(0.5))
                        .child(self.heading(artifact["name"].as_str().unwrap_or("Artifact"), cx))
                        .child(
                            self.fields(
                                &crate::overview::rows(artifact)
                                    .into_iter()
                                    .map(|(key, value)| {
                                        (
                                            key,
                                            Cell {
                                                value,
                                                reference: None,
                                            },
                                        )
                                    })
                                    .collect::<Vec<_>>(),
                                cx,
                            ),
                        ),
                );
            }
            view = view.child(section);
        }
        if !self.node.runtime
            && let Some(arguments) = self.node.data.get("arguments")
        {
            view = view.child(self.heading("Task arguments", cx)).child(
                self.fields(
                    &crate::overview::rows(arguments)
                        .into_iter()
                        .map(|(key, value)| {
                            (
                                key,
                                Cell {
                                    value,
                                    reference: None,
                                },
                            )
                        })
                        .collect::<Vec<_>>(),
                    cx,
                ),
            );
        }
        view.into_any_element()
    }

    fn notice(&self, message: &str, cx: &App) -> AnyElement {
        div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(copyable_text("node-notice", message.to_string()))
            .into_any_element()
    }
    fn heading(&self, title: &str, cx: &App) -> AnyElement {
        h_flex()
            .w_full()
            .pt_3()
            .gap_2()
            .child(
                crate::icons::overview(title)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(title.to_string()),
            )
            .into_any_element()
    }
    fn value(&self, cell: &Cell, cx: &mut Context<Self>) -> AnyElement {
        let value = cell.value.clone().unwrap_or_else(|| "<none>".into());
        let reference = cell.reference.as_ref().and_then(|reference| {
            let kind = self
                .session
                .discovery()
                .kinds()
                .iter()
                .find(|kind| {
                    kind.resource.group == reference.group && kind.resource.kind == reference.kind
                })?
                .clone();
            let target = ObjectRef::new(
                if kind.namespaced {
                    self.object.metadata.namespace.clone()
                } else {
                    None
                },
                reference.name.clone(),
            );
            Some((Arc::new(kind), target))
        });
        let value = match reference {
            Some((kind, target)) => Link::new("node-reference")
                .accessibility_label(value.clone())
                .cursor_pointer()
                .text_color(cx.theme().resource_link())
                .hover(|this| this.underline())
                .child(SelectableText::new("value", value))
                .on_activate(cx.listener(move |_, event, window, cx| {
                    if matches!(event, ClickEvent::Mouse(_))
                        && !TextSelection::selected_text(window, cx).is_empty()
                    {
                        return;
                    }
                    cx.emit(OwnerRequested {
                        kind: kind.clone(),
                        target: target.clone(),
                    });
                }))
                .into_any_element(),
            None => copyable_text("node-value", value).into_any_element(),
        };
        div()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .text_sm()
            .whitespace_normal()
            .child(value)
            .into_any_element()
    }
    fn fields(&self, fields: &[(String, Cell)], cx: &mut Context<Self>) -> AnyElement {
        let mut view = v_flex()
            .id(SharedString::from(format!(
                "node-fields-{}",
                fields
                    .iter()
                    .map(|(label, _)| label.as_str())
                    .collect::<Vec<_>>()
                    .join("-")
            )))
            .w_full()
            .gap_3();
        for (row, pair) in fields.chunks(2).enumerate() {
            view = view.child(
                h_flex()
                    .id(("node-row", row))
                    .w_full()
                    .flex_wrap()
                    .gap_x_4()
                    .gap_y_3()
                    .children(pair.iter().enumerate().map(|(index, (label, cell))| {
                        v_flex()
                            .id(("node-field", index))
                            .flex_1()
                            .flex_basis(px(190.))
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(label.clone()),
                            )
                            .child(self.value(cell, cx))
                    })),
            );
        }
        view.into_any_element()
    }
    fn render_group(&self, group: &Group, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .id(SharedString::from(format!("node-section-{}", group.title)))
            .w_full()
            .gap_3()
            .pb_4()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(self.heading(&group.title, cx))
            .child(self.fields(&group.fields, cx))
            .when_some(group.table.as_ref(), |this, table| {
                this.child(self.render_table(table, cx))
            })
            .into_any_element()
    }
    fn render_table(&self, table: &Table, cx: &mut Context<Self>) -> AnyElement {
        if table.rows.is_empty() {
            return div()
                .id(SharedString::from(format!(
                    "node-table-{}",
                    table.headers.join("-")
                )))
                .child(self.notice("<none>", cx))
                .into_any_element();
        }
        v_flex()
            .id(SharedString::from(format!(
                "node-table-{}",
                table.headers.join("-")
            )))
            .w_full()
            .min_w_0()
            .child(
                h_flex()
                    .w_full()
                    .bg(cx.theme().muted.opacity(0.5))
                    .children(table.headers.iter().map(|h| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .py_1p5()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(*h)
                    })),
            )
            .children(table.rows.iter().enumerate().map(|(row, cells)| {
                h_flex()
                    .id(("node-table-row", row))
                    .w_full()
                    .items_start()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .children(cells.iter().enumerate().map(|(column, cell)| {
                        div()
                            .id(("node-table-cell", column))
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .py_2()
                            .child(self.value(cell, cx))
                    }))
            }))
            .into_any_element()
    }
}

fn environment_value(env: &Value) -> Cell {
    if let Some(value) = env.get("value") {
        return Cell::of(Some(value));
    }
    let source = &env["valueFrom"];
    for (key, kind) in [("secretKeyRef", "Secret"), ("configMapKeyRef", "ConfigMap")] {
        if let Some(reference) = source.get(key) {
            return Cell::text(format!(
                "{kind} {} / {}{}",
                reference["name"].as_str().unwrap_or("?"),
                reference["key"].as_str().unwrap_or("?"),
                if reference["optional"] == true {
                    " (optional)"
                } else {
                    ""
                }
            ));
        }
    }
    if let Some(field) = source
        .pointer("/fieldRef/fieldPath")
        .and_then(Value::as_str)
    {
        return Cell::text(format!("Field {field}"));
    }
    if let Some(resource) = source.get("resourceFieldRef") {
        return Cell::text(format!(
            "Resource {}{} / divisor {}",
            resource["containerName"]
                .as_str()
                .map(|name| format!("{name}: "))
                .unwrap_or_default(),
            resource["resource"].as_str().unwrap_or("?"),
            resource["divisor"].as_str().unwrap_or("1")
        ));
    }
    Cell::of(env.get("valueFrom"))
}

fn resolve(object: &DynamicObject, node: &GraphNode) -> Option<Value> {
    if node.runtime {
        argo::resolved_template(&object.data, &node.data)
    } else if let Some(inline) = node.data.get("inline") {
        Some(inline.clone())
    } else {
        argo::resolved_template(&object.data, &node.data)
    }
}
fn template_target(
    session: &ClusterSession,
    object: &DynamicObject,
    node: &GraphNode,
) -> Option<(beacon_kube::Kind, String, String)> {
    let reference = node
        .data
        .get("templateRef")
        .or_else(|| object.data.pointer("/spec/workflowTemplateRef"));
    let (kind, name, template) = if let Some(reference) = reference {
        (
            if reference["clusterScope"] == true {
                "ClusterWorkflowTemplate"
            } else {
                "WorkflowTemplate"
            },
            reference["name"].as_str()?,
            reference["template"]
                .as_str()
                .or_else(|| node.data["templateName"].as_str())
                .or_else(|| node.data["template"].as_str())?,
        )
    } else {
        let scope = node.data["templateScope"].as_str()?;
        let (scope, name) = scope.split_once('/').unwrap_or(("namespaced", scope));
        if scope == "local" {
            return None;
        }
        (
            if scope == "cluster" {
                "ClusterWorkflowTemplate"
            } else {
                "WorkflowTemplate"
            },
            name,
            node.data["templateName"].as_str()?,
        )
    };
    let kind = session
        .discovery()
        .kinds()
        .iter()
        .find(|resource| resource.resource.group == argo::GROUP && resource.resource.kind == kind)?
        .clone();
    Some((kind, name.to_string(), template.to_string()))
}

impl Render for ArgoNodeView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let phase = if self.node.runtime {
            self.node.data["phase"].as_str().unwrap_or("Unknown")
        } else {
            "Template"
        };
        let tone = crate::argo::phase_tone(phase);
        let title = h_flex()
            .w_full()
            .flex_wrap()
            .gap_3()
            .items_center()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::MEDIUM)
                    .child(SelectableText::new("node-title", self.node.label.clone())),
            )
            .child(
                div()
                    .rounded_md()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .bg(cx.theme().tone_surface(tone))
                    .text_color(cx.theme().tone(tone))
                    .child(SelectableText::new("node-phase", phase.to_string())),
            );
        let mut body = v_flex().w_full().min_w_0().gap_3();
        if self.node.data.is_null() {
            body = body
                .child(self.notice("This node is no longer present in the workflow status.", cx));
        } else {
            body = body.child(match self.tab {
                TabKind::Summary => {
                    let mut summary = v_flex().w_full().gap_3().children(
                        self.node_summary()
                            .iter()
                            .map(|group| self.render_group(group, cx)),
                    );
                    if self.node.runtime
                        && matches!(
                            self.node.data["type"].as_str(),
                            Some("DAG" | "Steps" | "Retry" | "StepGroup" | "ContainerSet")
                        )
                        && array(&self.node.data, "/children").next().is_some()
                    {
                        summary = summary.child(
                            Button::new("view-subgraph")
                                .small()
                                .outline()
                                .label("View child graph")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    cx.emit(SubgraphRequested(view.node.id.clone()));
                                })),
                        );
                    }
                    let children: Vec<_> = array(&self.node.data, "/children")
                        .filter_map(Value::as_str)
                        .filter_map(|id| {
                            self.nodes
                                .get(id)
                                .map(|node| (id.to_string(), node.clone()))
                        })
                        .collect();
                    if !children.is_empty() {
                        summary = summary.child(self.heading("Child nodes", cx)).children(
                            children.into_iter().map(|(id, node)| {
                                let label = node["displayName"].as_str().unwrap_or(&id).to_string();
                                Button::new(SharedString::from(id.clone()))
                                    .ghost()
                                    .small()
                                    .label(label)
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view._load = None;
                                        view._template_load = None;
                                        view.node = GraphNode {
                                            id: id.clone(),
                                            label: node["displayName"]
                                                .as_str()
                                                .unwrap_or(&id)
                                                .into(),
                                            data: node.clone(),
                                            runtime: true,
                                            template_context: None,
                                            x: 0.,
                                            y: 0.,
                                        };
                                        view.pod = None;
                                        view.pod_error = None;
                                        view.template_error = None;
                                        view.template = resolve(&view.object, &view.node);
                                        view.update_yaml();
                                        view.load(window, cx);
                                        cx.notify();
                                    }))
                            }),
                        );
                    }
                    summary.into_any_element()
                }
                TabKind::Containers => self.render_containers(cx),
                TabKind::InputsOutputs => self.render_inputs_outputs(cx),
                TabKind::Yaml => div()
                    .w_full()
                    .font_family("monospace")
                    .text_sm()
                    .child(copyable_text("node-yaml", self.yaml.clone()))
                    .into_any_element(),
            });
        }
        v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .pb_4()
            .child(title)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SelectableText::new(
                        "node-workflow",
                        format!(
                            "{} · {}",
                            self.object.metadata.name.as_deref().unwrap_or(""),
                            self.object
                                .metadata
                                .namespace
                                .as_deref()
                                .unwrap_or("Cluster scoped")
                        ),
                    )),
            )
            .child(
                TabBar::new("node-tabs")
                    .small()
                    .selected_index(TABS.iter().position(|tab| *tab == self.tab).unwrap_or(0))
                    .children(
                        TABS.iter()
                            .map(|tab| Tab::new().label(tab.label(self.node.runtime))),
                    )
                    .on_click(cx.listener(|view, index: &usize, _, cx| {
                        if let Some(tab) = TABS.get(*index) {
                            view.tab = *tab;
                            cx.notify();
                        }
                    })),
            )
            .when(self.loading, |this| {
                this.child(self.notice("Loading Pod configuration…", cx))
            })
            .when_some(self.pod_error.as_ref(), |this, error| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().tone(Tone::Warning))
                        .child(copyable_text("node-pod-error", format!("Pod: {error}"))),
                )
            })
            .when_some(self.template_error.as_ref(), |this, error| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().tone(Tone::Warning))
                        .child(copyable_text(
                            "node-template-error",
                            format!("Template: {error}"),
                        )),
                )
            })
            .child(body)
    }
}
