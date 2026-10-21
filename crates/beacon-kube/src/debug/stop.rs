//! A cooperative stop never signals a PID in a potentially shared namespace.
use super::*;
use std::time::Duration;

pub(super) const KEEPALIVE: &str = include_str!("keepalive.sh");
const STOP: &str = include_str!("stop.sh");
const MARKER: &str = "beacon-debug-v1";
const ACK: &str = "beacon-debug-stop-requested";

fn control_path(name: &str) -> String {
    format!("/tmp/.beacon-debug-{name}")
}

pub(super) fn command(shell: &str, name: &str) -> Vec<String> {
    vec![
        shell.into(),
        "-c".into(),
        KEEPALIVE.into(),
        MARKER.into(),
        control_path(name),
    ]
}

/// Only the versioned command created by Beacon supports cooperative stop.
pub fn supports_stop(container: &Value) -> bool {
    let Some(name) = container["name"].as_str() else {
        return false;
    };
    let Some(argv) = container["command"].as_array() else {
        return false;
    };
    let Some(shell) = argv.first().and_then(Value::as_str) else {
        return false;
    };
    !name.is_empty()
        && !shell.is_empty()
        && !shell.chars().any(char::is_whitespace)
        && container["args"].as_array().is_none_or(Vec::is_empty)
        && argv
            == &command(shell, name)
                .into_iter()
                .map(Value::String)
                .collect::<Vec<_>>()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    Stopped,
    AlreadyStopped,
}

fn status<'a>(pod: &'a Pod, name: &str) -> Option<&'a k8s_openapi::api::core::v1::ContainerState> {
    pod.status
        .as_ref()?
        .ephemeral_container_statuses
        .as_ref()?
        .iter()
        .find(|status| status.name == name)?
        .state
        .as_ref()
}

fn same_pod(pod: &Pod, object: &DynamicObject) -> Result<()> {
    if object.metadata.uid.is_none() || pod.metadata.uid != object.metadata.uid {
        return Err(Error::Manifest(
            "This Pod was replaced. Refresh before stopping its debug container.".into(),
        ));
    }
    Ok(())
}

/// Stop a managed ephemeral container and confirm termination through Pod status.
pub async fn stop(
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    container: String,
) -> Result<StopOutcome> {
    tokio::time::timeout(
        Duration::from_secs(20),
        stop_inner(session, object, container),
    )
    .await
    .map_err(|_| {
        Error::Manifest(
            "Timed out stopping the debug container. Refresh its status before retrying.".into(),
        )
    })?
}

async fn stop_inner(
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    container: String,
) -> Result<StopOutcome> {
    let invalid = |text: &str| Error::Manifest(text.into());
    let namespace = object
        .metadata
        .namespace
        .as_deref()
        .ok_or_else(|| invalid("Pod namespace is missing."))?;
    let name = object
        .metadata
        .name
        .as_deref()
        .ok_or_else(|| invalid("Pod name is missing."))?;
    let api: Api<Pod> = Api::namespaced(session.client().clone(), namespace);
    let pod = api.get(name).await?;
    same_pod(&pod, &object)?;
    let target = pod
        .spec
        .as_ref()
        .and_then(|spec| spec.ephemeral_containers.as_ref())
        .and_then(|items| items.iter().find(|item| item.name == container))
        .ok_or_else(|| invalid("The selected ephemeral container no longer exists."))?;
    if status(&pod, &container).is_some_and(|state| state.terminated.is_some()) {
        return Ok(StopOutcome::AlreadyStopped);
    }
    if !supports_stop(&serde_json::to_value(target).map_err(|error| invalid(&error.to_string()))?) {
        return Err(invalid(
            "This debug container was created without safe stop support. Its entry is removed when the Pod is replaced.",
        ));
    }
    if !status(&pod, &container).is_some_and(|state| state.running.is_some()) {
        return Err(invalid(
            "The debug container is not running yet. Refresh its status before stopping it.",
        ));
    }
    let shell = target.command.as_ref().unwrap()[0].clone();
    let result = session
        .clone()
        .exec(
            namespace.into(),
            name.into(),
            Some(container.clone()),
            vec![
                shell,
                "-c".into(),
                STOP.into(),
                MARKER.into(),
                control_path(&container),
            ],
        )
        .await;
    let exec_error = match result {
        Ok(output) if output.note.is_none() && output.stdout.trim() == ACK => None,
        Ok(output) => Some(invalid(&output.note.unwrap_or_else(|| {
            if output.stderr.trim().is_empty() {
                "The debug container did not acknowledge the stop request.".into()
            } else {
                output.stderr
            }
        }))),
        Err(error) => Some(error),
    };
    // The main process can exit before exec's final status is delivered. Always
    // verify the container state, even when that closes the exec transport.
    loop {
        let current = api.get(name).await?;
        same_pod(&current, &object)?;
        if status(&current, &container).is_some_and(|state| state.terminated.is_some()) {
            return Ok(StopOutcome::Stopped);
        }
        if let Some(error) = exec_error {
            return Err(error);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

#[cfg(test)]
mod tests;
