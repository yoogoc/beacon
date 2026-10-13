//! YAML paths, completion contexts and source coordinates, including partially
//! written documents. Tree-sitter keeps strings/comments out of field lookup.
use beacon_kube::schema::{Issue, Schema};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionTextEdit, Documentation, Hover, HoverContents,
    MarkupContent, MarkupKind, Position, Range, TextEdit,
};
use std::{ops::Range as Bytes, sync::Arc};
use tree_sitter::{Node, Parser};

#[derive(Clone, Debug)]
pub(crate) struct Location {
    pub path: Vec<String>,
    pub key: Bytes<usize>,
    pub value: Bytes<usize>,
    pub structured: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct Problem {
    pub issue: Issue,
    pub range: Bytes<usize>,
}
fn parser() -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .unwrap();
    parser
}
pub(crate) fn locations(text: &str) -> Vec<Location> {
    let Some(tree) = parser().parse(text, None) else {
        return vec![];
    };
    let mut locations = Vec::new();
    visit(tree.root_node(), text, &[], &mut locations);
    locations
}
fn visit(node: Node<'_>, text: &str, path: &[String], locations: &mut Vec<Location>) {
    if matches!(node.kind(), "block_mapping_pair" | "flow_pair") {
        let Some(key) = node.child_by_field_name("key") else {
            return;
        };
        let raw = key.utf8_text(text.as_bytes()).unwrap_or("");
        let name = serde_saphyr::from_str::<String>(raw)
            .unwrap_or_else(|_| raw.trim_matches(['\'', '"']).to_owned());
        let mut path = path.to_vec();
        path.push(name);
        let value = node.child_by_field_name("value");
        let structured = value.is_some_and(node_contains_structure);
        locations.push(Location {
            path: path.clone(),
            key: key.byte_range(),
            value: value.map_or(node.end_byte()..node.end_byte(), |node| node.byte_range()),
            structured,
        });
        if let Some(value) = value {
            visit(value, text, &path, locations);
        }
        return;
    }
    if matches!(node.kind(), "block_sequence" | "flow_sequence") {
        let mut cursor = node.walk();
        for (index, item) in node
            .named_children(&mut cursor)
            .filter(|item| item.kind() != "comment")
            .enumerate()
        {
            let mut path = path.to_vec();
            path.push(index.to_string());
            visit(item, text, &path, locations);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        visit(child, text, path, locations);
    }
}
fn node_contains_structure(node: Node<'_>) -> bool {
    if matches!(
        node.kind(),
        "block_mapping" | "flow_mapping" | "block_sequence" | "flow_sequence"
    ) {
        return true;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .any(node_contains_structure)
}
pub(crate) fn position(text: &str, offset: usize) -> Position {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count();
    let tail = before.rsplit_once('\n').map_or(before, |(_, tail)| tail);
    Position::new(line as u32, tail.encode_utf16().count() as u32)
}
pub(crate) fn range(text: &str, bytes: Bytes<usize>) -> Range {
    Range::new(position(text, bytes.start), position(text, bytes.end))
}
pub(crate) fn analyze(schema: &Schema, text: &str) -> Vec<Problem> {
    let locations = locations(text);
    let mut problems = match serde_saphyr::from_str::<serde_json::Value>(text) {
        Ok(value) => schema
            .validate(&value)
            .into_iter()
            .map(|issue| {
                let path = issue
                    .pointer
                    .split('/')
                    .skip(1)
                    .map(|part| part.replace("~1", "/").replace("~0", "~"))
                    .collect::<Vec<_>>();
                let location = locations
                    .iter()
                    .filter(|location| path.starts_with(&location.path))
                    .max_by_key(|location| location.path.len());
                let range =
                    location.map_or(0..text.lines().next().map_or(0, str::len), |location| {
                        if location.structured || location.value.is_empty() {
                            location.key.clone()
                        } else {
                            location.value.clone()
                        }
                    });
                Problem { issue, range }
            })
            .collect(),
        Err(error) => {
            let start = error
                .location()
                .map(|location| {
                    if let Some(offset) = location
                        .span()
                        .byte_offset()
                        .and_then(|offset| usize::try_from(offset).ok())
                        && offset > 0
                    {
                        return offset.min(text.len());
                    }
                    if location.line() == 0 {
                        return text.len();
                    }
                    let line = location.line().saturating_sub(1) as usize;
                    let line_start = text
                        .split_inclusive('\n')
                        .take(line)
                        .map(str::len)
                        .sum::<usize>()
                        .min(text.len());
                    let column = location.column().saturating_sub(1) as usize;
                    let tail = &text[line_start..];
                    line_start
                        + tail
                            .char_indices()
                            .nth(column)
                            .map_or(tail.len(), |(offset, _)| offset)
                })
                .unwrap_or(text.len());
            let end = start + text[start..].chars().next().map_or(0, char::len_utf8);
            vec![Problem {
                issue: Issue {
                    pointer: String::new(),
                    message: format!("YAML syntax: {error}"),
                },
                range: start..end,
            }]
        }
    };
    problems.sort_by_key(|problem| problem.range.start);
    problems
}
fn description(schema: &Schema, path: &[String], required: bool) -> Option<String> {
    let value = schema.at(path)?;
    let mut parts = vec![format!(
        "{} · {}{}",
        path.join("."),
        value["type"]
            .as_str()
            .unwrap_or(if value["x-kubernetes-int-or-string"] == true {
                "integer or string"
            } else {
                "value"
            }),
        if required { " · required" } else { "" }
    )];
    if let Some(description) = value["description"].as_str() {
        parts.push(description.into());
    }
    if let Some(choices) = value["enum"].as_array() {
        parts.push(format!(
            "Allowed values: {}",
            choices
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(default) = value.get("default") {
        parts.push(format!("Default: {default}"));
    }
    Some(parts.join("\n\n"))
}
pub(crate) fn hover(schema: &Schema, text: &str, offset: usize) -> Option<Hover> {
    let location = locations(text)
        .into_iter()
        .filter(|location| {
            location.key.contains(&offset)
                || (!location.structured && location.value.contains(&offset))
        })
        .max_by_key(|location| location.path.len())?;
    let required = schema
        .fields(&location.path[..location.path.len() - 1])
        .iter()
        .any(|field| field.name == *location.path.last().unwrap() && field.required);
    let description = description(schema, &location.path, required)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::PlainText,
            value: description,
        }),
        range: Some(range(text, location.key)),
    })
}
pub(crate) fn completions(schema: &Schema, text: &str, offset: usize) -> Vec<CompletionItem> {
    let offset = offset.min(text.len());
    if !text.is_char_boundary(offset) {
        return vec![];
    }
    let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let line_end = text[offset..]
        .find('\n')
        .map_or(text.len(), |index| offset + index);
    let before = &text[line_start..offset];
    if before.trim_start().starts_with('#') {
        return vec![];
    }
    let original = locations(text);
    if let Some(location) = original.iter().find(|location| {
        !location.structured
            && position(text, location.key.start).line == position(text, offset).line
            && offset >= location.key.end
            && text[line_start..offset].contains(':')
    }) {
        let Some(value_schema) = schema.at(&location.path) else {
            return vec![];
        };
        let values = value_schema["enum"].as_array().cloned().unwrap_or_else(|| {
            if value_schema["type"] == "boolean" {
                vec![serde_json::json!(true), serde_json::json!(false)]
            } else {
                value_schema
                    .get("default")
                    .map(|value| vec![value.clone()])
                    .unwrap_or_default()
            }
        });
        return values
            .into_iter()
            .map(|value| {
                let insert = value.to_string();
                CompletionItem {
                    label: insert.clone(),
                    kind: Some(CompletionItemKind::ENUM_MEMBER),
                    detail: Some("Schema value".into()),
                    documentation: description(schema, &location.path, false)
                        .map(Documentation::String),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range: range(
                            text,
                            if location.value.is_empty() {
                                offset..line_end
                            } else {
                                location.value.clone()
                            },
                        ),
                        new_text: insert,
                    })),
                    ..Default::default()
                }
            })
            .collect();
    }
    let mut synthetic = text.to_owned();
    // Complete unfinished/empty mapping keys by parsing a temporary null pair.
    // No synthetic text ever enters the user's editor or save payload.
    let has_colon = text[line_start..line_end].contains(':');
    if !has_colon {
        if before.trim().is_empty() || before.trim() == "-" {
            synthetic.insert_str(offset, "__beacon_completion__: null");
        } else {
            synthetic.insert_str(line_end, ": null");
        }
    }
    let locations = locations(&synthetic);
    let Some(location) = locations
        .iter()
        .filter(|location| location.key.start <= offset && offset <= location.key.end)
        .max_by_key(|location| location.path.len())
    else {
        return vec![];
    };
    let parent = &location.path[..location.path.len() - 1];
    let start = location.key.start.min(offset);
    let end = if has_colon {
        location.key.end.min(text.len())
    } else {
        line_end
    };
    let prefix = text[start..offset].trim_matches(['\'', '"']);
    let existing = original
        .iter()
        .filter(|entry| entry.path.len() == parent.len() + 1 && entry.path.starts_with(parent))
        .map(|entry| entry.path.last().unwrap())
        .collect::<Vec<_>>();
    schema
        .fields(parent)
        .into_iter()
        .filter(|field| {
            field
                .name
                .to_lowercase()
                .starts_with(&prefix.to_lowercase())
                && (!existing.contains(&&field.name)
                    || field.name == location.path.last().unwrap().as_str())
        })
        .map(|field| {
            let mut path = parent.to_vec();
            path.push(field.name.clone());
            let insert = if has_colon {
                field.name.clone()
            } else {
                format!("{}: ", field.name)
            };
            CompletionItem {
                label: field.name.clone(),
                kind: Some(CompletionItemKind::FIELD),
                detail: Some(format!(
                    "{}{}",
                    field.schema["type"].as_str().unwrap_or("value"),
                    if field.required { " · required" } else { "" }
                )),
                sort_text: Some(format!(
                    "{}{name}",
                    if field.required { "0" } else { "1" },
                    name = field.name
                )),
                documentation: description(schema, &path, field.required)
                    .map(Documentation::String),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range: range(text, start..end),
                    new_text: insert,
                })),
                ..Default::default()
            }
        })
        .collect()
}

pub(crate) struct Provider(pub Arc<Schema>);
impl gpui_kit::component::input::CompletionProvider for Provider {
    fn completions(
        &self,
        text: &gpui_kit::base::input::Rope,
        offset: usize,
        _: lsp_types::CompletionContext,
        _: &mut gpui_kit::Window,
        _: &mut gpui_kit::App,
    ) -> gpui_kit::Task<anyhow::Result<lsp_types::CompletionResponse>> {
        gpui_kit::Task::ready(Ok(lsp_types::CompletionResponse::Array(completions(
            &self.0,
            &text.to_string(),
            offset,
        ))))
    }
    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut gpui_kit::App) -> bool {
        new_text
            .chars()
            .any(|ch| ch.is_alphanumeric() || matches!(ch, ':' | '\n' | ' ' | '-' | '_'))
    }
}
impl gpui_kit::component::input::HoverProvider for Provider {
    fn hover(
        &self,
        text: &gpui_kit::base::input::Rope,
        offset: usize,
        _: &mut gpui_kit::Window,
        _: &mut gpui_kit::App,
    ) -> gpui_kit::Task<anyhow::Result<Option<Hover>>> {
        gpui_kit::Task::ready(Ok(hover(&self.0, &text.to_string(), offset)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    pub(crate) fn schema() -> Schema {
        let container = json!({"type":"object","required":["name"],"properties":{"name":{"type":"string"},"image":{"type":"string","description":"Container image."}}});
        let spec = json!({"type":"object","required":["replicas"],"properties":{"replicas":{"type":"integer","description":"Desired replica count."},"mode":{"type":"string","enum":["Rolling","Recreate"]},"containers":{"type":"array","items":container}}});
        Schema {
            source: "Fixture OpenAPI v3".into(),
            version: "v1.35.0".into(),
            definitions: Default::default(),
            root: json!({"type":"object","properties":{"spec":spec}}),
        }
    }
    #[test]
    fn partial_mapping_and_indentless_container_keys_complete_at_the_real_path() {
        let schema = schema();
        let yaml = "spec:\n  re";
        let items = completions(&schema, yaml, yaml.len());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "replicas");
        assert!(items[0].detail.as_ref().unwrap().contains("required"));
        let yaml = "spec:\n  containers:\n  - name: api\n    im";
        let items = completions(&schema, yaml, yaml.len());
        assert!(items.iter().any(|item| item.label == "image"), "{items:?}");
        let yaml = "spec:\n  ";
        let items = completions(&schema, yaml, yaml.len());
        assert!(items.iter().any(|item| item.label == "mode"));
    }
    #[test]
    fn enum_values_docs_diagnostics_and_utf16_coordinates_keep_original_yaml() {
        let schema = schema();
        let yaml = "spec:\n  mode: ";
        let items = completions(&schema, yaml, yaml.len());
        assert_eq!(items.len(), 2);
        let CompletionTextEdit::Edit(edit) = items[0].text_edit.clone().unwrap() else {
            panic!()
        };
        assert_eq!(edit.range.start, position(yaml, yaml.len()));
        let yaml = "spec:\n  replicas: wrong\n  mode: Other\n  containers:\n  - image: busybox\n";
        let issues = analyze(&schema, yaml);
        assert!(
            issues
                .iter()
                .any(|issue| &yaml[issue.range.clone()] == "wrong")
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.issue.pointer == "/spec/containers/0")
        );
        let yaml = "spec:\n  replicas: 2";
        let docs = hover(&schema, yaml, yaml.find("replicas").unwrap()).unwrap();
        assert!(
            matches!(docs.contents, HoverContents::Markup(content) if content.value.contains("Desired replica count"))
        );
        assert_eq!(position("中文😀: 值", "中文😀".len()), Position::new(0, 4));
        let yaml = "spec:\n  replicas: [\n";
        let issues = analyze(&schema, yaml);
        assert!(issues[0].issue.message.contains("YAML syntax"));
        assert!(issues[0].range.start > 0);
    }
}
