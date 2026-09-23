//! Recovering the user's real `PATH`.
//!
//! When Beacon is launched from Finder, GNOME or a `.desktop` entry, its `PATH`
//! is the bare system default -- it does not contain `/opt/homebrew/bin`,
//! `~/.local/bin`, `~/.asdf/shims` or anything else a login shell sets up.
//! kubeconfig `exec` credential plugins (`aws`, `gke-gcloud-auth-plugin`,
//! `kubelogin`) then fail with a "no such file or directory" that points at
//! nothing the user recognises.
//!
//! So we ask the login shell what `PATH` should be, exactly like Zed and
//! VS Code do, and merge it into our own environment before any connection is
//! attempted.

use std::{
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

/// How long we are willing to wait for the login shell. A pathological profile
/// (network calls in `.zprofile`) must not hold up the whole application.
const SHELL_TIMEOUT: Duration = Duration::from_secs(3);

/// Merges the login shell's `PATH` into this process's `PATH`.
///
/// Returns the resulting `PATH` when it changed, `None` when there was nothing
/// to do (Windows, no `$SHELL`, shell failed, or nothing new to add).
///
/// # Safety-relevant ordering
///
/// This mutates the process environment, so it **must** be called from `main`
/// before any other thread is spawned -- in particular before the tokio runtime
/// and before GPUI starts.
pub fn merge_login_shell_path() -> Option<String> {
    if cfg!(windows) {
        // Windows processes inherit the full user PATH from the registry
        // regardless of how they are launched.
        return None;
    }

    let shell_path = query_login_shell_path()?;
    let current = std::env::var("PATH").unwrap_or_default();
    let merged = merge_paths(&shell_path, &current)?;

    // SAFETY: documented above -- callers invoke this before starting any
    // other thread, so no concurrent getenv/setenv can race with it.
    unsafe { std::env::set_var("PATH", &merged) };

    tracing::debug!(path = %merged, "merged login shell PATH");
    Some(merged)
}

/// Runs `$SHELL -lc 'printf %s "$PATH"'` with a timeout.
fn query_login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").ok()?;
    if shell.is_empty() {
        return None;
    }

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // `-l` runs the profile files. `-i` is deliberately left out: the
        // interactive rc files are slower and some of them write to stdout.
        let output = Command::new(&shell)
            .args(["-lc", "printf %s \"$PATH\""])
            .stdin(Stdio::null())
            .output();
        let _ = tx.send(output);
    });

    match rx.recv_timeout(SHELL_TIMEOUT) {
        Ok(Ok(output)) if output.status.success() => {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (!path.is_empty()).then_some(path)
        }
        Ok(Ok(output)) => {
            tracing::warn!(status = ?output.status, "login shell exited non-zero, keeping inherited PATH");
            None
        }
        Ok(Err(err)) => {
            tracing::warn!(%err, "could not run login shell, keeping inherited PATH");
            None
        }
        Err(_) => {
            tracing::warn!(
                timeout = ?SHELL_TIMEOUT,
                "login shell did not answer in time, keeping inherited PATH"
            );
            None
        }
    }
}

/// Puts the login shell's entries first, then appends anything the current
/// environment has that the shell did not mention.
///
/// Keeping the extras matters when Beacon *was* started from a terminal that
/// had a project-specific PATH -- we must not throw that away.
///
/// Returns `None` when the merge would be a no-op.
fn merge_paths(shell_path: &str, current: &str) -> Option<String> {
    let mut merged: Vec<&str> = shell_path.split(':').filter(|e| !e.is_empty()).collect();
    let mut added_anything = false;

    for entry in current.split(':').filter(|e| !e.is_empty()) {
        if !merged.contains(&entry) {
            merged.push(entry);
            added_anything = true;
        }
    }

    let result = merged.join(":");
    // No-op if we neither gained shell entries nor kept extras.
    if result == current && !added_anything {
        return None;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::merge_paths;

    #[test]
    fn shell_entries_come_first() {
        let merged = merge_paths("/opt/homebrew/bin:/usr/bin", "/usr/bin:/bin").unwrap();
        assert_eq!(merged, "/opt/homebrew/bin:/usr/bin:/bin");
    }

    #[test]
    fn keeps_entries_the_shell_did_not_report() {
        let merged = merge_paths("/usr/bin", "/usr/bin:/my/project/bin").unwrap();
        assert_eq!(merged, "/usr/bin:/my/project/bin");
    }

    #[test]
    fn does_not_duplicate_entries() {
        let merged = merge_paths("/usr/bin:/bin", "/bin:/usr/bin").unwrap();
        assert_eq!(merged, "/usr/bin:/bin");
    }

    #[test]
    fn identical_paths_are_a_noop() {
        assert!(merge_paths("/usr/bin:/bin", "/usr/bin:/bin").is_none());
    }

    #[test]
    fn ignores_empty_segments() {
        let merged = merge_paths("/usr/bin::", ":/bin:").unwrap();
        assert_eq!(merged, "/usr/bin:/bin");
    }
}
