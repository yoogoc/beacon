//! A small tail and a wider snapshot overlap at the first server timestamp.
//! The tail remains connected while the independent snapshot is being read.
use super::{BATCH_WINDOW, LogBuffer, LogEvent, LogOptions};
use futures::{AsyncBufRead, AsyncBufReadExt as _, TryStreamExt as _, channel::mpsc};
use k8s_openapi::{api::core::v1::Pod, jiff::Timestamp};
use kube::Api;
use tokio::sync::oneshot;

pub(super) const PREVIEW_LINES: usize = 100;
const HISTORY_BATCH: usize = 500;

pub(super) struct Anchor {
    timestamp: Timestamp,
    first: String,
    /// How many lines at this timestamp are already in the preview.
    count: usize,
}

pub(super) struct Preview {
    anchor: Option<Anchor>,
    seen: usize,
    tx: oneshot::Sender<Anchor>,
}

impl Preview {
    pub(super) fn new(tx: oneshot::Sender<Anchor>) -> Self {
        Self {
            anchor: None,
            seen: 0,
            tx,
        }
    }

    pub(super) fn observe(&mut self, line: &str) -> bool {
        let timestamp = server_timestamp(line);
        if self.seen == 0 {
            self.anchor = timestamp.map(|timestamp| Anchor {
                timestamp,
                first: line.into(),
                count: 1,
            });
        } else if let Some(anchor) = &mut self.anchor
            && anchor.count == self.seen
            && timestamp == Some(anchor.timestamp)
        {
            anchor.count += 1;
        }
        self.seen += 1;
        self.ready()
    }

    pub(super) fn ready(&self) -> bool {
        self.seen == PREVIEW_LINES
    }

    pub(super) fn start_history(self) {
        if let Some(anchor) = self.anchor {
            let _ = self.tx.send(anchor);
        }
    }
}

fn server_timestamp(line: &str) -> Option<Timestamp> {
    line.split_once(' ')?.0.parse().ok()
}

pub(super) fn without_timestamp(mut line: String) -> String {
    if let Some((prefix, _)) = line.split_once(' ')
        && prefix.parse::<Timestamp>().is_ok()
    {
        line.drain(..=prefix.len());
    }
    line
}

pub(super) async fn load_history(
    api: &Api<Pod>,
    pod: &str,
    options: &LogOptions,
    anchor: Anchor,
    sender: &mpsc::UnboundedSender<LogEvent>,
) -> Result<(), String> {
    let mut params = options.to_params();
    let show_timestamps = params.timestamps;
    params.follow = false;
    params.timestamps = true;
    let stream = api
        .log_stream(pod, &params)
        .await
        .map_err(|e| e.to_string())?;
    let mut lines = stream.lines();
    let mut history = LogBuffer::new();
    let mut at_boundary = false;
    while let Some(line) = lines.try_next().await.map_err(|e| e.to_string())? {
        let at_timestamp = server_timestamp(&line) == Some(anchor.timestamp);
        if at_boundary && !at_timestamp {
            break;
        }
        at_boundary = at_timestamp;
        history.push(line);
    }

    // A restarted or rotated container can return a disjoint snapshot. Only
    // splice history when the records at the seam still identify our preview.
    let Some(start) = history.len().checked_sub(anchor.count) else {
        return Ok(());
    };
    if !at_boundary || history.lines.get(start) != Some(&anchor.first) {
        return Ok(());
    }
    history.lines.truncate(start);
    send_history(history, show_timestamps, sender).await;
    Ok(())
}

pub(super) async fn load_range(
    stream: impl AsyncBufRead + Unpin,
    api: &Api<Pod>,
    pod: &str,
    options: &LogOptions,
    until: Timestamp,
    split: Option<Timestamp>,
    sender: &mpsc::UnboundedSender<LogEvent>,
) -> LogEvent {
    let first = range_snapshot(stream, options.since_time.max(split), until, true).await;
    let mut snapshot = match first {
        Ok(snapshot) => snapshot,
        Err(error) => return LogEvent::Failed(error),
    };
    let has_preview = !snapshot.is_empty();
    send_snapshot_tail(&mut snapshot, sender);
    send_history(snapshot, true, sender).await;

    if let Some(split) = split {
        let earlier = async {
            let stream = api
                .log_stream(pod, &options.to_params())
                .await
                .map_err(|e| e.to_string())?;
            range_snapshot(stream, options.since_time, split, false).await
        }
        .await;
        match earlier {
            Ok(mut snapshot) => {
                if !has_preview {
                    send_snapshot_tail(&mut snapshot, sender);
                }
                send_history(snapshot, true, sender).await;
            }
            Err(error) if !has_preview => return LogEvent::Failed(error),
            Err(error) => {
                let _ = sender.unbounded_send(LogEvent::HistoryFailed(error));
            }
        }
    }
    LogEvent::Closed
}

fn send_snapshot_tail(snapshot: &mut LogBuffer, sender: &mpsc::UnboundedSender<LogEvent>) {
    let tail = snapshot
        .lines
        .split_off(snapshot.len().saturating_sub(PREVIEW_LINES));
    if !tail.is_empty() {
        let _ = sender.unbounded_send(LogEvent::Lines(tail.into()));
    }
}

async fn range_snapshot(
    stream: impl AsyncBufRead + Unpin,
    since: Option<Timestamp>,
    until: Timestamp,
    inclusive_end: bool,
) -> Result<LogBuffer, String> {
    let mut lines = stream.lines();
    let mut snapshot = LogBuffer::new();
    while let Some(line) = lines.try_next().await.map_err(|e| e.to_string())? {
        let Some(time) = server_timestamp(&line) else {
            continue;
        };
        if time > until || (!inclusive_end && time == until) {
            break;
        }
        if since.is_none_or(|since| time >= since) {
            snapshot.push(line);
        }
    }
    Ok(snapshot)
}

async fn send_history(
    mut history: LogBuffer,
    show_timestamps: bool,
    sender: &mpsc::UnboundedSender<LogEvent>,
) {
    if history.is_empty() && history.dropped() > 0 {
        let _ = sender.unbounded_send(LogEvent::History {
            lines: Vec::new(),
            discarded: history.dropped(),
        });
    }
    // Give the foreground a frame between chunks, including the first preview.
    let mut tick =
        tokio::time::interval_at(tokio::time::Instant::now() + BATCH_WINDOW, BATCH_WINDOW);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while !history.lines.is_empty() {
        tick.tick().await;
        let start = history.lines.len().saturating_sub(HISTORY_BATCH);
        let batch = history
            .lines
            .drain(start..)
            .map(|line| {
                if show_timestamps {
                    line
                } else {
                    without_timestamp(line)
                }
            })
            .collect();
        if sender
            .unbounded_send(LogEvent::History {
                lines: batch,
                discarded: if start == 0 { history.dropped() } else { 0 },
            })
            .is_err()
        {
            break;
        }
    }
}
