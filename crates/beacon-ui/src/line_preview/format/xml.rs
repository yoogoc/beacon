use super::Limited;
use std::io::{self, Write};

pub(super) fn format(text: &str) -> Option<Vec<u8>> {
    let doc = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .ok()?;
    let root = doc.root_element();
    let range = root.range();
    let mut out = Limited(Vec::new());
    let prefix = text[..range.start].trim_end();
    if !prefix.is_empty() {
        out.write_all(prefix.as_bytes()).ok()?;
        out.write_all(b"\n").ok()?;
    }
    render(root, text, 0, &mut out).ok()?;
    let suffix = text[range.end..].trim_start();
    if !suffix.is_empty() {
        out.write_all(b"\n").ok()?;
        out.write_all(suffix.as_bytes()).ok()?;
    }
    Some(out.0)
}
fn indent(depth: usize, out: &mut Limited) -> io::Result<()> {
    out.write_all(b"\n")?;
    for _ in 0..depth {
        out.write_all(b"  ")?;
    }
    Ok(())
}
fn render(
    node: roxmltree::Node<'_, '_>,
    text: &str,
    depth: usize,
    out: &mut Limited,
) -> io::Result<()> {
    if depth > 64 {
        return Err(io::Error::other("XML nesting limit"));
    }
    let original = &text[node.range()];
    // Adding whitespace to mixed content or xml:space=preserve changes its data.
    if !node.is_element()
        || original.contains("<![CDATA[")
        || !node.children().any(|child| child.is_element())
        || node
            .children()
            .any(|child| child.is_text() && child.text().is_some_and(|s| !s.trim().is_empty()))
        || node.ancestors().any(|parent| {
            parent.attribute(("http://www.w3.org/XML/1998/namespace", "space")) == Some("preserve")
        })
    {
        return out.write_all(original.as_bytes());
    }
    let mut quote = None;
    let mut opening = None;
    for (index, ch) in original.char_indices() {
        if quote == Some(ch) {
            quote = None;
        } else if quote.is_none() {
            if matches!(ch, '\'' | '"') {
                quote = Some(ch);
            } else if ch == '>' {
                opening = Some(index + 1);
                break;
            }
        }
    }
    let opening = opening.ok_or_else(|| io::Error::other("missing XML start tag"))?;
    let closing = original
        .rfind("</")
        .ok_or_else(|| io::Error::other("missing XML end tag"))?;
    out.write_all(&original.as_bytes()[..opening])?;
    for child in node.children().filter(|child| !child.is_text()) {
        indent(depth + 1, out)?;
        render(child, text, depth + 1, out)?;
    }
    indent(depth, out)?;
    out.write_all(&original.as_bytes()[closing..])
}
