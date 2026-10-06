//! Read-only Argo overview projections and graph layout.
use std::collections::{BTreeMap, BTreeSet};

use beacon_columns::{Timestamp, format_duration};
use beacon_kube::{
    DynamicObject,
    argo::{self, Nodes},
};
use serde_json::{Value, json};

use crate::{
    overview::{Cell, Group, Projection, Summary, Table},
    theme::Tone,
};

pub(crate) fn text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(v) if v.is_empty() => None,
        Value::String(v) => Some(v.clone()),
        v => Some(v.to_string()),
    }
}
fn at(data: &Value, path: &str) -> Cell {
    Cell::of(data.pointer(path))
}
fn default(data: &Value, path: &str, fallback: &str) -> Cell {
    let cell = at(data, path);
    if cell.value.is_some() {
        cell
    } else {
        Cell::text(fallback)
    }
}
pub(crate) fn array<'a>(data: &'a Value, path: &str) -> impl Iterator<Item = &'a Value> + use<'a> {
    data.pointer(path)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}
fn group_table(title: &str, headers: Vec<&'static str>, rows: Vec<Vec<Cell>>) -> Group {
    let mut group = Group::new(title, vec![]);
    group.table = Some(Table { headers, rows });
    group
}
fn parameters(data: &Value, path: &str, title: &str, defaults: bool) -> Group {
    group_table(
        title,
        vec!["Name", if defaults { "Value / default" } else { "Value" }],
        array(data, path)
            .map(|p| {
                vec![
                    at(p, "/name"),
                    Cell::of(
                        p.get("value")
                            .or_else(|| p.get("default"))
                            .or_else(|| p.get("valueFrom")),
                    ),
                ]
            })
            .collect(),
    )
}
fn reference(data: &Value, path: &str) -> Cell {
    let reference = data.pointer(path).unwrap_or(&Value::Null);
    at(reference, "/name").link(
        argo::GROUP,
        if reference["clusterScope"] == true {
            "ClusterWorkflowTemplate"
        } else {
            "WorkflowTemplate"
        },
    )
}

pub(crate) fn phase_tone(phase: &str) -> Tone {
    match phase {
        "Succeeded" => Tone::Healthy,
        "Running" => Tone::Progressing,
        "Failed" | "Error" => Tone::Critical,
        "Pending" | "Suspended" => Tone::Warning,
        _ => Tone::Unknown,
    }
}
pub(crate) fn timestamp(value: Option<&Value>) -> Cell {
    let Some(value) = value
        .and_then(Value::as_str)
        .filter(|value| !value.starts_with("0001-"))
    else {
        return Cell::default();
    };
    match value.parse::<Timestamp>() {
        Ok(timestamp) => Cell::text(timestamp.strftime("%Y-%m-%d %H:%M:%S UTC").to_string()),
        Err(_) => Cell::text(value.to_string()),
    }
}
pub(crate) fn duration(node: &Value, now: Timestamp) -> Option<String> {
    let started_text = node["startedAt"].as_str()?;
    if started_text.starts_with("0001-") {
        return None;
    }
    let started = started_text.parse::<Timestamp>().ok()?;
    let finished = node["finishedAt"]
        .as_str()
        .filter(|v| !v.starts_with("0001-"))
        .and_then(|v| v.parse::<Timestamp>().ok())
        .unwrap_or(now);
    Some(format_duration(finished.duration_since(started).as_secs()))
}
pub(crate) fn summary(
    kind: &str,
    object: &DynamicObject,
    nodes: &Nodes,
    now: Timestamp,
) -> Summary {
    let data = &object.data;
    let mut hints = Vec::new();
    let (label, health) = match kind {
        "Workflow" => {
            if let Some(duration) = duration(&data["status"], now) {
                hints.push(format!("{duration} elapsed"));
            }
            let tasks: Vec<_> = nodes
                .values()
                .filter(|node| {
                    matches!(
                        node["type"].as_str(),
                        Some("Pod" | "HTTP" | "Plugin" | "Container")
                    )
                })
                .collect();
            if !tasks.is_empty() {
                hints.push(format!(
                    "{} / {} succeeded",
                    tasks
                        .iter()
                        .filter(|node| node["phase"] == "Succeeded")
                        .count(),
                    tasks.len()
                ));
            }
            (
                text(&data["status"]["phase"]).unwrap_or_else(|| "Pending".into()),
                true,
            )
        }
        "CronWorkflow" => {
            hints.push(format!(
                "{} active workflow(s)",
                array(data, "/status/active").count()
            ));
            (
                if data["spec"]["suspend"] == true {
                    "Suspended".into()
                } else {
                    text(&data["status"]["phase"]).unwrap_or_else(|| "Active".into())
                },
                false,
            )
        }
        "WorkflowTemplate" | "ClusterWorkflowTemplate" => {
            hints.push(format!(
                "{} template(s)",
                array(data, "/spec/templates").count()
            ));
            (
                if kind == "ClusterWorkflowTemplate" {
                    "Cluster scoped"
                } else {
                    "Namespaced"
                }
                .into(),
                false,
            )
        }
        "WorkflowEventBinding" => ("Event binding".into(), false),
        "WorkflowTaskResult" => (
            text(&data["phase"]).unwrap_or_else(|| "Task result".into()),
            data["phase"].is_string(),
        ),
        _ => ("Controller resource".into(), false),
    };
    let tone = if health {
        phase_tone(&label)
    } else {
        Tone::Unknown
    };
    Summary {
        label,
        tone,
        health,
        hints,
        message: text(&data["status"]["message"]).or_else(|| text(&data["message"])),
    }
}

pub(crate) fn project(kind: &str, object: &DynamicObject) -> Projection {
    let data = &object.data;
    let mut groups = Vec::new();
    match kind {
        "Workflow" => {
            let spec = argo::execution_spec(data);
            let mut config = Group::new(
                "Run configuration",
                vec![
                    ("Template", reference(data, "/spec/workflowTemplateRef")),
                    ("Entrypoint", at(spec, "/entrypoint")),
                    ("Started", timestamp(data.pointer("/status/startedAt"))),
                    ("Finished", timestamp(data.pointer("/status/finishedAt"))),
                    (
                        "Service account",
                        at(spec, "/serviceAccountName").link("", "ServiceAccount"),
                    ),
                    ("Parallelism", at(spec, "/parallelism")),
                    ("Retry strategy", at(spec, "/retryStrategy")),
                    ("Progress", at(data, "/status/progress")),
                ],
            );
            config.collapsed = true;
            groups.push(config);
            groups.push(parameters(
                spec,
                "/arguments/parameters",
                "Parameters",
                false,
            ));
            let mut outputs = outputs_group(&data["status"]["outputs"], "Outputs");
            outputs.collapsed = true;
            groups.push(outputs);
        }
        "CronWorkflow" => {
            let schedule = data
                .pointer("/spec/schedules")
                .and_then(Value::as_array)
                .filter(|v| !v.is_empty())
                .map(|v| {
                    v.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                });
            groups.push(Group::new(
                "Schedule",
                vec![
                    (
                        "Schedule(s)",
                        schedule
                            .map(Cell::text)
                            .unwrap_or_else(|| at(data, "/spec/schedule")),
                    ),
                    (
                        "Timezone",
                        default(data, "/spec/timezone", "Controller timezone"),
                    ),
                    (
                        "Last scheduled",
                        timestamp(data.pointer("/status/lastScheduledTime")),
                    ),
                    (
                        "Concurrency policy",
                        default(data, "/spec/concurrencyPolicy", "Allow"),
                    ),
                    (
                        "Starting deadline",
                        at(data, "/spec/startingDeadlineSeconds"),
                    ),
                    ("Suspend", default(data, "/spec/suspend", "false")),
                    ("When expression", at(data, "/spec/when")),
                    ("Stop expression", at(data, "/spec/stopStrategy/expression")),
                ],
            ));
            let active = group_table(
                "Active workflows",
                vec!["Workflow"],
                array(data, "/status/active")
                    .map(|run| vec![at(run, "/name").link(argo::GROUP, "Workflow")])
                    .collect(),
            );
            groups.push(active);
            groups.push(Group::new(
                "Workflow configuration",
                vec![
                    (
                        "Template",
                        reference(data, "/spec/workflowSpec/workflowTemplateRef"),
                    ),
                    ("Entrypoint", at(data, "/spec/workflowSpec/entrypoint")),
                    (
                        "Service account",
                        at(data, "/spec/workflowSpec/serviceAccountName")
                            .link("", "ServiceAccount"),
                    ),
                    (
                        "Successful history limit",
                        default(data, "/spec/successfulJobsHistoryLimit", "3"),
                    ),
                    (
                        "Failed history limit",
                        default(data, "/spec/failedJobsHistoryLimit", "1"),
                    ),
                ],
            ));
            groups.push(parameters(
                data,
                "/spec/workflowSpec/arguments/parameters",
                "Parameters",
                false,
            ));
        }
        "WorkflowTemplate" | "ClusterWorkflowTemplate" => {
            groups.push(Group::new(
                "Template configuration",
                vec![
                    ("Entrypoint", at(data, "/spec/entrypoint")),
                    (
                        "Service account",
                        at(data, "/spec/serviceAccountName").link("", "ServiceAccount"),
                    ),
                    ("Parallelism", at(data, "/spec/parallelism")),
                    ("Retry strategy", at(data, "/spec/retryStrategy")),
                ],
            ));
            groups.push(parameters(
                data,
                "/spec/arguments/parameters",
                "Parameters",
                true,
            ));
        }
        "WorkflowEventBinding" => {
            groups.push(Group::new(
                "Binding",
                vec![
                    (
                        "Target template",
                        reference(data, "/spec/submit/workflowTemplateRef"),
                    ),
                    (
                        "Cluster scope",
                        default(
                            data,
                            "/spec/submit/workflowTemplateRef/clusterScope",
                            "false",
                        ),
                    ),
                    ("Selector", at(data, "/spec/event/selector")),
                    ("Discriminator", at(data, "/spec/event/discriminator")),
                ],
            ));
            groups.push(parameters(
                data,
                "/spec/submit/arguments/parameters",
                "Argument mapping",
                false,
            ));
        }
        "WorkflowTaskSet" => {
            groups.push(group_table(
                "Controller tasks",
                vec!["Node", "Type", "Phase", "Message / outputs"],
                data.pointer("/spec/tasks")
                    .and_then(Value::as_object)
                    .into_iter()
                    .flatten()
                    .map(|(id, task)| {
                        vec![
                            Cell::text(id.clone()),
                            Cell::text(template_type(task)),
                            Cell::of(
                                data.pointer("/status/nodes")
                                    .and_then(|n| n.get(id))
                                    .and_then(|n| n.get("phase")),
                            ),
                            Cell::of(
                                data.pointer("/status/nodes")
                                    .and_then(|n| n.get(id))
                                    .and_then(|n| {
                                        n.get("message")
                                            .filter(|v| text(v).is_some())
                                            .or_else(|| n.get("outputs"))
                                    }),
                            ),
                        ]
                    })
                    .collect(),
            ));
        }
        "WorkflowTaskResult" => {
            groups.push(Group::new(
                "Task result",
                vec![
                    ("Phase", at(data, "/phase")),
                    ("Progress", at(data, "/progress")),
                    ("Message", at(data, "/message")),
                ],
            ));
            groups.push(outputs_group(&data["outputs"], "Outputs"));
        }
        "WorkflowArtifactGCTask" => {
            let mut rows = Vec::new();
            for (node, entry) in data
                .pointer("/spec/artifactsByNode")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                for (name, artifact) in entry["artifacts"].as_object().into_iter().flatten() {
                    let result = data
                        .pointer("/status/artifactResultsByNode")
                        .and_then(|r| r.get(node))
                        .and_then(|r| r.get("artifactResults"))
                        .and_then(|r| r.get(name));
                    rows.push(vec![
                        Cell::text(node.clone()),
                        Cell::text(name.clone()),
                        Cell::text(
                            artifact_location(&with_archive_location(
                                artifact,
                                &entry["archiveLocation"],
                            ))
                            .unwrap_or_else(|| "<none>".into()),
                        ),
                        Cell::text(match result {
                            Some(result) if result["success"] == true => "Deleted".into(),
                            Some(result) => {
                                text(&result["error"]).unwrap_or_else(|| "Not deleted".into())
                            }
                            None => "Pending".into(),
                        }),
                    ]);
                }
            }
            groups.push(group_table(
                "Artifacts",
                vec!["Node", "Artifact", "Location", "Deletion result"],
                rows,
            ));
        }
        _ => {}
    }
    if object
        .metadata
        .owner_references
        .as_ref()
        .is_some_and(|owners| !owners.is_empty())
        && let Some(owner) = groups.first_mut()
    {
        owner.owner = true;
    }
    // Skip large controller-persisted execution data before cloning. A workflow
    // may contain thousands of nodes; rendering its overview must not copy them.
    let safe = Value::Object(
        data.as_object()
            .into_iter()
            .flatten()
            .map(|(key, value)| {
                let value = if key == "status" {
                    filtered_object(
                        value,
                        &[
                            "nodes",
                            "compressedNodes",
                            "storedTemplates",
                            "storedWorkflowSpec",
                            "storedWorkflowTemplateSpec",
                        ],
                    )
                } else if key == "spec"
                    && matches!(
                        kind,
                        "Workflow" | "WorkflowTemplate" | "ClusterWorkflowTemplate"
                    )
                {
                    filtered_object(value, &["templates"])
                } else {
                    value.clone()
                };
                (key.clone(), value)
            })
            .collect(),
    );
    Projection {
        groups,
        conditions: data
            .pointer("/status/conditions")
            .map(crate::overview::rows)
            .unwrap_or_default(),
        additional: crate::overview::sections(argo::GROUP, kind, &safe),
    }
}

/// A label scopes the watch; an owner UID prevents name reuse from mixing
/// histories belonging to different CronWorkflows. Deleted runs are unavailable.
pub(crate) fn recent_runs(
    object: &DynamicObject,
    runs: Vec<std::sync::Arc<DynamicObject>>,
    now: Timestamp,
) -> Group {
    let mut runs: Vec<_> = runs
        .into_iter()
        .filter(|run| {
            run.metadata.namespace == object.metadata.namespace
                && run
                    .metadata
                    .owner_references
                    .as_ref()
                    .is_some_and(|owners| {
                        owners.iter().any(|owner| {
                            owner.kind == "CronWorkflow"
                                && owner.name == object.metadata.name.as_deref().unwrap_or("")
                                && object
                                    .metadata
                                    .uid
                                    .as_ref()
                                    .is_none_or(|uid| &owner.uid == uid)
                        })
                    })
        })
        .collect();
    runs.sort_by(|a, b| {
        b.metadata
            .creation_timestamp
            .cmp(&a.metadata.creation_timestamp)
            .then_with(|| a.metadata.name.cmp(&b.metadata.name))
    });
    group_table(
        "Recent runs",
        vec!["Workflow", "Phase", "Started", "Duration"],
        runs.into_iter()
            .take(20)
            .map(|run| {
                vec![
                    Cell::text(run.metadata.name.clone().unwrap_or_default())
                        .link(argo::GROUP, "Workflow"),
                    at(&run.data, "/status/phase"),
                    timestamp(run.data.pointer("/status/startedAt")),
                    Cell {
                        value: duration(&run.data["status"], now),
                        reference: None,
                    },
                ]
            })
            .collect(),
    )
}

fn filtered_object(value: &Value, skipped: &[&str]) -> Value {
    match value.as_object() {
        Some(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !skipped.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        None => value.clone(),
    }
}

pub(crate) fn template_type(template: &Value) -> &'static str {
    for (key, label) in [
        ("container", "Container"),
        ("script", "Script"),
        ("dag", "DAG"),
        ("steps", "Steps"),
        ("containerSet", "ContainerSet"),
        ("suspend", "Suspend"),
        ("http", "HTTP"),
        ("resource", "Resource"),
        ("data", "Data"),
        ("plugin", "Plugin"),
        ("templateRef", "Template reference"),
    ] {
        if template.get(key).is_some() {
            return label;
        }
    }
    "Template"
}
pub(crate) fn artifact_location(artifact: &Value) -> Option<String> {
    if let (Some(bucket), Some(key)) = (
        artifact.pointer("/s3/bucket").and_then(Value::as_str),
        artifact.pointer("/s3/key").and_then(Value::as_str),
    ) {
        return Some(format!("s3://{bucket}/{key}"));
    }
    if let (Some(bucket), Some(key)) = (
        artifact.pointer("/gcs/bucket").and_then(Value::as_str),
        artifact.pointer("/gcs/key").and_then(Value::as_str),
    ) {
        return Some(format!("gs://{bucket}/{key}"));
    }
    for path in [
        "/http/url",
        "/git/repo",
        "/oss/key",
        "/azure/blob",
        "/artifactory/url",
        "/hdfs/path",
        "/raw/data",
        "/from",
        "/fromExpression",
    ] {
        if let Some(v) = artifact.pointer(path).and_then(text) {
            return Some(v);
        }
    }
    None
}
fn with_archive_location(artifact: &Value, archive: &Value) -> Value {
    let mut resolved = artifact.clone();
    if let Some(map) = resolved.as_object_mut() {
        for key in ["s3", "gcs", "oss", "azure", "artifactory", "hdfs"] {
            if let Some(source) = archive[key].as_object()
                && let Some(target) = map.get_mut(key).and_then(Value::as_object_mut)
            {
                for (key, value) in source {
                    target.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
        }
    }
    resolved
}

pub(crate) fn outputs_group(outputs: &Value, title: &str) -> Group {
    let mut group = parameters(outputs, "/parameters", title, false);
    group.fields = vec![
        ("Result".into(), at(outputs, "/result")),
        ("Exit code".into(), at(outputs, "/exitCode")),
    ];
    for artifact in array(outputs, "/artifacts") {
        group.fields.push((
            format!(
                "Artifact / {}",
                artifact["name"].as_str().unwrap_or("unnamed")
            ),
            Cell::text(artifact_location(artifact).unwrap_or_else(|| artifact.to_string())),
        ));
    }
    group
}

#[derive(Clone, Debug)]
pub(crate) struct GraphNode {
    pub id: String,
    pub label: String,
    pub data: Value,
    pub runtime: bool,
    pub template_context: Option<String>,
    pub x: f32,
    pub y: f32,
}
#[derive(Default, Clone, Debug)]
pub(crate) struct Graph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<(usize, usize)>,
    pub width: f32,
    pub height: f32,
}
pub(crate) const CARD_WIDTH: f32 = 172.;
pub(crate) const CARD_HEIGHT: f32 = 60.;

impl Graph {
    pub(crate) fn workflow(object: &DynamicObject, nodes: &Nodes, scope: Option<&str>) -> Self {
        let root = scope.or(object.metadata.name.as_deref());
        let is_group = root.and_then(|id| nodes.get(id)).is_some_and(|node| {
            matches!(
                node["type"].as_str(),
                Some("DAG" | "Steps" | "Retry" | "StepGroup" | "ContainerSet")
            )
        });
        let selected: Vec<_> = nodes
            .iter()
            .filter(|(id, node)| {
                !is_group
                    || (Some(id.as_str()) != root
                        && (node["boundaryID"].as_str() == root
                            || root
                                .and_then(|root| nodes.get(root))
                                .and_then(|n| n["children"].as_array())
                                .is_some_and(|children| {
                                    children
                                        .iter()
                                        .any(|child| child.as_str() == Some(id.as_str()))
                                })))
            })
            .map(|(id, node)| {
                let mut node = node.clone();
                if node["id"].as_str().is_none() {
                    node["id"] = json!(id);
                }
                GraphNode {
                    id: id.clone(),
                    label: node["displayName"]
                        .as_str()
                        .or_else(|| node["name"].as_str())
                        .unwrap_or(id)
                        .into(),
                    data: node,
                    runtime: true,
                    template_context: None,
                    x: 0.,
                    y: 0.,
                }
            })
            .collect();
        let lookup: BTreeMap<_, _> = selected
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        // Collapse edges through nested boundaries to the visible group node.
        // An inner DAG's final Pod often owns the edge to the next outer task.
        let representative = |id: &str| {
            let mut current = id;
            let mut visited = BTreeSet::new();
            loop {
                if let Some(index) = lookup.get(current) {
                    return Some(*index);
                }
                if !visited.insert(current) {
                    return None;
                }
                current = nodes.get(current)?.get("boundaryID")?.as_str()?;
            }
        };
        let mut edges = Vec::new();
        for (id, node) in nodes {
            if let Some(from) = representative(id) {
                for child in array(node, "/children").filter_map(Value::as_str) {
                    if let Some(to) = representative(child) {
                        edges.push((from, to));
                    }
                }
            }
        }
        Self::layout(selected, edges)
    }

    pub(crate) fn template(spec: &Value, name: &str) -> Self {
        let Some(template) = array(spec, "/templates").find(|t| t["name"] == name) else {
            return Self::default();
        };
        if let Some(tasks) = template.pointer("/dag/tasks").and_then(Value::as_array) {
            let nodes: Vec<_> = tasks
                .iter()
                .enumerate()
                .map(|(i, task)| GraphNode {
                    id: task["name"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| i.to_string()),
                    label: task["name"].as_str().unwrap_or("Unnamed task").into(),
                    data: task.clone(),
                    runtime: false,
                    template_context: Some(name.to_string()),
                    x: 0.,
                    y: 0.,
                })
                .collect();
            let ids: BTreeMap<_, _> = nodes
                .iter()
                .enumerate()
                .map(|(i, n)| (n.id.as_str(), i))
                .collect();
            let mut edges = BTreeSet::new();
            for (to, node) in nodes.iter().enumerate() {
                for name in array(&node.data, "/dependencies")
                    .filter_map(Value::as_str)
                    .chain(
                        node.data["depends"]
                            .as_str()
                            .into_iter()
                            .flat_map(|v| {
                                v.split(|c: char| {
                                    !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
                                })
                            })
                            .map(|token| token.split('.').next().unwrap_or(token)),
                    )
                {
                    if let Some(from) = ids.get(name) {
                        edges.insert((*from, to));
                    }
                }
            }
            return Self::layout(nodes, edges.into_iter().collect());
        }
        if let Some(groups) = template["steps"].as_array() {
            let mut nodes = Vec::new();
            let mut edges = Vec::new();
            let mut previous = Vec::new();
            for (group, steps) in groups.iter().enumerate() {
                let mut current = Vec::new();
                for (index, step) in steps.as_array().into_iter().flatten().enumerate() {
                    let at = nodes.len();
                    current.push(at);
                    nodes.push(GraphNode {
                        id: format!("{group}-{index}"),
                        label: step["name"].as_str().unwrap_or("Unnamed step").into(),
                        data: step.clone(),
                        runtime: false,
                        template_context: Some(name.to_string()),
                        x: 0.,
                        y: 0.,
                    });
                    edges.extend(previous.iter().map(|from| (*from, at)));
                }
                if !current.is_empty() {
                    previous = current;
                }
            }
            return Self::layout(nodes, edges);
        }
        Self::layout(
            vec![GraphNode {
                id: name.into(),
                label: name.into(),
                data: json!({"template":name}),
                runtime: false,
                template_context: Some(name.to_string()),
                x: 0.,
                y: 0.,
            }],
            vec![],
        )
    }

    fn layout(nodes: Vec<GraphNode>, mut edges: Vec<(usize, usize)>) -> Self {
        let count = nodes.len();
        if count == 0 {
            return Self::default();
        }
        edges.sort_unstable();
        edges.dedup();
        edges.retain(|(a, b)| *a < count && *b < count && a != b);
        let mut indegree = vec![0; count];
        let mut outgoing = vec![Vec::new(); count];
        let mut layers = vec![0; count];
        for &(from, to) in &edges {
            indegree[to] += 1;
            outgoing[from].push(to);
        }
        let mut ready: BTreeSet<_> = (0..count).filter(|i| indegree[*i] == 0).collect();
        while let Some(from) = ready.pop_first() {
            for &to in &outgoing[from] {
                layers[to] = layers[to].max(layers[from] + 1);
                indegree[to] -= 1;
                if indegree[to] == 0 {
                    ready.insert(to);
                }
            }
        }
        // A malformed cycle must still be inspectable and must not hang layout.
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (i, layer) in layers.iter().enumerate() {
            groups.entry(*layer).or_default().push(i);
        }
        let width = (groups.values().map(Vec::len).max().unwrap_or(1) as f32 * (CARD_WIDTH + 20.)
            + 24.)
            .max(360.);
        let height = groups.keys().next_back().copied().unwrap_or(0) as f32 * (CARD_HEIGHT + 28.)
            + CARD_HEIGHT
            + 24.;
        let mut graph = Self {
            nodes,
            edges,
            width,
            height,
        };
        for (layer, group) in groups {
            let start = (width - (group.len() as f32 * (CARD_WIDTH + 20.) - 20.)) / 2.;
            for (column, index) in group.into_iter().enumerate() {
                graph.nodes[index].x = start + column as f32 * (CARD_WIDTH + 20.);
                graph.nodes[index].y = 12. + layer as f32 * (CARD_HEIGHT + 28.);
            }
        }
        graph
    }
}

#[cfg(test)]
mod tests {
    use super::{Graph, project};
    use beacon_kube::DynamicObject;
    use serde_json::json;
    fn object(kind: &str, data: serde_json::Value) -> DynamicObject {
        let mut object:DynamicObject=serde_json::from_value(json!({"apiVersion":"argoproj.io/v1alpha1","kind":kind,"metadata":{"name":"run","namespace":"default"}})).unwrap();
        object.data = data;
        object
    }
    #[test]
    fn enhanced_depends_and_parallel_steps_keep_graph_dependencies() {
        let spec = json!({"templates":[{"name":"dag","dag":{"tasks":[{"name":"a"},{"name":"b"},{"name":"c","depends":"a.Succeeded && (b.Failed || b.Omitted)"}]}},{"name":"steps","steps":[[{"name":"a"},{"name":"b"}],[{"name":"c"}]]}]});
        for name in ["dag", "steps"] {
            let graph = Graph::template(&spec, name);
            assert_eq!(graph.edges, vec![(0, 2), (1, 2)]);
            assert_eq!(graph.nodes[0].y, graph.nodes[1].y);
            assert!(graph.nodes[2].y > graph.nodes[1].y);
        }
    }
    #[test]
    fn cycles_and_missing_edges_remain_inspectable() {
        let spec = json!({"templates":[{"name":"main","dag":{"tasks":[{"name":"a","dependencies":["b","missing"]},{"name":"b","dependencies":["a"]}]}}]});
        let graph = Graph::template(&spec, "main");
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 2);
    }
    #[test]
    fn controller_outputs_are_top_level_and_additional_does_not_duplicate_nodes() {
        let result = project(
            "WorkflowTaskResult",
            &object(
                "WorkflowTaskResult",
                json!({"phase":"Succeeded","outputs":{"exitCode":"0","result":"ok","parameters":[{"name":"count","value":"0"}]}}),
            ),
        );
        assert_eq!(result.groups[1].fields[1].1.value.as_deref(), Some("0"));
        assert_eq!(
            result.groups[1].table.as_ref().unwrap().rows[0][1]
                .value
                .as_deref(),
            Some("0")
        );
        let workflow = project(
            "Workflow",
            &object(
                "Workflow",
                json!({"status":{"nodes":{"large":{"message":"unique node text"}},"phase":"Running"}}),
            ),
        );
        assert!(!format!("{:?}", workflow.additional).contains("unique node text"));
    }
    #[test]
    fn nested_workflow_graph_shows_boundaries_without_flattening_every_subgraph() {
        let workflow = object("Workflow", json!({}));
        let nodes=serde_json::from_value(json!({"run":{"id":"run","type":"DAG","children":["sub"]},"sub":{"id":"sub","type":"DAG","boundaryID":"run","children":["leaf"]},"leaf":{"id":"leaf","type":"Pod","boundaryID":"sub"}})).unwrap();
        assert_eq!(Graph::workflow(&workflow, &nodes, None).nodes[0].id, "sub");
        assert_eq!(
            Graph::workflow(&workflow, &nodes, Some("sub")).nodes[0].id,
            "leaf"
        );
    }
    #[test]
    fn collapsed_dag_preserves_links_from_inner_leaf_to_outer_task() {
        let workflow = object("Workflow", json!({}));
        let nodes=serde_json::from_value(json!({"run":{"type":"DAG","children":["extract"]},"extract":{"type":"Pod","boundaryID":"run","children":["nested"]},"nested":{"type":"DAG","boundaryID":"run","children":["leaf"]},"leaf":{"type":"Pod","boundaryID":"nested","children":["publish"]},"publish":{"type":"Pod","boundaryID":"run"}})).unwrap();
        let graph = Graph::workflow(&workflow, &nodes, None);
        let labels: Vec<_> = graph
            .edges
            .iter()
            .map(|(a, b)| (graph.nodes[*a].id.as_str(), graph.nodes[*b].id.as_str()))
            .collect();
        assert_eq!(labels, [("extract", "nested"), ("nested", "publish")]);
    }

    #[test]
    fn pending_zero_timestamps_do_not_report_centuries_of_runtime() {
        let now = "2026-10-06T12:00:00Z".parse().unwrap();
        assert!(super::duration(&json!({"startedAt":"0001-01-01T00:00:00Z"}), now).is_none());
        assert_eq!(
            super::duration(
                &json!({"startedAt":"2026-10-06T11:59:00Z","finishedAt":"0001-01-01T00:00:00Z"}),
                now
            )
            .as_deref(),
            Some("60s")
        );
    }

    #[test]
    fn cron_history_rejects_reused_names_and_other_namespaces() {
        let mut cron = object("CronWorkflow", json!({}));
        cron.metadata.uid = Some("current".into());
        let make_run = |name: &str, namespace: &str, uid: &str| {
            let mut run = object("Workflow", json!({"status":{"phase":"Succeeded"}}));
            run.metadata.name = Some(name.into());
            run.metadata.namespace = Some(namespace.into());
            run.metadata.owner_references=Some(serde_json::from_value(json!([{"apiVersion":"argoproj.io/v1alpha1","kind":"CronWorkflow","name":"run","uid":uid}])).unwrap());
            std::sync::Arc::new(run)
        };
        let history = super::recent_runs(
            &cron,
            vec![
                make_run("good", "default", "current"),
                make_run("old", "default", "previous"),
                make_run("other", "production", "current"),
            ],
            "2026-10-06T12:00:00Z".parse().unwrap(),
        );
        let rows = history.table.unwrap().rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].value.as_deref(), Some("good"));
    }

    #[test]
    fn garbage_collection_uses_archive_locations_and_reports_failures() {
        let projection = project(
            "WorkflowArtifactGCTask",
            &object(
                "WorkflowArtifactGCTask",
                json!({"spec":{"artifactsByNode":{"node":{"archiveLocation":{"s3":{"bucket":"shared"}},"artifacts":{"data":{"s3":{"key":"data.json"}}}}}},"status":{"artifactResultsByNode":{"node":{"artifactResults":{"data":{"success":false,"error":"AccessDenied"}}}}}}),
            ),
        );
        let rows = &projection.groups[0].table.as_ref().unwrap().rows;
        assert_eq!(rows[0][2].value.as_deref(), Some("s3://shared/data.json"));
        assert_eq!(rows[0][3].value.as_deref(), Some("AccessDenied"));
    }
}
