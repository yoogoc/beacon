//! Workload selectors and log identities across Pod replacement and restart.
use crate::{DynamicObject, labels::LabelSelector};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

pub fn workload_selector(object: &DynamicObject) -> Result<String, String> {
    let selector = object
        .data
        .pointer("/spec/selector")
        .ok_or("This workload has no Pod selector.")?;
    let mut parts = Vec::new();
    if let Some(labels) = selector.get("matchLabels").and_then(Value::as_object) {
        let sorted: BTreeMap<_, _> = labels.iter().collect();
        for (key, value) in sorted {
            parts.push(format!(
                "{key}={}",
                value.as_str().ok_or("Selector values must be strings.")?
            ));
        }
    }
    for expression in selector
        .get("matchExpressions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let key = expression["key"]
            .as_str()
            .ok_or("Selector key is missing.")?;
        let values = expression["values"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|value| value.as_str().ok_or("Selector values must be strings."))
            .collect::<Result<Vec<_>, _>>()?;
        parts.push(match expression["operator"].as_str() {
            Some("In") => format!("{key} in ({})", values.join(",")),
            Some("NotIn") => format!("{key} notin ({})", values.join(",")),
            Some("Exists") => key.into(),
            Some("DoesNotExist") => format!("!{key}"),
            _ => return Err("Unsupported selector operator.".into()),
        });
    }
    let query = LabelSelector::parse(&parts.join(","))?;
    if query.is_empty() {
        return Err("This workload has an empty Pod selector.".into());
    }
    Ok(query.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Source {
    pub namespace: String,
    pub pod: String,
    pub uid: String,
    pub container: String,
}
impl Source {
    pub fn prefix(&self) -> String {
        format!("[{}/{}:{}]", self.namespace, self.pod, self.container)
    }
}

/// A restart changes the stream even though the Pod/container name did not.
pub fn sources<'a>(
    pods: impl Iterator<Item = &'a Arc<DynamicObject>>,
    container: Option<&str>,
) -> BTreeMap<Source, i64> {
    let mut sources = BTreeMap::new();
    for pod in pods {
        let (Some(namespace), Some(name), Some(uid)) = (
            &pod.metadata.namespace,
            &pod.metadata.name,
            &pod.metadata.uid,
        ) else {
            continue;
        };
        for spec in pod
            .data
            .pointer("/spec/containers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name_container) = spec["name"].as_str() else {
                continue;
            };
            if container.is_some_and(|selected| selected != name_container) {
                continue;
            }
            let status = pod
                .data
                .pointer("/status/containerStatuses")
                .and_then(Value::as_array)
                .and_then(|statuses| {
                    statuses
                        .iter()
                        .find(|status| status["name"] == name_container)
                });
            // Wait for a container to exist, then follow even unready containers.
            if status.is_none_or(|status| {
                status.pointer("/state/running").is_none()
                    && status.pointer("/state/terminated").is_none()
            }) {
                continue;
            }
            let restarts = status
                .and_then(|status| status["restartCount"].as_i64())
                .unwrap_or(0);
            sources.insert(
                Source {
                    namespace: namespace.clone(),
                    pod: name.clone(),
                    uid: uid.clone(),
                    container: name_container.into(),
                },
                restarts,
            );
        }
    }
    sources
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn expression_selector_and_restart_identity_are_preserved() {
        let workload: DynamicObject = serde_json::from_value(json!({"spec":{"selector":{"matchLabels":{"app":"web"},"matchExpressions":[{"key":"track","operator":"In","values":["canary","stable"]}]}}})).unwrap();
        assert_eq!(
            workload_selector(&workload).unwrap(),
            "app=web,track in (canary,stable)"
        );
        let mut pod: DynamicObject = serde_json::from_value(json!({"metadata":{"name":"web-1","namespace":"default","uid":"uid-1"},"spec":{"containers":[{"name":"app"},{"name":"sidecar"}]},"status":{"containerStatuses":[{"name":"app","restartCount":1,"state":{"running":{}}}]}})).unwrap();
        let first = sources([Arc::new(pod.clone())].iter(), None);
        assert_eq!(first.len(), 1);
        assert_eq!(*first.values().next().unwrap(), 1);
        pod.data["status"]["containerStatuses"][0]["restartCount"] = json!(2);
        let second = sources([Arc::new(pod.clone())].iter(), None);
        assert_eq!(first.keys().next(), second.keys().next());
        assert_ne!(first.values().next(), second.values().next());
        pod.metadata.uid = Some("uid-2".into());
        assert_ne!(
            first.keys().next(),
            sources([Arc::new(pod)].iter(), None).keys().next()
        );
    }
}
