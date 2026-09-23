//! What an Event says, in the two places Beacon shows one.
//!
//! Events are the one built-in kind whose table has no `NAME` column: an
//! event's name is a hash with a timestamp in it, and kubectl does not print
//! it. What matters is when, how bad, why, about what, and the message -- so
//! that is the table, and the same reading is what the detail pane's Events tab
//! renders as a list.
//!
//! The awkward part is "when". An Event carries up to four timestamps
//! depending on its age and on whether it repeated, and reading the wrong one
//! shows a five-day-old time for something that happened a minute ago.

use serde_json::Value;

use crate::{
    CellValue, ColumnDef, ColumnSet, ColumnSource, ColumnWidth, Timestamp, format_duration,
};

/// One event, as both views read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSummary {
    /// How long ago it last happened, already formatted.
    pub last_seen: String,
    /// `Normal` or `Warning`.
    pub kind: String,
    pub reason: String,
    /// `pod/api-7f9`, the way kubectl writes it -- lowercased, because that
    /// is how you would type it back into `kubectl get`.
    pub object: String,
    pub message: String,
    /// How many times it has happened. One is the common case and is not worth
    /// showing; anything else is the point of the row.
    pub count: i64,
}

impl EventSummary {
    pub fn read(data: &Value, now: Timestamp) -> Self {
        Self {
            last_seen: last_seen(data)
                .map(|at| format_duration(now.duration_since(at).as_secs()))
                .unwrap_or_default(),
            kind: text(data, &["type"]).unwrap_or_default(),
            reason: text(data, &["reason"]).unwrap_or_default(),
            object: involved_object(data).unwrap_or_default(),
            message: text(data, &["message"]).unwrap_or_default(),
            count: count(data),
        }
    }

    /// Whether this is something to look at rather than something that
    /// happened.
    pub fn is_warning(&self) -> bool {
        self.kind == "Warning"
    }
}

/// `kubectl get events`, which starts at Last Seen rather than at Name.
pub fn column_set(namespaced: bool) -> ColumnSet {
    let mut columns = vec![ColumnDef::new(
        "Last Seen",
        ColumnWidth::Fixed(88.0),
        ColumnSource::Computed(|cell| {
            CellValue::text(EventSummary::read(cell.data, cell.now).last_seen)
        }),
    )];

    if namespaced {
        columns.push(ColumnDef::new(
            "Namespace",
            ColumnWidth::Flex(1.0),
            ColumnSource::Namespace,
        ));
    }

    columns.extend([
        ColumnDef::new(
            "Type",
            ColumnWidth::Fixed(88.0),
            ColumnSource::Computed(|cell| {
                CellValue::text(EventSummary::read(cell.data, cell.now).kind)
            }),
        ),
        ColumnDef::new(
            "Reason",
            ColumnWidth::Fixed(168.0),
            ColumnSource::Computed(|cell| {
                CellValue::text(EventSummary::read(cell.data, cell.now).reason)
            }),
        ),
        ColumnDef::new(
            "Object",
            ColumnWidth::Flex(1.6),
            ColumnSource::Computed(|cell| {
                CellValue::text(EventSummary::read(cell.data, cell.now).object)
            }),
        ),
        ColumnDef::new(
            "Message",
            ColumnWidth::Flex(4.0),
            ColumnSource::Computed(|cell| {
                CellValue::text(EventSummary::read(cell.data, cell.now).message)
            }),
        ),
    ]);

    ColumnSet { columns }
}

/// The most recent time this event happened.
///
/// Four fields can carry it. `series.lastObservedTime` is set once an event
/// repeats; `lastTimestamp` is the older equivalent; `eventTime` is what the
/// newer API writes for a one-off; `firstTimestamp` is the fallback for an
/// event that happened exactly once under the old API. Reading them in the
/// wrong order shows when a repeating event *started*, which for a pod that
/// has been failing for a week is five days wrong.
fn last_seen(data: &Value) -> Option<Timestamp> {
    let candidates = [
        &["series", "lastObservedTime"][..],
        &["lastTimestamp"][..],
        &["eventTime"][..],
        &["firstTimestamp"][..],
    ];

    candidates
        .iter()
        .filter_map(|path| text(data, path))
        .find_map(|at| at.parse().ok())
}

fn count(data: &Value) -> i64 {
    at(data, &["series", "count"])
        .or_else(|| data.get("count"))
        .and_then(Value::as_i64)
        .unwrap_or(1)
}

/// `pod/api-7f9`, or just the kind when the event names no object.
fn involved_object(data: &Value) -> Option<String> {
    let object = data.get("involvedObject")?;
    let kind = object.get("kind").and_then(Value::as_str)?.to_lowercase();
    match object.get("name").and_then(Value::as_str) {
        Some(name) => Some(format!("{kind}/{name}")),
        None => Some(kind),
    }
}

fn at<'a>(data: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(data, |node, key| node.get(key))
}

fn text(data: &Value, path: &[&str]) -> Option<String> {
    at(data, path)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cell;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use serde_json::json;

    fn now() -> Timestamp {
        "2026-09-23T12:00:00Z".parse().expect("fixed clock")
    }

    fn summary(data: Value) -> EventSummary {
        EventSummary::read(&data, now())
    }

    #[test]
    fn an_event_reads_as_kubectl_prints_it() {
        let summary = summary(json!({
            "type": "Warning",
            "reason": "BackOff",
            "message": "Back-off restarting failed container",
            "involvedObject": { "kind": "Pod", "name": "api-7f9" },
            "lastTimestamp": "2026-09-23T11:55:00Z",
            "count": 12
        }));

        assert_eq!(summary.last_seen, "5m");
        assert_eq!(summary.kind, "Warning");
        assert_eq!(summary.reason, "BackOff");
        assert_eq!(summary.object, "pod/api-7f9");
        assert_eq!(summary.count, 12);
        assert!(summary.is_warning());
    }

    /// A repeating event's `lastTimestamp` can be days behind its series. The
    /// series is what "last seen" means.
    #[test]
    fn a_repeating_event_reports_its_most_recent_occurrence() {
        let summary = summary(json!({
            "firstTimestamp": "2026-09-18T12:00:00Z",
            "lastTimestamp": "2026-09-18T12:00:00Z",
            "series": { "count": 300, "lastObservedTime": "2026-09-23T11:59:00Z" }
        }));

        assert_eq!(summary.last_seen, "60s");
        assert_eq!(summary.count, 300);
    }

    /// The newer API writes `eventTime` and nothing else for a one-off.
    #[test]
    fn a_new_api_event_has_only_an_event_time() {
        let summary = summary(json!({
            "eventTime": "2026-09-23T09:00:00Z",
            "type": "Normal",
            "reason": "Scheduled"
        }));

        assert_eq!(summary.last_seen, "3h");
        assert_eq!(summary.count, 1, "a one-off event has happened once");
    }

    #[test]
    fn an_event_with_no_timestamps_does_not_invent_one() {
        assert_eq!(summary(json!({ "reason": "Started" })).last_seen, "");
    }

    /// Events are the one table that does not start with Name: an event's name
    /// is a hash, and kubectl does not print it either.
    #[test]
    fn the_event_table_starts_at_last_seen() {
        assert_eq!(
            column_set(true).headers(),
            [
                "Last Seen",
                "Namespace",
                "Type",
                "Reason",
                "Object",
                "Message"
            ]
        );
        assert_eq!(
            column_set(false).headers(),
            ["Last Seen", "Type", "Reason", "Object", "Message"]
        );
    }

    #[test]
    fn the_event_table_renders_a_row() {
        let data = json!({
            "type": "Normal",
            "reason": "Pulled",
            "message": "Container image already present on machine",
            "involvedObject": { "kind": "Pod", "name": "api-7f9" },
            "lastTimestamp": "2026-09-23T11:00:00Z"
        });
        let metadata = ObjectMeta::default();
        let cell = Cell {
            metadata: &metadata,
            data: &data,
            now: now(),
            usage: None,
        };

        let values: Vec<String> = column_set(false)
            .columns
            .iter()
            .map(|column| column.resolve(&cell).display().to_string())
            .collect();

        assert_eq!(
            values,
            [
                "60m",
                "Normal",
                "Pulled",
                "pod/api-7f9",
                "Container image already present on machine"
            ]
        );
    }
}
