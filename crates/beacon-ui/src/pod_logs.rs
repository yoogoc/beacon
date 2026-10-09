//! Incremental search results for the bounded Pod log stream.
use beacon_kube::LogBuffer;
use gpui_kit::SharedString;
use std::{collections::VecDeque, ops::Range};

pub(crate) struct Row {
    pub id: usize,
    pub text: SharedString,
    pub matches: Vec<Range<usize>>,
}

#[derive(Default)]
pub(crate) struct Display {
    pub rows: VecDeque<Row>,
    query: String,
    seen: usize,
}

pub(crate) struct Change {
    pub reset: bool,
    pub removed: usize,
    pub added: usize,
}

impl Display {
    pub fn update(&mut self, logs: &LogBuffer, query: &str) -> Change {
        let total = logs.dropped() + logs.len();
        let reset = query != self.query || total < self.seen;
        if reset {
            self.rows.clear();
            self.seen = 0;
            self.query = query.to_owned();
        }
        let old_count = self.rows.len();
        while self.rows.front().is_some_and(|row| row.id < logs.dropped()) {
            self.rows.pop_front();
        }
        let removed = old_count - self.rows.len();
        let retained = self.rows.len();
        for (index, text) in logs
            .lines()
            .enumerate()
            .skip(self.seen.saturating_sub(logs.dropped()))
        {
            let matches = matches(text, &self.query);
            if self.query.is_empty() || !matches.is_empty() {
                self.rows.push_back(Row {
                    id: logs.dropped() + index,
                    text: text.to_owned().into(),
                    matches,
                });
            }
        }
        self.seen = total;
        Change {
            reset,
            removed,
            added: self.rows.len() - retained,
        }
    }

    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(|row| row.text.as_ref())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Literal, case-insensitive Unicode search. Lowercasing can expand a character
/// (İ → i + combining dot), so map matches back to original UTF-8 boundaries.
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
