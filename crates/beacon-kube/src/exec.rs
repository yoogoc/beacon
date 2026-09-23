//! Running a command in a container.
//!
//! One-shot only: a command is sent, its output is collected, and the process
//! ends. That covers the thing people actually reach for — `ls /etc`,
//! `cat a config file`, `env` — without the machinery an interactive session
//! needs, which is a VT parser, a grid, key encoding and resize plumbing.
//!
//! The interactive version is the biggest single piece of work in the project
//! and it is deliberately not here. What is here is the 90% that a read-mostly
//! client is for.

use k8s_openapi::api::core::v1::Pod;
use kube::{Api, api::AttachParams};
use tokio::io::AsyncReadExt as _;

use crate::{Error, Result};

/// How long a command is given before it is abandoned.
///
/// Without this, `cat /dev/zero` or a command waiting on stdin holds a
/// WebSocket open until the window closes.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Beyond this the pane is not showing output any more, it is showing a wall.
const MAX_OUTPUT: usize = 1024 * 1024;

/// What a command produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    /// Set when the command was cut short rather than finishing.
    pub note: Option<String>,
}

impl Output {
    /// Everything, in the order a terminal would have shown it -- which is not
    /// quite true of interleaved streams, but is what a pane can show.
    pub fn combined(&self) -> String {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (false, false) => format!("{}\n{}", self.stdout, self.stderr),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.stdout.is_empty() && self.stderr.is_empty()
    }
}

/// Splits a command line the way a shell would, as far as quoting goes.
///
/// Not a shell: there is no expansion, no pipes and no redirection, because
/// none of that happens here — the argv is handed to the container directly.
/// Quoting is supported because paths have spaces in them.
pub fn split(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut any = false;

    for character in command.chars() {
        match (quote, character) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), c) => current.push(c),
            (None, c @ ('"' | '\'')) => {
                quote = Some(c);
                // `""` is an argument, even though it adds no characters.
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() || any {
                    parts.push(std::mem::take(&mut current));
                    any = false;
                }
            }
            (None, c) => current.push(c),
        }
    }

    if !current.is_empty() || any {
        parts.push(current);
    }
    parts
}

/// Runs a command in a container and collects what it wrote.
pub async fn run(
    client: &kube::Client,
    namespace: &str,
    pod: &str,
    container: Option<&str>,
    command: &[String],
) -> Result<Output> {
    if command.is_empty() {
        return Err(Error::Forward {
            what: "nothing to run".to_string(),
            cause: "the command was empty".to_string(),
        });
    }

    let api: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let mut params = AttachParams::default()
        .stdin(false)
        .stdout(true)
        .stderr(true);
    if let Some(container) = container {
        params = params.container(container);
    }

    let mut process = api.exec(pod, command.to_vec(), &params).await?;

    let stdout = process.stdout();
    let stderr = process.stderr();
    let status = process.take_status();

    let collected = tokio::time::timeout(TIMEOUT, async move {
        let out = read(stdout).await;
        let err = read(stderr).await;

        // The exec status arrives on its own channel, and it is the only place
        // a command that could not *start* is reported. Without it, `ls` in a
        // distroless image looks exactly like a command that printed nothing.
        let failure = match status {
            Some(status) => status.await.and_then(failure_message),
            None => None,
        };

        (out, err, failure)
    })
    .await;

    let mut output = match collected {
        Ok((stdout, stderr, failure)) => Output {
            stdout,
            stderr: match (stderr.is_empty(), failure) {
                (_, None) => stderr,
                (true, Some(failure)) => failure,
                (false, Some(failure)) => format!("{stderr}\n{failure}"),
            },
            note: None,
        },
        Err(_) => {
            process.abort();
            Output {
                note: Some(format!("Stopped after {}s.", TIMEOUT.as_secs())),
                ..Default::default()
            }
        }
    };

    if output.stdout.len() >= MAX_OUTPUT || output.stderr.len() >= MAX_OUTPUT {
        output.note = Some(format!("Output truncated at {} KiB.", MAX_OUTPUT / 1024));
    }

    tracing::info!(%pod, %namespace, command = ?command, "ran a command");
    Ok(output)
}

/// The message from a non-zero exit, or `None` when the command succeeded.
///
/// A command that ran and exited non-zero reports `reason: NonZeroExitCode`
/// and has already written its own diagnosis to stderr, so repeating the
/// status would be noise. A command that never started has nothing else to
/// say, and that message is the whole answer.
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

async fn read(stream: Option<impl tokio::io::AsyncRead + Unpin>) -> String {
    let Some(mut stream) = stream else {
        return String::new();
    };

    let mut collected = Vec::new();
    let mut chunk = [0u8; 8192];

    while collected.len() < MAX_OUTPUT {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => collected.extend_from_slice(&chunk[..read]),
        }
    }

    collected.truncate(MAX_OUTPUT);
    // Container output is whatever the process wrote; it is not required to be
    // UTF-8, and a lossy rendering beats refusing to show it.
    String::from_utf8_lossy(&collected).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_splits_on_whitespace() {
        assert_eq!(split("ls -la /etc"), ["ls", "-la", "/etc"]);
        assert_eq!(split("  env  "), ["env"]);
        assert!(split("").is_empty());
        assert!(split("   ").is_empty());
    }

    /// Paths have spaces in them, so quoting has to work even though this is
    /// not a shell.
    #[test]
    fn quoting_keeps_an_argument_together() {
        assert_eq!(
            split(r#"cat "/etc/my config.yaml""#),
            ["cat", "/etc/my config.yaml"]
        );
        assert_eq!(
            split("sh -c 'echo hello world'"),
            ["sh", "-c", "echo hello world"]
        );
    }

    /// An empty string is an argument. Dropping it silently would change what
    /// the command means.
    #[test]
    fn an_empty_quoted_argument_survives() {
        assert_eq!(split(r#"sh -c "" x"#), ["sh", "-c", "", "x"]);
    }

    #[test]
    fn quotes_inside_a_word_do_not_split_it() {
        assert_eq!(split(r#"echo a"b c"d"#), ["echo", "ab cd"]);
    }

    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;

    fn status(state: &str, reason: Option<&str>, message: &str) -> Status {
        Status {
            status: Some(state.to_string()),
            reason: reason.map(str::to_string),
            message: Some(message.to_string()),
            ..Default::default()
        }
    }

    /// A command that could not start says so only on the status channel.
    /// Without reading it, `ls` in a distroless image looks identical to a
    /// command that ran and printed nothing.
    #[test]
    fn a_command_that_could_not_start_is_reported() {
        let failed = status(
            "Failure",
            Some("InternalError"),
            "exec: \"ls\": executable file not found in $PATH",
        );
        assert!(
            failure_message(failed)
                .expect("a message")
                .contains("executable file not found")
        );
    }

    /// A command that ran and exited non-zero has already written its own
    /// diagnosis to stderr; repeating the status on top of it is noise.
    #[test]
    fn a_non_zero_exit_is_left_to_stderr() {
        assert!(
            failure_message(status(
                "Failure",
                Some("NonZeroExitCode"),
                "command terminated with non-zero exit code"
            ))
            .is_none()
        );
    }

    #[test]
    fn a_successful_command_has_no_failure_message() {
        assert!(failure_message(status("Success", None, "")).is_none());
        assert!(failure_message(Status::default()).is_none());
    }

    #[test]
    fn output_reads_as_one_stream() {
        let only_out = Output {
            stdout: "hello".into(),
            ..Default::default()
        };
        assert_eq!(only_out.combined(), "hello");

        let both = Output {
            stdout: "hello".into(),
            stderr: "oh no".into(),
            note: None,
        };
        assert_eq!(both.combined(), "hello\noh no");

        assert!(Output::default().is_empty());
        assert_eq!(Output::default().combined(), "");
    }
}
