use super::*;

/// Ignore server bookkeeping and status while retaining routing-relevant
/// metadata, such as Ingress annotations and workload labels.
pub(super) fn target_snapshot(object: &DynamicObject) -> Value {
    let mut data = object.data.clone();
    if let Some(map) = data.as_object_mut() {
        map.remove("status");
    }
    serde_json::json!({
        "data": data,
        "labels": object.metadata.labels,
        "annotations": object.metadata.annotations,
    })
}

/// Admission may add defaults, but uncertain create responses cannot silently
/// adopt an object whose explicitly reviewed configuration has changed.
pub(super) fn contains_intent(manifest: &Value, object: &DynamicObject) -> bool {
    fn contains(expected: &Value, actual: &Value) -> bool {
        match (expected, actual) {
            (Value::Object(expected), Value::Object(actual)) => {
                expected.iter().all(|(key, value)| {
                    actual
                        .get(key)
                        .is_some_and(|actual| contains(value, actual))
                })
            }
            (Value::Array(expected), Value::Array(actual)) => {
                expected.len() == actual.len()
                    && expected.iter().zip(actual).all(|(a, b)| contains(a, b))
            }
            _ => expected == actual,
        }
    }
    let metadata = serde_json::to_value(&object.metadata).expect("serializable metadata");
    let metadata_matches = ["labels", "annotations"].iter().all(|key| {
        manifest
            .pointer(&format!("/metadata/{key}"))
            .is_none_or(|expected| {
                metadata
                    .get(key)
                    .is_some_and(|actual| contains(expected, actual))
            })
    });
    metadata_matches
        && manifest.as_object().is_some_and(|fields| {
            fields
                .iter()
                .filter(|(key, _)| {
                    !matches!(key.as_str(), "metadata" | "status" | "apiVersion" | "kind")
                })
                .all(|(key, value)| {
                    object
                        .data
                        .get(key)
                        .is_some_and(|actual| contains(value, actual))
                })
        })
}

/// Unportable identities and storage bindings cannot be safely cloned by
/// stripping metadata. Controller-owned objects must be handled at their owner.
pub fn unsupported(kind: &Kind, object: Option<&DynamicObject>) -> Option<&'static str> {
    if !kind.supports("get") {
        return Some("This resource does not support reading full objects.");
    }
    if kind.resource.group.is_empty()
        && matches!(
            kind.resource.kind.as_str(),
            "Node"
                | "Namespace"
                | "PersistentVolume"
                | "PersistentVolumeClaim"
                | "Endpoints"
                | "Event"
                | "Binding"
                | "ComponentStatus"
        )
        || matches!(
            kind.resource.group.as_str(),
            "events.k8s.io" | "metrics.k8s.io" | "coordination.k8s.io"
        )
        || kind.resource.kind == "EndpointSlice"
    {
        return Some(
            "Cluster identities, generated resources and persistent storage require their own migration workflow.",
        );
    }
    if matches!(
        kind.resource.kind.as_str(),
        "CustomResourceDefinition" | "APIService"
    ) {
        return Some(
            "API definitions require a dedicated migration workflow; transfer resource instances instead.",
        );
    }
    if kind.resource.group == "batch" && kind.resource.kind == "Job" {
        return Some(
            "Use Create a new Job for Job executions. Transfer a CronJob template instead.",
        );
    }
    let object = object?;
    if object
        .metadata
        .owner_references
        .as_ref()
        .is_some_and(|owners| !owners.is_empty())
    {
        return Some(
            "This resource has an owner. Transfer its owning workload instead of a controller-managed child.",
        );
    }
    if kind.resource.group.is_empty()
        && kind.resource.kind == "Secret"
        && matches!(
            object.data["type"].as_str(),
            Some("kubernetes.io/service-account-token" | "bootstrap.kubernetes.io/token")
        )
    {
        return Some(
            "Cluster-generated authentication tokens cannot be transferred. Create credentials at the destination.",
        );
    }
    if kind.resource.group.is_empty()
        && kind.resource.kind == "Service"
        && object
            .data
            .pointer("/spec/selector")
            .and_then(Value::as_object)
            .is_none_or(serde_json::Map::is_empty)
        && object.data.pointer("/spec/type").and_then(Value::as_str) != Some("ExternalName")
    {
        return Some(
            "A Service without a selector relies on external endpoints. Transfer its endpoint configuration separately.",
        );
    }
    if kind.resource.group == "apps"
        && kind.resource.kind == "StatefulSet"
        && object
            .data
            .pointer("/spec/volumeClaimTemplates")
            .and_then(Value::as_array)
            .is_some_and(|claims| !claims.is_empty())
    {
        return Some(
            "This StatefulSet provisions persistent volumes. Migrate its data and storage bindings separately.",
        );
    }
    None
}

pub(super) fn prepare(
    kind: &Kind,
    object: &DynamicObject,
    name: &str,
    namespace: Option<&str>,
) -> Result<Value> {
    if name.is_empty()
        || name.len() > 253
        || name.bytes().any(|ch| {
            !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && !matches!(ch, b'-' | b'.')
        })
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name.as_bytes()[name.len() - 1].is_ascii_alphanumeric()
    {
        return Err(invalid("Choose a valid lowercase target resource name."));
    }
    let mut value = serde_json::to_value(object).map_err(|error| invalid(error.to_string()))?;
    value["apiVersion"] = Value::String(kind.resource.api_version.clone());
    value["kind"] = Value::String(kind.resource.kind.clone());
    value
        .as_object_mut()
        .expect("dynamic object")
        .remove("status");
    let metadata = value["metadata"]
        .as_object_mut()
        .ok_or_else(|| invalid("The source metadata is missing."))?;
    for field in [
        "uid",
        "resourceVersion",
        "managedFields",
        "creationTimestamp",
        "deletionTimestamp",
        "deletionGracePeriodSeconds",
        "generation",
        "selfLink",
        "generateName",
        "ownerReferences",
        "finalizers",
    ] {
        metadata.remove(field);
    }
    metadata.insert("name".into(), Value::String(name.into()));
    metadata.remove("namespace");
    if let Some(namespace) = namespace {
        metadata.insert("namespace".into(), Value::String(namespace.into()));
    }
    if let Some(annotations) = metadata
        .get_mut("annotations")
        .and_then(Value::as_object_mut)
    {
        annotations.retain(|key, _| {
            key != "kubectl.kubernetes.io/last-applied-configuration"
                && !key.starts_with("meta.helm.sh/")
                && !key.starts_with("argocd.argoproj.io/")
                && key != "deployment.kubernetes.io/revision"
        });
    }
    if let Some(labels) = metadata.get_mut("labels").and_then(Value::as_object_mut) {
        labels.retain(|key, _| {
            !key.starts_with("argocd.argoproj.io/") && key != "app.kubernetes.io/managed-by"
        });
    }
    // Nested controller templates also contain ObjectMeta, and can carry
    // stale timestamps, UIDs, managed fields or ownership after YAML imports.
    for pointer in [
        "/spec/template/metadata",
        "/spec/jobTemplate/metadata",
        "/spec/jobTemplate/spec/template/metadata",
    ] {
        if let Some(metadata) = value.pointer_mut(pointer).and_then(Value::as_object_mut) {
            for field in [
                "uid",
                "resourceVersion",
                "managedFields",
                "creationTimestamp",
                "generation",
                "ownerReferences",
                "finalizers",
                "namespace",
            ] {
                metadata.remove(field);
            }
        }
    }
    if kind.resource.group.is_empty() && kind.resource.kind == "Service" {
        let spec = value["spec"]
            .as_object_mut()
            .ok_or_else(|| invalid("Service spec is missing."))?;
        if spec.get("clusterIP").and_then(Value::as_str) != Some("None") {
            for field in ["clusterIP", "clusterIPs", "ipFamilies", "ipFamilyPolicy"] {
                spec.remove(field);
            }
        }
        for field in ["healthCheckNodePort", "loadBalancerIP"] {
            spec.remove(field);
        }
        if let Some(ports) = spec.get_mut("ports").and_then(Value::as_array_mut) {
            for port in ports {
                if let Some(port) = port.as_object_mut() {
                    port.remove("nodePort");
                }
            }
        }
    }
    if kind.resource.group.is_empty() && kind.resource.kind == "ServiceAccount" {
        // API-generated token names cannot travel to another namespace.
        value.as_object_mut().unwrap().remove("secrets");
    }
    if kind.resource.group.is_empty()
        && kind.resource.kind == "Pod"
        && let Some(spec) = value.get_mut("spec").and_then(Value::as_object_mut)
    {
        for field in ["nodeName", "ephemeralContainers", "overhead"] {
            spec.remove(field);
        }
    }
    if kind.resource.group == "batch" && kind.resource.kind == "Job" {
        let mut fresh = crate::jobs::manifest(kind, object)?;
        fresh["metadata"] = value["metadata"].clone();
        value = fresh;
    }
    if kind.resource.group == "argoproj.io" && kind.resource.kind == "Workflow" {
        return Err(invalid(
            "Use Submit or Resubmit for Workflow runs; transfer a WorkflowTemplate instead.",
        ));
    }
    // PVC references do not move data. Never offer a misleading successful
    // transfer when the destination may contain a different volume's data.
    if pod_spec(&value).is_some_and(|spec| {
        spec["volumes"].as_array().is_some_and(|volumes| {
            volumes
                .iter()
                .any(|volume| volume.get("persistentVolumeClaim").is_some())
        })
    }) {
        return Err(invalid(
            "This workload references persistent volumes. Migrate storage and its data separately before transferring the workload.",
        ));
    }
    Ok(value)
}
fn pod_spec(value: &Value) -> Option<&Value> {
    match value["kind"].as_str()? {
        "Pod" => value.get("spec"),
        "CronJob" => value.pointer("/spec/jobTemplate/spec/template/spec"),
        _ => value.pointer("/spec/template/spec"),
    }
}

pub(super) fn waiting(kind: &Kind, object: &DynamicObject) -> Option<String> {
    if object.metadata.deletion_timestamp.is_some() {
        return Some("The target is being deleted.".into());
    }
    let status = &object.data["status"];
    let number = |value: &Value| value.as_i64().unwrap_or(0);
    let observed = number(&status["observedGeneration"]) >= object.metadata.generation.unwrap_or(1);
    let desired = object
        .data
        .pointer("/spec/replicas")
        .and_then(Value::as_i64)
        .unwrap_or(1);
    let ready = match (kind.resource.group.as_str(), kind.resource.kind.as_str()) {
        ("apps", "Deployment") => {
            observed
                && number(&status["updatedReplicas"]) >= desired
                && number(&status["availableReplicas"]) >= desired
        }
        ("apps", "StatefulSet") => {
            observed
                && number(&status["readyReplicas"]) >= desired
                && status["currentRevision"] == status["updateRevision"]
        }
        ("apps", "DaemonSet") => {
            observed
                && number(&status["numberReady"]) >= number(&status["desiredNumberScheduled"])
                && number(&status["updatedNumberScheduled"])
                    >= number(&status["desiredNumberScheduled"])
        }
        ("apps", "ReplicaSet") | ("", "ReplicationController") => {
            observed && number(&status["readyReplicas"]) >= desired
        }
        ("", "Pod") => status["conditions"].as_array().is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition["type"] == "Ready" && condition["status"] == "True")
        }),
        _ => true,
    };
    (!ready)
        .then(|| "Target workload is not ready yet. Check its status, then verify again.".into())
}
