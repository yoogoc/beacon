//! Detect configuration contents without rewriting them or treating scalar secrets as YAML.
use gpui_kit::component::{Theme, input::TextDecoration};
use serde_json::Value;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Plain,
    Json,
    Yaml,
    Toml,
    Shell,
    Env,
    Html,
    Xml,
    Ini,
    Properties,
    Nginx,
    Pem,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Self::Plain => "Plain text",
            Self::Json => "JSON",
            Self::Yaml => "YAML",
            Self::Toml => "TOML",
            Self::Shell => "Shell",
            Self::Env => "Env",
            Self::Html => "HTML",
            Self::Xml => "XML",
            Self::Ini => "INI",
            Self::Properties => "Properties",
            Self::Nginx => "Nginx",
            Self::Pem => "PEM",
        }
    }
    pub fn language(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Shell | Self::Env => "bash",
            Self::Html | Self::Xml => "html",
            _ => "text",
        }
    }
    fn lexical(self) -> bool {
        matches!(self, Self::Ini | Self::Properties | Self::Nginx | Self::Pem)
    }
}

pub(crate) struct Analysis {
    pub format: Format,
    pub tokens: Vec<(Range<usize>, &'static str)>,
}

pub(crate) fn analyze(key: &str, text: &str) -> Analysis {
    let format = detect(key, text);
    Analysis {
        format,
        tokens: tokens(format, text),
    }
}

fn detect(key: &str, text: &str) -> Format {
    let key = key.to_ascii_lowercase();
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return Format::Plain;
    }
    // Analysis runs in the background. Cover Kubernetes data values while bounding
    // parsing of unusually large local drafts.
    if text.len() <= 2 * 1024 * 1024
        && matches!(text.as_bytes()[0], b'{' | b'[')
        && serde_json::from_str::<Value>(text).is_ok_and(|v| v.is_object() || v.is_array())
    {
        return Format::Json;
    }
    if text.starts_with("-----BEGIN ") && text.contains("-----END ") {
        return Format::Pem;
    }
    if text.starts_with("<?xml") {
        return Format::Xml;
    }
    if text.starts_with('<') && text.contains('>') {
        return if key.ends_with(".xml") {
            Format::Xml
        } else {
            Format::Html
        };
    }
    if text.starts_with("#!") && text.lines().next().is_some_and(|line| line.contains("sh")) {
        return Format::Shell;
    }
    let lines: Vec<_> = text
        .lines()
        .take(200)
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(['#', ';']))
        .collect();
    let assignments: Vec<_> = lines
        .iter()
        .filter_map(|line| {
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (name, value) = line.split_once('=')?;
            let name = name.trim();
            (!name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || "_.-\"'".contains(c)))
            .then_some((name, value))
        })
        .collect();
    let section_count = lines
        .iter()
        .filter(|line| line.starts_with('[') && line.ends_with(']'))
        .count();
    // A section header followed by assignments can be accepted as a YAML flow
    // sequence by permissive parsers; recognize the configuration structure first.
    if !assignments.is_empty()
        && assignments.len() + section_count == lines.len()
        && !matches!(key.as_str(), "password" | "token" | "secret")
    {
        if key.ends_with(".properties") {
            return Format::Properties;
        }
        if key.ends_with(".ini") {
            return Format::Ini;
        }
        if key == ".env"
            || key.ends_with(".env")
            || lines.iter().any(|line| line.starts_with("export "))
        {
            return Format::Env;
        }
        if key.ends_with(".toml") {
            return Format::Toml;
        }
        if lines
            .iter()
            .any(|line| line.starts_with('[') && line.ends_with(']'))
        {
            return if assignments.iter().all(|(_, value)| {
                let value = value.trim();
                value.starts_with(['"', '\'', '[', '{'])
                    || value.parse::<f64>().is_ok()
                    || matches!(value, "true" | "false")
            }) {
                Format::Toml
            } else {
                Format::Ini
            };
        }
        if assignments.iter().all(|(name, _)| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        }) {
            return Format::Env;
        }
        return Format::Properties;
    }
    if text.len() <= 2 * 1024 * 1024
        && serde_saphyr::from_str::<Value>(text).is_ok_and(|v| v.is_object() || v.is_array())
    {
        return Format::Yaml;
    }
    if text.contains(';')
        && lines.iter().any(|line| {
            ["server {", "http {", "events {", "location ", "upstream "]
                .iter()
                .any(|marker| line.starts_with(marker))
        })
    {
        return Format::Nginx;
    }
    // Filename hints also keep a partially edited document highlighted.
    if key.ends_with(".json")
        || key.ends_with(".jsonc")
        || matches!(key.as_str(), ".dockerconfigjson" | ".dockercfg")
    {
        Format::Json
    } else if key.ends_with(".yaml") || key.ends_with(".yml") {
        Format::Yaml
    } else if key.ends_with(".toml") {
        Format::Toml
    } else if key.ends_with(".sh") || key.ends_with(".bash") {
        Format::Shell
    } else if key.ends_with(".html") || key.ends_with(".htm") {
        Format::Html
    } else if key.ends_with(".xml") {
        Format::Xml
    } else if key.ends_with(".ini") {
        Format::Ini
    } else if key.ends_with(".properties") {
        Format::Properties
    } else if key.contains("nginx") && key.ends_with(".conf") {
        Format::Nginx
    } else {
        Format::Plain
    }
}

fn tokens(format: Format, text: &str) -> Vec<(Range<usize>, &'static str)> {
    if !format.lexical() {
        return vec![];
    }
    let mut tokens = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        let start = offset + line.len() - line.trim_start().len();
        if trimmed.starts_with(['#', ';']) {
            tokens.push((start..start + trimmed.len(), "comment"));
        } else if (format == Format::Pem && trimmed.starts_with("-----"))
            || (format == Format::Ini && trimmed.starts_with('[') && trimmed.ends_with(']'))
        {
            tokens.push((start..start + trimmed.len(), "type"));
        } else {
            let key_end = if matches!(format, Format::Ini | Format::Properties) {
                trimmed
                    .find(['=', ':'])
                    .or_else(|| trimmed.find(char::is_whitespace))
            } else if format == Format::Nginx {
                trimmed.find(char::is_whitespace)
            } else {
                None
            };
            if let Some(end) = key_end.filter(|end| *end > 0) {
                tokens.push((start..start + end, "property"));
            }
            let mut index = key_end.map_or(0, |end| {
                end + trimmed[end..].chars().next().unwrap().len_utf8()
            });
            while index < trimmed.len() {
                let c = trimmed[index..].chars().next().unwrap();
                let from = index;
                let role = if c == '#'
                    && (index == 0 || trimmed[..index].ends_with(char::is_whitespace))
                {
                    index = trimmed.len();
                    "comment"
                } else if matches!(c, '"' | '\'') {
                    index += c.len_utf8();
                    let mut escaped = false;
                    while index < trimmed.len() {
                        let next = trimmed[index..].chars().next().unwrap();
                        index += next.len_utf8();
                        if next == c && !escaped {
                            break;
                        }
                        escaped = next == '\\' && !escaped;
                    }
                    "string"
                } else if c.is_ascii_digit() {
                    index += trimmed[index..]
                        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                        .unwrap_or(trimmed.len() - index);
                    "number"
                } else {
                    index += c.len_utf8();
                    continue;
                };
                tokens.push((start + from..start + index, role));
            }
        }
        offset += line.len();
    }
    tokens
}

pub(crate) fn decorations(
    tokens: &[(Range<usize>, &'static str)],
    theme: &Theme,
) -> Vec<TextDecoration> {
    tokens
        .iter()
        .filter_map(|(range, role)| {
            theme
                .highlight_theme
                .style
                .syntax
                .style(role)
                .map(|style| TextDecoration::new(range.clone(), style))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Format, analyze};
    #[test]
    fn content_wins_over_names_but_scalars_and_secrets_remain_plain() {
        for (key, text, expected) in [
            ("config", "{\"enabled\":true}", Format::Json),
            (
                "config.json",
                "services:\n  api:\n    port: 80\n",
                Format::Yaml,
            ),
            ("config", "[server]\nport = 80\n", Format::Toml),
            ("config", "[server]\nhost=localhost\n", Format::Ini),
            ("config", "server {\n listen 80;\n}", Format::Nginx),
            (
                "config",
                "# proxy\nworker_processes auto;\nhttp {\n server {\n listen 80;\n }\n}",
                Format::Nginx,
            ),
            (
                "config",
                "nginx: |\n  server {\n    listen 80;\n  }\n",
                Format::Yaml,
            ),
            ("config", "<?xml version=\"1.0\"?><root/>", Format::Xml),
            ("script", "#!/bin/sh\necho hello", Format::Shell),
            ("config", "PORT=80\nHOST=localhost", Format::Env),
            ("config", "app.name=beacon\n", Format::Properties),
            ("password", "hunter2", Format::Plain),
            ("password", "abc==", Format::Plain),
            ("token", "true", Format::Plain),
            (
                "tls.crt",
                "-----BEGIN CERTIFICATE-----\nYWJj\n-----END CERTIFICATE-----",
                Format::Pem,
            ),
        ] {
            assert_eq!(analyze(key, text).format, expected, "{key}");
        }
    }
    #[test]
    fn lexical_highlights_use_valid_utf8_ranges_without_changing_content() {
        let text = "# 说明\n[服务]\nname=\"应用\"\nport=8080\n";
        let analysis = analyze("config.ini", text);
        assert!(
            analysis
                .tokens
                .iter()
                .any(|(range, role)| *role == "string" && &text[range.clone()] == "\"应用\"")
        );
        assert!(
            analysis
                .tokens
                .iter()
                .all(|(range, _)| text.get(range.clone()).is_some())
        );
        let text = "name\u{2003}\"应用\"\n";
        let analysis = analyze("config.ini", text);
        assert!(
            analysis
                .tokens
                .iter()
                .all(|(range, _)| text.get(range.clone()).is_some())
        );
        assert!(
            analysis
                .tokens
                .iter()
                .any(|(range, role)| *role == "string" && &text[range.clone()] == "\"应用\"")
        );
    }
}
