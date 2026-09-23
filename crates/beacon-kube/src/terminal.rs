//! An interactive shell in a container.
//!
//! This is the one place Beacon opens a two-way stream: bytes go down to the
//! container's stdin, bytes come back from its stdout, and a resize channel
//! tells it how big the window is. The API server multiplexes all three over
//! one WebSocket.
//!
//! Everything about *interpreting* those bytes -- the escape sequences, the
//! grid, the colours -- belongs to the UI, because it is a rendering problem.
//! What lives here is the plumbing, which is a stream in one direction and two
//! channels in the other.

use futures::{Sink, SinkExt as _, Stream, channel::mpsc};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api,
    api::{AttachParams, TerminalSize},
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// What the container wrote, and what became of the session.
#[derive(Debug)]
pub enum TerminalEvent {
    Output(Vec<u8>),
    /// The shell exited, which ends the session.
    Closed,
    Failed(String),
}

/// The writing end of a session.
///
/// Cloneable and cheap: it is two channel senders, and a send never blocks the
/// thread that types.
#[derive(Clone)]
pub struct Terminal {
    input: mpsc::UnboundedSender<Vec<u8>>,
    resize: mpsc::UnboundedSender<(u16, u16)>,
}

impl Terminal {
    /// Sends keystrokes, already encoded.
    ///
    /// Encoding is the UI's job: what a key means depends on the terminal's
    /// current modes, which is state the grid owns.
    pub fn send(&self, bytes: Vec<u8>) {
        let _ = self.input.unbounded_send(bytes);
    }

    /// Tells the container how big the window is.
    ///
    /// Without this a shell wraps at eighty columns whatever the pane is, and
    /// anything full-screen draws itself in the wrong place.
    pub fn resize(&self, columns: u16, rows: u16) {
        let _ = self.resize.unbounded_send((columns, rows));
    }
}

/// The command a shell session starts with.
///
/// Tried in order, because a container may have neither: distroless images
/// have no shell at all, and `bash` is far from universal.
pub const SHELLS: [&str; 3] = ["/bin/bash", "/bin/sh", "sh"];

/// Opens an interactive session.
///
/// The returned stream is a channel, safe to poll from the foreground thread;
/// dropping it ends the session and closes the WebSocket.
pub fn attach(
    client: kube::Client,
    runtime: &tokio::runtime::Handle,
    namespace: String,
    pod: String,
    container: Option<String>,
    command: Vec<String>,
) -> (Terminal, impl Stream<Item = TerminalEvent> + use<>) {
    let (input_tx, mut input_rx) = mpsc::unbounded::<Vec<u8>>();
    let (resize_tx, mut resize_rx) = mpsc::unbounded::<(u16, u16)>();
    let (events_tx, events_rx) = mpsc::unbounded();

    runtime.spawn(async move {
        let api: Api<Pod> = Api::namespaced(client, &namespace);

        let mut params = AttachParams::interactive_tty();
        if let Some(container) = container {
            params = params.container(container);
        }

        let mut process = match api.exec(&pod, command, &params).await {
            Ok(process) => process,
            Err(error) => {
                let _ = events_tx.unbounded_send(TerminalEvent::Failed(error.to_string()));
                return;
            }
        };

        let (Some(mut stdin), Some(mut stdout)) = (process.stdin(), process.stdout()) else {
            let _ = events_tx.unbounded_send(TerminalEvent::Failed(
                "the API server did not open a terminal".to_string(),
            ));
            return;
        };
        let mut sizes = process.terminal_size();
        // The exec status is the only place a shell that could not *start* is
        // reported. Without it a distroless container -- which has no shell at
        // all -- produces a pane that opens and closes with nothing said.
        let status = process.take_status();

        let mut chunk = [0u8; 8192];
        loop {
            tokio::select! {
                read = stdout.read(&mut chunk) => match read {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if events_tx
                            .unbounded_send(TerminalEvent::Output(chunk[..read].to_vec()))
                            .is_err()
                        {
                            // The pane is gone. Dropping everything here closes
                            // the WebSocket, which is what ends the shell.
                            return;
                        }
                    }
                },
                typed = futures::StreamExt::next(&mut input_rx) => match typed {
                    Some(bytes) => {
                        if stdin.write_all(&bytes).await.is_err() {
                            break;
                        }
                        let _ = stdin.flush().await;
                    }
                    None => break,
                },
                size = futures::StreamExt::next(&mut resize_rx) => {
                    if let (Some((columns, rows)), Some(sizes)) = (size, sizes.as_mut()) {
                        let _ = send_size(sizes, columns, rows).await;
                    }
                }
            }
        }

        let failure = match status {
            Some(status) => status.await.and_then(failure_message),
            None => None,
        };

        let _ = events_tx.unbounded_send(match failure {
            Some(failure) => TerminalEvent::Failed(failure),
            None => TerminalEvent::Closed,
        });
    });

    (
        Terminal {
            input: input_tx,
            resize: resize_tx,
        },
        events_rx,
    )
}

/// The message from a failed exec, or `None` when the shell simply exited.
///
/// A shell that ran and exited non-zero is an ordinary ending -- `exit 1` is
/// not an error to report -- so only the messages about *starting* are kept.
fn failure_message(
    status: k8s_openapi::apimachinery::pkg::apis::meta::v1::Status,
) -> Option<String> {
    if status.status.as_deref() != Some("Failure") {
        return None;
    }
    if status.reason.as_deref() == Some("NonZeroExitCode") {
        return None;
    }
    status.message.filter(|message| !message.is_empty())
}

async fn send_size<S>(sizes: &mut S, columns: u16, rows: u16) -> Result<(), S::Error>
where
    S: Sink<TerminalSize> + Unpin,
{
    sizes
        .send(TerminalSize {
            width: columns,
            height: rows,
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session that nobody is reading has to end, or the WebSocket and the
    /// shell behind it live until the process does.
    #[tokio::test]
    async fn dropping_the_stream_closes_the_writing_end() {
        let (input, mut rx) = mpsc::unbounded::<Vec<u8>>();
        let (resize, _resize_rx) = mpsc::unbounded::<(u16, u16)>();
        let terminal = Terminal { input, resize };

        terminal.send(b"ls\n".to_vec());
        assert_eq!(
            futures::StreamExt::next(&mut rx).await,
            Some(b"ls\n".to_vec())
        );

        drop(rx);
        // A send after the reader is gone is a no-op rather than a panic: the
        // pane may type one more key while the session is tearing down.
        terminal.send(b"x".to_vec());
    }

    #[tokio::test]
    async fn a_resize_carries_both_dimensions() {
        let (input, _input_rx) = mpsc::unbounded::<Vec<u8>>();
        let (resize, mut rx) = mpsc::unbounded::<(u16, u16)>();
        let terminal = Terminal { input, resize };

        terminal.resize(120, 40);
        assert_eq!(futures::StreamExt::next(&mut rx).await, Some((120, 40)));
    }

    /// A container with no shell is the case this exists for: the session
    /// opens, closes immediately, and the only explanation is on the status
    /// channel.
    #[test]
    fn a_shell_that_could_not_start_is_reported() {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;

        let failed = Status {
            status: Some("Failure".into()),
            reason: Some("InternalError".into()),
            message: Some("exec: \"/bin/sh\": stat /bin/sh: no such file or directory".into()),
            ..Default::default()
        };
        assert!(failure_message(failed).is_some());

        // A shell that ran and exited is not a failure to report.
        let exited = Status {
            status: Some("Failure".into()),
            reason: Some("NonZeroExitCode".into()),
            message: Some("command terminated with non-zero exit code".into()),
            ..Default::default()
        };
        assert!(failure_message(exited).is_none());
    }

    /// Distroless images have no shell at all, so the list has to be tried
    /// rather than assumed.
    #[test]
    fn the_shell_list_starts_with_the_nicest_one() {
        assert_eq!(SHELLS[0], "/bin/bash");
        assert!(SHELLS.contains(&"/bin/sh"));
    }
}
