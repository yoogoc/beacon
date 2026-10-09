//! Persisted list layouts and named filters, keyed by context / API group / kind.
use crate::filters::Field;
use beacon_kube::{ClusterId, Kind, labels::LabelSelector};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ColumnLayout {
    pub order: Vec<String>,
    pub hidden: BTreeSet<String>,
    pub widths: BTreeMap<String, f32>,
    pub sort: Option<ColumnSort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ColumnSort {
    pub column: String,
    pub descending: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct FilterPreset {
    pub namespaces: BTreeSet<String>,
    pub search: String,
    pub labels: String,
    pub fields: BTreeMap<Field, String>,
    pub sort: Option<ColumnSort>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ResourcePreferences {
    pub columns: ColumnLayout,
    pub filters: BTreeMap<String, FilterPreset>,
}

impl ResourcePreferences {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .columns
            .widths
            .values()
            .any(|width| !width.is_finite() || !(56. ..=4000.).contains(width))
        {
            return Err("Column widths must be between 56 and 4000 pixels.".into());
        }
        for (name, filter) in &self.filters {
            if name.trim().is_empty() || name.len() > 120 {
                return Err("Filter names must contain 1–120 characters.".into());
            }
            LabelSelector::parse(&filter.labels).map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

pub(crate) fn key(id: &ClusterId, kind: &Kind) -> String {
    serde_json::to_string(&(id.as_str(), &kind.resource.group, &kind.resource.kind))
        .expect("strings are serializable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use beacon_kube::resources;

    #[test]
    fn layouts_are_isolated_by_original_context_and_api_group_but_not_version() {
        let mut kind = Kind {
            resource: resources::pod(),
            namespaced: true,
            verbs: vec![],
        };
        let first = key(&ClusterId::new("context-a"), &kind);
        assert_ne!(first, key(&ClusterId::new("context-b"), &kind));
        kind.resource.version = "v2".into();
        assert_eq!(first, key(&ClusterId::new("context-a"), &kind));
        kind.resource.group = "example.com".into();
        assert_ne!(first, key(&ClusterId::new("context-a"), &kind));
    }

    #[test]
    fn invalid_widths_and_filters_cannot_be_persisted() {
        let mut preferences = ResourcePreferences::default();
        for width in [f32::NAN, f32::INFINITY, 0., 4001.] {
            preferences.columns.widths.insert("name".into(), width);
            assert!(preferences.validate().is_err());
        }
        preferences.columns.widths.insert("name".into(), 320.);
        preferences
            .filters
            .insert(" ".into(), FilterPreset::default());
        assert!(preferences.validate().is_err());
        preferences.filters.clear();
        preferences.filters.insert(
            "Production".into(),
            FilterPreset {
                labels: "app in (".into(),
                ..Default::default()
            },
        );
        assert!(preferences.validate().is_err());
        preferences.filters.get_mut("Production").unwrap().labels = "app=web".into();
        assert!(preferences.validate().is_ok());
    }
}
