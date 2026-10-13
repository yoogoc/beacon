//! Bounded, incremental structured log parsing and filtering shared by Pod and
//! aggregated streams. Raw lines remain intact for inspection and export.
use beacon_columns::Timestamp;
use beacon_kube::LogBuffer;
use gpui_kit::SharedString;
use regex::{Regex, RegexBuilder};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ops::Range,
    sync::Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Trace,
    Debug,
    Info,
    Warning,
    Error,
    Unknown,
}
impl Level {
    pub const ALL: [Self; 6] = [
        Self::Trace,
        Self::Debug,
        Self::Info,
        Self::Warning,
        Self::Error,
        Self::Unknown,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Trace => "Trace",
            Self::Debug => "Debug",
            Self::Info => "Info",
            Self::Warning => "Warning",
            Self::Error => "Error",
            Self::Unknown => "Unknown",
        }
    }
    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "trace" | "10" => Self::Trace,
            "debug" | "20" => Self::Debug,
            "info" | "information" | "notice" | "30" => Self::Info,
            "warn" | "warning" | "40" => Self::Warning,
            "error" | "err" | "fatal" | "critical" | "panic" | "50" | "60" => Self::Error,
            _ => Self::Unknown,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Text,
    Json,
    Logfmt,
}
impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Json => "JSON",
            Self::Logfmt => "logfmt",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Record {
    pub raw: SharedString,
    pub source: Option<String>,
    pub timestamp: Option<Timestamp>,
    pub format: Format,
    pub fields: BTreeMap<String, String>,
    pub level: Level,
}
impl Record {
    pub fn parse(raw: &str) -> Self {
        let mut payload = raw.trim_start();
        let source = if payload.starts_with('[') {
            payload
                .find("] ")
                .filter(|&end| payload[..end].contains('/') && payload[..end].contains(':'))
                .map(|end| {
                    let source = payload[1..end].to_owned();
                    payload = &payload[end + 2..];
                    source
                })
        } else {
            None
        };
        let timestamp = payload.split_once(' ').and_then(|(time, rest)| {
            time.parse::<Timestamp>().ok().inspect(|_| {
                payload = rest;
            })
        });
        let (format, mut fields) =
            if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(payload) {
                let mut fields = BTreeMap::new();
                for (name, value) in object {
                    flatten(&name, &value, &mut fields, 0);
                }
                (Format::Json, fields)
            } else if let Ok(fields) = logfmt(payload) {
                (Format::Logfmt, fields)
            } else {
                (Format::Text, BTreeMap::new())
            };
        let timestamp = timestamp.or_else(|| {
            ["time", "timestamp", "ts", "@timestamp"]
                .iter()
                .find_map(|key| fields.get(*key)?.parse().ok())
        });
        let level = ["level", "severity", "log.level", "severityText"]
            .iter()
            .find_map(|key| fields.get(*key))
            .map(|value| Level::parse(value))
            .unwrap_or_else(|| {
                payload
                    .split(|ch: char| !ch.is_alphanumeric())
                    .take(5)
                    .map(Level::parse)
                    .find(|level| *level != Level::Unknown)
                    .unwrap_or(Level::Unknown)
            });
        if let Some(source) = &source {
            fields.insert("_source".into(), source.clone());
        }
        Self {
            raw: raw.to_owned().into(),
            source,
            timestamp,
            format,
            fields,
            level,
        }
    }
    fn presentation(&self, filter: &Filter) -> SharedString {
        if !filter.compact || self.format == Format::Text {
            return self.raw.clone();
        }
        let mut parts = Vec::new();
        if let Some(source) = &self.source {
            parts.push(format!("[{source}]"));
        }
        if let Some(timestamp) = self.timestamp {
            parts.push(timestamp.to_string());
        }
        let keys: Vec<_> = if let Some(fields) = &filter.fields {
            fields.iter().cloned().collect()
        } else {
            self.fields
                .keys()
                .filter(|key| {
                    !matches!(
                        key.as_str(),
                        "level"
                            | "severity"
                            | "time"
                            | "timestamp"
                            | "ts"
                            | "@timestamp"
                            | "_source"
                    )
                })
                .cloned()
                .collect()
        };
        for key in keys {
            if let Some(value) = self.fields.get(&key) {
                parts.push(format!("{key}={}", serde_json::to_string(value).unwrap()));
            }
        }
        parts.join(" ").into()
    }
}
fn flatten(prefix: &str, value: &Value, fields: &mut BTreeMap<String, String>, depth: usize) {
    match value {
        Value::Object(values) if !values.is_empty() && depth < 12 => {
            for (key, value) in values {
                flatten(&format!("{prefix}.{key}"), value, fields, depth + 1);
            }
        }
        Value::String(value) => {
            fields.insert(prefix.into(), value.clone());
        }
        value => {
            fields.insert(prefix.into(), value.to_string());
        }
    }
}

/// Strict logfmt: quoted values use JSON escapes, malformed lines remain raw.
/// This also powers field filters, so values with spaces need quotes.
pub(crate) fn logfmt(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut rest = text.trim();
    let mut fields = BTreeMap::new();
    while !rest.is_empty() {
        let key;
        if rest.starts_with('"') {
            let (quoted_key, tail) = quoted(rest)?;
            rest = tail
                .strip_prefix('=')
                .ok_or("Use field=value; quote values containing spaces.")?;
            key = quoted_key;
        } else {
            let end = rest
                .find(|ch: char| ch == '=' || ch.is_whitespace())
                .ok_or("Use field=value; quote values containing spaces.")?;
            if end == 0 || rest.as_bytes()[end] != b'=' {
                return Err("Use field=value; quote values containing spaces.".into());
            }
            key = rest[..end].to_owned();
            rest = &rest[end + 1..];
        }
        let value;
        if rest.starts_with('"') {
            let (quoted_value, tail) = quoted(rest)?;
            value = quoted_value;
            rest = tail;
            if rest.chars().next().is_some_and(|ch| !ch.is_whitespace()) {
                return Err("Separate fields with spaces.".into());
            }
        } else {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            value = rest[..end].to_owned();
            rest = &rest[end..];
        }
        fields.insert(key, value);
        rest = rest.trim_start();
    }
    if fields.is_empty() {
        Err("No structured fields.".into())
    } else {
        Ok(fields)
    }
}
fn quoted(rest: &str) -> Result<(String, &str), String> {
    let mut escaped = false;
    for (index, ch) in rest.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            let end = index + 1;
            let value = serde_json::from_str::<String>(&rest[..end])
                .map_err(|_| "Invalid quoted field value.")?;
            return Ok((value, &rest[end..]));
        }
    }
    Err("Unclosed quoted field value.".into())
}
pub(crate) fn predicate_text(predicates: &BTreeMap<String, String>) -> String {
    predicates
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                serde_json::to_string(key).unwrap(),
                serde_json::to_string(value).unwrap()
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Filter {
    pub query: String,
    pub regex: bool,
    pub levels: BTreeSet<String>,
    pub predicates: BTreeMap<String, String>,
    pub fields: Option<BTreeSet<String>>,
    pub compact: bool,
    pub bookmarks: bool,
}
impl Filter {
    fn expression(&self) -> Result<Option<Regex>, String> {
        if !self.regex || self.query.is_empty() {
            return Ok(None);
        }
        RegexBuilder::new(&self.query)
            .case_insensitive(true)
            .size_limit(1024 * 1024)
            .build()
            .map(Some)
            .map_err(|error| format!("Invalid regular expression: {error}"))
    }
    fn includes(&self, record: &Record, regex: Option<&Regex>) -> bool {
        (self.levels.is_empty() || self.levels.contains(record.level.label()))
            && self
                .predicates
                .iter()
                .all(|(key, value)| record.fields.get(key).is_some_and(|actual| actual == value))
            && (self.query.is_empty()
                || regex.map_or_else(
                    || !matches(&record.raw, &self.query).is_empty(),
                    |regex| regex.is_match(&record.raw),
                ))
    }
}

#[derive(Clone)]
pub(crate) struct Row {
    pub id: usize,
    pub text: SharedString,
    pub record: Arc<Record>,
    pub matches: Vec<Range<usize>>,
    pub bookmarked: bool,
}
#[derive(Default)]
pub(crate) struct Display {
    pub rows: VecDeque<Row>,
    records: VecDeque<(usize, usize, Arc<Record>)>, // sequence, session ID, parsed line
    pub bookmarks: BTreeMap<usize, Arc<Record>>,
    pub fields: BTreeMap<String, usize>,
    pub error: Option<String>,
    filter: Filter,
    expression: Option<Regex>,
    seen: usize,
    next_id: usize,
    dirty: bool,
    source: Option<String>,
}
pub(crate) struct Change {
    pub reset: bool,
    pub removed: usize,
    pub added: usize,
}
impl Display {
    pub fn set_source(&mut self, source: String) {
        self.source = Some(source);
    }
    pub fn reset_stream(&mut self) {
        self.records.clear();
        self.fields.clear();
        self.rows.clear();
        self.seen = 0;
        self.dirty = true;
    }
    pub fn toggle_bookmark(&mut self, id: usize) {
        if self.bookmarks.remove(&id).is_none()
            && let Some((_, _, record)) = self.records.iter().find(|(_, row, _)| *row == id)
        {
            self.bookmarks.insert(id, record.clone());
        }
        self.dirty = true;
    }
    pub fn record(&self, id: usize) -> Option<Arc<Record>> {
        self.records
            .iter()
            .find(|(_, row, _)| *row == id)
            .map(|(_, _, record)| record.clone())
            .or_else(|| self.bookmarks.get(&id).cloned())
    }
    #[cfg(test)]
    pub fn update(&mut self, logs: &LogBuffer, query: &str) -> Change {
        self.update_with(
            logs,
            &Filter {
                query: query.into(),
                ..Default::default()
            },
        )
    }
    pub fn update_with(&mut self, logs: &LogBuffer, filter: &Filter) -> Change {
        let total = logs.dropped() + logs.len();
        if total < self.seen {
            self.reset_stream();
        }
        let changed = filter != &self.filter;
        let reset = self.dirty || changed;
        if changed {
            match filter.expression() {
                Ok(expression) => {
                    self.expression = expression;
                    self.error = None;
                }
                Err(error) => {
                    self.expression = None;
                    self.error = Some(error);
                }
            }
        }
        self.dirty = false;
        self.filter = filter.clone();
        while self
            .records
            .front()
            .is_some_and(|(sequence, _, _)| *sequence < logs.dropped())
        {
            let (_, _, record) = self.records.pop_front().unwrap();
            for key in record.fields.keys() {
                if let Some(count) = self.fields.get_mut(key) {
                    *count -= 1;
                    if *count == 0 {
                        self.fields.remove(key);
                    }
                }
            }
        }
        let mut arrivals = Vec::new();
        for (index, text) in logs
            .lines()
            .enumerate()
            .skip(self.seen.saturating_sub(logs.dropped()))
        {
            let mut record = Record::parse(text);
            if record.source.is_none()
                && let Some(source) = &self.source
            {
                record.source = Some(source.clone());
                record.fields.insert("_source".into(), source.clone());
            }
            let record = Arc::new(record);
            for key in record.fields.keys() {
                *self.fields.entry(key.clone()).or_default() += 1;
            }
            let id = self.next_id;
            self.next_id += 1;
            self.records
                .push_back((logs.dropped() + index, id, record.clone()));
            arrivals.push((id, record));
        }
        self.seen = total;
        let expression = self.expression.clone();
        let regex = expression.as_ref();
        let old = self.rows.len();
        if reset {
            self.rows.clear();
        } else {
            while self
                .rows
                .front()
                .is_some_and(|row| self.records.front().is_none_or(|(_, id, _)| row.id < *id))
            {
                self.rows.pop_front();
            }
        }
        let removed = old - self.rows.len();
        let retained = self.rows.len();
        let candidates: Vec<_> = if filter.bookmarks {
            self.bookmarks
                .iter()
                .map(|(id, record)| (*id, record.clone()))
                .collect()
        } else if reset {
            self.records
                .iter()
                .map(|(_, id, record)| (*id, record.clone()))
                .collect()
        } else {
            arrivals
        };
        if filter.bookmarks && !reset {
            self.rows.clear();
        }
        if self.error.is_none() {
            for (id, record) in candidates {
                if filter.includes(&record, regex) {
                    let text = record.presentation(filter);
                    let matches = if let Some(regex) = regex {
                        regex
                            .find_iter(&text)
                            .filter(|found| !found.is_empty())
                            .map(|found| found.range())
                            .collect()
                    } else {
                        matches(&text, &filter.query)
                    };
                    self.rows.push_back(Row {
                        id,
                        text,
                        record,
                        matches,
                        bookmarked: self.bookmarks.contains_key(&id),
                    });
                }
            }
        }
        Change {
            reset: reset || filter.bookmarks,
            removed,
            added: self.rows.len().saturating_sub(retained),
        }
    }
    /// Export always keeps the original lines, independent of field projection.
    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(|row| row.record.raw.as_ref())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Literal, case-insensitive Unicode search; map expanded lowercase characters
/// back to the original UTF-8 boundaries before highlighting.
pub(crate) fn matches(text: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut lower = String::new();
    let mut original = Vec::new();
    for (start, ch) in text.char_indices() {
        let lowered = ch.to_lowercase().collect::<String>();
        original.extend(std::iter::repeat_n(
            start..start + ch.len_utf8(),
            lowered.len(),
        ));
        lower.push_str(&lowered);
    }
    let query = query.to_lowercase();
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (start, matched) in lower.match_indices(&query) {
        let range = original[start].start..original[start + matched.len() - 1].end;
        if let Some(last) = ranges.last_mut()
            && last.end > range.start
        {
            last.end = last.end.max(range.end);
        } else {
            ranges.push(range);
        }
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_matches_keep_original_boundaries_and_literal_punctuation() {
        assert_eq!(matches("İ ERROR 错误 error", "error"), vec![3..8, 16..21]);
        assert_eq!(matches("İ", "i"), vec![0..2]);
        assert_eq!(matches("a.*b A.*B", ".*"), vec![1..3, 6..8]);
        assert_eq!(matches("错误错误", "错误"), vec![0..6, 6..12]);
        assert!(matches("abc", "").is_empty());
    }
    #[test]
    fn json_logfmt_prefixes_nested_fields_and_malformed_lines() {
        let line = "[default/web:app] 2026-10-13T12:00:00.123Z {\"level\":\"fatal\",\"msg\":\"错误\",\"http\":{\"status\":503},\"tags\":[\"api\"]}";
        let record = Record::parse(line);
        assert_eq!(record.format, Format::Json);
        assert_eq!(record.level, Level::Error);
        assert_eq!(record.fields["http.status"], "503");
        assert_eq!(record.fields["tags"], "[\"api\"]");
        assert_eq!(record.raw.as_ref(), line);
        assert_eq!(record.source.as_deref(), Some("default/web:app"));
        assert!(record.timestamp.is_some());
        let record = Record::parse("level=warn msg=\"a quoted \\\"value\\\"\" empty= count=2");
        assert_eq!(record.format, Format::Logfmt);
        assert_eq!(record.fields["msg"], "a quoted \"value\"");
        assert_eq!(record.fields["empty"], "");
        assert_eq!(record.level, Level::Warning);
        assert_eq!(
            Record::parse("level=warn msg=\"unclosed").format,
            Format::Text
        );
    }
    #[test]
    fn field_filters_round_trip_quoted_keys_values_and_unicode() {
        let predicates = BTreeMap::from([
            ("http status".into(), "503".into()),
            ("a=b".into(), "quoted \"value\" 中文".into()),
        ]);
        assert_eq!(logfmt(&predicate_text(&predicates)).unwrap(), predicates);
    }
    #[test]
    fn filters_projection_bookmarks_and_raw_export_are_independent() {
        let mut logs = LogBuffer::new();
        logs.extend(
            [
                "{\"level\":\"error\",\"msg\":\"request failed\",\"status\":503}",
                "level=info msg=ready",
            ]
            .map(str::to_string),
        );
        let mut display = Display::default();
        let filter = Filter {
            query: "request.*failed".into(),
            regex: true,
            compact: true,
            fields: Some(BTreeSet::from(["msg".into()])),
            predicates: BTreeMap::from([("status".into(), "503".into())]),
            levels: BTreeSet::from(["Error".into()]),
            ..Default::default()
        };
        display.update_with(&logs, &filter);
        assert_eq!(display.rows.len(), 1);
        assert!(!display.rows[0].text.contains("status"));
        assert!(display.text().contains("\"status\":503"));
        assert!(!display.rows[0].matches.is_empty());
        display.toggle_bookmark(0);
        display.update_with(&logs, &filter);
        assert!(display.rows[0].bookmarked);
        display.reset_stream();
        logs.clear();
        logs.push("new line".into());
        display.update_with(
            &logs,
            &Filter {
                bookmarks: true,
                ..Default::default()
            },
        );
        assert_eq!(display.rows.len(), 1);
        assert!(display.rows[0].record.raw.contains("failed"));
        display.update_with(
            &logs,
            &Filter {
                query: "[".into(),
                regex: true,
                ..Default::default()
            },
        );
        assert!(display.rows.is_empty());
        assert!(
            display
                .error
                .as_ref()
                .unwrap()
                .contains("regular expression")
        );
    }
    #[test]
    fn search_updates_follow_appends_query_changes_and_ring_eviction() {
        let mut logs = LogBuffer::new();
        let mut display = Display::default();
        logs.extend(["INFO boot".into(), "ERROR first".into()]);
        display.update(&logs, "error");
        assert_eq!(display.text(), "ERROR first");
        logs.extend(["info ready".into(), "error second".into()]);
        let change = display.update(&logs, "error");
        assert!(!change.reset);
        assert_eq!(change.added, 1);
        assert_eq!(display.text(), "ERROR first\nerror second");
        display.update(&logs, "ready");
        assert_eq!(display.text(), "info ready");
        for _ in 0..50_000 {
            logs.push("INFO new".into());
        }
        display.update(&logs, "ready");
        assert!(display.rows.is_empty());
        logs.clear();
        logs.push("ready again".into());
        assert!(display.update(&logs, "ready").reset);
        assert_eq!(display.text(), "ready again");
    }
}
