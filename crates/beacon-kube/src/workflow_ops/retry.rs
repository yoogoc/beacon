//! In-place retry follows Argo's reset plan: preserve successful execution
//! nodes, prune failed branches and revisit the surrounding template boundaries.
//! See workflow/util/util.go in the Argo Workflows repository.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Value, json};

use crate::{DynamicObject, Error, Result, argo};

struct Graph {
    children: BTreeMap<String, Vec<String>>,
    // Argo keeps one parent when a DAG task depends on multiple nodes. Sorted
    // node IDs make the chosen (last) parent stable across repeated reviews.
    parents: BTreeMap<String, String>,
    root: Option<String>,
    exit: BTreeSet<String>,
    order: Vec<String>,
    successful_descendant: BTreeSet<String>,
}

fn failed(node: &Value) -> bool {
    matches!(node["phase"].as_str(), Some("Failed" | "Error"))
}

fn group(node: &Value) -> bool {
    matches!(
        node["type"].as_str(),
        Some("DAG" | "Steps" | "StepGroup" | "TaskGroup")
    )
}

fn execution(node: &Value) -> bool {
    matches!(
        node["type"].as_str(),
        Some("Pod" | "Container" | "HTTP" | "Plugin")
    )
}

fn node_ids(node: &Value, field: &str, id: &str) -> Result<Vec<String>> {
    match node.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(ids)) => ids
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    Error::Manifest(format!(
                        "Workflow node {id} has an invalid {field} reference"
                    ))
                })
            })
            .collect(),
        _ => Err(Error::Manifest(format!(
            "Workflow node {id} has an invalid {field} list"
        ))),
    }
}

impl Graph {
    fn new(nodes: &argo::Nodes, workflow: &str) -> Result<Self> {
        let mut children = BTreeMap::new();
        let mut parents = BTreeMap::new();
        let mut incoming: BTreeMap<String, usize> =
            nodes.keys().map(|id| (id.clone(), 0)).collect();
        let roots: Vec<_> = nodes
            .iter()
            .filter(|(_, node)| node["name"] == workflow)
            .map(|(id, _)| id.clone())
            .collect();
        if !nodes.is_empty() && roots.len() != 1 {
            return Err(Error::Manifest(format!(
                "Workflow {workflow} has nodes but no unique root node; its retry plan cannot be determined"
            )));
        }
        let root = roots.into_iter().next();
        let exit_prefix = format!("{workflow}.onExit");
        let mut exit: BTreeSet<_> = nodes
            .iter()
            .filter(|(_, node)| {
                node["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with(&exit_prefix))
            })
            .map(|(id, _)| id.clone())
            .collect();
        for (id, node) in nodes {
            if node["id"].as_str().is_some_and(|value| value != id) {
                return Err(Error::Manifest(format!(
                    "Workflow node {id} disagrees with its recorded ID; retry cannot safely identify its Pod"
                )));
            }
            let references = node_ids(node, "children", id)?;
            for child in &references {
                let count = incoming.get_mut(child).ok_or_else(|| {
                    Error::Manifest(format!(
                        "Workflow node {id} references missing child {child}; refresh before retrying"
                    ))
                })?;
                *count += 1;
                parents.insert(child.clone(), id.clone());
            }
            children.insert(id.clone(), references);
        }
        // Validate the entire status, including disconnected exit handlers. An
        // iterative topological traversal handles large/deep graphs without a
        // recursive stack or an unbounded walk on malformed status.
        let mut queue: VecDeque<_> = incoming
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(id, _)| id.clone())
            .collect();
        let mut topological = Vec::with_capacity(nodes.len());
        while let Some(id) = queue.pop_front() {
            topological.push(id.clone());
            for child in &children[&id] {
                let count = incoming.get_mut(child).expect("validated child");
                *count -= 1;
                if *count == 0 {
                    queue.push_back(child.clone());
                }
            }
        }
        if topological.len() != nodes.len() {
            return Err(Error::Manifest(
                "Workflow node status contains a cycle; retry cannot safely reset its graph".into(),
            ));
        }
        if root.as_ref().is_some_and(|root| parents.contains_key(root)) {
            return Err(Error::Manifest(
                "The Workflow root has a parent; retry cannot safely reset its graph".into(),
            ));
        }
        let mut successful_descendant = BTreeSet::new();
        for id in topological.iter().rev() {
            if children[id].iter().any(|child| {
                nodes[child]["phase"] == "Succeeded" || successful_descendant.contains(child)
            }) {
                successful_descendant.insert(id.clone());
            }
        }
        let mut queue: VecDeque<_> = exit.iter().cloned().collect();
        while let Some(id) = queue.pop_front() {
            for child in &children[&id] {
                if exit.insert(child.clone()) {
                    queue.push_back(child.clone());
                }
            }
        }
        // Argo's reset plan processes seeds breadth first from the main root;
        // ancestor deletions take precedence over resetting their descendants.
        let mut queue: VecDeque<_> = root.iter().cloned().collect();
        let mut visited = BTreeSet::new();
        let mut order = Vec::with_capacity(nodes.len());
        while let Some(id) = queue.pop_front() {
            if !visited.insert(id.clone()) || exit.contains(&id) {
                continue;
            }
            queue.extend(children[&id].iter().cloned());
            order.push(id);
        }
        if let Some((id, _)) = nodes
            .iter()
            .find(|(id, _)| !visited.contains(*id) && !exit.contains(*id))
        {
            return Err(Error::Manifest(format!(
                "Workflow node {id} has no path from its root; retry cannot determine its parent"
            )));
        }
        Ok(Self {
            children,
            parents,
            root,
            exit,
            order,
            successful_descendant,
        })
    }

    fn subtree(&self, start: &str) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        let mut queue = VecDeque::from([start.to_owned()]);
        while let Some(id) = queue.pop_front() {
            if result.insert(id.clone()) {
                queue.extend(self.children[&id].iter().cloned());
            }
        }
        result
    }

    fn seek(&self, start: &str, predicate: impl Fn(&str) -> bool) -> Result<String> {
        let mut current = Some(start);
        while let Some(id) = current {
            if predicate(id) {
                return Ok(id.to_owned());
            }
            current = self.parents.get(id).map(String::as_str);
        }
        Err(Error::Manifest(format!(
            "Workflow node {start} has an invalid template boundary; refresh before retrying"
        )))
    }

    fn reset_boundaries(
        &self,
        nodes: &argo::Nodes,
        start: &str,
        reset: &mut BTreeSet<String>,
    ) -> Result<Option<String>> {
        let mut current = start.to_owned();
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(current.clone()) {
                return Err(Error::Manifest(format!(
                    "Workflow node {current} has a cyclic template boundary; retry cannot safely continue"
                )));
            }
            if let Some(parent) = self.parents.get(&current)
                && nodes[parent]["type"] == "Retry"
            {
                reset.insert(parent.clone());
                current = parent.clone();
            }
            if let Some(parent) = self.parents.get(&current)
                && matches!(
                    nodes[parent]["type"].as_str(),
                    Some("StepGroup" | "TaskGroup")
                )
            {
                reset.insert(parent.clone());
            }
            let boundary = nodes[&current]["boundaryID"].as_str().unwrap_or("");
            if boundary.is_empty() {
                return Ok(self.parents.get(&current).cloned());
            }
            let found = self.seek(&current, |id| id == boundary)?;
            reset.insert(found.clone());
            current = found;
        }
    }

    fn reset_path(
        &self,
        nodes: &argo::Nodes,
        start: &str,
        reset: &mut BTreeSet<String>,
        delete: &mut BTreeSet<String>,
    ) -> Result<()> {
        delete.extend(self.subtree(start));
        if self.root.as_deref() == Some(start) {
            return Ok(());
        }
        let mut current = Some(start.to_owned());
        while let Some(id) = current {
            let node = &nodes[&id];
            if group(node) {
                reset.insert(id.clone());
                current = self.reset_boundaries(nodes, &id, reset)?;
                continue;
            }
            if node["type"] == "Container" {
                let pod = self.seek(&id, |ancestor| nodes[ancestor]["type"] == "Pod")?;
                reset.insert(pod.clone());
                delete.extend(self.subtree(&pod));
                current = Some(pod);
                continue;
            }
            if node["type"] == "Retry" && failed(node) {
                reset.insert(id.clone());
            }
            current = self.parents.get(&id).cloned();
        }
        Ok(())
    }
}

fn reset_node(node: &mut Value, now: &str) {
    if node["type"] == "Suspend"
        && let Some(parameters) = node
            .pointer_mut("/outputs/parameters")
            .and_then(Value::as_array_mut)
    {
        for parameter in parameters {
            *parameter = json!({"name":parameter["name"],"valueFrom":{"supplied":{}}});
        }
    }
    if node["phase"] != "Skipped" {
        node["phase"] = json!("Running");
    }
    node["message"] = json!("");
    node["startedAt"] = json!(now);
    node.as_object_mut()
        .expect("validated node")
        .remove("finishedAt");
}

pub(super) fn status(source: &DynamicObject) -> Result<(Value, BTreeSet<String>)> {
    let name = source
        .metadata
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            Error::Manifest("Workflow has no name; retry cannot determine its root".into())
        })?;
    if !matches!(
        source.data.pointer("/status/phase").and_then(Value::as_str),
        Some("Failed" | "Error")
    ) {
        return Err(Error::Manifest(
            "Only a failed or errored Workflow can retry its failed steps".into(),
        ));
    }
    let mut nodes = argo::nodes(&source.data).map_err(Error::Manifest)?;
    let graph = Graph::new(&nodes, name)?;
    let mut seeds = BTreeSet::new();
    for (id, node) in &nodes {
        if !failed(node) || !(execution(node) || graph.children[id].is_empty()) {
            continue;
        }
        let context = if node.pointer("/nodeFlag/retried") == Some(&json!(true)) {
            // The successful attempt belongs to a sibling under the Retry
            // wrapper, not necessarily to the failed attempt's own subtree.
            nodes
                .iter()
                .find(|(parent, node)| {
                    node["type"] == "Retry" && graph.children[*parent].contains(id)
                })
                .map(|(parent, _)| parent)
                .unwrap_or(id)
        } else {
            id
        };
        if !graph.successful_descendant.contains(context) {
            seeds.insert(id.clone());
        }
    }
    let mut reset = BTreeSet::new();
    let mut delete = graph.exit.clone();
    for id in &graph.order {
        if seeds.contains(id) && !delete.contains(id) {
            graph.reset_path(&nodes, id, &mut reset, &mut delete)?;
        }
    }
    let now = k8s_openapi::jiff::Timestamp::now().to_string();
    nodes.retain(|id, _| !delete.contains(id));
    let existing: BTreeSet<_> = nodes.keys().cloned().collect();
    for (id, node) in &mut nodes {
        if reset.contains(id) {
            reset_node(node, &now);
        }
        for field in ["children", "outboundNodes"] {
            if let Some(references) = node.get_mut(field).and_then(Value::as_array_mut) {
                references
                    .retain(|reference| reference.as_str().is_some_and(|id| existing.contains(id)));
            }
        }
    }
    let mut status = source
        .data
        .get("status")
        .cloned()
        .filter(Value::is_object)
        .ok_or_else(|| Error::Manifest("Workflow has no status to retry".into()))?;
    status["phase"] = json!("");
    status["message"] = json!("");
    status["startedAt"] = json!(now);
    status
        .as_object_mut()
        .expect("validated status")
        .remove("finishedAt");
    status["persistentVolumeClaims"] = json!([]);
    if let Some(spec) = status
        .get_mut("storedWorkflowSpec")
        .and_then(Value::as_object_mut)
    {
        spec.remove("shutdown");
    }
    let mut conditions = status
        .get("conditions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    conditions.retain(|condition| condition["type"] != "Completed");
    conditions.push(json!({"type":"Completed","status":"False"}));
    status["conditions"] = json!(conditions);
    super::replace_nodes(
        &mut status,
        nodes,
        source
            .data
            .pointer("/status/compressedNodes")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
    )?;
    Ok((status, delete))
}

#[cfg(test)]
mod tests {
    use super::status;
    use crate::{DynamicObject, argo};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use flate2::{Compression, write::GzEncoder};
    use serde_json::{Value, json};
    use std::{collections::BTreeSet, io::Write};

    fn workflow(nodes: Value) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion":"argoproj.io/v1alpha1","kind":"Workflow",
            "metadata":{"name":"run","namespace":"default","uid":"workflow-uid","resourceVersion":"7"},
            "spec":{"entrypoint":"main","shutdown":"Terminate"},
            "status":{"phase":"Failed","message":"failed","startedAt":"2025-01-01T00:00:00Z","finishedAt":"2025-01-02T00:00:00Z",
                "conditions":[{"type":"Completed","status":"True","message":"previous"},{"type":"PodRunning","status":"False"}],
                "persistentVolumeClaims":[{"name":"scratch"}],
                "storedWorkflowSpec":{"shutdown":"Terminate","entrypoint":"main"},"nodes":nodes}
        })).unwrap()
    }

    fn names(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).into()).collect()
    }

    #[test]
    fn failed_branch_is_removed_while_successful_outputs_and_sibling_pods_survive() {
        let source = workflow(json!({
            "run":{"id":"run","name":"run","type":"DAG","phase":"Failed","children":["good","bad"],"outboundNodes":["good","bad","missing"],"message":"task failed","finishedAt":"2025-01-02T00:00:00Z"},
            "good":{"id":"good","name":"run.good","type":"Pod","phase":"Succeeded","outputs":{"parameters":[{"name":"result","value":"keep"}]},"finishedAt":"2025-01-02T00:00:00Z"},
            "bad":{"id":"bad","name":"run.bad","type":"Pod","phase":"Failed","children":["later"]},
            "later":{"id":"later","name":"run.later","type":"Skipped","phase":"Omitted"}
        }));
        let before = source.clone();
        let (result, deleted) = status(&source).unwrap();
        assert_eq!(deleted, names(&["bad", "later"]));
        assert_eq!(
            result["nodes"]["good"],
            source.data["status"]["nodes"]["good"]
        );
        assert_eq!(result["nodes"]["run"]["phase"], "Running");
        assert_eq!(result["nodes"]["run"]["children"], json!(["good"]));
        assert_eq!(result["nodes"]["run"]["outboundNodes"], json!(["good"]));
        assert!(result["nodes"]["run"].get("finishedAt").is_none());
        assert_eq!(result["phase"], "");
        assert_eq!(
            result["conditions"],
            json!([{"type":"PodRunning","status":"False"},{"type":"Completed","status":"False"}])
        );
        assert_eq!(result["persistentVolumeClaims"], json!([]));
        assert!(result["storedWorkflowSpec"].get("shutdown").is_none());
        assert_eq!(result["storedWorkflowSpec"]["entrypoint"], "main");
        assert!(result.get("finishedAt").is_none());
        assert_eq!(source.data, before.data);
    }

    #[test]
    fn nested_boundaries_fan_out_groups_and_failed_retry_wrapper_are_reset() {
        let source = workflow(json!({
            "run":{"id":"run","name":"run","type":"Steps","phase":"Failed","children":["steps"]},
            "steps":{"id":"steps","name":"run[0]","type":"StepGroup","phase":"Failed","boundaryID":"run","children":["retry"]},
            "retry":{"id":"retry","name":"run[0].nested","type":"Retry","phase":"Failed","boundaryID":"run","children":["nested"]},
            "nested":{"id":"nested","name":"run[0].nested(0)","type":"DAG","phase":"Failed","boundaryID":"run","children":["fan"]},
            "fan":{"id":"fan","name":"run[0].nested(0).task","type":"TaskGroup","phase":"Failed","boundaryID":"nested","children":["ok","bad"]},
            "ok":{"id":"ok","name":"run[0].nested(0).task(0)","type":"Pod","phase":"Succeeded"},
            "bad":{"id":"bad","name":"run[0].nested(0).task(1)","type":"Pod","phase":"Error"}
        }));
        let (result, deleted) = status(&source).unwrap();
        assert_eq!(deleted, names(&["bad"]));
        for id in ["run", "steps", "retry", "nested", "fan"] {
            assert_eq!(result["nodes"][id]["phase"], "Running", "{id}");
        }
        assert_eq!(result["nodes"]["ok"]["phase"], "Succeeded");
        assert_eq!(result["nodes"]["fan"]["children"], json!(["ok"]));
    }

    #[test]
    fn successful_retry_attempt_and_continue_on_success_protect_old_failures() {
        let source = workflow(json!({
            "run":{"id":"run","name":"run","type":"DAG","phase":"Failed","children":["retry","continued","bad"]},
            "retry":{"id":"retry","name":"run.retry","type":"Retry","phase":"Succeeded","children":["old","new"]},
            "old":{"id":"old","name":"run.retry(0)","type":"Pod","phase":"Failed","nodeFlag":{"retried":true}},
            "new":{"id":"new","name":"run.retry(1)","type":"Pod","phase":"Succeeded","nodeFlag":{"retried":true}},
            "continued":{"id":"continued","name":"run.continued","type":"Pod","phase":"Failed","children":["continued-ok"]},
            "continued-ok":{"id":"continued-ok","name":"run.continued-ok","type":"Pod","phase":"Succeeded"},
            "bad":{"id":"bad","name":"run.bad","type":"HTTP","phase":"Error"}
        }));
        let (result, deleted) = status(&source).unwrap();
        assert_eq!(deleted, names(&["bad"]));
        for id in ["retry", "old", "new", "continued", "continued-ok"] {
            assert_eq!(result["nodes"][id], source.data["status"]["nodes"][id]);
        }
    }

    #[test]
    fn container_failure_recreates_the_whole_pod_and_completed_exit_handlers() {
        let source = workflow(json!({
            "run":{"id":"run","name":"run","type":"DAG","phase":"Failed","children":["pod"]},
            "pod":{"id":"pod","name":"run.main","type":"Pod","phase":"Running","children":["first","second"]},
            "first":{"id":"first","name":"run.main.first","type":"Container","phase":"Succeeded","boundaryID":"pod"},
            "second":{"id":"second","name":"run.main.second","type":"Container","phase":"Failed","boundaryID":"pod"},
            "exit":{"id":"exit","name":"run.onExit","type":"Steps","phase":"Succeeded","children":["exit-pod"]},
            "exit-pod":{"id":"exit-pod","name":"run.onExit[0].notify","type":"Pod","phase":"Succeeded"}
        }));
        let (result, deleted) = status(&source).unwrap();
        assert_eq!(
            deleted,
            names(&["pod", "first", "second", "exit", "exit-pod"])
        );
        assert_eq!(result["nodes"].as_object().unwrap().len(), 1);
        assert_eq!(result["nodes"]["run"]["phase"], "Running");
        assert_eq!(result["nodes"]["run"]["children"], json!([]));
    }

    #[test]
    fn errored_non_execution_leaves_are_recreated_and_initial_validation_failure_has_no_pods() {
        for kind in ["Skipped", "Suspend", "Plugin"] {
            let source = workflow(json!({
                "run":{"id":"run","name":"run","type":"DAG","phase":"Error","children":["bad"]},
                "bad":{"id":"bad","name":"run.bad","type":kind,"phase":"Error"}
            }));
            assert_eq!(status(&source).unwrap().1, names(&["bad"]));
        }
        let (result, deleted) = status(&workflow(json!({}))).unwrap();
        assert!(deleted.is_empty());
        assert_eq!(result["nodes"], json!({}));
        assert_eq!(result["phase"], "");
    }

    #[test]
    fn compressed_nodes_are_written_back_compressed_and_offloaded_nodes_refuse_retry() {
        let expected = json!({
            "run":{"id":"run","name":"run","type":"DAG","phase":"Failed","children":["bad"]},
            "bad":{"id":"bad","name":"run.bad","type":"Pod","phase":"Failed"}
        });
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(expected.to_string().as_bytes()).unwrap();
        let mut source = workflow(json!({}));
        source.data["status"]["compressedNodes"] =
            json!(STANDARD.encode(encoder.finish().unwrap()));
        let (result, deleted) = status(&source).unwrap();
        assert_eq!(deleted, names(&["bad"]));
        assert_eq!(result["nodes"], json!({}));
        assert!(
            result["compressedNodes"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        let nodes = argo::nodes(&json!({"status":result})).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes["run"]["phase"], "Running");
        source.data["status"]
            .as_object_mut()
            .unwrap()
            .remove("compressedNodes");
        source.data["status"]["offloadNodeStatusVersion"] = json!("database-v1");
        assert!(
            status(&source)
                .unwrap_err()
                .to_string()
                .contains("offloaded")
        );
    }

    #[test]
    fn malformed_graphs_and_template_boundaries_fail_before_any_mutation() {
        let cases = [
            (
                json!({"other":{"id":"other","name":"other","type":"Pod","phase":"Failed"}}),
                "root",
            ),
            (
                json!({"run":{"id":"run","name":"run","type":"DAG","phase":"Failed","children":["missing"]}}),
                "missing child",
            ),
            (
                json!({"run":{"id":"run","name":"run","type":"DAG","phase":"Failed","children":["loop"]},"loop":{"id":"loop","name":"run.loop","type":"DAG","phase":"Failed","children":["loop"]}}),
                "cycle",
            ),
            (
                json!({"run":{"id":"run","name":"run","type":"DAG","phase":"Failed"},"orphan":{"id":"orphan","name":"run.orphan","type":"Pod","phase":"Failed"}}),
                "root",
            ),
            (
                json!({"run":{"id":"run","name":"run","type":"DAG","phase":"Failed","children":["nested"]},"nested":{"id":"nested","name":"run.nested","type":"DAG","phase":"Failed","boundaryID":"missing","children":["bad"]},"bad":{"id":"bad","name":"run.bad","type":"Pod","phase":"Failed"}}),
                "boundary",
            ),
            (
                json!({"run":{"id":"run","name":"run","type":"DAG","phase":"Failed","children":["bad"]},"bad":{"id":"bad","name":"run.bad","type":"Container","phase":"Failed"}}),
                "boundary",
            ),
        ];
        for (nodes, message) in cases {
            let source = workflow(nodes);
            let before = source.data.clone();
            assert!(
                status(&source).unwrap_err().to_string().contains(message),
                "{message}"
            );
            assert_eq!(source.data, before);
        }
    }

    #[test]
    fn skipped_group_phase_is_preserved_when_a_descendant_is_reset() {
        let source = workflow(json!({
            "run":{"id":"run","name":"run","type":"Steps","phase":"Failed","children":["skipped"]},
            "skipped":{"id":"skipped","name":"run[0]","type":"StepGroup","phase":"Skipped","boundaryID":"run","children":["bad"]},
            "bad":{"id":"bad","name":"run.bad","type":"Pod","phase":"Failed"}
        }));
        let (result, _) = status(&source).unwrap();
        assert_eq!(result["nodes"]["skipped"]["phase"], "Skipped");
        assert_eq!(result["nodes"]["run"]["phase"], "Running");
    }
}
