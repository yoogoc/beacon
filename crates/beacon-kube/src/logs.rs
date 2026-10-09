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

use std::{collections::VecDeque, path::PathBuf};

use futures::{
    AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, Stream, StreamExt as _, channel::mpsc,
};
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

    fn download_params(&self) -> LogParams {
        LogParams {
            follow: false,
            tail_lines: None,
            ..self.to_params()
        }
    }
}

/// Download the available snapshot, independently of the bounded display buffer.
pub async fn download(
    client: kube::Client,
    namespace: String,
    pod: String,
    options: LogOptions,
    path: PathBuf,
) -> Result<u64, String> {
    let api: Api<Pod> = Api::namespaced(client, &namespace);
    let stream = api
        .log_stream(&pod, &options.download_params())
        .await
        .map_err(|error| crate::Error::Api(error).user_message())?;
    save_stream(stream, path).await
}

/// Keep the destination intact until the entire response has been saved.
/// A cancelled or failed download drops the temporary file beside it.
async fn save_stream(mut stream: impl AsyncRead + Unpin, path: PathBuf) -> Result<u64, String> {
    use tokio::io::AsyncWriteExt as _;

    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    let temp = tokio::task::spawn_blocking(move || tempfile::NamedTempFile::new_in(directory))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let (file, temp) = temp.into_parts();
    let mut output = tokio::fs::File::from_std(file);
    let mut buffer = [0; 64 * 1024];
    let mut total = 0;
    loop {
        let count = stream
            .read(&mut buffer)
            .await
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        output
            .write_all(&buffer[..count])
            .await
            .map_err(|error| error.to_string())?;
        total += count as u64;
    }
    output.flush().await.map_err(|error| error.to_string())?;
    output.sync_all().await.map_err(|error| error.to_string())?;
    drop(output);
    // Commit with one rename on the network runtime. No await after it: once
    // the destination has changed, the caller must receive a successful result.
    temp.persist(path).map_err(|error| error.to_string())?;
    Ok(total)
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
    use futures::TryStreamExt as _;

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

    async fn log_server(
        body: String,
        status: &'static str,
    ) -> (kube::Client, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.unwrap());
                assert!(header.len() < 8192);
            }
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
            String::from_utf8(header).unwrap()
        });
        let client = kube::Client::try_from(kube::Config::new(
            format!("http://{address}").parse().unwrap(),
        ))
        .unwrap();
        (client, server)
    }

    #[tokio::test]
    async fn download_requests_the_selected_snapshot_without_a_tail_limit() {
        let text: String = (0..5005)
            .map(|index| format!("line {index} 中文\r\n"))
            .collect();
        let (client, server) = log_server(text.clone(), "200 OK").await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sidecar.log");
        std::fs::write(&path, "older download").unwrap();
        let options = LogOptions {
            container: Some("sidecar".into()),
            previous: true,
            timestamps: true,
        };
        let bytes = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            download(
                client,
                "default".into(),
                "demo".into(),
                options,
                path.clone(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(bytes, text.len() as u64);
        assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        let request = server.await.unwrap();
        let uri = request.split_whitespace().nth(1).unwrap();
        let url = url::Url::parse(&format!("http://mock{uri}")).unwrap();
        assert_eq!(url.path(), "/api/v1/namespaces/default/pods/demo/log");
        let query: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
        assert_eq!(
            query.get("container").map(|value| value.as_ref()),
            Some("sidecar")
        );
        assert_eq!(
            query.get("previous").map(|value| value.as_ref()),
            Some("true")
        );
        assert_eq!(
            query.get("timestamps").map(|value| value.as_ref()),
            Some("true")
        );
        assert!(!query.contains_key("tailLines"));
        assert_ne!(
            query.get("follow").map(|value| value.as_ref()),
            Some("true")
        );
    }

    #[tokio::test]
    async fn downloads_are_not_truncated_to_the_display_buffer_size() {
        let text = "x\n".repeat(MAX_BYTES / 2 + 100);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("complete.log");
        let bytes = save_stream(futures::io::Cursor::new(text.as_bytes()), path.clone())
            .await
            .unwrap();
        assert_eq!(bytes, text.len() as u64);
        assert_eq!(std::fs::read(path).unwrap(), text.as_bytes());
    }

    #[tokio::test]
    async fn forbidden_log_requests_preserve_the_existing_file() {
        let body = r#"{"apiVersion":"v1","kind":"Status","status":"Failure","message":"pods/log is forbidden","reason":"Forbidden","code":403}"#;
        let (client, server) = log_server(body.into(), "403 Forbidden").await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("keep.log");
        std::fs::write(&path, "existing").unwrap();
        let error = download(
            client,
            "default".into(),
            "demo".into(),
            LogOptions::default(),
            path.clone(),
        )
        .await
        .unwrap_err();
        assert!(
            error.contains("forbidden") && error.contains("403"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "existing");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_broken_response_removes_the_partial_file_and_keeps_the_destination() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("keep.log");
        std::fs::write(&path, "existing").unwrap();
        let stream = futures::stream::iter([
            Ok(b"partial log\n".to_vec()),
            Err(std::io::Error::other("response interrupted")),
        ])
        .into_async_read();
        assert!(save_stream(stream, path.clone()).await.is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "existing");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn cancelling_a_download_removes_its_temporary_file() {
        use std::task::Poll;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("keep.log");
        std::fs::write(&path, "existing").unwrap();
        let (ready, waiting) = tokio::sync::oneshot::channel();
        let mut ready = Some(ready);
        let mut sent = false;
        let stream = futures::stream::poll_fn(move |_| {
            if !sent {
                sent = true;
                Poll::Ready(Some(Ok::<_, std::io::Error>(b"partial log\n".to_vec())))
            } else {
                if let Some(ready) = ready.take() {
                    let _ = ready.send(());
                }
                Poll::Pending
            }
        })
        .into_async_read();
        let downloading = tokio::spawn(save_stream(stream, path.clone()));
        tokio::time::timeout(std::time::Duration::from_secs(10), waiting)
            .await
            .unwrap()
            .unwrap();
        downloading.abort();
        assert!(downloading.await.unwrap_err().is_cancelled());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "existing");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
