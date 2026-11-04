use super::{Limited, PRETTY_LIMIT};
use std::io::{self, Write};

pub(super) fn format(text: &str) -> Option<Vec<u8>> {
    let evidence = text.starts_with(['{', '['])
        || text.starts_with("- ")
        || text.contains(": ")
        || text.ends_with(':');
    if !evidence {
        return None;
    }
    if let Some((key, _)) = text.split_once(':') {
        if ["INFO", "WARN", "WARNING", "ERROR", "DEBUG", "TRACE"].contains(&key) {
            return None;
        }
        if !text.starts_with(['{', '[', '-'])
            && !key
                .chars()
                .all(|ch| ch.is_alphanumeric() || "_./-".contains(ch))
        {
            return None;
        }
    }
    let value = parse(text)?;
    if !value.is_object() && !value.is_array() {
        return None;
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_yaml::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(text, None)?;
    if tree.root_node().has_error() {
        return None;
    }
    let mut out = Limited(Vec::new());
    render(tree.root_node(), text, 0, &mut out).ok()?;
    let formatted = std::str::from_utf8(&out.0).ok()?;
    // Formatting may only change layout. Keep anchors, tags, scalar spelling,
    // comments and source order rather than serializing a materialized map.
    if parse(formatted).as_ref() != Some(&value) {
        return Some(text.as_bytes().to_vec());
    }
    Some(out.0)
}
fn parse(text: &str) -> Option<serde_json::Value> {
    let mut budget = serde_saphyr::Budget::default();
    budget.max_depth = 32;
    budget.max_nodes = 20_000;
    budget.max_total_scalar_bytes = PRETTY_LIMIT;
    budget.max_total_comment_bytes = PRETTY_LIMIT;
    budget.max_inclusion_depth = 0;
    budget.max_documents = 1;
    let mut options = serde_saphyr::Options::default();
    options.budget = Some(budget);
    serde_saphyr::from_str_with_options(text, options).ok()
}
fn render(
    node: tree_sitter::Node<'_>,
    text: &str,
    depth: usize,
    out: &mut Limited,
) -> io::Result<()> {
    if depth > 128 {
        return Err(io::Error::other("YAML nesting limit"));
    }
    let mut cursor = node.walk();
    let children = node.named_children(&mut cursor).collect::<Vec<_>>();
    if matches!(node.kind(), "flow_mapping" | "flow_sequence") {
        // Flow comments have newline semantics; preserve their exact layout.
        if text[node.byte_range()].contains('#') {
            return out.write_all(text[node.byte_range()].as_bytes());
        }
        out.write_all(&text.as_bytes()[node.start_byte()..node.start_byte() + 1])?;
        for (index, child) in children.iter().enumerate() {
            out.write_all(b"\n")?;
            for _ in 0..=depth {
                out.write_all(b"  ")?;
            }
            render(*child, text, depth + 1, out)?;
            if index + 1 < children.len() {
                out.write_all(b",")?;
            }
        }
        if !children.is_empty() {
            out.write_all(b"\n")?;
            for _ in 0..depth {
                out.write_all(b"  ")?;
            }
        }
        return out.write_all(&text.as_bytes()[node.end_byte() - 1..node.end_byte()]);
    }
    let mut offset = node.start_byte();
    for child in children {
        out.write_all(&text.as_bytes()[offset..child.start_byte()])?;
        render(child, text, depth, out)?;
        offset = child.end_byte();
    }
    out.write_all(&text.as_bytes()[offset..node.end_byte()])
}
