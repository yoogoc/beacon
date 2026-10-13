//! The selected entrypoint can declare required inputs even when a reusable
//! WorkflowTemplate has no workflow-wide arguments.
use std::collections::BTreeMap;

use serde_json::Value;

use super::Parameter;
use crate::{Error, Result};

fn text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(value) => Some(value.clone()),
        _ => Some(value.to_string()),
    }
}

/// Keep global argument values and references authoritative, using the selected
/// template's inputs to supply defaults, constraints and missing definitions.
/// A reference-only Workflow may not carry its templates in spec; its existing
/// global arguments remain usable when no local entrypoint can be resolved.
pub(super) fn merge(
    spec: &Value,
    entrypoint: &str,
    mut base: Vec<Parameter>,
) -> Result<Vec<Parameter>> {
    let mut indices = BTreeMap::new();
    for (index, parameter) in base.iter_mut().enumerate() {
        if parameter.key.trim().is_empty() {
            return Err(Error::Manifest(
                "a Workflow argument parameter has an empty name".into(),
            ));
        }
        if indices.insert(parameter.key.clone(), index).is_some() {
            return Err(Error::Manifest(format!(
                "Workflow argument parameter {} is declared more than once",
                parameter.key
            )));
        }
        parameter.required = parameter.value.is_none() && !parameter.runtime;
    }
    if entrypoint.is_empty() {
        return Ok(base);
    }
    let mut templates = spec
        .get("templates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|template| template["name"].as_str() == Some(entrypoint));
    let Some(template) = templates.next() else {
        return Ok(base);
    };
    if templates.next().is_some() {
        return Err(Error::Manifest(format!(
            "entrypoint template {entrypoint} is declared more than once"
        )));
    }
    let parameters = match template.pointer("/inputs/parameters") {
        None | Some(Value::Null) => return Ok(base),
        Some(Value::Array(parameters)) => parameters,
        _ => {
            return Err(Error::Manifest(format!(
                "entrypoint {entrypoint} has an invalid input parameter list"
            )));
        }
    };
    let mut inputs = BTreeMap::new();
    for input in parameters {
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| {
                Error::Manifest(format!(
                    "entrypoint {entrypoint} has an input parameter without a nonempty name"
                ))
            })?;
        if inputs.insert(name, ()).is_some() {
            return Err(Error::Manifest(format!(
                "input parameter {name} is declared more than once in entrypoint {entrypoint}"
            )));
        }
        let value = input
            .get("value")
            .and_then(text)
            .or_else(|| input.get("default").and_then(text));
        let runtime = input.get("valueFrom").is_some_and(Value::is_object);
        let description = input
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        let choices: Vec<_> = input
            .get("enum")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(text)
            .collect();
        if let Some(index) = indices.get(name).copied() {
            let global = &mut base[index];
            // A supplied global argument replaces the input definition's value
            // source, including its default or valueFrom reference.
            if global.value.is_none() && !global.runtime {
                global.value = value;
                global.runtime = runtime;
            }
            if global.description.trim().is_empty() {
                global.description = description.into();
            }
            if global.choices.is_empty() {
                global.choices = choices;
            }
            global.required = global.value.is_none() && !global.runtime;
        } else {
            indices.insert(name.to_owned(), base.len());
            base.push(Parameter {
                key: name.into(),
                name: name.into(),
                required: value.is_none() && !runtime,
                value,
                description: description.into(),
                choices,
                runtime,
            });
        }
    }
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::{Parameter, merge};
    use serde_json::json;

    fn global(name: &str, value: Option<&str>, runtime: bool) -> Parameter {
        Parameter {
            key: name.into(),
            name: name.into(),
            value: value.map(str::to_owned),
            description: String::new(),
            choices: vec![],
            required: value.is_none() && !runtime,
            runtime,
        }
    }

    #[test]
    fn entrypoint_inputs_define_required_default_and_runtime_arguments() {
        let spec = json!({"templates":[{"name":"main","inputs":{"parameters":[
            {"name":"message","description":"Message to print","enum":["hello","bye"]},
            {"name":"count","default":3},
            {"name":"blank","default":""},
            {"name":"token","valueFrom":{"secretKeyRef":{"name":"token","key":"value"}}}
        ]}}]});
        let parameters = merge(&spec, "main", vec![]).unwrap();
        assert_eq!(parameters.len(), 4);
        assert!(parameters[0].required);
        assert_eq!(parameters[0].name, "message");
        assert_eq!(parameters[0].description, "Message to print");
        assert_eq!(parameters[0].choices, vec!["hello", "bye"]);
        assert_eq!(parameters[1].value.as_deref(), Some("3"));
        assert!(!parameters[1].required);
        assert_eq!(parameters[2].value.as_deref(), Some(""));
        assert!(!parameters[2].required);
        assert!(parameters[3].runtime);
        assert!(!parameters[3].required);
        assert!(parameters[3].value.is_none());
    }

    #[test]
    fn changing_entrypoint_merges_only_its_inputs_and_keeps_global_arguments() {
        let spec = json!({"templates":[
            {"name":"first","inputs":{"parameters":[{"name":"source"},{"name":"shared","default":"first"}]}},
            {"name":"second","inputs":{"parameters":[{"name":"target","default":"prod"},{"name":"shared","default":"second"}]}}
        ]});
        let base = vec![global("shared", Some("global"), false)];
        let first = merge(&spec, "first", base.clone()).unwrap();
        let second = merge(&spec, "second", base.clone()).unwrap();
        assert_eq!(first[0].value.as_deref(), Some("global"));
        assert_eq!(first[1].key, "source");
        assert!(first[1].required);
        assert_eq!(second[0].value.as_deref(), Some("global"));
        assert_eq!(second[1].key, "target");
        assert_eq!(second[1].value.as_deref(), Some("prod"));
        assert_eq!(merge(&spec, "", base.clone()).unwrap().len(), 1);
        assert_eq!(
            merge(
                &json!({"workflowTemplateRef":{"name":"remote"}}),
                "main",
                base
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn global_values_references_and_constraints_override_entrypoint_defaults() {
        let spec = json!({"templates":[{"name":"main","inputs":{"parameters":[
            {"name":"message","default":"from-input","description":"Input description","enum":["from-input"],"valueFrom":{"configMapKeyRef":{"name":"input","key":"message"}}},
            {"name":"token","default":"do-not-use","description":"Runtime token"}
        ]}}]});
        let mut message = global("message", Some("global"), false);
        message.description = "Global description".into();
        message.choices = vec!["global".into()];
        let parameters = merge(&spec, "main", vec![message, global("token", None, true)]).unwrap();
        assert_eq!(parameters.len(), 2);
        assert_eq!(parameters[0].value.as_deref(), Some("global"));
        assert!(!parameters[0].runtime);
        assert_eq!(parameters[0].description, "Global description");
        assert_eq!(parameters[0].choices, vec!["global"]);
        assert!(parameters[1].runtime);
        assert!(!parameters[1].required);
        assert!(parameters[1].value.is_none());
        assert_eq!(parameters[1].description, "Runtime token");
    }

    #[test]
    fn missing_global_values_use_input_defaults_references_and_fallback_metadata() {
        let spec = json!({"templates":[{"name":"main","inputs":{"parameters":[
            {"name":"mode","default":"safe","description":"Execution mode","enum":["safe","fast"]},
            {"name":"token","valueFrom":{"configMapKeyRef":{"name":"runtime","key":"token"}}},
            {"name":"required"}
        ]}}]});
        let parameters = merge(
            &spec,
            "main",
            vec![global("mode", None, false), global("token", None, false)],
        )
        .unwrap();
        assert_eq!(parameters[0].value.as_deref(), Some("safe"));
        assert!(!parameters[0].required);
        assert_eq!(parameters[0].description, "Execution mode");
        assert_eq!(parameters[0].choices, vec!["safe", "fast"]);
        assert!(parameters[1].runtime);
        assert!(!parameters[1].required);
        assert!(parameters[2].required);
    }

    #[test]
    fn explicit_input_value_precedes_its_default_and_null_is_unset() {
        let spec = json!({"templates":[{"name":"main","inputs":{"parameters":[
            {"name":"enabled","value":false,"default":true},
            {"name":"blank","value":"","default":"unused"},
            {"name":"missing","value":null,"default":null}
        ]}}]});
        let parameters = merge(&spec, "main", vec![]).unwrap();
        assert_eq!(parameters[0].value.as_deref(), Some("false"));
        assert_eq!(parameters[1].value.as_deref(), Some(""));
        assert!(parameters[2].value.is_none());
        assert!(parameters[2].required);
    }

    #[test]
    fn malformed_or_duplicate_parameter_names_are_rejected() {
        for invalid in [
            json!({}),
            json!({"name":null}),
            json!({"name":7}),
            json!({"name":""}),
            json!({"name":"  "}),
        ] {
            let spec = json!({"templates":[{"name":"main","inputs":{"parameters":[invalid]}}]});
            assert!(
                merge(&spec, "main", vec![])
                    .unwrap_err()
                    .to_string()
                    .contains("nonempty name")
            );
        }
        let spec = json!({"templates":[{"name":"main","inputs":{"parameters":[{"name":"message"},{"name":"message","default":"again"}]}}]});
        assert!(
            merge(&spec, "main", vec![])
                .unwrap_err()
                .to_string()
                .contains("more than once")
        );
        assert!(merge(&json!({}), "main", vec![global("", None, false)]).is_err());
        assert!(
            merge(
                &json!({}),
                "main",
                vec![
                    global("same", None, false),
                    global("same", Some("value"), false)
                ]
            )
            .is_err()
        );
    }
}
