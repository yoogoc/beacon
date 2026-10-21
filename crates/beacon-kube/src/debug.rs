//! Add a non-privileged ephemeral container without replacing the Pod.
use crate::{ClusterSession, DynamicObject, Error, Result};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api,
    api::{Patch, PatchParams},
};
use serde_json::{Value, json};
use std::sync::Arc;

mod stop;
pub use stop::{StopOutcome, stop, supports_stop};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Baseline,
    Restricted,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub name: String,
    pub image: String,
    pub target: Option<String>,
    pub shell: String,
    pub profile: Profile,
}

pub fn patch(pod: &Pod, options: &Options) -> Result<Value> {
    let invalid = |message: &str| Error::Manifest(message.into());
    if options.name.is_empty()
        || options.name.len() > 63
        || !options
            .name
            .bytes()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')
        || options.name.starts_with('-')
        || options.name.ends_with('-')
    {
        return Err(invalid(
            "Use a container name with lowercase letters, digits and hyphens (up to 63 characters).",
        ));
    }
    if options.image.trim().is_empty() || options.image.chars().any(char::is_whitespace) {
        return Err(invalid("Enter a container image without whitespace."));
    }
    if options.shell.trim().is_empty() || options.shell.chars().any(char::is_whitespace) {
        return Err(invalid("Enter one shell executable, such as /bin/sh."));
    }
    let spec = pod
        .spec
        .as_ref()
        .ok_or_else(|| invalid("Pod spec is unavailable."))?;
    if pod
        .status
        .as_ref()
        .and_then(|status| status.phase.as_deref())
        != Some("Running")
    {
        return Err(invalid("Debug containers require a running Pod."));
    }
    if spec
        .containers
        .iter()
        .any(|container| container.name == options.name)
        || spec
            .init_containers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|container| container.name == options.name)
        || spec
            .ephemeral_containers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|container| container.name == options.name)
    {
        return Err(invalid("That container name already exists in this Pod."));
    }
    if let Some(target) = &options.target
        && !spec
            .containers
            .iter()
            .any(|container| &container.name == target)
        && !spec
            .init_containers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|container| &container.name == target)
    {
        return Err(invalid("The selected target container no longer exists."));
    }
    let uid = pod
        .metadata
        .uid
        .as_deref()
        .ok_or_else(|| invalid("Pod UID is missing."))?;
    let rv = pod
        .metadata
        .resource_version
        .as_deref()
        .ok_or_else(|| invalid("Pod resource version is missing."))?;
    let security = match options.profile {
        Profile::Baseline => {
            json!({"privileged":false,"allowPrivilegeEscalation":false,"seccompProfile":{"type":"RuntimeDefault"}})
        }
        Profile::Restricted => {
            json!({"privileged":false,"allowPrivilegeEscalation":false,"runAsNonRoot":true,"runAsUser":1000,"capabilities":{"drop":["ALL"]},"seccompProfile":{"type":"RuntimeDefault"}})
        }
    };
    let mut container = json!({"name":options.name,"image":options.image,"imagePullPolicy":"IfNotPresent","command":stop::command(&options.shell, &options.name),"securityContext":security});
    if let Some(target) = &options.target {
        container["targetContainerName"] = json!(target);
    }
    Ok(
        json!({"metadata":{"uid":uid,"resourceVersion":rv},"spec":{"ephemeralContainers":[container]}}),
    )
}

pub async fn create(
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    options: Options,
) -> Result<()> {
    let namespace = object
        .metadata
        .namespace
        .as_deref()
        .ok_or_else(|| Error::Manifest("Pod namespace is missing.".into()))?;
    let name = object
        .metadata
        .name
        .as_deref()
        .ok_or_else(|| Error::Manifest("Pod name is missing.".into()))?;
    let api: Api<Pod> = Api::namespaced(session.client().clone(), namespace);
    let pod = api.get(name).await?;
    if pod.metadata.uid != object.metadata.uid {
        return Err(Error::Manifest(
            "This Pod was replaced. Refresh before starting a debug container.".into(),
        ));
    }
    let request = patch(&pod, &options)?;
    api.patch_ephemeral_containers(name, &PatchParams::default(), &Patch::Strategic(request))
        .await?;
    tracing::info!(context = %session.id(), namespace, pod = name, container = %options.name, "created debug container");
    Ok(())
}

pub async fn wait_running(
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    container: String,
) -> Result<()> {
    wait_running_with_progress(session, object, container, |_, _| {}).await
}

pub async fn wait_running_with_progress(
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    container: String,
    report: impl FnMut(&str, &str) + Send,
) -> Result<()> {
    wait_running_until(
        session,
        object,
        container,
        std::time::Duration::from_secs(120),
        report,
    )
    .await
}

async fn wait_running_until(
    session: Arc<ClusterSession>,
    object: Arc<DynamicObject>,
    container: String,
    timeout: std::time::Duration,
    mut report: impl FnMut(&str, &str) + Send,
) -> Result<()> {
    let api: Api<Pod> = Api::namespaced(
        session.client().clone(),
        object.metadata.namespace.as_deref().unwrap_or_default(),
    );
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = (String::new(), String::new());
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            let detail = if last.0.is_empty() {
                "No container status was reported.".into()
            } else {
                format!("{}: {}", last.0, last.1)
            };
            return Err(Error::Manifest(format!(
                "The debug container is still starting. {detail} Retry opening its terminal when it becomes ready."
            )));
        }
        let pod = tokio::time::timeout(remaining.min(std::time::Duration::from_secs(10)), api.get(object.metadata.name.as_deref().unwrap_or_default()))
            .await.map_err(|_| Error::Manifest("Timed out reading the debug container status. Retry when the cluster is reachable.".into()))??;
        if pod.metadata.uid != object.metadata.uid {
            return Err(Error::Manifest(
                "The Pod was replaced while its debug container was starting.".into(),
            ));
        }
        if let Some(status) = pod
            .status
            .as_ref()
            .and_then(|status| status.ephemeral_container_statuses.as_ref())
            .and_then(|statuses| statuses.iter().find(|status| status.name == container))
            && let Some(state) = &status.state
        {
            if state.running.is_some() {
                return Ok(());
            }
            if let Some(terminated) = &state.terminated {
                return Err(Error::Manifest(format!(
                    "Debug container exited with code {}: {}",
                    terminated.exit_code,
                    terminated
                        .message
                        .as_deref()
                        .or(terminated.reason.as_deref())
                        .unwrap_or_default()
                )));
            }
            if let Some(waiting) = &state.waiting {
                let status = (
                    waiting
                        .reason
                        .clone()
                        .unwrap_or_else(|| "ContainerCreating".into()),
                    waiting.message.clone().unwrap_or_default(),
                );
                if status != last {
                    report(&status.0, &status.1);
                    last = status;
                }
                if matches!(
                    waiting.reason.as_deref(),
                    Some(
                        "ImagePullBackOff"
                            | "ErrImagePull"
                            | "CreateContainerConfigError"
                            | "CreateContainerError"
                            | "InvalidImageName"
                            | "CrashLoopBackOff"
                            | "StartError"
                            | "RunContainerError"
                    )
                ) {
                    return Err(Error::Manifest(format!(
                        "{}: {}",
                        waiting.reason.as_deref().unwrap_or("Waiting"),
                        waiting.message.as_deref().unwrap_or_default()
                    )));
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(500).min(remaining)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires the local Postman debug-container simulation"]
    async fn postman_simulation_exercises_debug_creation_and_runtime_failure() {
        let base = std::env::var("BEACON_DEBUG_SIMULATION_URL")
            .expect("Set BEACON_DEBUG_SIMULATION_URL to the local /kubernetes simulation URL");
        let session = Arc::new(ClusterSession::for_testing(
            crate::ClusterId::new("debug-simulation"),
            base,
            vec![],
        ));
        for name in ["ready", "runtime-error"] {
            let object: Arc<DynamicObject> = Arc::new(serde_json::from_value(json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"default","uid":format!("uid-{name}")}})).unwrap());
            create(
                session.clone(),
                object.clone(),
                Options {
                    name: "debugger".into(),
                    image: "busybox:stable".into(),
                    shell: "/bin/sh".into(),
                    target: Some("app".into()),
                    profile: Profile::Restricted,
                },
            )
            .await
            .unwrap();
            let result = wait_running(session.clone(), object, "debugger".into()).await;
            if name == "ready" {
                result.unwrap();
            } else {
                let error = result.unwrap_err().user_message();
                assert!(
                    error.contains("CreateContainerError")
                        && error.contains("target container namespaces"),
                    "{error}"
                );
            }
        }
    }
    #[tokio::test]
    async fn startup_reports_runtime_errors_and_bounds_pending_waits() {
        for reason in [
            "CreateContainerError",
            "InvalidImageName",
            "CrashLoopBackOff",
            "ContainerCreating",
        ] {
            let object = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web","namespace":"default","uid":"pod","resourceVersion":"1"},"spec":{"containers":[{"name":"app"}]},"status":{"phase":"Running","ephemeralContainerStatuses":[{"name":"debugger","state":{"waiting":{"reason":reason,"message":"runtime detail"}}}]}});
            let fixture = crate::test_support::Fixture::start(vec![object.clone()]).await;
            let session = Arc::new(ClusterSession::for_testing(
                crate::ClusterId::new("debug-status"),
                fixture.url.clone(),
                vec![],
            ));
            let mut progress = Vec::new();
            let error = wait_running_until(
                session,
                Arc::new(serde_json::from_value(object).unwrap()),
                "debugger".into(),
                std::time::Duration::from_millis(20),
                |reason, message| progress.push((reason.to_owned(), message.to_owned())),
            )
            .await
            .unwrap_err()
            .user_message();
            assert!(error.contains(reason), "{error}");
            assert!(error.contains("runtime detail"));
            assert_eq!(progress, [(reason.to_owned(), "runtime detail".to_owned())]);
        }
    }
    #[tokio::test]
    async fn creating_a_debug_container_uses_the_subresource_and_preserves_existing_containers() {
        let original = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web","namespace":"default","uid":"web-uid","resourceVersion":"42"},"spec":{"containers":[{"name":"app","image":"distroless"}],"ephemeralContainers":[{"name":"existing","image":"busybox"}]},"status":{"phase":"Running"}});
        let fixture = crate::test_support::Fixture::start(vec![original.clone()]).await;
        let session = Arc::new(ClusterSession::for_testing(
            crate::ClusterId::new("debug-fixture"),
            fixture.url.clone(),
            vec![],
        ));
        let object: Arc<DynamicObject> =
            Arc::new(serde_json::from_value(original.clone()).unwrap());
        let options = Options {
            name: "debugger".into(),
            image: "busybox:stable".into(),
            target: Some("app".into()),
            shell: "/bin/sh".into(),
            profile: Profile::Restricted,
        };
        create(session.clone(), object.clone(), options.clone())
            .await
            .unwrap();
        wait_running(session.clone(), object.clone(), "debugger".into())
            .await
            .unwrap();
        let actual = fixture.object("Pod", "web");
        assert_eq!(actual["spec"]["containers"], original["spec"]["containers"]);
        assert_eq!(
            actual["spec"]["ephemeralContainers"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(fixture.requests.lock().unwrap().iter().any(|(line, _)| {
            line.starts_with("PATCH /api/v1/namespaces/default/pods/web/ephemeralcontainers")
        }));
        let mut replaced = actual;
        replaced["metadata"]["uid"] = json!("replacement");
        fixture.put(replaced);
        assert!(
            create(session, object, options)
                .await
                .unwrap_err()
                .user_message()
                .contains("replaced")
        );
    }
    #[test]
    fn debug_patch_preserves_existing_containers_and_uses_uid_guards() {
        let pod: Pod = serde_json::from_value(json!({"metadata":{"uid":"pod-uid","resourceVersion":"12"},"spec":{"containers":[{"name":"app","image":"distroless"}],"ephemeralContainers":[{"name":"existing","image":"busybox"}]},"status":{"phase":"Running"}})).unwrap();
        let mut options = Options {
            name: "debugger".into(),
            image: "busybox:stable".into(),
            target: Some("app".into()),
            shell: "/bin/sh".into(),
            profile: Profile::Restricted,
        };
        let request = patch(&pod, &options).unwrap();
        assert_eq!(request["metadata"]["uid"], "pod-uid");
        assert_eq!(request["metadata"]["resourceVersion"], "12");
        assert!(request["spec"].get("containers").is_none());
        assert_eq!(
            request["spec"]["ephemeralContainers"][0]["securityContext"]["runAsNonRoot"],
            true
        );
        assert_eq!(
            request["spec"]["ephemeralContainers"][0]["targetContainerName"],
            "app"
        );
        options.name = "existing".into();
        assert!(patch(&pod, &options).is_err());
        options.name = "debugger".into();
        options.target = Some("missing".into());
        assert!(patch(&pod, &options).is_err());
        options.target = None;
        options.image = "bad image".into();
        assert!(patch(&pod, &options).is_err());
    }
}
