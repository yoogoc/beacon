//! Recovering the user's real `PATH`.
//!
//! When Beacon is launched from Finder, GNOME or a `.desktop` entry, its `PATH`
//! is the bare system default -- it does not contain `/opt/homebrew/bin`,
//! `~/.local/bin`, `~/.asdf/shims` or anything else a login shell sets up.
//! kubeconfig `exec` credential plugins (`aws`, `gke-gcloud-auth-plugin`,
//! `kubelogin`) then fail with a "no such file or directory" that points at
//! nothing the user recognises.
//!
//! So we ask the user's shell what `PATH` should be, exactly like Zed and
//! VS Code do, and merge it into our own environment before any connection is
//! attempted.
//!
//! The shell has to be started **interactive as well as login**, which is not
//! obvious and is easy to get wrong in a way that tests clean. `zsh -lc` reads
//! `.zshenv`, `.zprofile` and `.zlogin` but *not* `.zshrc`, and `.zshrc` is
//! where `brew shellenv`, mise, asdf, nvm, pnpm and krew usually end up. Worse,
//! probing `zsh -lc` from a terminal looks perfectly healthy: the terminal's
//! own interactive shell already had those entries and the child simply
//! inherits them. The gap only appears from Finder, where there is nothing to
//! inherit -- which is the one case this module exists for.
//!
//! Being interactive means the rc files may print things, so the answer is
//! delimited by a marker and everything around it is thrown away.

use std::{
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

/// How long we are willing to wait for one shell. A pathological profile
/// (a network call in `.zshrc`) must not hold up the whole application.
///
/// Five rather than three because an interactive shell does more: this machine
/// takes ~0.5s, but a profile with nvm or conda in it is routinely seconds.
/// The budget is per attempt, and there are at most two.
const SHELL_TIMEOUT: Duration = Duration::from_secs(5);

/// Wraps the answer so that whatever an rc file printed can be discarded.
const MARKER: &str = "__beacon_path_7f3a__";

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

/// Asks `$SHELL` what `PATH` is.
///
/// Interactive *and* login first, for the reason in the module comment. The
/// login-only attempt is the fallback, and it is worth having twice over: a
/// shell that rejects `-i` without a terminal fails immediately and costs
/// nothing, and a `.zshrc` slow enough to time out is a file the fallback does
/// not read at all -- so the second attempt tends to succeed exactly when the
/// first one could not.
fn query_login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").ok()?;
    if shell.is_empty() {
        return None;
    }

    ask(&shell, &["-i", "-l", "-c"]).or_else(|| ask(&shell, &["-l", "-c"]))
}

/// Runs one shell with the given flags and reads the marked answer out of it.
///
/// The flags are passed separately rather than clustered as `-ilc`: bash and
/// zsh accept either, fish only accepts them apart.
fn ask(shell: &str, flags: &[&str]) -> Option<String> {
    let shell = shell.to_string();
    let arguments: Vec<String> = flags.iter().map(|flag| flag.to_string()).collect();
    let script = format!("printf '%s%s%s' '{MARKER}' \"$PATH\" '{MARKER}'");

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let output = Command::new(&shell)
            .args(&arguments)
            .arg(&script)
            // Without this an rc file that reads from stdin waits forever --
            // the timeout would cover it, but only by spending the whole budget.
            .stdin(Stdio::null())
            .output();
        let _ = tx.send(output);
    });

    match rx.recv_timeout(SHELL_TIMEOUT) {
        // The status is deliberately not checked. An interactive shell can
        // exit non-zero for reasons that have nothing to do with us -- the
        // last command in an rc file, a `compinit` complaint -- and if the
        // markers are there, the answer between them is still the answer.
        Ok(Ok(output)) => extract(&String::from_utf8_lossy(&output.stdout)),
        Ok(Err(err)) => {
            tracing::warn!(?flags, %err, "could not run the shell");
            None
        }
        Err(_) => {
            tracing::warn!(?flags, timeout = ?SHELL_TIMEOUT, "the shell did not answer in time");
            None
        }
    }
}

/// Pulls the answer out from between the two markers.
///
/// An interactive shell's rc files may print anything at all around it -- a
/// banner, an update notice, a fortune -- so the payload is delimited rather
/// than assumed to be the whole of stdout.
fn extract(stdout: &str) -> Option<String> {
    let (_, rest) = stdout.split_once(MARKER)?;
    let (path, _) = rest.split_once(MARKER)?;
    let path = path.trim();
    (!path.is_empty()).then(|| path.to_string())
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
    use super::{MARKER, extract, merge_paths};

    #[test]
    fn the_answer_survives_a_chatty_rc_file() {
        let stdout = format!("Welcome!\nnode: v22\n{MARKER}/opt/homebrew/bin:/usr/bin{MARKER}");
        assert_eq!(extract(&stdout).unwrap(), "/opt/homebrew/bin:/usr/bin");
    }

    #[test]
    fn output_without_markers_is_not_a_path() {
        // A shell that died before running the script prints its own error,
        // and taking that as PATH would be worse than keeping what we have.
        assert!(extract("zsh: can't find terminal definition").is_none());
    }

    #[test]
    fn a_half_written_answer_is_refused() {
        assert!(extract(&format!("{MARKER}/opt/homebrew/bin")).is_none());
    }

    #[test]
    fn an_empty_path_is_not_an_answer() {
        assert!(extract(&format!("{MARKER}{MARKER}")).is_none());
    }

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
