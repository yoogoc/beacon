//! Parse and match the label selectors used by resource list filters.
//!
//! Matching uses kube's expressions, including the missing-key semantics of
//! `!=` and `notin`. Commas outside value sets combine requirements with AND.

use std::{collections::BTreeMap, fmt};

use kube::core::{Expression, SelectorExt};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LabelSelector(Vec<Expression>);

static EMPTY_LABELS: BTreeMap<String, String> = BTreeMap::new();

impl LabelSelector {
    pub fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        if input.is_empty() {
            return Ok(Self::default());
        }
        let mut parts = Vec::new();
        let mut start = 0;
        let mut in_set = false;
        for (index, c) in input.char_indices() {
            match c {
                '(' if !in_set => in_set = true,
                ')' if in_set => in_set = false,
                '(' | ')' => return Err("Unexpected parenthesis in label selector.".into()),
                ',' if !in_set => {
                    parts.push(&input[start..index]);
                    start = index + 1;
                }
                _ => {}
            }
        }
        if in_set {
            return Err("Close the value set with a ')' before applying.".into());
        }
        parts.push(&input[start..]);
        parts
            .into_iter()
            .map(parse_requirement)
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    pub fn matches(&self, labels: Option<&BTreeMap<String, String>>) -> bool {
        let labels = labels.unwrap_or(&EMPTY_LABELS);
        self.0.iter().all(|requirement| requirement.matches(labels))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn has_equality(&self, key: &str, value: &str) -> bool {
        self.0.iter().any(
            |expression| matches!(expression, Expression::Equal(k, v) if k == key && v == value),
        )
    }

    /// Toggle an exact match while retaining any other selector expressions.
    pub fn toggle_equality(&mut self, key: &str, value: &str) -> Result<(), String> {
        validate_key(key)?;
        validate_value(value)?;
        if self.has_equality(key, value) {
            self.0.retain(|expression| {
                !matches!(expression, Expression::Equal(k, v) if k == key && v == value)
            });
        } else {
            self.0.push(Expression::Equal(key.into(), value.into()));
        }
        Ok(())
    }
}

impl fmt::Display for LabelSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, expression) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(",")?;
            }
            write!(f, "{expression}")?;
        }
        Ok(())
    }
}

fn parse_requirement(input: &str) -> Result<Expression, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Each comma-separated label requirement must contain a key.".into());
    }
    if let Some(key) = input.strip_prefix('!') {
        let key = key.trim();
        validate_key(key)?;
        return Ok(Expression::DoesNotExist(key.into()));
    }
    let key_end = input
        .find(|c: char| c.is_whitespace() || matches!(c, '=' | '!' | '(' | ')'))
        .unwrap_or(input.len());
    let key = &input[..key_end];
    validate_key(key)?;
    let rest = input[key_end..].trim();
    if rest.is_empty() {
        return Ok(Expression::Exists(key.into()));
    }
    for operator in ["!=", "==", "="] {
        if let Some(value) = rest.strip_prefix(operator) {
            let value = value.trim();
            validate_value(value)?;
            return Ok(if operator == "!=" {
                Expression::NotEqual(key.into(), value.into())
            } else {
                Expression::Equal(key.into(), value.into())
            });
        }
    }
    for operator in ["notin", "in"] {
        if let Some(values) = rest.strip_prefix(operator) {
            let values = values.trim();
            let Some(values) = values.strip_prefix('(').and_then(|s| s.strip_suffix(')')) else {
                return Err(format!(
                    "Use {key} {operator} (value1,value2) for a value set."
                ));
            };
            let values = values
                .split(',')
                .map(|value| {
                    let value = value.trim();
                    validate_value(value)?;
                    Ok(value.to_string())
                })
                .collect::<Result<_, String>>()?;
            return Ok(if operator == "in" {
                Expression::In(key.into(), values)
            } else {
                Expression::NotIn(key.into(), values)
            });
        }
    }
    Err(format!(
        "Unsupported operator after {key}. Use =, ==, !=, in or notin."
    ))
}

fn validate_key(key: &str) -> Result<(), String> {
    let (prefix, name) = key
        .rsplit_once('/')
        .map_or((None, key), |(p, n)| (Some(p), n));
    if !valid_name(name) || name.is_empty() {
        return Err(format!(
            "Invalid label key {key:?}: use a name of 1–63 alphanumeric, '-', '_' or '.' characters, starting and ending with an alphanumeric character."
        ));
    }
    if let Some(prefix) = prefix
        && (prefix.is_empty()
            || prefix.len() > 253
            || !prefix.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                    && part
                        .as_bytes()
                        .first()
                        .is_some_and(u8::is_ascii_alphanumeric)
                    && part
                        .as_bytes()
                        .last()
                        .is_some_and(u8::is_ascii_alphanumeric)
            }))
    {
        return Err(format!(
            "Invalid label key prefix {prefix:?}: use a lowercase DNS name of at most 253 characters."
        ));
    }
    Ok(())
}

fn valid_name(value: &str) -> bool {
    value.is_empty()
        || (value.len() <= 63
            && value
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && value
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')))
}

fn validate_value(value: &str) -> Result<(), String> {
    if valid_name(value) {
        Ok(())
    } else {
        Err(format!(
            "Invalid label value {value:?}: use at most 63 alphanumeric, '-', '_' or '.' characters, starting and ending with an alphanumeric character, or an empty value."
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::LabelSelector;
    use std::collections::BTreeMap;

    fn labels() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("app".into(), "web".into()),
            ("env".into(), "prod".into()),
            ("empty".into(), "".into()),
            ("app.kubernetes.io/name".into(), "demo".into()),
        ])
    }

    #[test]
    fn equality_sets_and_existence_combine_with_and() {
        for input in [
            "",
            " app = web , env in (prod, qa),!debug ",
            "app==web,env notin (dev)",
            "app.kubernetes.io/name=demo",
            "empty=",
            "empty in ()",
            "missing!=x",
            "missing notin (x)",
        ] {
            assert!(
                LabelSelector::parse(input)
                    .unwrap()
                    .matches(Some(&labels())),
                "{input}"
            );
        }
        for input in [
            "app=Web",
            "app!=web",
            "app in (db,worker)",
            "env notin (prod)",
            "!app",
            "missing",
            "missing=",
            "app=web,env=dev",
        ] {
            assert!(
                !LabelSelector::parse(input)
                    .unwrap()
                    .matches(Some(&labels())),
                "{input}"
            );
        }
    }

    #[test]
    fn unlabeled_resources_follow_kubernetes_negative_match_semantics() {
        for input in ["", "!app", "app!=web", "app notin (web)"] {
            assert!(
                LabelSelector::parse(input).unwrap().matches(None),
                "{input}"
            );
        }
        for input in ["app", "app=", "app=web", "app in (web)"] {
            assert!(
                !LabelSelector::parse(input).unwrap().matches(None),
                "{input}"
            );
        }
    }

    #[test]
    fn rejects_partial_expressions_and_invalid_keys_or_values() {
        for input in [
            ",",
            "app=web,",
            "app=web,,env=prod",
            "app in (web",
            "app in ((web))",
            "app in web",
            "app in (web) trailing",
            "app===web",
            "=web",
            "app!",
            "!app=web",
            "app=with space",
            "UPPER.example/app=web",
            "example..com/app=web",
            "/app=web",
            "example.com/a/b=web",
            "app=中文",
            "app||env",
        ] {
            assert!(LabelSelector::parse(input).is_err(), "{input}");
        }
        assert!(LabelSelector::parse(&format!("{}=web", "a".repeat(64))).is_err());
        assert!(LabelSelector::parse(&format!("app={}", "a".repeat(64))).is_err());
    }

    #[test]
    fn toggling_equalities_preserves_other_requirements_and_round_trips() {
        let mut selector = LabelSelector::parse("env in (qa,prod),app==web").unwrap();
        selector.toggle_equality("app", "web").unwrap();
        assert_eq!(selector.len(), 1);
        selector.toggle_equality("empty", "").unwrap();
        assert!(selector.has_equality("empty", ""));
        assert!(selector.matches(Some(&labels())));
        assert_eq!(
            LabelSelector::parse(&selector.to_string()).unwrap(),
            selector
        );
        let previous = selector.clone();
        assert!(selector.toggle_equality("bad key", "x").is_err());
        assert_eq!(selector, previous);
    }
}
