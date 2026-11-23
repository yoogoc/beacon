//! Reading and pruning Beacon's daily log files off the UI thread.

use jiff::{ToSpan as _, civil::Date};

use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

pub(super) const MAX_BYTES: u64 = 1024 * 1024;
const MAX_LINES: usize = 5000;
const RETENTION_DAYS: i64 = 30;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    pub path: PathBuf,
    bytes: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
    Unknown,
}

impl Level {
    fn parse(line: &str) -> Option<Self> {
        let mut parts = line.split_whitespace();
        let timestamp = parts.next()?;
        if timestamp.as_bytes().get(4) != Some(&b'-') || !timestamp.contains('T') {
            return None;
        }
        match parts.next()? {
            "ERROR" => Some(Self::Error),
            "WARN" => Some(Self::Warn),
            "INFO" => Some(Self::Info),
            "DEBUG" => Some(Self::Debug),
            "TRACE" => Some(Self::Trace),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub(super) struct Line {
    pub text: String,
    pub level: Level,
}

pub(super) struct Snapshot {
    pub stamp: Option<Stamp>,
    pub lines: Vec<Line>,
    pub truncated: bool,
}

fn log_date(name: &str) -> Option<Date> {
    let date = name.strip_prefix("beacon.log.")?;
    if date.len() != 10 || date.as_bytes()[4] != b'-' || date.as_bytes()[7] != b'-' {
        return None;
    }
    date.parse().ok()
}

pub(super) fn remove_expired(dir: &Path, today: Date) -> std::io::Result<()> {
    let oldest_kept = today.saturating_sub((RETENTION_DAYS - 1).days());
    for entry in fs::read_dir(dir)? {
        let result = entry.and_then(|entry| {
            let date = entry.file_name().to_str().and_then(log_date);
            if date.is_some_and(|day| day < oldest_kept) && entry.file_type()?.is_file() {
                fs::remove_file(entry.path())?;
            }
            Ok(())
        });
        if let Err(error) = result
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(directory = %dir.display(), %error, "could not prune application log entry");
        }
    }
    Ok(())
}

/// None means unchanged; an empty snapshot means no log file exists yet.
pub(super) fn read_latest(
    dir: &Path,
    previous: Option<&Stamp>,
) -> Result<Option<Snapshot>, String> {
    let read = || -> std::io::Result<Option<Snapshot>> {
        let files = match fs::read_dir(dir) {
            Ok(files) => files,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some(Snapshot {
                    stamp: None,
                    lines: Vec::new(),
                    truncated: false,
                }));
            }
            Err(error) => return Err(error),
        };
        let mut paths = Vec::new();
        for entry in files {
            let entry = entry?;
            if entry.file_name().to_str().and_then(log_date).is_some()
                && entry.file_type()?.is_file()
            {
                paths.push(entry.path());
            }
        }
        let Some(path) = paths.into_iter().max() else {
            return Ok(Some(Snapshot {
                stamp: None,
                lines: Vec::new(),
                truncated: false,
            }));
        };
        let mut file = fs::File::open(&path)?;
        let metadata = file.metadata()?;
        let stamp = Stamp {
            path,
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
        };
        if previous == Some(&stamp) {
            return Ok(None);
        }
        let offset = stamp.bytes.saturating_sub(MAX_BYTES);
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES).read_to_end(&mut bytes)?;
        let mut truncated = offset > 0;
        // Discard the first incomplete line, including any partial UTF-8 character.
        let start = if offset > 0 {
            bytes
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |index| index + 1)
        } else {
            0
        };
        let text = String::from_utf8_lossy(&bytes[start..]);
        let mut level = Level::Unknown;
        let mut lines: Vec<_> = text
            .lines()
            .map(|line| {
                level = Level::parse(line).unwrap_or(level);
                Line {
                    text: line.to_string(),
                    level,
                }
            })
            .collect();
        if lines.len() > MAX_LINES {
            truncated = true;
            lines.drain(..lines.len() - MAX_LINES);
        }
        Ok(Some(Snapshot {
            stamp: Some(stamp),
            lines,
            truncated,
        }))
    };
    read().map_err(|error| format!("Could not read app logs in {}: {error}", dir.display()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Filter {
    All,
    Warnings,
    Errors,
}

impl Filter {
    pub const ALL: [Self; 3] = [Self::All, Self::Warnings, Self::Errors];
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All levels",
            Self::Warnings => "Warn + Error",
            Self::Errors => "Error",
        }
    }
    pub fn accepts(self, line: &Line, query: &str) -> bool {
        let level_matches = match self {
            Self::All => true,
            Self::Warnings => matches!(line.level, Level::Warn | Level::Error),
            Self::Errors => line.level == Level::Error,
        };
        level_matches && (query.is_empty() || line.text.to_lowercase().contains(query))
    }
}

#[cfg(test)]
mod tests {
    use super::{Filter, Level, MAX_BYTES, read_latest, remove_expired};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct Dir(PathBuf);
    impl Dir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "beacon-log-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, text: impl AsRef<[u8]>) {
            fs::write(self.0.join(name), text).unwrap();
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn cleanup_keeps_thirty_utc_dates_and_only_removes_daily_log_files() {
        let dir = Dir::new();
        let expired = "beacon.log.2024-02-29";
        let kept = [
            "beacon.log.2024-03-01",
            "beacon.log.2024-03-30",
            "beacon.log.2024-04-01",
            "beacon.log.2024-02-30",
            "beacon.log.2024-02-28.bak",
            "export.2024-02-28",
        ];
        for name in kept.into_iter().chain([expired]) {
            dir.write(name, "log");
        }
        let folder = dir.0.join("beacon.log.2024-02-27");
        fs::create_dir(&folder).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.0.join(kept[0]), dir.0.join("beacon.log.2024-02-26"))
            .unwrap();

        remove_expired(&dir.0, "2024-03-30".parse().unwrap()).unwrap();
        assert!(!dir.0.join(expired).exists());
        for name in kept {
            assert!(dir.0.join(name).is_file(), "{name}");
        }
        assert!(folder.is_dir());
        #[cfg(unix)]
        assert!(dir.0.join("beacon.log.2024-02-26").is_symlink());
    }

    #[test]
    fn reads_latest_daily_file_and_detects_rotation_and_growth() {
        let dir = Dir::new();
        dir.write("beacon.log.2026-09-29", "old\n");
        dir.write("unrelated.txt", "ignore\n");
        dir.write("beacon.log.2026-09-30", "today\n");
        let snapshot = read_latest(&dir.0, None).unwrap().unwrap();
        assert_eq!(snapshot.lines[0].text, "today");
        assert!(
            read_latest(&dir.0, snapshot.stamp.as_ref())
                .unwrap()
                .is_none()
        );
        dir.write("beacon.log.2026-09-30", "today\nnew\n");
        assert_eq!(
            read_latest(&dir.0, snapshot.stamp.as_ref())
                .unwrap()
                .unwrap()
                .lines
                .len(),
            2
        );
        dir.write("beacon.log.2026-10-01", "rotated\n");
        assert_eq!(
            read_latest(&dir.0, snapshot.stamp.as_ref())
                .unwrap()
                .unwrap()
                .lines[0]
                .text,
            "rotated"
        );
    }

    #[test]
    fn missing_and_empty_logs_are_supported() {
        let dir = Dir::new();
        assert!(
            read_latest(&dir.0.join("missing"), None)
                .unwrap()
                .unwrap()
                .lines
                .is_empty()
        );
        assert!(read_latest(&dir.0, None).unwrap().unwrap().lines.is_empty());
        dir.write("beacon.log.2026-09-30", "");
        assert!(read_latest(&dir.0, None).unwrap().unwrap().lines.is_empty());
    }

    #[test]
    fn reads_a_bounded_tail_without_a_partial_first_line() {
        let dir = Dir::new();
        dir.write(
            "beacon.log.2026-09-30",
            format!("{}\n证书校验失败\n", "x".repeat(MAX_BYTES as usize + 100)),
        );
        let snapshot = read_latest(&dir.0, None).unwrap().unwrap();
        assert!(snapshot.truncated);
        assert_eq!(snapshot.lines.len(), 1);
        assert_eq!(snapshot.lines[0].text, "证书校验失败");
        dir.write("beacon.log.2026-09-30", "line\n".repeat(6000));
        assert_eq!(
            read_latest(&dir.0, None).unwrap().unwrap().lines.len(),
            5000
        );
    }

    #[test]
    fn level_filters_keep_multiline_errors_and_search_ignores_case() {
        let dir = Dir::new();
        dir.write("beacon.log.2026-09-30", "2026-09-30T00:00:00Z INFO beacon: startup\n2026-09-30T00:00:01Z ERROR beacon: Connection failed\n  caused by certificate\n2026-09-30T00:00:02Z WARN beacon: retry\n");
        let lines = read_latest(&dir.0, None).unwrap().unwrap().lines;
        assert_eq!(lines[2].level, Level::Error);
        assert_eq!(
            lines
                .iter()
                .filter(|line| Filter::Errors.accepts(line, ""))
                .count(),
            2
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| Filter::Warnings.accepts(line, ""))
                .count(),
            3
        );
        assert!(Filter::All.accepts(&lines[1], "connection"));
    }
}
