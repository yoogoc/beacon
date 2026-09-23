use std::path::PathBuf;

use crate::ClusterId;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "no kubeconfig found (looked at {searched:?}); set KUBECONFIG or create ~/.kube/config"
    )]
    NoKubeconfig { searched: Vec<PathBuf> },

    #[error("failed to read kubeconfig: {0}")]
    Kubeconfig(#[from] kube::config::KubeconfigError),

    #[error("context {0:?} is not present in the kubeconfig")]
    UnknownContext(String),

    #[error("kubernetes api: {0}")]
    Api(#[from] kube::Error),

    #[error("{what}: {cause}")]
    Forward { what: String, cause: String },

    /// A connection attempt that failed, already carrying everything needed to
    /// act on it. See [`diagnose`].
    #[error("could not connect to {context}: {diagnosis}")]
    Connect { context: String, diagnosis: String },
}

impl Error {
    pub fn connect(context: &ClusterId, source: &(dyn std::error::Error + 'static)) -> Self {
        Self::Connect {
            context: context.to_string(),
            diagnosis: diagnose(source),
        }
    }
}

/// Renders an error the way somebody who has to fix it needs to read it.
///
/// Two things are wrong with printing a `kube::Error` directly. The top-level
/// `Display` is a summary -- "auth error" -- and the cause that names the actual
/// problem is further down the chain. And the single most common failure, an
/// `exec` credential plugin that is not on `PATH`, produces "no such file or
/// directory" with no mention of what was looked for or where. Beacon launched
/// from Finder has a different `PATH` than the terminal the user tests in, so
/// that error is genuinely baffling without it.
pub fn diagnose(source: &(dyn std::error::Error + 'static)) -> String {
    let mut chain = vec![source.to_string()];
    let mut current = source.source();
    while let Some(error) = current {
        let message = error.to_string();
        // kube nests errors that restate their cause verbatim; repeating it
        // makes the chain harder to read, not easier.
        if !chain.last().is_some_and(|last| last.contains(&message)) {
            chain.push(message);
        }
        current = error.source();
    }

    let mut diagnosis = chain.join(": ");

    if is_exec_plugin_failure(source) {
        diagnosis.push_str(&format!(
            "\nthis context authenticates with an exec credential plugin, \
             and Beacon searched PATH={}",
            std::env::var("PATH").unwrap_or_else(|_| "<unset>".into())
        ));
    }

    diagnosis
}

/// Whether the failure was a kubeconfig `exec` plugin that could not be run.
fn is_exec_plugin_failure(source: &(dyn std::error::Error + 'static)) -> bool {
    use kube::client::AuthError;

    let mut current = Some(source);
    while let Some(error) = current {
        if let Some(auth) = error.downcast_ref::<AuthError>() {
            return matches!(
                auth,
                AuthError::AuthExecStart(_)
                    | AuthError::AuthExecRun { .. }
                    | AuthError::MissingCommand
            );
        }
        current = error.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("{message}")]
    struct Layer {
        message: String,
        #[source]
        source: Option<Box<Layer>>,
    }

    fn layer(message: &str, source: Option<Layer>) -> Layer {
        Layer {
            message: message.to_string(),
            source: source.map(Box::new),
        }
    }

    #[test]
    fn a_diagnosis_reads_the_whole_chain() {
        let error = layer(
            "auth error",
            Some(layer(
                "unable to run auth exec",
                Some(layer("no such file or directory", None)),
            )),
        );

        assert_eq!(
            diagnose(&error),
            "auth error: unable to run auth exec: no such file or directory"
        );
    }

    /// kube wraps errors whose `Display` already contains the cause. Printing
    /// both turns a two-line diagnosis into a stutter.
    #[test]
    fn a_diagnosis_does_not_repeat_a_restated_cause() {
        let error = layer(
            "failed to read kubeconfig: file is empty",
            Some(layer("file is empty", None)),
        );

        assert_eq!(diagnose(&error), "failed to read kubeconfig: file is empty");
    }

    /// The PATH hint is the entire point of the diagnosis for exec plugins, and
    /// it must not appear on unrelated failures.
    #[test]
    fn only_exec_plugin_failures_get_the_path_hint() {
        let plain = layer("connection refused", None);
        assert!(!diagnose(&plain).contains("PATH="));

        let exec = kube::Error::Auth(kube::client::AuthError::AuthExecStart(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no such file or directory",
        )));
        let diagnosis = diagnose(&exec);
        assert!(diagnosis.contains("exec credential plugin"), "{diagnosis}");
        assert!(diagnosis.contains("PATH="), "{diagnosis}");
    }
}
