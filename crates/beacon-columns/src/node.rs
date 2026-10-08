//! Node scheduling metadata and EKS node group labels.

use serde_json::Value;

use crate::{CellValue, ColumnDef, ColumnSet, ColumnSource, ColumnWidth};

fn taints(data: &Value) -> &[Value] {
    data.pointer("/spec/taints")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

pub fn taint_count(data: &Value) -> usize {
    taints(data).len()
}

/// Kubernetes' familiar `key=value:effect` notation, with valueless taints
/// written as `key:effect`.
pub fn taint_descriptions(data: &Value) -> Vec<String> {
    taints(data)
        .iter()
        .map(|taint| {
            let key = taint["key"].as_str().unwrap_or("<unknown>");
            let value = taint["value"].as_str().filter(|value| !value.is_empty());
            let effect = taint["effect"].as_str().unwrap_or("<unknown>");
            match value {
                Some(value) => format!("{key}={value}:{effect}"),
                None => format!("{key}:{effect}"),
            }
        })
        .collect()
}

/// EKS managed groups take precedence over eksctl's self-managed group label.
pub fn add_node_group(columns: &mut ColumnSet) {
    let index = columns
        .columns
        .iter()
        .position(|column| column.header == "Roles")
        .map_or(1, |index| index + 1);
    columns.columns.insert(
        index,
        ColumnDef::new(
            "Node group",
            ColumnWidth::Fixed(180.0),
            ColumnSource::Computed(|cell| {
                cell.metadata
                    .labels
                    .as_ref()
                    .and_then(|labels| {
                        [
                            "eks.amazonaws.com/nodegroup",
                            "alpha.eksctl.io/nodegroup-name",
                        ]
                        .into_iter()
                        .find_map(|key| labels.get(key).filter(|value| !value.is_empty()))
                    })
                    .cloned()
                    .map(CellValue::text)
                    .unwrap_or_default()
            }),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, Timestamp};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use serde_json::json;

    #[test]
    fn taints_preserve_values_and_all_scheduling_effects() {
        let data = json!({"spec":{"taints":[
            {"key":"dedicated", "value":"gpu", "effect":"NoSchedule"},
            {"key":"node.kubernetes.io/not-ready", "effect":"NoExecute"},
            {"key":"preference", "value":"", "effect":"PreferNoSchedule"}
        ]}});
        assert_eq!(taint_count(&data), 3);
        assert_eq!(
            taint_descriptions(&data),
            [
                "dedicated=gpu:NoSchedule",
                "node.kubernetes.io/not-ready:NoExecute",
                "preference:PreferNoSchedule"
            ]
        );
        for data in [
            json!({}),
            json!({"spec":{"taints":[]}}),
            json!({"spec":{"taints":null}}),
        ] {
            assert_eq!(taint_count(&data), 0);
            assert!(taint_descriptions(&data).is_empty());
        }
    }

    #[test]
    fn node_group_uses_managed_then_eksctl_labels_without_guessing() {
        let mut columns = ColumnSet::for_kind("", "Node", false);
        assert!(!columns.headers().contains(&"Node group"));
        add_node_group(&mut columns);
        let column = columns
            .columns
            .iter()
            .find(|column| column.header == "Node group")
            .unwrap();
        for (labels, expected) in [
            (
                json!({"eks.amazonaws.com/nodegroup":"workers", "alpha.eksctl.io/nodegroup-name":"legacy"}),
                "workers",
            ),
            (
                json!({"alpha.eksctl.io/nodegroup-name":"self-managed"}),
                "self-managed",
            ),
            (
                json!({"eks.amazonaws.com/nodegroup":"", "alpha.eksctl.io/nodegroup-name":"legacy"}),
                "legacy",
            ),
            (json!({"karpenter.sh/nodepool":"default"}), "<none>"),
            (json!({}), "<none>"),
        ] {
            let metadata = ObjectMeta {
                labels: serde_json::from_value(labels).unwrap(),
                ..Default::default()
            };
            assert_eq!(
                column
                    .resolve(&Cell {
                        metadata: &metadata,
                        data: &Value::Null,
                        now: Timestamp::now(),
                        usage: None
                    })
                    .display(),
                expected
            );
        }
    }
}
