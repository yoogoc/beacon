//! Per-cluster defaults for opening resource YAML. Fold state stays in the editor.

use serde::{
    Deserialize, Serialize,
    de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_saphyr::Spanned;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Field {
    pub path: String,
    pub collapsed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct YamlFolding {
    pub fields: Vec<Field>,
}

impl Default for YamlFolding {
    fn default() -> Self {
        Self {
            fields: [
                ("metadata.managedFields", true),
                ("status", true),
                ("metadata.labels", false),
                ("metadata.annotations", false),
                ("spec", false),
            ]
            .into_iter()
            .map(|(path, collapsed)| Field {
                path: path.into(),
                collapsed,
            })
            .collect(),
        }
    }
}

impl YamlFolding {
    pub fn validate(&self) -> Result<(), String> {
        let mut paths = std::collections::BTreeSet::new();
        for field in &self.fields {
            validate_path(&field.path)?;
            if !paths.insert(&field.path) {
                return Err(format!("YAML field '{}' is already listed.", field.path));
            }
        }
        Ok(())
    }

    pub fn add(&mut self, path: &str) -> Result<(), String> {
        let path = path.trim();
        validate_path(path)?;
        if self.fields.iter().any(|field| field.path == path) {
            return Err(format!("YAML field '{path}' is already listed."));
        }
        self.fields.push(Field {
            path: path.into(),
            collapsed: true,
        });
        Ok(())
    }

    /// Source locations come from the YAML parser, so block strings, quoted
    /// keys, and indentless sequences cannot be mistaken for mapping fields.
    pub fn initial_lines(&self, yaml: &str) -> Vec<usize> {
        let paths: Vec<Vec<&str>> = self
            .fields
            .iter()
            .filter(|field| field.collapsed)
            .map(|field| field.path.split('.').collect())
            .collect();
        if paths.is_empty() {
            return Vec::new();
        }
        let paths: Vec<&[&str]> = paths.iter().map(Vec::as_slice).collect();
        let mut lines = Vec::new();
        let result = serde_saphyr::with_deserializer_from_str(yaml, |deserializer| {
            Locator {
                paths: &paths,
                lines: &mut lines,
            }
            .deserialize(deserializer)
        });
        if result.is_err() {
            // A presentation preference must never prevent opening a document.
            return Vec::new();
        }
        lines.sort_unstable();
        lines.dedup();
        // Inline values have no body to fold. Preserve the editor's normal
        // behavior for mappings, lists, and multiline scalar blocks.
        let mut source_lines = yaml.lines().enumerate();
        lines.retain(|line| {
            source_lines
                .by_ref()
                .find(|(index, _)| index == line)
                .is_some_and(|(_, header)| {
                    let header = header.trim_end();
                    [":", "|", "|-", "|+", ">", ">-", ">+"]
                        .iter()
                        .any(|suffix| header.ends_with(suffix))
                })
        });
        lines
    }
}

fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 256
        || path.chars().any(|c| c.is_whitespace() || c.is_control())
        || path.split('.').any(str::is_empty)
        || path.contains(['[', ']'])
    {
        return Err("Use a dot-separated YAML field path, such as metadata.annotations or spec.template.spec.containers.".into());
    }
    Ok(())
}

struct Locator<'a, 'b> {
    paths: &'a [&'a [&'a str]],
    lines: &'b mut Vec<usize>,
}

impl<'de> DeserializeSeed<'de> for Locator<'_, '_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Locator<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a resource YAML value")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<(), M::Error> {
        while let Some(key) = map.next_key::<Spanned<String>>()? {
            let matches: Vec<_> = self
                .paths
                .iter()
                .copied()
                .filter(|path| path.first() == Some(&key.value.as_str()))
                .collect();
            if matches.iter().any(|path| path.len() == 1)
                && let Some(line) = key.referenced.line().checked_sub(1)
            {
                self.lines.push(line as usize);
            }
            let children: Vec<_> = matches
                .iter()
                .filter(|path| path.len() > 1)
                .map(|path| &path[1..])
                .collect();
            if children.is_empty() {
                map.next_value::<IgnoredAny>()?;
            } else {
                map.next_value_seed(Locator {
                    paths: &children,
                    lines: self.lines,
                })?;
            }
        }
        Ok(())
    }

    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<(), S::Error> {
        // A path below a list applies to every item, e.g. spec.containers.resources.
        while seq
            .next_element_seed(Locator {
                paths: self.paths,
                lines: self.lines,
            })?
            .is_some()
        {}
        Ok(())
    }

    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::YamlFolding;

    fn header_lines(folding: &YamlFolding, yaml: &str) -> Vec<String> {
        folding
            .initial_lines(yaml)
            .into_iter()
            .map(|line| yaml.lines().nth(line).unwrap().trim().to_string())
            .collect()
    }

    #[test]
    fn custom_paths_match_nested_fields_and_each_list_item() {
        let mut folding = YamlFolding { fields: Vec::new() };
        folding
            .add("spec.template.spec.containers.resources")
            .unwrap();
        folding.add("metadata.annotations").unwrap();
        let yaml = "metadata:\n  \"annotations\":\n    app: web\nspec:\n  template:\n    spec:\n      containers:\n      - name: web\n        resources:\n          limits:\n            cpu: '1'\n      - name: sidecar\n        resources:\n          requests:\n            memory: 64Mi\nstatus:\n  resources:\n    cpu: '2'\n";
        assert_eq!(
            header_lines(&folding, yaml),
            ["\"annotations\":", "resources:", "resources:"]
        );
    }

    #[test]
    fn disabling_fields_and_explicit_empty_settings_leave_yaml_open() {
        let yaml = "metadata:\n  managedFields:\n  - manager: beacon\n    operation: Apply\nstatus:\n  phase: Running\n  ready: true\n";
        let mut folding = YamlFolding::default();
        folding
            .fields
            .iter_mut()
            .find(|field| field.path == "status")
            .unwrap()
            .collapsed = false;
        assert_eq!(header_lines(&folding, yaml), ["managedFields:"]);
        for field in &mut folding.fields {
            field.collapsed = false;
        }
        assert!(folding.initial_lines(yaml).is_empty());
        let empty: YamlFolding = serde_json::from_str(r#"{"fields":[]}"#).unwrap();
        assert!(empty.initial_lines(yaml).is_empty());
    }

    #[test]
    fn literal_content_and_scalar_ancestors_do_not_match_field_paths() {
        let mut folding = YamlFolding { fields: Vec::new() };
        folding.add("data.script.metadata").unwrap();
        folding.add("spec.template.spec.containers").unwrap();
        let yaml = "data:\n  script: |\n    metadata:\n      managedFields:\n      - manager: literal\nspec:\n  template: unavailable\n";
        assert!(folding.initial_lines(yaml).is_empty());
    }

    #[test]
    fn invalid_and_duplicate_paths_are_rejected_without_changing_settings() {
        let mut folding = YamlFolding::default();
        let original = folding.clone();
        for path in [
            "",
            "spec..containers",
            ".status",
            "status.",
            "spec containers",
            "spec.containers[0]",
            "status",
        ] {
            assert!(folding.add(path).is_err(), "{path}");
            assert_eq!(folding, original);
        }
        folding.add(" spec.template.spec.containers ").unwrap();
        folding.validate().unwrap();
    }
}
