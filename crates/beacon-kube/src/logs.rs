//! Following a pod's logs.
//!
//! Two things make this different from a watch. The stream is bytes, not
//! objects, so there is nothing to coalesce on except lines. And it is
//! unbounded: a pod that logs a megabyte a second will fill any buffer given
//! long enough, so the buffer has to have a ceiling and has to say when it
//! starts dropping.
//!
//! The ceiling is a ring: oldest lines go first, because for a log the recent
//! end is the one being read. How many were dropped is kept, so the pane can
//! say so rather than silently showing a window into the middle of something.

use std::collections::VecDeque;

use futures::{AsyncBufReadExt as _, Stream, StreamExt as _, channel::mpsc};
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, api::LogParams};

/// The most lines kept. Beyond this, reading is scrolling, not reading.
const MAX_LINES: usize = 50_000;

/// And a byte ceiling, because fifty thousand lines of JSON is not the same
/// amount of memory as fifty thousand lines of `INFO ok`.
const MAX_BYTES: usize = 10 * 1024 * 1024;

/// How long lines accumulate before being sent, matching the watch batching:
/// a chatty pod would otherwise be one channel message and one render per line.
const BATCH_WINDOW: std::time::Duration = std::time::Duration::from_millis(16);

/// Flush early once this many lines are waiting.
const BATCH_LIMIT: usize = 2_000;

/// What to follow.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogOptions {
    /// `None` means the pod's only container, or its default one.
    pub container: Option<String>,
    /// The logs of the previous instance, which is the only place the reason a
    /// container is crash-looping is written down.
    pub previous: bool,
    pub timestamps: bool,
}

impl LogOptions {
    fn to_params(&self) -> LogParams {
        LogParams {
            container: self.container.clone(),
            // Reading logs without following them is reading a snapshot of
            // something that is still happening.
            follow: !self.previous,
            previous: self.previous,
            timestamps: self.timestamps,
            // Enough to see how something got where it is, without pulling a
            // week of history to show the last minute.
            tail_lines: Some(2_000),
            ..Default::default()
        }
    }
}

/// What a subscriber receives.
#[derive(Debug)]
pub enum LogEvent {
    Lines(Vec<String>),
    /// The stream ended. A followed stream ends when the container does.
    Closed,
    Failed(String),
}

/// Lines held for display, oldest dropped first.
#[derive(Debug, Default)]
pub struct LogBuffer {
    lines: VecDeque<String>,
    bytes: usize,
    dropped: usize,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, line: String) {
        self.bytes += line.len();
        self.lines.push_back(line);
        self.trim();
    }

    pub fn extend(&mut self, lines: impl IntoIterator<Item = String>) {
        for line in lines {
            self.push(line);
        }
    }

    fn trim(&mut self) {
        while self.lines.len() > MAX_LINES || (self.bytes > MAX_BYTES && self.lines.len() > 1) {
            if let Some(dropped) = self.lines.pop_front() {
                self.bytes -= dropped.len();
                self.dropped += 1;
            }
        }
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// How many lines fell off the front. Non-zero means what is shown starts
    /// mid-stream, which the pane says out loud.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.bytes = 0;
        self.dropped = 0;
    }

    /// Everything held, for the clipboard or a file.
    pub fn to_text(&self) -> String {
        self.lines
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Follows one container's logs.
///
/// The returned stream is a channel, so it can be polled from the foreground
/// thread; the request itself runs on whichever runtime `spawn` is given.
pub fn follow(
    client: kube::Client,
    namespace: String,
    pod: String,
    options: LogOptions,
    runtime: &tokio::runtime::Handle,
) -> impl Stream<Item = LogEvent> + use<> {
    let (sender, receiver) = mpsc::unbounded();

    runtime.spawn(async move {
        let api: Api<Pod> = Api::namespaced(client, &namespace);

        let stream = match api.log_stream(&pod, &options.to_params()).await {
            Ok(stream) => stream,
            Err(error) => {
                let _ = sender.unbounded_send(LogEvent::Failed(error.to_string()));
                return;
            }
        };

        // `futures`' line reader, not tokio's: kube hands back a
        // `futures::AsyncBufRead`, and the two traits are not interchangeable.
        let lines = stream.lines();
        futures::pin_mut!(lines);

        let mut pending: Vec<String> = Vec::new();
        let mut ticker = tokio::time::interval(BATCH_WINDOW);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            let flush = tokio::select! {
                line = lines.next() => match line {
                    Some(Ok(line)) => {
                        pending.push(line);
                        pending.len() >= BATCH_LIMIT
                    }
                    None => {
                        if !pending.is_empty() {
                            let _ = sender
                                .unbounded_send(LogEvent::Lines(std::mem::take(&mut pending)));
                        }
                        let _ = sender.unbounded_send(LogEvent::Closed);
                        return;
                    }
                    Some(Err(error)) => {
                        let _ = sender.unbounded_send(LogEvent::Failed(error.to_string()));
                        return;
                    }
                },
                _ = ticker.tick(), if !pending.is_empty() => true,
            };

            if flush {
                let batch = std::mem::take(&mut pending);
                // A send failure means the pane is gone, which is the signal
                // to stop reading -- and, because the response body drops with
                // this task, to close the connection.
                if sender.unbounded_send(LogEvent::Lines(batch)).is_err() {
                    return;
                }
            }
        }
    });

    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(index: usize) -> String {
        format!("line {index}")
    }

    #[test]
    fn a_buffer_keeps_what_it_is_given() {
        let mut buffer = LogBuffer::new();
        buffer.extend((0..3).map(line));

        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.dropped(), 0);
        assert_eq!(buffer.to_text(), "line 0\nline 1\nline 2");
    }

    /// The recent end is the one being read, so the old end is what goes.
    #[test]
    fn a_full_buffer_drops_the_oldest_lines() {
        let mut buffer = LogBuffer::new();
        buffer.extend((0..MAX_LINES + 10).map(line));

        assert_eq!(buffer.len(), MAX_LINES);
        assert_eq!(buffer.dropped(), 10);
        assert_eq!(buffer.lines().next(), Some("line 10"));
        assert_eq!(
            buffer.lines().last(),
            Some(format!("line {}", MAX_LINES + 9).as_str())
        );
    }

    /// Fifty thousand lines of JSON is not the same memory as fifty thousand
    /// lines of `ok`, so the byte ceiling bites first for the former.
    #[test]
    fn a_heavy_buffer_is_bounded_by_bytes_not_lines() {
        let mut buffer = LogBuffer::new();
        let fat = "x".repeat(64 * 1024);

        for _ in 0..500 {
            buffer.push(fat.clone());
        }

        assert!(buffer.len() < 500, "trimmed: {} lines", buffer.len());
        assert!(buffer.bytes <= MAX_BYTES + fat.len());
        assert!(buffer.dropped() > 0);
    }

    /// A single line larger than the whole ceiling must still be shown, not
    /// trimmed into nothing.
    #[test]
    fn one_enormous_line_survives() {
        let mut buffer = LogBuffer::new();
        buffer.push("y".repeat(MAX_BYTES * 2));

        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.dropped(), 0);
    }

    #[test]
    fn clearing_forgets_the_drops_too() {
        let mut buffer = LogBuffer::new();
        buffer.extend((0..MAX_LINES + 5).map(line));
        assert!(buffer.dropped() > 0);

        buffer.clear();
        assert!(buffer.is_empty());
        assert_eq!(buffer.dropped(), 0);
    }

    /// Previous logs are a finished thing; following them would hang waiting
    /// for a container that already exited.
    #[test]
    fn previous_logs_are_not_followed() {
        let live = LogOptions::default().to_params();
        assert!(live.follow);
        assert!(!live.previous);

        let previous = LogOptions {
            previous: true,
            ..Default::default()
        }
        .to_params();
        assert!(!previous.follow);
        assert!(previous.previous);
    }

    #[test]
    fn options_reach_the_request() {
        let params = LogOptions {
            container: Some("sidecar".into()),
            previous: false,
            timestamps: true,
        }
        .to_params();

        assert_eq!(params.container.as_deref(), Some("sidecar"));
        assert!(params.timestamps);
        assert!(params.tail_lines.is_some(), "a bounded first screen");
    }
}
