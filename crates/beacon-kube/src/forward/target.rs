//! Resolve logical targets without exposing a Service's cluster IP locally.
use crate::{Error, Result};
use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{Pod, Service},
    },
    apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
};
use kube::{
    Api,
    api::ListParams,
    core::{Selector, SelectorExt},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetKind {
    Pod,
    Service,
    Deployment,
}
impl TargetKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pod => "Pod",
            Self::Service => "Service",
            Self::Deployment => "Deployment",
        }
    }
    pub fn from_resource(group: &str, kind: &str) -> Option<Self> {
        match (group, kind) {
            ("", "Pod") => Some(Self::Pod),
            ("", "Service") => Some(Self::Service),
            ("apps", "Deployment") => Some(Self::Deployment),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub kind: TargetKind,
    pub namespace: String,
    pub name: String,
    /// Service port for a Service, otherwise the remote container port.
    pub port: u16,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub pod: String,
    pub uid: Option<String>,
    pub port: u16,
}
fn invalid(message: impl Into<String>) -> Error {
    Error::Manifest(message.into())
}
impl Target {
    pub fn describe(&self) -> String {
        format!(
            "{} {}/{}:{}",
            self.kind.label(),
            self.namespace,
            self.name,
            self.port
        )
    }
    pub fn validate(&self) -> Result<()> {
        if self.port == 0 || self.namespace.trim().is_empty() || self.name.trim().is_empty() {
            return Err(invalid(
                "Choose a namespace, target name and remote port from 1 to 65535.",
            ));
        }
        Ok(())
    }
    pub async fn resolve(&self, client: kube::Client) -> Result<Resolved> {
        self.validate()?;
        let pods: Api<Pod> = Api::namespaced(client.clone(), &self.namespace);
        if self.kind == TargetKind::Pod {
            let pod = pods.get(&self.name).await?;
            if !running(&pod) {
                return Err(invalid("The target Pod is not running or is terminating."));
            }
            return resolved(&pod, self.port);
        }
        let (selector, port) = match self.kind {
            TargetKind::Service => {
                let service = Api::<Service>::namespaced(client, &self.namespace)
                    .get(&self.name)
                    .await?;
                let spec = service
                    .spec
                    .ok_or_else(|| invalid("Service has no specification."))?;
                let labels = spec.selector.filter(|s| !s.is_empty()).ok_or_else(|| invalid("This Service has no Pod selector. Select a Pod or a Service with a selector to forward."))?;
                let port = spec
                    .ports
                    .unwrap_or_default()
                    .into_iter()
                    .find(|p| {
                        p.port == i32::from(self.port)
                            && p.protocol.as_deref().unwrap_or("TCP") == "TCP"
                    })
                    .ok_or_else(|| {
                        invalid("This Service does not expose the selected TCP port.")
                    })?;
                (
                    LabelSelector {
                        match_labels: Some(labels),
                        ..Default::default()
                    },
                    port.target_port.unwrap_or(IntOrString::Int(port.port)),
                )
            }
            TargetKind::Deployment => {
                let deployment = Api::<Deployment>::namespaced(client, &self.namespace)
                    .get(&self.name)
                    .await?;
                let spec = deployment
                    .spec
                    .ok_or_else(|| invalid("Deployment has no specification."))?;
                (spec.selector, IntOrString::Int(i32::from(self.port)))
            }
            TargetKind::Pod => unreachable!(),
        };
        let selector: Selector = selector
            .try_into()
            .map_err(|error| invalid(format!("Invalid Pod selector: {error}")))?;
        let label_query = selector.to_string();
        if label_query.is_empty() {
            return Err(invalid("The target has an empty Pod selector."));
        }
        let mut params = ListParams::default().labels(&label_query).limit(500);
        let mut available = Vec::new();
        loop {
            let page = pods.list(&params).await?;
            available.extend(page.items.into_iter().filter(|pod| {
                pod.metadata.namespace.as_deref() == Some(&self.namespace)
                    && running(pod)
                    && ready(pod)
                    && selector.matches(&pod.metadata.labels.clone().unwrap_or_default())
            }));
            match page.metadata.continue_.filter(|token| !token.is_empty()) {
                Some(token) => params = params.continue_token(&token),
                None => break,
            }
        }
        available.sort_by(|a, b| a.metadata.name.cmp(&b.metadata.name));
        for pod in available {
            let number = match &port {
                IntOrString::Int(number) => u16::try_from(*number).ok().filter(|n| *n > 0),
                IntOrString::String(name) => pod.spec.as_ref().and_then(|spec| {
                    spec.containers
                        .iter()
                        .chain(spec.init_containers.iter().flatten().filter(|container| {
                            container.restart_policy.as_deref() == Some("Always")
                        }))
                        .flat_map(|container| container.ports.iter().flatten())
                        .find(|port| {
                            port.name.as_deref() == Some(name)
                                && port.protocol.as_deref().unwrap_or("TCP") == "TCP"
                        })
                        .and_then(|port| u16::try_from(port.container_port).ok())
                        .filter(|port| *port > 0)
                }),
            };
            if let Some(number) = number {
                return resolved(&pod, number);
            }
        }
        Err(invalid(
            "No running, ready Pod exposes the requested port. Beacon will retry while a forward remains open.",
        ))
    }
}
fn resolved(pod: &Pod, port: u16) -> Result<Resolved> {
    Ok(Resolved {
        pod: pod
            .metadata
            .name
            .clone()
            .ok_or_else(|| invalid("The target Pod has no name."))?,
        uid: pod.metadata.uid.clone(),
        port,
    })
}
fn running(pod: &Pod) -> bool {
    pod.metadata.deletion_timestamp.is_none()
        && pod.status.as_ref().and_then(|s| s.phase.as_deref()) == Some("Running")
}
fn ready(pod: &Pod) -> bool {
    pod.status
        .as_ref()
        .and_then(|status| status.conditions.as_ref())
        .is_some_and(|conditions| {
            conditions
                .iter()
                .any(|condition| condition.type_ == "Ready" && condition.status == "True")
        })
}
