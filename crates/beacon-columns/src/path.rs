//! The part of JSONPath that `additionalPrinterColumns` actually uses.
//!
//! A CRD's printer columns are JSONPath expressions. Most are plain field
//! paths (`.spec.replicas`, `.status.phase`), but two other forms turn up often
//! enough that a client without them shows `<none>` in the column a CRD author
//! considered most important:
//!
//! ```text
//! .status.conditions[?(@.type=="Accepted")].status   // gateway-api, and most
//!                                                    // condition-bearing CRDs
//! .status.addresses[*].value                         // anything with a list
//! ```
//!
//! So this evaluates field paths, indices, wildcards and equality filters.
//! JSONPath is multi-valued by nature -- a wildcard selects many nodes -- so
//! evaluation returns a list, which is also what lets `a,b,c` be rendered the
//! way kubectl renders it.
//!
//! Everything outside that subset (recursive descent, comparisons other than
//! equality, script expressions) selects nothing, and the cell reads `<none>`.
//! That is what kubectl shows for a path that matches nothing, and it is much
//! better than a plausible wrong value.

use serde_json::Value;

/// Selects the nodes an expression matches, in document order.
///
/// Accepts the spellings CRDs are written in: `.spec.replicas`,
/// `spec.replicas`, `{.spec.replicas}` and `$.spec.replicas`.
pub fn evaluate<'a>(expression: &str, object: &'a Value) -> Vec<&'a Value> {
    let Some(segments) = parse(expression) else {
        return Vec::new();
    };

    let mut selected = vec![object];
    for segment in &segments {
        let mut next = Vec::new();
        for node in selected {
            segment.select(node, &mut next);
        }
        if next.is_empty() {
            return Vec::new();
        }
        selected = next;
    }
    selected
}

/// The first field name in a path, which decides what the path is rooted at.
///
/// `None` when the expression is unsupported, or does not start with a field.
pub fn root(expression: &str) -> Option<String> {
    match parse(expression)?.into_iter().next()? {
        Segment::Field(name) => Some(name),
        _ => None,
    }
}

#[derive(Debug, PartialEq)]
enum Segment {
    Field(String),
    Index(usize),
    /// `[*]`: every element of an array, or every value of an object.
    Wildcard,
    /// `[?(@.field=="value")]`: the elements whose field equals a literal.
    Filter {
        field: String,
        value: String,
    },
}

impl Segment {
    fn select<'a>(&self, node: &'a Value, out: &mut Vec<&'a Value>) {
        match self {
            Self::Field(name) => out.extend(node.get(name)),
            Self::Index(index) => out.extend(node.get(index)),
            Self::Wildcard => match node {
                Value::Array(items) => out.extend(items),
                Value::Object(fields) => out.extend(fields.values()),
                _ => {}
            },
            Self::Filter { field, value } => {
                let Value::Array(items) = node else { return };
                out.extend(
                    items
                        .iter()
                        .filter(|item| item.get(field).is_some_and(|found| matches(found, value))),
                );
            }
        }
    }
}

/// Compares a selected node against a filter's literal.
///
/// The literal is always text in the expression, so `status == "True"` and
/// `replicas == 3` both have to work against their JSON types.
fn matches(node: &Value, literal: &str) -> bool {
    match node {
        Value::String(text) => text == literal,
        Value::Bool(value) => value.to_string() == literal,
        Value::Number(value) => value.to_string() == literal,
        _ => false,
    }
}

/// Parses an expression, or returns `None` if it uses anything unsupported.
fn parse(expression: &str) -> Option<Vec<Segment>> {
    let expression = expression
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .trim()
        .trim_start_matches('$');

    // `..` is recursive descent, which nothing here can answer.
    if expression.is_empty() || expression.contains("..") {
        return None;
    }

    let mut segments = Vec::new();
    let mut rest = expression;

    while !rest.is_empty() {
        rest = rest.strip_prefix('.').unwrap_or(rest);

        // A field name runs until the next `.` or `[`.
        let name_end = rest.find(['.', '[']).unwrap_or(rest.len());
        if name_end > 0 {
            segments.push(Segment::Field(rest[..name_end].to_string()));
            rest = &rest[name_end..];
            continue;
        }

        if let Some(after) = rest.strip_prefix('[') {
            let close = after.find(']')?;
            segments.push(parse_bracket(&after[..close])?);
            rest = &after[close + 1..];
            continue;
        }

        // A `.` that led nowhere: an empty segment, which is malformed.
        return None;
    }

    (!segments.is_empty()).then_some(segments)
}

fn parse_bracket(inner: &str) -> Option<Segment> {
    if inner == "*" {
        return Some(Segment::Wildcard);
    }

    if let Some(condition) = inner.strip_prefix('?') {
        // `?(@.type=="Accepted")`, and the same without the parentheses or the
        // quotes, both of which appear in the wild.
        let condition = condition
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .trim();
        let (field, value) = condition.split_once("==")?;
        let field = field.trim().strip_prefix("@.")?.trim();
        let value = value.trim().trim_matches(['"', '\'']);

        // Only a direct field of the element; `@.a.b` would need another level
        // of selection and does not occur in printer columns.
        if field.is_empty() || field.contains(['.', '[']) {
            return None;
        }

        return Some(Segment::Filter {
            field: field.to_string(),
            value: value.to_string(),
        });
    }

    inner.trim().parse().ok().map(Segment::Index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object() -> Value {
        json!({
            "metadata": { "name": "web" },
            "spec": { "replicas": 3, "template": { "name": "web" } },
            "status": {
                "phase": "Running",
                "conditions": [
                    { "type": "Accepted", "status": "True" },
                    { "type": "Programmed", "status": "False" }
                ],
                "addresses": [{ "value": "10.0.0.1" }, { "value": "10.0.0.2" }]
            }
        })
    }

    fn one(expression: &str) -> Option<Value> {
        let object = object();
        let found = evaluate(expression, &object);
        assert!(found.len() <= 1, "expected at most one match: {found:?}");
        found.first().map(|value| (*value).clone())
    }

    fn all(expression: &str) -> Vec<Value> {
        let object = object();
        evaluate(expression, &object).into_iter().cloned().collect()
    }

    #[test]
    fn reads_a_dotted_path() {
        assert_eq!(one(".spec.replicas"), Some(json!(3)));
        assert_eq!(one(".spec.template.name"), Some(json!("web")));
    }

    /// CRDs are written with and without the leading dot, and sometimes wrapped
    /// in braces borrowed from `kubectl -o jsonpath`.
    #[test]
    fn accepts_the_spellings_crds_use() {
        for expression in [
            ".status.phase",
            "status.phase",
            "{.status.phase}",
            "$.status.phase",
            " .status.phase ",
        ] {
            assert_eq!(
                one(expression),
                Some(json!("Running")),
                "for {expression:?}"
            );
        }
    }

    #[test]
    fn reads_an_index() {
        assert_eq!(one(".status.conditions[0].type"), Some(json!("Accepted")));
    }

    /// The idiom every condition-bearing CRD uses for its most important column.
    #[test]
    fn selects_by_a_condition_filter() {
        assert_eq!(
            one(r#".status.conditions[?(@.type=="Accepted")].status"#),
            Some(json!("True"))
        );
        assert_eq!(
            one(r#".status.conditions[?(@.type=="Programmed")].status"#),
            Some(json!("False"))
        );
    }

    /// Single quotes and a missing pair of parentheses both occur in real CRDs.
    #[test]
    fn a_filter_tolerates_the_variations_crds_are_written_in() {
        for expression in [
            r#".status.conditions[?(@.type=="Accepted")].status"#,
            r#".status.conditions[?(@.type=='Accepted')].status"#,
            r#".status.conditions[? @.type=="Accepted" ].status"#,
        ] {
            assert_eq!(one(expression), Some(json!("True")), "for {expression:?}");
        }
    }

    #[test]
    fn a_filter_that_matches_nothing_selects_nothing() {
        assert!(all(r#".status.conditions[?(@.type=="Ready")].status"#).is_empty());
    }

    /// JSONPath is multi-valued, and a column that selects several nodes shows
    /// all of them.
    #[test]
    fn a_wildcard_selects_every_element() {
        assert_eq!(
            all(".status.addresses[*].value"),
            [json!("10.0.0.1"), json!("10.0.0.2")]
        );
    }

    #[test]
    fn a_path_that_does_not_exist_selects_nothing() {
        assert!(all(".spec.missing").is_empty());
        assert!(all(".status.conditions[9]").is_empty());
        assert!(all(".spec.replicas.nested").is_empty());
    }

    /// Outside the subset the answer is "nothing", never a guess.
    #[test]
    fn unsupported_expressions_select_nothing() {
        for expression in [
            "..name",
            ".status.conditions[?(@.type!=\"Ready\")].status",
            ".status.conditions[?(@.nested.type==\"Ready\")]",
            "",
            "{}",
            ".spec..replicas",
        ] {
            assert!(all(expression).is_empty(), "for {expression:?}");
        }
    }

    #[test]
    fn root_names_what_the_path_starts_at() {
        assert_eq!(root(".metadata.labels.app").as_deref(), Some("metadata"));
        assert_eq!(root("status.phase").as_deref(), Some("status"));
        assert_eq!(
            root(".status.addresses[*].value").as_deref(),
            Some("status")
        );
        assert_eq!(root("[0].name"), None);
    }

    /// Filters compare against text, but the field they read may be a number or
    /// a bool.
    #[test]
    fn a_filter_compares_across_json_types() {
        let object = json!({ "items": [
            { "port": 80, "name": "http" },
            { "port": 443, "name": "https" },
            { "ready": true, "name": "flagged" }
        ]});

        let found = evaluate(r#".items[?(@.port==443)].name"#, &object);
        assert_eq!(found, [&json!("https")]);

        let found = evaluate(r#".items[?(@.ready==true)].name"#, &object);
        assert_eq!(found, [&json!("flagged")]);
    }
}
