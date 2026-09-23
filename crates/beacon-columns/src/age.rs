//! kubectl-compatible age formatting.
//!
//! This is a direct port of `k8s.io/apimachinery/pkg/util/duration.HumanDuration`.
//! The exact thresholds matter more than they look: users read these columns
//! next to a terminal running `kubectl get`, and a cell that says `90m` where
//! kubectl says `1h30m` reads as a bug.

// k8s-openapi 0.28 models timestamps with jiff, and re-exports it so that
// dependents do not have to pick a matching version themselves.
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::jiff::Timestamp;

/// Formats an age the way kubectl's `AGE` column does.
pub fn format_duration(seconds: i64) -> String {
    if seconds < -1 {
        return "<invalid>".to_string();
    }
    if seconds < 0 {
        return "0s".to_string();
    }
    if seconds < 60 * 2 {
        return format!("{seconds}s");
    }

    let minutes = seconds / 60;
    if minutes < 10 {
        let rem = seconds % 60;
        return if rem == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m{rem}s")
        };
    }
    if minutes < 60 * 3 {
        return format!("{minutes}m");
    }

    let hours = seconds / 3600;
    if hours < 8 {
        let rem = minutes % 60;
        return if rem == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h{rem}m")
        };
    }
    if hours < 48 {
        return format!("{hours}h");
    }

    let days = hours / 24;
    if hours < 24 * 8 {
        let rem = hours % 24;
        return if rem == 0 {
            format!("{days}d")
        } else {
            format!("{days}d{rem}h")
        };
    }
    if hours < 24 * 365 * 2 {
        return format!("{days}d");
    }

    let years = days / 365;
    if hours < 24 * 365 * 8 {
        let rem = days % 365;
        return if rem == 0 {
            format!("{years}y")
        } else {
            format!("{years}y{rem}d")
        };
    }

    format!("{years}y")
}

/// Age of an object relative to `now`.
pub fn format_age(creation: &Time, now: Timestamp) -> String {
    format_duration(now.duration_since(creation.0).as_secs())
}

#[cfg(test)]
mod tests {
    use super::format_duration;

    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;

    /// Every expectation here is what `kubectl get` prints for the same age.
    #[test]
    fn matches_kubectl() {
        let cases = [
            (0, "0s"),
            (1, "1s"),
            (59, "59s"),
            (119, "119s"),
            (2 * MINUTE, "2m"),
            (2 * MINUTE + 5, "2m5s"),
            (9 * MINUTE + 59, "9m59s"),
            (10 * MINUTE, "10m"),
            (179 * MINUTE, "179m"),
            (3 * HOUR, "3h"),
            (3 * HOUR + 25 * MINUTE, "3h25m"),
            (7 * HOUR + 59 * MINUTE, "7h59m"),
            (8 * HOUR, "8h"),
            (47 * HOUR, "47h"),
            (2 * DAY, "2d"),
            (2 * DAY + 3 * HOUR, "2d3h"),
            (7 * DAY, "7d"),
            (8 * DAY, "8d"),
            (400 * DAY, "400d"),
            (730 * DAY, "2y"),
            (731 * DAY, "2y1d"),
        ];

        for (seconds, expected) in cases {
            assert_eq!(format_duration(seconds), expected, "for {seconds}s");
        }
    }

    /// Clock skew between the workstation and the API server routinely produces
    /// objects created "in the future". kubectl shows 0s rather than a negative.
    #[test]
    fn clock_skew_does_not_produce_negative_ages() {
        assert_eq!(format_duration(-1), "0s");
        assert_eq!(format_duration(-2), "<invalid>");
    }
}
