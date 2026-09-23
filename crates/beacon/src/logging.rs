//! Logging setup.
//!
//! A desktop application has no terminal to print to when it is launched from
//! Finder or a `.desktop` entry, so the file is the log. Users are asked for
//! it when something goes wrong, which is why it lands in the platform's
//! standard data directory rather than somewhere Beacon invented.

use std::path::PathBuf;

use anyhow::Context as _;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Everything below this is noise unless someone sets `RUST_LOG`. `kube` and
/// `hyper` at debug level are particularly loud once watches are running.
const DEFAULT_FILTER: &str = "beacon=info,beacon_ui=info,beacon_kube=info,warn";

/// Installs the subscriber. The returned guard flushes the file writer, so it
/// must stay alive for the whole of `main`.
pub fn init() -> anyhow::Result<WorkerGuard> {
    let dir = log_dir()?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("could not create the log directory {}", dir.display()))?;

    let appender = tracing_appender::rolling::daily(&dir, "beacon.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_ansi(false).with_writer(writer))
        // Also mirror to stderr when there is a terminal attached, which is how
        // it is run during development.
        .with(cfg!(debug_assertions).then(|| fmt::layer().with_writer(std::io::stderr)))
        .init();

    tracing::info!(dir = %dir.display(), "logging to file");
    Ok(guard)
}

fn log_dir() -> anyhow::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("dev", "beacon", "Beacon")
        .context("could not determine a data directory for this platform")?;
    Ok(dirs.data_local_dir().join("logs"))
}
