//! Compatibility transport for clusters that accept SPDY exec but reject
//! WebSocket exec. A local PTY is required: with pipes, kubectl disables TTY
//! allocation, so interactive input and terminal resizing stop working.

use std::io::{Read, Write};

use futures::{StreamExt as _, channel::mpsc};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::terminal::TerminalEvent;

/// Keep the exact context of the GUI session; never rely on current-context.
fn command(
    context: &str,
    namespace: &str,
    pod: &str,
    container: Option<&str>,
    argv: &[String],
) -> CommandBuilder {
    let mut command = CommandBuilder::new("kubectl");
    command.args([
        "--context",
        context,
        "--namespace",
        namespace,
        "--request-timeout=30s",
        "exec",
        "-i",
        "-t",
        pod,
    ]);
    if let Some(container) = container {
        command.args(["--container", container]);
    }
    command.arg("--");
    command.args(argv);
    command.env("TERM", "xterm-256color");
    // The native attempt already failed before starting a command. Going
    // straight to SPDY avoids repeating the rejected WebSocket handshake.
    command.env("KUBECTL_REMOTE_COMMAND_WEBSOCKETS", "false");
    command
}

enum Input {
    Bytes(Vec<u8>),
    Resize(u16, u16),
}

enum Event {
    Output(Vec<u8>),
    Finished(Result<portable_pty::ExitStatus, String>),
    Failed(String),
}

/// Cancelling the async task kills kubectl, which releases the blocking PTY
/// reader and waiter too. A detached child must never outlive a closed pane.
struct KillOnDrop(Option<Box<dyn ChildKiller + Send + Sync>>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
        }
    }
}

struct Process {
    _kill: KillOnDrop,
    input: tokio::sync::mpsc::UnboundedSender<Input>,
    events: mpsc::UnboundedReceiver<Event>,
}

fn spawn(command: CommandBuilder, runtime: &tokio::runtime::Handle) -> Result<Process, String> {
    let pair = native_pty_system()
        .openpty(PtySize {
            cols: 100,
            rows: 28,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| format!("Could not open a local terminal: {error}"))?;
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| error.to_string())?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| error.to_string())?;
    let mut child = pair.slave.spawn_command(command).map_err(|error| {
        format!("WebSocket exec was refused and kubectl could not start: {error}. Install kubectl or make it available on Beacon's PATH.")
    })?;
    let kill = KillOnDrop(Some(child.clone_killer()));
    drop(pair.slave);

    let (events_tx, events) = mpsc::unbounded();
    let output_tx = events_tx.clone();
    let (drained_tx, drained_rx) = std::sync::mpsc::channel();
    runtime.spawn_blocking(move || {
        let mut chunk = [0; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    if output_tx
                        .unbounded_send(Event::Output(chunk[..count].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
                // Unix PTYs may signal EOF with EIO when the slave closes.
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    tracing::debug!(%error, "kubectl PTY reader closed");
                    break;
                }
            }
        }
        let _ = drained_tx.send(());
    });

    let exit_tx = events_tx.clone();
    runtime.spawn_blocking(move || {
        let status = child.wait().map_err(|error| error.to_string());
        // Preserve the final error/output before delivering Finished.
        let _ = drained_rx.recv();
        let _ = exit_tx.unbounded_send(Event::Finished(status));
    });

    let (input, input_rx) = tokio::sync::mpsc::unbounded_channel();
    runtime.spawn_blocking(move || write_input(pair.master, writer, input_rx, events_tx));
    Ok(Process {
        _kill: kill,
        input,
        events,
    })
}

fn write_input(
    master: Box<dyn MasterPty + Send>,
    mut writer: Box<dyn Write + Send>,
    mut input: tokio::sync::mpsc::UnboundedReceiver<Input>,
    events: mpsc::UnboundedSender<Event>,
) {
    while let Some(input) = input.blocking_recv() {
        let result = match input {
            Input::Bytes(bytes) => writer
                .write_all(&bytes)
                .and_then(|_| writer.flush())
                .map_err(|error| error.to_string()),
            Input::Resize(cols, rows) => master
                .resize(PtySize {
                    cols,
                    rows,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|error| error.to_string()),
        };
        if let Err(error) = result {
            let _ = events.unbounded_send(Event::Failed(error));
            break;
        }
    }
}

pub(crate) struct Target<'a> {
    pub context: &'a str,
    pub namespace: &'a str,
    pub pod: &'a str,
    pub container: Option<&'a str>,
}

pub(crate) async fn run(target: Target<'_>, argv: &[String]) -> crate::Result<crate::exec::Output> {
    use crate::exec::{MAX_OUTPUT, Output, TIMEOUT, read};
    use std::process::Stdio;
    let failure = |error: std::io::Error| crate::Error::Forward {
        what: "kubectl exec failed".into(),
        cause: error.to_string(),
    };
    let mut command = tokio::process::Command::new("kubectl");
    command.args([
        "--context",
        target.context,
        "--namespace",
        target.namespace,
        "--request-timeout=30s",
        "exec",
        target.pod,
    ]);
    if let Some(container) = target.container {
        command.args(["--container", container]);
    }
    command
        .arg("--")
        .args(argv)
        .env("KUBECTL_REMOTE_COMMAND_WEBSOCKETS", "false")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(failure)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    match tokio::time::timeout(TIMEOUT, async {
        let (stdout, stderr) = tokio::join!(read(stdout), read(stderr));
        let status = child.wait().await;
        (stdout, stderr, status)
    })
    .await
    {
        Ok((stdout, stderr, status)) => {
            let status = status.map_err(failure)?;
            let note = if stdout.len() >= MAX_OUTPUT || stderr.len() >= MAX_OUTPUT {
                Some(format!("Output truncated at {} KiB.", MAX_OUTPUT / 1024))
            } else if !status.success() && stderr.is_empty() {
                Some(format!("kubectl exited with {status}"))
            } else {
                None
            };
            Ok(Output {
                stdout,
                stderr,
                note,
            })
        }
        Err(_) => {
            let _ = child.kill().await;
            Ok(Output {
                note: Some(format!("Stopped after {}s.", TIMEOUT.as_secs())),
                ..Default::default()
            })
        }
    }
}

pub(crate) async fn attach(
    target: Target<'_>,
    argv: &[String],
    input: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    resize: &mut mpsc::UnboundedReceiver<(u16, u16)>,
    events: &mpsc::UnboundedSender<TerminalEvent>,
) -> Result<(), String> {
    let command = command(
        target.context,
        target.namespace,
        target.pod,
        target.container,
        argv,
    );
    let runtime = tokio::runtime::Handle::current();
    let mut process = tokio::task::spawn_blocking(move || spawn(command, &runtime))
        .await
        .map_err(|error| error.to_string())??;
    let startup_timeout = tokio::time::sleep(std::time::Duration::from_secs(40));
    tokio::pin!(startup_timeout);
    let mut connected = false;
    loop {
        tokio::select! {
            _ = &mut startup_timeout, if !connected => {
                return Err("kubectl produced no output within 40s. Check the cluster connection and try again.".into());
            }
            event = process.events.next() => match event {
                Some(Event::Output(bytes)) => {
                    if !connected {
                        connected = true;
                        let _ = events.unbounded_send(TerminalEvent::Connected);
                    }
                    if events.unbounded_send(TerminalEvent::Output(bytes)).is_err() { return Ok(()); }
                }
                Some(Event::Finished(Ok(status))) if status.success() => {
                    process._kill.0 = None;
                    let _ = events.unbounded_send(TerminalEvent::Closed);
                    return Ok(());
                }
                Some(Event::Finished(Ok(status))) => {
                    process._kill.0 = None;
                    return Err(format!("kubectl exited with status {}. See the terminal output for details; this container may not include the requested shell or command.", status.exit_code()));
                }
                Some(Event::Finished(Err(error)) | Event::Failed(error)) => return Err(error),
                None => return Err("The kubectl terminal closed unexpectedly.".into()),
            },
            bytes = input.next() => match bytes {
                Some(bytes) => { let _ = process.input.send(Input::Bytes(bytes)); }
                None => return Ok(()),
            },
            size = resize.next() => match size {
                Some((cols, rows)) => { let _ = process.input.send(Input::Resize(cols, rows)); }
                None => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_pins_the_target_and_preserves_command_arguments() {
        let command = command(
            "web-dev-cluster",
            "team-a",
            "pod-a",
            Some("app"),
            &[
                "sh".into(),
                "-c".into(),
                "printf '$HOME; hello world'".into(),
            ],
        );
        let args: Vec<_> = command
            .get_argv()
            .iter()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            args,
            [
                "kubectl",
                "--context",
                "web-dev-cluster",
                "--namespace",
                "team-a",
                "--request-timeout=30s",
                "exec",
                "-i",
                "-t",
                "pod-a",
                "--container",
                "app",
                "--",
                "sh",
                "-c",
                "printf '$HOME; hello world'"
            ]
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn pty_forwards_input_resize_and_final_output() {
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "printf 'READY\\n'; IFS= read -r line; printf 'GOT:%s\\n' \"$line\"; stty size",
        ]);
        let runtime = tokio::runtime::Handle::current();
        let mut process = tokio::task::spawn_blocking(move || spawn(command, &runtime))
            .await
            .unwrap()
            .unwrap();
        process.input.send(Input::Resize(120, 40)).unwrap();
        let mut output = String::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut sent = false;
            while let Some(event) = process.events.next().await {
                match event {
                    Event::Output(bytes) => {
                        output.push_str(&String::from_utf8_lossy(&bytes));
                        if !sent && output.contains("READY") {
                            process
                                .input
                                .send(Input::Bytes(b"beacon-test\n".to_vec()))
                                .unwrap();
                            sent = true;
                        }
                    }
                    Event::Finished(Ok(status)) => {
                        process._kill.0 = None;
                        assert!(status.success());
                        break;
                    }
                    _ => panic!("PTY failed"),
                }
            }
        })
        .await
        .unwrap();
        assert!(output.contains("GOT:beacon-test"), "{output:?}");
        assert!(output.contains("40 120"), "{output:?}");
    }
}
