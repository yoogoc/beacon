//! Argo's Kubernetes representations. No Argo server connection is required.
use std::{collections::BTreeMap, io::Read};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use flate2::read::GzDecoder;
use serde_json::Value;

use crate::DynamicObject;

pub const GROUP: &str = "argoproj.io";
pub const KINDS: [&str; 8] = [
    "Workflow",
    "CronWorkflow",
    "WorkflowTemplate",
    "ClusterWorkflowTemplate",
    "WorkflowEventBinding",
    "WorkflowTaskSet",
    "WorkflowTaskResult",
    "WorkflowArtifactGCTask",
];

pub fn rank(group: &str, kind: &str) -> Option<u8> {
    (group == GROUP)
        .then(|| KINDS.iter().position(|candidate| *candidate == kind))
        .flatten()
        .map(|index| index as u8)
}

pub fn is_controller(group: &str, kind: &str) -> bool {
    rank(group, kind).is_some_and(|rank| rank >= 5)
}

pub type Nodes = BTreeMap<String, Value>;
const MAX_NODE_BYTES: u64 = 32 * 1024 * 1024;

/// Controller compression is base64(gzip(JSON)). Decode away from the UI thread.
/// Offloaded nodes live in Argo's database, not in the Kubernetes resource.
pub fn nodes(data: &Value) -> Result<Nodes, String> {
    if let Some(nodes) = data.pointer("/status/nodes").and_then(Value::as_object)
        && !nodes.is_empty()
    {
        return validate_nodes(
            nodes
                .iter()
                .map(|(id, node)| (id.clone(), node.clone()))
                .collect(),
        );
    }
    if let Some(encoded) = data
        .pointer("/status/compressedNodes")
        .and_then(Value::as_str)
        && !encoded.is_empty()
    {
        if encoded.len() as u64 > MAX_NODE_BYTES * 4 / 3 + 4 {
            return Err("Compressed node status exceeds the 32 MiB limit.".into());
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|error| format!("Cannot decode node status: {error}"))?;
        let mut decoded = Vec::new();
        GzDecoder::new(bytes.as_slice())
            .take(MAX_NODE_BYTES + 1)
            .read_to_end(&mut decoded)
            .map_err(|error| format!("Cannot decompress node status: {error}"))?;
        if decoded.len() as u64 > MAX_NODE_BYTES {
            return Err("Expanded node status exceeds the 32 MiB limit.".into());
        }
        return validate_nodes(
            serde_json::from_slice(&decoded)
                .map_err(|error| format!("Cannot read node status: {error}"))?,
        );
    }
    if data
        .pointer("/status/offloadNodeStatusVersion")
        .and_then(Value::as_str)
        .is_some_and(|v| !v.is_empty())
    {
        return Err("Node status is offloaded to Argo's database and is unavailable through the Kubernetes API. View this workflow in the Argo UI.".into());
    }
    Ok(Nodes::new())
}

fn validate_nodes(nodes: Nodes) -> Result<Nodes, String> {
    if let Some((id, _)) = nodes.iter().find(|(_, node)| !node.is_object()) {
        return Err(format!("Node status entry {id} is not an object."));
    }
    Ok(nodes)
}

/// Equivalent to Argo UI's getPodName, including v1, containerSet boundaries,
/// collisions and widened node hashes. Kubernetes names are ASCII.
pub fn pod_name(workflow: &DynamicObject, node: &Value) -> Option<String> {
    if !matches!(node.get("type")?.as_str()?, "Pod" | "Container") {
        return None;
    }
    let id = node.get("id")?.as_str()?;
    if workflow
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("workflows.argoproj.io/pod-name-format"))
        .is_some_and(|v| v == "v1")
    {
        return Some(id.into());
    }
    let workflow_name = workflow.metadata.name.as_deref()?;
    let pod_id = if node["type"] == "Container" {
        node["boundaryID"].as_str().unwrap_or(id)
    } else {
        id
    };
    if pod_id == workflow_name {
        return Some(workflow_name.into());
    }
    let template = node["templateName"]
        .as_str()
        .or_else(|| {
            node.pointer("/templateRef/template")
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    let mut prefix = if template.is_empty() {
        workflow_name.into()
    } else {
        format!("{workflow_name}-{template}")
    };
    let hash = pod_id
        .strip_prefix(&format!("{workflow_name}-"))
        .map(str::to_string)
        .unwrap_or_else(|| {
            let hash = node["name"]
                .as_str()
                .unwrap_or(id)
                .bytes()
                .fold(2_166_136_261_u32, |hash, byte| {
                    (hash ^ u32::from(byte)).wrapping_mul(16_777_619)
                });
            hash.to_string()
        });
    let mut end = prefix
        .len()
        .min(242.min(253_usize.saturating_sub(hash.len() + 1)));
    while !prefix.is_char_boundary(end) {
        end -= 1;
    }
    prefix.truncate(end);
    Some(format!("{prefix}-{hash}"))
}

pub fn execution_spec(data: &Value) -> &Value {
    data.pointer("/status/storedWorkflowSpec")
        .filter(|spec| spec.is_object())
        .or_else(|| {
            data.pointer("/status/storedWorkflowTemplateSpec")
                .filter(|spec| spec.is_object())
        })
        .unwrap_or(&data["spec"])
}

fn find_template<'a>(spec: &'a Value, name: &str) -> Option<&'a Value> {
    spec["templates"]
        .as_array()?
        .iter()
        .find(|template| template["name"] == name)
}

/// Resolve the templates persisted by the controller using the same scope keys
/// as the official UI. Never match merely by suffix: clusters and namespaces
/// may define different templates with the same name.
pub fn resolved_template(data: &Value, node: &Value) -> Option<Value> {
    let spec = execution_spec(data);
    let mut holder = node.clone();
    let scope = node["templateScope"].as_str().unwrap_or("");
    let mut templates = Vec::new();
    for _ in 0..10 {
        let reference = &holder["templateRef"];
        let name = holder["templateName"]
            .as_str()
            .or_else(|| holder["template"].as_str())
            .unwrap_or("");
        let stored = if reference.is_object() {
            let resource = reference["name"].as_str()?;
            let template = reference["template"].as_str()?;
            let reference_scope = if reference["clusterScope"] == true {
                "cluster"
            } else {
                "namespaced"
            };
            if scope.is_empty() || !scope.contains('/') {
                format!("{resource}/{template}")
            } else {
                format!("{reference_scope}/{resource}/{template}")
            }
        } else if !scope.is_empty() && scope.split('/').next() != Some("local") {
            format!("{scope}/{name}")
        } else {
            String::new()
        };
        let template = if stored.is_empty() {
            find_template(spec, name)?
        } else {
            data.pointer("/status/storedTemplates")?.get(&stored)?
        };
        templates.push(template.clone());
        if template.get("template").is_none() && template.get("templateRef").is_none() {
            let mut resolved = serde_json::Map::new();
            for template in templates.into_iter().rev() {
                for (key, value) in template.as_object()? {
                    if !matches!(key.as_str(), "template" | "templateRef") {
                        resolved.insert(key.clone(), value.clone());
                    }
                }
            }
            return Some(Value::Object(resolved));
        }
        holder = template.clone();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{nodes, pod_name, rank, resolved_template};
    use crate::DynamicObject;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use flate2::{Compression, write::GzEncoder};
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn argo_cd_and_borrowed_kind_names_are_not_workflows() {
        assert_eq!(rank("argoproj.io", "Application"), None);
        assert_eq!(rank("example.com", "Workflow"), None);
        assert_eq!(rank("argoproj.io", "WorkflowArtifactGCTask"), Some(7));
    }

    #[test]
    fn compressed_nodes_and_offloaded_status_are_distinct() {
        let expected = json!({"run-1":{"id":"run-1","phase":"Succeeded"}});
        let mut gzip = GzEncoder::new(Vec::new(), Compression::default());
        gzip.write_all(expected.to_string().as_bytes()).unwrap();
        let data = json!({"status":{"compressedNodes":STANDARD.encode(gzip.finish().unwrap())}});
        assert_eq!(nodes(&data).unwrap()["run-1"]["phase"], "Succeeded");
        assert!(nodes(&json!({"status":{"compressedNodes":"invalid"}})).is_err());
        assert!(
            nodes(&json!({"status":{"offloadNodeStatusVersion":"v1"}}))
                .unwrap_err()
                .contains("offloaded")
        );
    }

    #[test]
    fn pod_names_keep_controller_hashes_and_container_boundaries() {
        let mut workflow: DynamicObject = serde_json::from_value(json!({"apiVersion":"argoproj.io/v1alpha1","kind":"Workflow","metadata":{"name":"etl"}})).unwrap();
        let node = json!({"id":"etl-123-collision","name":"etl.main.task","type":"Pod","templateName":"normalize"});
        assert_eq!(
            pod_name(&workflow, &node).as_deref(),
            Some("etl-normalize-123-collision")
        );
        assert_eq!(pod_name(&workflow, &json!({"id":"etl-222","boundaryID":"etl-123","type":"Container","templateName":"many"})).as_deref(), Some("etl-many-123"));
        workflow.metadata.annotations =
            Some([("workflows.argoproj.io/pod-name-format".into(), "v1".into())].into());
        assert_eq!(
            pod_name(&workflow, &node).as_deref(),
            Some("etl-123-collision")
        );
    }

    #[test]
    fn stored_templates_resolve_exact_scope_and_local_aliases() {
        let data = json!({"spec":{"templates":[{"name":"alias","template":"main","retryStrategy":{"limit":2}},{"name":"main","container":{"image":"local"}}]},"status":{"storedTemplates":{"cluster/shared/task":{"container":{"image":"cluster"}},"namespaced/shared/task":{"container":{"image":"namespaced"}}}}});
        assert_eq!(
            resolved_template(&data, &json!({"templateName":"alias"})).unwrap()["container"]["image"],
            "local"
        );
        assert_eq!(resolved_template(&data, &json!({"templateScope":"local/","templateRef":{"name":"shared","template":"task","clusterScope":true}})).unwrap()["container"]["image"], "cluster");
        assert!(
            resolved_template(
                &data,
                &json!({"templateScope":"cluster/other","templateName":"task"})
            )
            .is_none()
        );
    }
    #[test]
    fn malformed_nodes_report_errors_before_graph_projection() {
        assert!(
            nodes(&json!({"status":{"nodes":{"bad":42}}}))
                .unwrap_err()
                .contains("bad")
        );
        assert!(
            nodes(&json!({"status":{"compressedNodes":STANDARD.encode(b"not gzip")}})).is_err()
        );
    }

    #[test]
    fn stored_execution_spec_and_local_scopes_take_precedence() {
        let data = json!({"spec":{"templates":[{"name":"main","container":{"image":"submitted"}}]},"status":{"storedWorkflowSpec":null,"storedWorkflowTemplateSpec":{"templates":[{"name":"main","container":{"image":"executed"}}]}}});
        assert_eq!(
            resolved_template(
                &data,
                &json!({"templateName":"main","templateScope":"local/"})
            )
            .unwrap()["container"]["image"],
            "executed"
        );
    }

    #[test]
    fn widened_hashes_remain_intact_when_pod_prefixes_are_truncated() {
        let workflow:DynamicObject=serde_json::from_value(json!({"apiVersion":"argoproj.io/v1alpha1","kind":"Workflow","metadata":{"name":"a".repeat(220)}})).unwrap();
        let id = format!(
            "{}-18446744073709551615-collision",
            workflow.metadata.name.as_deref().unwrap()
        );
        let name = pod_name(
            &workflow,
            &json!({"id":id,"type":"Pod","templateName":"long-template-name-long-template-name"}),
        )
        .unwrap();
        assert_eq!(name.len(), 253);
        assert!(name.ends_with("-18446744073709551615-collision"));
    }
}
