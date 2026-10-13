//! Argo operations through its Kubernetes representations, following the CLI's
//! distinct retry/resubmit, suspend-node and shutdown semantics.
//!
//! https://argo-workflows.readthedocs.io/en/latest/cli/argo_retry/
//! https://github.com/argoproj/argo-workflows/blob/main/workflow/util/util.go
use crate::{
    ClusterSession, DeleteTarget, DynamicObject, Error, Kind, ObjectRef, Result, argo, resources,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::{Compression, write::GzEncoder};
use kube::{
    Api,
    api::{DeleteParams, Patch, PatchParams, Preconditions},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write as _,
    sync::Arc,
    time::Duration,
};
mod parameters;
mod retry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Submit,
    Retry,
    Resubmit,
    Suspend,
    Resume,
    Stop,
    Terminate,
}
impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Submit => "Submit Workflow",
            Self::Retry => "Retry failed steps",
            Self::Resubmit => "Resubmit Workflow",
            Self::Suspend => "Suspend Workflow",
            Self::Resume => "Resume Workflow",
            Self::Stop => "Stop Workflow",
            Self::Terminate => "Terminate Workflow",
        }
    }
    pub fn verb(self) -> &'static str {
        if matches!(self, Self::Submit | Self::Resubmit) {
            "create"
        } else {
            "patch"
        }
    }
    pub fn creates(self) -> bool {
        matches!(self, Self::Submit | Self::Resubmit)
    }
    pub fn applies(self, group: &str, kind: &str) -> bool {
        group == argo::GROUP
            && if self == Self::Submit {
                matches!(
                    kind,
                    "WorkflowTemplate" | "ClusterWorkflowTemplate" | "CronWorkflow"
                )
            } else {
                kind == "Workflow"
            }
    }
    pub fn available(self, kind: &str, object: &DynamicObject) -> bool {
        if self == Self::Submit {
            return matches!(
                kind,
                "WorkflowTemplate" | "ClusterWorkflowTemplate" | "CronWorkflow"
            );
        }
        if kind != "Workflow" {
            return false;
        }
        let phase = object
            .data
            .pointer("/status/phase")
            .and_then(Value::as_str)
            .unwrap_or("");
        let completed = matches!(phase, "Succeeded" | "Failed" | "Error")
            || object.metadata.labels.as_ref().is_some_and(|labels| {
                labels
                    .get("workflows.argoproj.io/completed")
                    .is_some_and(|value| value == "true")
            });
        let suspended = object
            .data
            .pointer("/spec/suspend")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || object
                .data
                .pointer("/status/nodes")
                .and_then(Value::as_object)
                .is_some_and(|nodes| nodes.values().any(active_suspend));
        let unknown_nodes = object
            .data
            .pointer("/status/compressedNodes")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
            || object
                .data
                .pointer("/status/offloadNodeStatusVersion")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty());
        let shutdown = object
            .data
            .pointer("/spec/shutdown")
            .and_then(Value::as_str)
            .unwrap_or("");
        match self {
            Self::Retry => matches!(phase, "Failed" | "Error"),
            Self::Resubmit => completed,
            Self::Suspend => !completed && !suspended && shutdown.is_empty(),
            Self::Resume => !completed && (suspended || unknown_nodes) && shutdown.is_empty(),
            Self::Stop => !completed && shutdown.is_empty(),
            Self::Terminate => !completed && shutdown != "Terminate",
            Self::Submit => false,
        }
    }
}
fn active_suspend(node: &Value) -> bool {
    node["type"] == "Suspend" && node["phase"] == "Running"
}
fn text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        _ => Some(value.to_string()),
    }
}

#[derive(Clone, Debug)]
pub struct Parameter {
    pub key: String,
    pub name: String,
    pub value: Option<String>,
    pub description: String,
    pub choices: Vec<String>,
    pub required: bool,
    pub runtime: bool,
}
pub fn parameters(kind: &str, object: &DynamicObject, action: Action) -> Result<Vec<Parameter>> {
    parameters_for_entrypoint(kind, object, action, &default_entrypoint(kind, object))
}
pub fn parameters_for_entrypoint(
    kind: &str,
    object: &DynamicObject,
    action: Action,
    entrypoint: &str,
) -> Result<Vec<Parameter>> {
    let mut result = vec![];
    if action == Action::Resume {
        for (id, node) in argo::nodes(&object.data).map_err(Error::Manifest)? {
            if !active_suspend(&node) {
                continue;
            }
            for param in node
                .pointer("/outputs/parameters")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if param.pointer("/valueFrom/supplied").is_none() {
                    continue;
                }
                let name = param["name"].as_str().ok_or_else(|| {
                    Error::Manifest("a suspended output parameter has no name".into())
                })?;
                let value = param
                    .get("value")
                    .and_then(text)
                    .or_else(|| param.pointer("/valueFrom/default").and_then(text));
                result.push(Parameter {
                    key: format!("{id}/{name}"),
                    name: format!("{} · {name}", node["displayName"].as_str().unwrap_or(&id)),
                    required: value.is_none(),
                    value,
                    description: "Output supplied to the suspended step before resuming.".into(),
                    choices: vec![],
                    runtime: false,
                });
            }
        }
        return Ok(result);
    }
    if !action.creates() {
        return Ok(result);
    }
    let spec = if kind == "CronWorkflow" {
        &object.data["spec"]["workflowSpec"]
    } else {
        &object.data["spec"]
    };
    for param in spec
        .pointer("/arguments/parameters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = param["name"]
            .as_str()
            .ok_or_else(|| Error::Manifest("a Workflow parameter has no name".into()))?;
        let value = param
            .get("value")
            .and_then(text)
            .or_else(|| param.get("default").and_then(text));
        let runtime = param
            .get("valueFrom")
            .is_some_and(|value| value.is_object());
        result.push(Parameter {
            key: name.into(),
            name: name.into(),
            required: value.is_none() && !runtime,
            value,
            description: param
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            choices: param
                .get("enum")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(text)
                .collect(),
            runtime,
        });
    }
    parameters::merge(spec, entrypoint, result)
}

#[derive(Clone, Default)]
pub struct Inputs {
    pub namespace: String,
    pub entrypoint: String,
    /// Omitted keys keep runtime valueFrom references. An explicit empty value
    /// is a valid supplied string and remains distinct from a missing value.
    pub values: BTreeMap<String, String>,
}
#[derive(Clone)]
pub struct Plan {
    pub action: Action,
    pub source_kind: Kind,
    pub source: Arc<DynamicObject>,
    pub workflow_kind: Kind,
    pub inputs: Inputs,
    pub proposed: Value,
    pub pods: Vec<DeleteTarget>,
}

pub fn entrypoints(kind: &str, object: &DynamicObject) -> Vec<String> {
    let spec = if kind == "CronWorkflow" {
        &object.data["spec"]["workflowSpec"]
    } else {
        &object.data["spec"]
    };
    spec.get("templates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|template| template["name"].as_str().map(str::to_owned))
        .collect()
}
pub fn default_entrypoint(kind: &str, object: &DynamicObject) -> String {
    let spec = if kind == "CronWorkflow" {
        &object.data["spec"]["workflowSpec"]
    } else {
        &object.data["spec"]
    };
    spec.get("entrypoint")
        .and_then(Value::as_str)
        .unwrap_or("")
        .into()
}

fn clean_metadata(source: &DynamicObject) -> Value {
    let mut metadata = json!({"labels":source.metadata.labels.clone().unwrap_or_default(),"annotations":source.metadata.annotations.clone().unwrap_or_default()});
    let labels = metadata["labels"].as_object_mut().unwrap();
    labels.retain(|name, _| {
        !matches!(
            name.as_str(),
            "workflows.argoproj.io/phase"
                | "workflows.argoproj.io/completed"
                | "workflows.argoproj.io/workflow-archiving-status"
        ) && !name.starts_with("workflows.argoproj.io/creator")
            && !name.starts_with("workflows.argoproj.io/actor")
    });
    metadata["annotations"]
        .as_object_mut()
        .unwrap()
        .remove("kubectl.kubernetes.io/last-applied-configuration");
    metadata
}

pub fn new_workflow(
    kind: &str,
    source: &DynamicObject,
    inputs: &Inputs,
    action: Action,
) -> Result<Value> {
    let namespace = if kind == "ClusterWorkflowTemplate" {
        inputs.namespace.as_str()
    } else {
        source.metadata.namespace.as_deref().unwrap_or("")
    };
    if namespace.is_empty() {
        return Err(Error::Manifest(
            "choose the namespace for the Workflow".into(),
        ));
    }
    let name = source
        .metadata
        .name
        .as_deref()
        .ok_or_else(|| Error::Manifest("the source has no name".into()))?;
    let source_spec = if kind == "CronWorkflow" {
        &source.data["spec"]["workflowSpec"]
    } else {
        &source.data["spec"]
    };
    let mut spec = if matches!(kind, "WorkflowTemplate" | "ClusterWorkflowTemplate") {
        json!({"workflowTemplateRef":{"name":name,"clusterScope":kind=="ClusterWorkflowTemplate"}})
    } else {
        source_spec.clone()
    };
    if !spec.is_object() {
        return Err(Error::Manifest("the source has no Workflow spec".into()));
    }
    spec.as_object_mut().unwrap().remove("shutdown");
    if spec.get("activeDeadlineSeconds").and_then(Value::as_i64) == Some(0) {
        spec.as_object_mut()
            .unwrap()
            .remove("activeDeadlineSeconds");
    }
    let entrypoint = if inputs.entrypoint.is_empty() {
        default_entrypoint(kind, source)
    } else {
        inputs.entrypoint.clone()
    };
    if entrypoint.is_empty() && source_spec.get("workflowTemplateRef").is_none() {
        return Err(Error::Manifest(
            "choose an entrypoint before submitting".into(),
        ));
    }
    let entries = entrypoints(kind, source);
    if !entries.is_empty() && !entrypoint.is_empty() && !entries.contains(&entrypoint) {
        return Err(Error::Manifest(
            "the chosen entrypoint is not present in this template".into(),
        ));
    }
    if !entrypoint.is_empty() {
        spec["entrypoint"] = json!(entrypoint);
    }
    let definitions = parameters_for_entrypoint(kind, source, action, &entrypoint)?;
    for definition in &definitions {
        let value = inputs
            .values
            .get(&definition.key)
            .or(definition.value.as_ref());
        if definition.required && value.is_none() {
            return Err(Error::Manifest(format!(
                "provide a value for {}",
                definition.name
            )));
        }
        if let Some(value) = value
            && !definition.choices.is_empty()
            && !definition.choices.contains(value)
        {
            return Err(Error::Manifest(format!(
                "{} must be one of {}",
                definition.name,
                definition.choices.join(", ")
            )));
        }
    }
    if inputs
        .values
        .keys()
        .any(|key| !definitions.iter().any(|definition| &definition.key == key))
    {
        return Err(Error::Manifest(
            "an unknown Workflow parameter was supplied".into(),
        ));
    }
    let mut arguments = source_spec
        .get("arguments")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let mut params = arguments
        .get("parameters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // A bare global declaration must not shadow a runtime reference on the
    // selected template input. Let Argo resolve that input from the template.
    params.retain(|param| {
        !(param.get("value").is_none()
            && param.get("valueFrom").is_none()
            && definitions
                .iter()
                .any(|definition| definition.key == param["name"] && definition.runtime)
            && !param["name"]
                .as_str()
                .is_some_and(|name| inputs.values.contains_key(name)))
    });
    for definition in &definitions {
        if !params.iter().any(|param| param["name"] == definition.key)
            && (!definition.runtime || inputs.values.contains_key(&definition.key))
        {
            let mut param = json!({"name":definition.key});
            if let Some(value) = &definition.value {
                param["value"] = json!(value);
            }
            params.push(param);
        }
    }
    for param in &mut params {
        if let Some(value) = param["name"]
            .as_str()
            .and_then(|name| inputs.values.get(name))
            .cloned()
            .or_else(|| {
                if param.get("value").is_none() && param.get("valueFrom").is_none() {
                    param.get("default").and_then(text).or_else(|| {
                        definitions
                            .iter()
                            .find(|definition| definition.key == param["name"])
                            .and_then(|definition| definition.value.clone())
                    })
                } else {
                    None
                }
            })
        {
            param.as_object_mut().unwrap().remove("valueFrom");
            param["value"] = json!(value);
        }
        param.as_object_mut().unwrap().remove("default");
    }
    arguments["parameters"] = json!(params);
    spec["arguments"] = arguments;
    let mut metadata = clean_metadata(source);
    let workflow_metadata_spec = if kind == "CronWorkflow" {
        &source.data["spec"]
    } else {
        source_spec
    };
    if let Some(workflow_metadata) = workflow_metadata_spec
        .get("workflowMetadata")
        .and_then(Value::as_object)
    {
        if let Some(finalizers) = workflow_metadata
            .get("finalizers")
            .and_then(Value::as_array)
        {
            metadata["finalizers"] = json!(finalizers);
        }
        for field in ["labels", "annotations"] {
            if let Some(values) = workflow_metadata.get(field).and_then(Value::as_object) {
                metadata[field]
                    .as_object_mut()
                    .unwrap()
                    .extend(values.clone());
            }
        }
    }
    if kind == "CronWorkflow" {
        let uid = source
            .metadata
            .uid
            .as_deref()
            .ok_or_else(|| Error::Manifest("the CronWorkflow has no UID".into()))?;
        metadata["labels"]["workflows.argoproj.io/cron-workflow"] = json!(name);
        metadata["ownerReferences"] = json!([{"apiVersion":"argoproj.io/v1alpha1","kind":"CronWorkflow","name":name,"uid":uid,"controller":true}]);
    }
    if matches!(kind, "WorkflowTemplate" | "ClusterWorkflowTemplate") {
        let label = if kind == "WorkflowTemplate" {
            "workflows.argoproj.io/workflow-template"
        } else {
            "workflows.argoproj.io/cluster-workflow-template"
        };
        metadata["labels"][label] = json!(name);
    }
    metadata["namespace"] = json!(namespace);
    let prefix = if action == Action::Resubmit {
        source.metadata.generate_name.as_deref().unwrap_or(name)
    } else {
        name
    };
    metadata["generateName"] = json!(format!(
        "{}-",
        prefix
            .chars()
            .take(240)
            .collect::<String>()
            .trim_end_matches('-')
    ));
    metadata["annotations"]["beacon.dev/run-source"] = json!(format!("{kind}/{name}"));
    if action == Action::Resubmit {
        metadata["annotations"]["beacon.dev/previous-workflow"] = json!(name);
    }
    if matches!(kind, "WorkflowTemplate" | "ClusterWorkflowTemplate") {
        metadata["annotations"]["beacon.dev/template-uid"] = json!(source.metadata.uid);
    }
    Ok(
        json!({"apiVersion":"argoproj.io/v1alpha1","kind":"Workflow","metadata":metadata,"spec":spec}),
    )
}

fn replace_nodes(status: &mut Value, nodes: argo::Nodes, compressed: bool) -> Result<()> {
    if compressed {
        let data =
            serde_json::to_vec(&nodes).map_err(|error| Error::Manifest(error.to_string()))?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&data)
            .map_err(|error| Error::Manifest(error.to_string()))?;
        status["compressedNodes"] = json!(
            STANDARD.encode(
                encoder
                    .finish()
                    .map_err(|error| Error::Manifest(error.to_string()))?
            )
        );
        status["nodes"] = json!({});
    } else {
        status["nodes"] = json!(nodes);
        status.as_object_mut().unwrap().remove("compressedNodes");
    }
    status
        .as_object_mut()
        .unwrap()
        .remove("offloadNodeStatusVersion");
    Ok(())
}
fn escape(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}
fn identity_patch(source: &DynamicObject) -> Result<Vec<Value>> {
    let uid = source
        .metadata
        .uid
        .as_ref()
        .ok_or_else(|| Error::Manifest("Workflow source has no UID".into()))?;
    let version = source
        .metadata
        .resource_version
        .as_ref()
        .ok_or_else(|| Error::Manifest("Workflow source has no resourceVersion".into()))?;
    Ok(vec![
        json!({"op":"test","path":"/metadata/uid","value":uid}),
        json!({"op":"test","path":"/metadata/resourceVersion","value":version}),
    ])
}

fn control_patch(
    source: &DynamicObject,
    action: Action,
    inputs: &Inputs,
) -> Result<(Value, BTreeSet<String>)> {
    let mut patch = identity_patch(source)?;
    let mut deleted = BTreeSet::new();
    match action {
        Action::Suspend=>patch.push(json!({"op":"add","path":"/spec/suspend","value":true})),
        Action::Stop|Action::Terminate=>patch.push(json!({"op":"add","path":"/spec/shutdown","value":if action==Action::Stop{"Stop"}else{"Terminate"}})),
        Action::Resume=>{
            let mut nodes=argo::nodes(&source.data).map_err(Error::Manifest)?;
            if source.data["spec"].get("suspend").is_some(){patch.push(json!({"op":"remove","path":"/spec/suspend"}));}
            let definitions=parameters("Workflow",source,action)?;
            if inputs.values.keys().any(|key|!definitions.iter().any(|definition|&definition.key==key)){return Err(Error::Manifest("suspended step parameters changed; review again".into()));}
            let now=k8s_openapi::jiff::Timestamp::now().to_string();
            let mut changed=false;
            let mut global_outputs = BTreeMap::new();
            for (id,node) in &mut nodes {
                if !active_suspend(node){continue;}changed=true;
                for param in node.pointer_mut("/outputs/parameters").and_then(Value::as_array_mut).into_iter().flatten(){
                    if param.pointer("/valueFrom/supplied").is_none(){continue;}
                    let name=param["name"].as_str().unwrap_or_default();
                    let value=inputs.values.get(&format!("{id}/{name}")).cloned().or_else(||param.get("value").and_then(text)).or_else(||param.pointer("/valueFrom/default").and_then(text)).ok_or_else(||Error::Manifest(format!("provide the suspended output parameter {name}")))?;
                    if let Some(global)=param.get("globalName").and_then(Value::as_str).filter(|name| !name.is_empty()) { global_outputs.insert(global.to_owned(), value.clone()); }
                    param.as_object_mut().unwrap().remove("valueFrom");param["value"]=json!(value);
                }
                node["phase"]=json!("Succeeded");node["message"]=json!("Resumed from Beacon");node["finishedAt"]=json!(now);
            }
            if changed {let mut status=source.data.get("status").cloned().filter(Value::is_object).unwrap_or_else(||json!({}));
                if !global_outputs.is_empty() {
                    if !status.get("outputs").is_some_and(Value::is_object) { status["outputs"] = json!({}); }
                    let mut global_parameters = status.pointer("/outputs/parameters").and_then(Value::as_array).cloned().unwrap_or_default();
                    for (name,value) in global_outputs { if let Some(parameter) = global_parameters.iter_mut().find(|param| param["name"] == name) { parameter["value"] = json!(value); parameter.as_object_mut().unwrap().remove("valueFrom"); } else { global_parameters.push(json!({"name":name,"value":value})); } }
                    status["outputs"]["parameters"] = json!(global_parameters);
                }
                replace_nodes(&mut status,nodes,source.data.pointer("/status/compressedNodes").and_then(Value::as_str).is_some_and(|value|!value.is_empty()))?;patch.push(json!({"op":"add","path":"/status","value":status}));}
        }
        Action::Retry=>{
            let (status,to_delete)=retry::status(source)?;deleted=to_delete;
            patch.push(json!({"op":"add","path":"/status","value":status}));
            for field in ["shutdown","activeDeadlineSeconds"]{if source.data["spec"].get(field).is_some()&&(field=="shutdown"||source.data["spec"][field]==0){patch.push(json!({"op":"remove","path":format!("/spec/{field}")}));}}
            for label in ["workflows.argoproj.io/completed","workflows.argoproj.io/workflow-archiving-status"] {if source.metadata.labels.as_ref().is_some_and(|labels|labels.contains_key(label)){patch.push(json!({"op":"remove","path":format!("/metadata/labels/{}",escape(label))}));}}
        }
        _=>return Err(Error::Manifest("this action does not patch a Workflow".into())),
    }
    Ok((json!(patch), deleted))
}

async fn patch(
    session: &ClusterSession,
    kind: &Kind,
    source: &DynamicObject,
    proposed: &Value,
    dry_run: bool,
) -> Result<DynamicObject> {
    let namespace = source
        .metadata
        .namespace
        .as_deref()
        .ok_or_else(|| Error::Manifest("Workflow has no namespace".into()))?;
    let name = source
        .metadata
        .name
        .as_deref()
        .ok_or_else(|| Error::Manifest("Workflow has no name".into()))?;
    let api: Api<DynamicObject> =
        Api::namespaced_with(session.write_client().clone(), namespace, &kind.resource);
    let patch = Patch::<Value>::Json(
        serde_json::from_value(proposed.clone())
            .map_err(|error| Error::Manifest(error.to_string()))?,
    );
    Ok(api
        .patch(
            name,
            &PatchParams {
                dry_run,
                ..Default::default()
            },
            &patch,
        )
        .await?)
}

pub async fn load(
    session: Arc<ClusterSession>,
    kind: Kind,
    target: ObjectRef,
    uid: Option<String>,
    action: Action,
) -> Result<Arc<DynamicObject>> {
    if !action.applies(&kind.resource.group, &kind.resource.kind) {
        return Err(Error::Manifest(
            "this action does not apply to the selected resource".into(),
        ));
    }
    let object = session
        .get_object(kind.resource.clone(), target.namespace, target.name)
        .await?;
    if uid
        .as_ref()
        .is_some_and(|uid| object.metadata.uid.as_ref() != Some(uid))
    {
        return Err(Error::Manifest(
            "the Workflow source was replaced; refresh before running".into(),
        ));
    }
    if !action.available(&kind.resource.kind, &object) {
        return Err(Error::Manifest(
            "the Workflow state changed; this action is no longer available".into(),
        ));
    }
    if action == Action::Resume && object.data.pointer("/spec/suspend") != Some(&json!(true)) {
        let nodes = argo::nodes(&object.data).map_err(Error::Manifest)?;
        if !nodes.values().any(active_suspend) {
            return Err(Error::Manifest(
                "the Workflow has no active Suspend step to resume".into(),
            ));
        }
    }
    if action == Action::Suspend
        && object
            .data
            .pointer("/status/compressedNodes")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
        && argo::nodes(&object.data)
            .map_err(Error::Manifest)?
            .values()
            .any(active_suspend)
    {
        return Err(Error::Manifest(
            "the Workflow is already suspended; use Resume".into(),
        ));
    }
    Ok(Arc::new(object))
}

pub async fn prepare(
    session: Arc<ClusterSession>,
    source_kind: Kind,
    source: Arc<DynamicObject>,
    action: Action,
    inputs: Inputs,
) -> Result<Plan> {
    let current = load(
        session.clone(),
        source_kind.clone(),
        ObjectRef::of(&source),
        source.metadata.uid.clone(),
        action,
    )
    .await?;
    if action.creates() && current.data["spec"] != source.data["spec"] {
        return Err(Error::Manifest(
            "the source template changed; reload it before reviewing parameters".into(),
        ));
    }
    let namespace = if source_kind.resource.kind == "ClusterWorkflowTemplate" {
        if inputs.namespace.trim().is_empty() {
            return Err(Error::Manifest(
                "choose the namespace for the Workflow".into(),
            ));
        }
        Some(inputs.namespace.clone())
    } else {
        current.metadata.namespace.clone()
    };
    let rules = session.clone().rules(namespace.clone()).await;
    let allowed = if action.creates() {
        rules.allows("create", argo::GROUP, "workflows")
    } else {
        rules.allows_named(
            "patch",
            argo::GROUP,
            "workflows",
            current.metadata.name.as_deref().unwrap_or(""),
        )
    };
    if !allowed {
        return Err(Error::Manifest(format!(
            "you do not have {} on argoproj.io/workflows in namespace {}",
            action.verb(),
            namespace.as_deref().unwrap_or("default")
        )));
    }
    let workflow_kind = session
        .discovery()
        .kinds()
        .iter()
        .find(|kind| kind.resource.group == argo::GROUP && kind.resource.kind == "Workflow")
        .cloned()
        .ok_or_else(|| Error::Manifest("Workflows are not served in this cluster".into()))?;
    let mut pods = vec![];
    let proposed = if action.creates() {
        let proposed = new_workflow(&source_kind.resource.kind, &current, &inputs, action)?;
        session
            .clone()
            .create(workflow_kind.clone(), proposed.clone(), true)
            .await?;
        proposed
    } else {
        let (proposed, deleted) = control_patch(&current, action, &inputs)?;
        if action == Action::Retry {
            let nodes = argo::nodes(&current.data).map_err(Error::Manifest)?;
            let names: BTreeSet<_> = deleted
                .iter()
                .filter_map(|id| nodes.get(id))
                .filter_map(|node| argo::pod_name(&current, node))
                .collect();
            let uid = current.metadata.uid.as_deref().unwrap_or("");
            for name in names {
                let pod = match session
                    .clone()
                    .get_object(resources::pod(), current.metadata.namespace.clone(), name)
                    .await
                {
                    Ok(pod) => pod,
                    Err(Error::Api(kube::Error::Api(status))) if status.code == 404 => continue,
                    Err(error) => return Err(error),
                };
                let owned = pod
                    .metadata
                    .owner_references
                    .as_ref()
                    .is_some_and(|owners| {
                        owners.iter().any(|owner| {
                            owner.kind == "Workflow"
                                && owner.api_version.starts_with("argoproj.io/")
                                && owner.uid == uid
                        })
                    });
                if !owned {
                    return Err(Error::Manifest(format!(
                        "{} is not owned by this Workflow; it will not be deleted",
                        ObjectRef::of(&pod)
                    )));
                }
                if !rules.allows_named(
                    "delete",
                    "",
                    "pods",
                    pod.metadata.name.as_deref().unwrap_or(""),
                ) {
                    return Err(Error::Manifest(format!(
                        "you do not have delete on pods for {}",
                        ObjectRef::of(&pod)
                    )));
                }
                pods.push(DeleteTarget {
                    reference: ObjectRef::of(&pod),
                    uid: pod
                        .metadata
                        .uid
                        .clone()
                        .ok_or_else(|| Error::Manifest("a retry Pod has no UID".into()))?,
                });
            }
        }
        patch(&session, &workflow_kind, &current, &proposed, true).await?;
        proposed
    };
    Ok(Plan {
        action,
        source_kind,
        source: current,
        workflow_kind,
        inputs,
        proposed,
        pods,
    })
}

pub async fn execute(session: Arc<ClusterSession>, plan: &Plan) -> Result<DynamicObject> {
    let current = load(
        session.clone(),
        plan.source_kind.clone(),
        ObjectRef::of(&plan.source),
        plan.source.metadata.uid.clone(),
        plan.action,
    )
    .await?;
    if plan.action.creates() {
        if current.data["spec"] != plan.source.data["spec"] {
            return Err(Error::Manifest(
                "the source changed after review; review again".into(),
            ));
        }
        return session
            .create(plan.workflow_kind.clone(), plan.proposed.clone(), false)
            .await;
    }
    let rules = session
        .clone()
        .rules(current.metadata.namespace.clone())
        .await;
    if !rules.allows_named(
        "patch",
        argo::GROUP,
        "workflows",
        current.metadata.name.as_deref().unwrap_or(""),
    ) {
        return Err(Error::Manifest(
            "you do not have patch on argoproj.io/workflows".into(),
        ));
    }
    if plan.action == Action::Retry
        && current.metadata.resource_version != plan.source.metadata.resource_version
    {
        return Err(Error::Manifest(
            "Workflow nodes changed after review; review the retry again".into(),
        ));
    }
    if plan.action == Action::Resume {
        let before: BTreeMap<_, _> = argo::nodes(&plan.source.data)
            .map_err(Error::Manifest)?
            .into_iter()
            .filter(|(_, node)| active_suspend(node))
            .map(|(id, node)| (id, node.pointer("/outputs/parameters").cloned()))
            .collect();
        let after: BTreeMap<_, _> = argo::nodes(&current.data)
            .map_err(Error::Manifest)?
            .into_iter()
            .filter(|(_, node)| active_suspend(node))
            .map(|(id, node)| (id, node.pointer("/outputs/parameters").cloned()))
            .collect();
        if before.keys().collect::<Vec<_>>() != after.keys().collect::<Vec<_>>() {
            return Err(Error::Manifest(
                "the suspended steps changed after review; review again".into(),
            ));
        }
        if before != after {
            return Err(Error::Manifest(
                "the suspended step outputs changed after review; review again".into(),
            ));
        }
    }
    // Preflight every target before the first deletion. The server still
    // rechecks each request if permissions change during execution.
    for target in &plan.pods {
        if !rules.allows_named("delete", "", "pods", &target.reference.name) {
            return Err(Error::Manifest(format!(
                "you do not have delete on pods for {}; no Pods were deleted",
                target.reference
            )));
        }
    }
    let mut removed = 0;
    for target in &plan.pods {
        let api: Api<DynamicObject> = Api::namespaced_with(
            session.write_client().clone(),
            target.reference.namespace.as_deref().unwrap_or("default"),
            &resources::pod(),
        );
        match api
            .delete(
                &target.reference.name,
                &DeleteParams {
                    preconditions: Some(Preconditions {
                        uid: Some(target.uid.clone()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(_) => removed += 1,
            Err(kube::Error::Api(status)) if status.code == 404 => {}
            Err(error) => {
                return Err(Error::Manifest(format!(
                    "Retry removed {removed} Pod(s), but could not delete {}: {}. Workflow was not changed.",
                    target.reference,
                    Error::Api(error).user_message()
                )));
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let remaining = tokio::time::timeout_at(deadline, session
                .clone()
                .get_object(
                    resources::pod(),
                    target.reference.namespace.clone(),
                    target.reference.name.clone(),
                )).await.map_err(|_| Error::Manifest(format!("Retry removed {removed} Pod(s), but {} has not terminated within 60 seconds. Workflow was not changed.", target.reference)))?;
            match remaining {
                Err(Error::Api(kube::Error::Api(status))) if status.code == 404 => break,
                Ok(pod) if pod.metadata.uid.as_deref() != Some(&target.uid) => {
                    return Err(Error::Manifest(format!(
                        "Retry removed {removed} Pod(s), but {} was replaced. Workflow was not changed.",
                        target.reference
                    )));
                }
                Err(error) => {
                    return Err(Error::Manifest(format!(
                        "Retry removed {removed} Pod(s), but could not verify {}: {}. Workflow was not changed.",
                        target.reference,
                        error.user_message()
                    )));
                }
                _ if tokio::time::Instant::now() >= deadline => {
                    return Err(Error::Manifest(format!(
                        "Retry removed {removed} Pod(s), but {} has not terminated within 60 seconds. Workflow was not changed.",
                        target.reference
                    )));
                }
                _ => tokio::time::sleep(Duration::from_millis(250)).await,
            }
        }
    }
    let proposed = if plan.action == Action::Retry {
        plan.proposed.clone()
    } else {
        control_patch(&current, plan.action, &plan.inputs)?.0
    };
    patch(&session,&plan.workflow_kind,&current,&proposed,false).await.map_err(|error|if removed>0{Error::Manifest(format!("Retry removed {removed} Pod(s), but Workflow update failed: {}. Refresh before retrying.",error.user_message()))}else{error})
}

#[cfg(test)]
mod tests;
