//! The part of JSONPath that `additionalPrinterColumns` actually uses.
//!
//! A CRD's printer columns are JSONPath expressions, but in practice they are
//! field paths: `.spec.replicas`, `.status.phase`, `.metadata.labels.app`. This
//! module evaluates exactly that -- dotted names and numeric indices -- and
//! declines everything else.
//!
//! Declining is deliberate. An expression with a filter (`[?(@.type=="Ready")]`)
//! resolves to nothing here and the cell renders as `<none>`, which is what
//! kubectl shows for a path that does not match. A half-implemented filter that
//! returned the wrong element would be worse than an empty cell.

use serde_json::Value;

/// Looks up a field path in an object.
///
/// Accepts the forms a CRD is written in: `.spec.replicas`, `spec.replicas`
/// and `{.spec.replicas}`. Returns `None` for a path that does not resolve, and
/// for any expression outside the supported subset.
pub fn evaluate<'a>(expression: &str, object: &'a Value) -> Option<&'a Value> {
    let mut current = object;
    for segment in segments(expression)? {
        current = match segment {
            Segment::Field(name) => current.get(name)?,
            Segment::Index(index) => current.get(index)?,
        };
    }
    Some(current)
}

/// The first segment of a path, which decides what the path is rooted at.
pub fn root(expression: &str) -> Option<&str> {
    match segments(expression)?.next()? {
        Segment::Field(name) => Some(name),
        Segment::Index(_) => None,
    }
}

enum Segment<'a> {
    Field(&'a str),
    Index(usize),
}

/// Splits a path, or returns `None` if it uses anything we do not support.
fn segments(expression: &str) -> Option<impl Iterator<Item = Segment<'_>>> {
    let expression = expression
        .trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .trim()
        .trim_start_matches('$')
        .trim_start_matches('.');

    if expression.is_empty() {
        return None;
    }

    // `*` is a wildcard, `?` a filter, `..` a recursive descent, `@` the
    // current node inside a filter. None of them are field paths.
    if expression.contains(['*', '?', '@'])
        || expression.contains("..")
        || expression.contains('\'')
    {
        return None;
    }

    Some(
        expression
            .split('.')
            .flat_map(|part| {
                // `containers[0]` is one name followed by one index.
                let (name, rest) = match part.split_once('[') {
                    Some((name, rest)) => (name, Some(rest)),
                    None => (part, None),
                };

                let index = rest
                    .and_then(|rest| rest.strip_suffix(']'))
                    .and_then(|index| index.parse().ok())
                    .map(Segment::Index);

                [(!name.is_empty()).then_some(Segment::Field(name)), index]
            })
            .flatten(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn object() -> Value {
        json!({
            "spec": { "replicas": 3, "template": { "name": "web" } },
            "status": { "phase": "Running", "conditions": [{ "type": "Ready" }] }
        })
    }

    #[test]
    fn reads_a_dotted_path() {
        assert_eq!(evaluate(".spec.replicas", &object()), Some(&json!(3)));
        assert_eq!(
            evaluate(".spec.template.name", &object()),
            Some(&json!("web"))
        );
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
        ] {
            assert_eq!(
                evaluate(expression, &object()),
                Some(&json!("Running")),
                "for {expression:?}"
            );
        }
    }

    #[test]
    fn reads_an_index() {
        assert_eq!(
            evaluate(".status.conditions[0].type", &object()),
            Some(&json!("Ready"))
        );
    }

    #[test]
    fn a_path_that_does_not_exist_resolves_to_nothing() {
        assert_eq!(evaluate(".spec.missing", &object()), None);
        assert_eq!(evaluate(".status.conditions[9]", &object()), None);
    }

    /// Expressions outside the subset must resolve to nothing rather than to
    /// something plausible but wrong.
    #[test]
    fn unsupported_expressions_are_declined() {
        for expression in [
            ".status.conditions[?(@.type==\"Ready\")].status",
            ".spec.containers[*].image",
            "..name",
            "",
            "{}",
        ] {
            assert_eq!(evaluate(expression, &object()), None, "for {expression:?}");
        }
    }

    #[test]
    fn root_names_what_the_path_starts_at() {
        assert_eq!(root(".metadata.labels.app"), Some("metadata"));
        assert_eq!(root("status.phase"), Some("status"));
        assert_eq!(root(".spec.containers[*].image"), None);
    }
}
