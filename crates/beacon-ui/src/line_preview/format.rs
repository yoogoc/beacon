use gpui_kit::SharedString;
use std::io::{self, Write};
mod fields;
mod xml;
mod yaml;

pub(super) const LIMIT: usize = 256 * 1024;
const PRETTY_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Format {
    Plain,
    Json,
    Yaml,
    Xml,
    Logfmt,
    KeyValue,
    Query,
}
impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Self::Plain => "Plain text",
            Self::Json => "JSON",
            Self::Yaml => "YAML",
            Self::Xml => "XML",
            Self::Logfmt => "logfmt",
            Self::KeyValue => "Key/value",
            Self::Query => "URL query",
        }
    }
    pub fn language(self) -> &'static str {
        match self {
            Self::Plain => "text",
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Xml => "html",
            Self::Logfmt | Self::KeyValue | Self::Query => "bash",
        }
    }
}

pub(super) struct Formatted {
    pub raw: SharedString,
    pub pretty: SharedString,
    pub format: Format,
    pub notice: Option<&'static str>,
}
impl Formatted {
    pub fn new(raw: SharedString) -> Self {
        let mut result = Self {
            pretty: raw.clone(),
            raw,
            format: Format::Plain,
            notice: None,
        };
        if result.raw.len() > LIMIT {
            result.notice = Some(
                "Line preview limited to 256 KiB. Copy the raw line for its complete contents.",
            );
            return result;
        }
        let payload = payload(&result.raw);
        let parsed = if payload.starts_with('<') {
            xml::format(payload).map(|out| (Format::Xml, out))
        } else if payload.starts_with(['{', '['])
            && serde_json::from_str::<serde_json::Value>(payload).is_ok()
        {
            pretty_json(payload).ok().map(|out| (Format::Json, out))
        } else if fields::query_candidate(payload) {
            fields::query(payload).map(|out| (Format::Query, out))
        } else {
            fields::assignments(payload)
                .or_else(|| yaml::format(payload).map(|out| (Format::Yaml, out)))
        };
        if let Some((format, output)) = parsed {
            result.pretty = String::from_utf8(output)
                .expect("formatted text is UTF-8")
                .into();
            result.format = format;
            if result.pretty.len() > LIMIT {
                result.notice = Some(
                    "Line preview limited to 256 KiB. Copy the raw line for its complete contents.",
                );
            }
        } else if payload.starts_with(['{', '[', '<']) {
            result.notice =
                Some("Invalid or unsupported structured data. Showing the original text.");
        }
        result
    }
    pub fn display(&self, raw: bool) -> SharedString {
        let text = if raw { &self.raw } else { &self.pretty };
        let mut end = text.len().min(LIMIT);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == text.len() {
            text.clone()
        } else {
            text[..end].to_owned().into()
        }
    }
}

// Format the validated token stream rather than serializing Value: key order,
// duplicate keys, string escapes and large numeric literals stay byte-exact.
fn pretty_json(text: &str) -> io::Result<Vec<u8>> {
    let mut compact = Vec::with_capacity(text.len());
    let (mut quoted, mut escaped) = (false, false);
    for byte in text.bytes() {
        if quoted || !byte.is_ascii_whitespace() {
            compact.push(byte);
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
        }
    }
    let mut output = Limited(Vec::new());
    let mut depth = 0;
    quoted = false;
    escaped = false;
    let newline = |out: &mut Limited, depth: usize| -> io::Result<()> {
        out.write_all(b"\n")?;
        for _ in 0..depth {
            out.write_all(b"  ")?;
        }
        Ok(())
    };
    for (index, &byte) in compact.iter().enumerate() {
        if quoted {
            output.write_all(&[byte])?;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => {
                quoted = true;
                output.write_all(&[byte])?;
            }
            b'{' | b'[' => {
                output.write_all(&[byte])?;
                depth += 1;
                if !matches!(compact.get(index + 1), Some(b'}' | b']')) {
                    newline(&mut output, depth)?;
                }
            }
            b'}' | b']' => {
                depth -= 1;
                if !matches!(compact.get(index.wrapping_sub(1)), Some(b'{' | b'[')) {
                    newline(&mut output, depth)?;
                }
                output.write_all(&[byte])?;
            }
            b',' => {
                output.write_all(b",")?;
                newline(&mut output, depth)?;
            }
            b':' => output.write_all(b": ")?,
            _ => output.write_all(&[byte])?,
        }
    }
    Ok(output.0)
}
struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len() + bytes.len() > PRETTY_LIMIT {
            return Err(io::Error::other("formatted preview limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Kubernetes log timestamps and aggregate source labels precede the JSON body.
/// Only recognized prefixes are removed; arbitrary text before braces is kept.
fn payload(raw: &str) -> &str {
    let mut text = raw.trim_start_matches('\u{feff}').trim();
    if text.starts_with('[')
        && let Some((source, rest)) = text.split_once("] ")
        && source.contains('/')
        && source.contains(':')
        && !source.contains(['"', ' ', '\n'])
    {
        text = rest;
    }
    if let Some((stamp, rest)) = text.split_once(' ')
        && stamp.parse::<beacon_columns::Timestamp>().is_ok()
    {
        text = rest.trim_start();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn preserves_raw_bytes_and_formats_only_recognized_json_payloads() {
        for prefix in [
            "",
            "2026-11-04T10:00:00Z ",
            "[demo/web:main] 2026-11-04T10:00:00Z ",
        ] {
            let raw = format!("{prefix}{{\"消息\":\"hi\\nthere\",\"data\":[true,null,42]}}");
            let value = Formatted::new(raw.clone().into());
            assert_eq!(value.format, Format::Json);
            assert_eq!(value.raw.as_ref(), raw);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&value.pretty).unwrap(),
                serde_json::json!({"消息":"hi\nthere","data":[true,null,42]})
            );
            assert!(value.pretty.contains('\n'));
        }
        for raw in ["plain text", "prefix {\"a\":1}", "{\"broken\":", "[1,2,"] {
            let value = Formatted::new(raw.into());
            assert_eq!(value.format, Format::Plain);
            assert_eq!(value.pretty.as_ref(), raw);
        }
    }
    #[::core::prelude::v1::test]
    fn oversized_and_deep_json_fall_back_without_splitting_utf8() {
        let raw = "中".repeat(LIMIT / 3 + 2);
        let value = Formatted::new(raw.clone().into());
        assert!(value.notice.is_some());
        assert!(value.display(true).len() <= LIMIT);
        assert_eq!(value.raw.as_ref(), raw);
        let deep = format!("{}0{}", "[".repeat(150), "]".repeat(150));
        let value = Formatted::new(deep.clone().into());
        assert!(value.format == Format::Plain && value.notice.is_some());
        assert_eq!(value.raw.as_ref(), deep);
    }

    #[::core::prelude::v1::test]
    fn formatting_preserves_order_duplicates_numeric_precision_and_string_escapes() {
        let raw = r#"{ "z":184467440737095516151, "z":1.234567890123456789, "a":[{},[],"braces {} [] \\\"", "\u4e2d"] }"#;
        let value = Formatted::new(raw.into());
        assert_eq!(value.format, Format::Json);
        assert!(value.pretty.contains("184467440737095516151"));
        assert!(value.pretty.contains("1.234567890123456789"));
        assert!(value.pretty.contains(r#""\u4e2d""#));
        assert_eq!(value.pretty.matches("\"z\":").count(), 2);
        assert!(value.pretty.find("\"z\"").unwrap() < value.pretty.find("\"a\"").unwrap());
        assert!(serde_json::from_str::<serde_json::Value>(&value.pretty).is_ok());
    }

    #[::core::prelude::v1::test]
    fn yaml_flow_is_indented_without_changing_scalars_order_or_nested_values() {
        for raw in [
            "{name: api, ports: [80, 443], enabled: true}",
            "name: api",
            "[alpha, beta, 'http://example.test/a:b']",
            "config: {path: '/app/config', env: [dev, prod]}",
        ] {
            let parsed = Formatted::new(raw.into());
            assert_eq!(parsed.format, Format::Yaml, "{raw}");
            assert_eq!(parsed.raw.as_ref(), raw);
            assert_eq!(
                serde_saphyr::from_str::<serde_json::Value>(&parsed.pretty).unwrap(),
                serde_saphyr::from_str::<serde_json::Value>(raw).unwrap()
            );
            if raw.starts_with('{') {
                assert!(parsed.pretty.contains('\n'));
                assert!(parsed.pretty.find("name").unwrap() < parsed.pretty.find("ports").unwrap());
            }
        }
        for raw in ["{x: 'a # b', y: c}", "{x: &v [a, b], y: *v}"] {
            let parsed = Formatted::new(raw.into());
            assert_eq!(parsed.format, Format::Yaml);
            assert_eq!(
                serde_saphyr::from_str::<serde_json::Value>(&parsed.pretty).unwrap(),
                serde_saphyr::from_str::<serde_json::Value>(raw).unwrap()
            );
        }
    }

    #[::core::prelude::v1::test]
    fn ordered_logfmt_and_env_assignments_keep_duplicates_quotes_and_empty_values() {
        for (raw, expected) in [
            (
                r#"level=info msg="启动完成" key=1 key=2 empty= path=/a"#,
                Format::Logfmt,
            ),
            ("export APP_NAME='hello world'", Format::KeyValue),
            ("PORT = 8080 HOST = '0.0.0.0'", Format::KeyValue),
            ("display_name = Hello World", Format::KeyValue),
            ("APP_ENV=production # keep comment", Format::KeyValue),
            ("API_URL=https://example.test?q=x&retry=2", Format::KeyValue),
            ("PASSWORD='x;y=$(echo keep)'", Format::KeyValue),
        ] {
            let parsed = Formatted::new(raw.into());
            assert_eq!(parsed.format, expected, "{raw}");
            assert_eq!(parsed.raw.as_ref(), raw);
            if expected == Format::Logfmt {
                assert!(parsed.pretty.contains("key=1\nkey=2\nempty=\npath=/a"));
                assert_eq!(
                    crate::pod_logs::logfmt(&parsed.pretty).unwrap(),
                    crate::pod_logs::logfmt(raw).unwrap()
                );
            }
        }
        for raw in [
            "plain text",
            "INFO: request started",
            "https://example.test/plain",
            "12:30:45",
            r#"level=info msg="unterminated"#,
            "some prose key=value",
        ] {
            let parsed = Formatted::new(raw.into());
            assert_eq!(parsed.format, Format::Plain, "{raw}");
            assert_eq!(parsed.pretty.as_ref(), raw);
        }
    }

    #[::core::prelude::v1::test]
    fn xml_formats_structure_but_preserves_mixed_content_cdata_and_xml_space() {
        let raw = r#"<?xml version="1.0"?><root a="&gt;"><item id="1">中</item><!-- keep --><item id="2"/></root>"#;
        let parsed = Formatted::new(raw.into());
        assert_eq!(parsed.format, Format::Xml);
        assert!(
            parsed
                .pretty
                .contains("\n  <item id=\"1\">中</item>\n  <!-- keep -->")
        );
        assert!(roxmltree::Document::parse(&parsed.pretty).is_ok());
        assert_eq!(parsed.raw.as_ref(), raw);
        for raw in [
            "<p>Hello <b>world</b> !</p>",
            "<root xml:space=\"preserve\"><x/><y/></root>",
            "<root><![CDATA[ ]]><x/></root>",
        ] {
            let parsed = Formatted::new(raw.into());
            assert_eq!(parsed.format, Format::Xml);
            assert_eq!(parsed.pretty.as_ref(), raw);
        }
        for raw in [
            "<x><y></x>",
            "<!DOCTYPE x [<!ENTITY y SYSTEM 'file:///no-file-read'>]><x>&y;</x>",
        ] {
            let parsed = Formatted::new(raw.into());
            assert_eq!(parsed.format, Format::Plain);
            assert_eq!(parsed.pretty.as_ref(), raw);
        }
    }

    #[::core::prelude::v1::test]
    fn url_queries_decode_for_reading_and_preserve_duplicates_and_raw_credentials() {
        let one = Formatted::new("?q=hello+world".into());
        assert_eq!(one.format, Format::Query);
        assert_eq!(one.pretty.as_ref(), "\"q\" = \"hello world\"");
        for raw in [
            "https://example.test/api?q=hello+world&tag=%E4%B8%AD&tag=b&empty=",
            "q=hello+world&tag=%E4%B8%AD&tag=b&empty=",
        ] {
            let parsed = Formatted::new(raw.into());
            assert_eq!(parsed.format, Format::Query);
            assert!(parsed.pretty.contains("\"q\" = \"hello world\""));
            assert!(parsed.pretty.contains("\"tag\" = \"中\"\n\"tag\" = \"b\""));
            assert!(parsed.pretty.contains("\"empty\" = \"\""));
            assert_eq!(parsed.raw.as_ref(), raw);
        }
        for raw in ["q=x&bad=%QQ", "q=x&bad=%FF", "https://example.test/?q=%QQ"] {
            let parsed = Formatted::new(raw.into());
            assert_eq!(parsed.format, Format::Plain);
            assert_eq!(parsed.pretty.as_ref(), raw);
        }
    }
}
