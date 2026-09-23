//! Reading a status string as a colour.
//!
//! Kubernetes has no enum for this. `STATUS` is a string assembled from a
//! phase, a container state and a handful of special cases, and the set of
//! values is open -- any waiting reason a kubelet invents ends up in it. So the
//! mapping is a table of what we know plus a rule for the rest, and the rule
//! matters more than the table: an unrecognised status must read as *unknown*,
//! never as healthy.

use crate::theme::Tone;

/// The tone for a value of the `Status` column.
pub fn tone(status: &str) -> Tone {
    // Initialization has its own namespace: `Init:ImagePullBackOff` is a
    // failure, `Init:1/3` is just progress.
    if let Some(rest) = status.strip_prefix("Init:") {
        return match rest.split(['/', ':']).next() {
            // `Init:1/3` and `Init:ExitCode:1` both start with a digit-ish
            // segment; only the latter carries a reason worth colouring.
            Some(segment) if segment.chars().all(|c| c.is_ascii_digit()) => Tone::Progressing,
            Some(_) => tone(rest),
            None => Tone::Progressing,
        };
    }

    match status {
        "Running" | "Active" | "Bound" | "Ready" => Tone::Healthy,

        // Finished and inert. Not a problem, but not worth the eye it would
        // draw in a list where half the rows are completed jobs.
        "Completed" | "Succeeded" | "Terminated" => Tone::Unknown,

        "Pending"
        | "ContainerCreating"
        | "PodInitializing"
        | "Terminating"
        | "ContainerStatusUnknown"
        | "Released" => Tone::Progressing,

        // Reachable, wrong, and usually the user's to fix.
        "NotReady"
        | "Unknown"
        | "SchedulingGated"
        | "Evicted"
        | "NodeLost"
        | "ImagePullBackOff"
        | "ErrImagePull"
        | "ErrImageNeverPull"
        | "InvalidImageName"
        | "CreateContainerConfigError"
        | "CreateContainerError"
        | "ContainerCannotRun" => Tone::Warning,

        "Error" | "Failed" | "CrashLoopBackOff" | "OOMKilled" | "DeadlineExceeded"
        | "RunContainerError" | "StartError" => Tone::Critical,

        // `ExitCode:137`, `Signal:9` -- a container that died without the
        // kubelet naming a reason.
        other if other.starts_with("ExitCode:") || other.starts_with("Signal:") => Tone::Critical,

        _ => Tone::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_common_states() {
        assert_eq!(tone("Running"), Tone::Healthy);
        assert_eq!(tone("Pending"), Tone::Progressing);
        assert_eq!(tone("CrashLoopBackOff"), Tone::Critical);
        assert_eq!(tone("ImagePullBackOff"), Tone::Warning);
    }

    /// Half the rows of a cluster running CronJobs are completed pods. They are
    /// fine, and colouring them green would drown out the ones that are not.
    #[test]
    fn a_finished_pod_is_not_highlighted() {
        assert_eq!(tone("Completed"), Tone::Unknown);
        assert_eq!(tone("Succeeded"), Tone::Unknown);
    }

    /// `Init:` covers both progress and failure, and they must not look alike.
    #[test]
    fn initialization_separates_progress_from_failure() {
        assert_eq!(tone("Init:0/3"), Tone::Progressing);
        assert_eq!(tone("Init:2/2"), Tone::Progressing);
        assert_eq!(tone("Init:ImagePullBackOff"), Tone::Warning);
        assert_eq!(tone("Init:CrashLoopBackOff"), Tone::Critical);
        assert_eq!(tone("Init:ExitCode:1"), Tone::Critical);
        assert_eq!(tone("Init:Signal:9"), Tone::Critical);
    }

    #[test]
    fn a_container_that_died_unexplained_is_critical() {
        assert_eq!(tone("ExitCode:137"), Tone::Critical);
        assert_eq!(tone("Signal:9"), Tone::Critical);
    }

    /// New waiting reasons appear with every Kubernetes release. Whatever they
    /// are, they must not arrive pretending to be healthy.
    #[test]
    fn an_unrecognised_status_is_unknown() {
        assert_eq!(tone("SomeNewKubeletReason"), Tone::Unknown);
        assert_eq!(tone(""), Tone::Unknown);
    }
}
