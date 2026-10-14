//! Session-owned loopback listeners. Logical targets are resolved again after
//! Pod replacement without rebinding the local port. Stopping a listener also
//! aborts every in-flight tunnel through it.
use crate::{Error, Result};
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinSet};
mod target;
pub use target::{Resolved, Target, TargetKind};
pub type ForwardId = u64;

#[derive(Debug, Clone)]
pub struct Forward {
    pub id: ForwardId,
    pub namespace: String,
    pub target: Target,
    pub pod: String,
    pub remote_port: u16,
    pub local_port: u16,
    pub connections: usize,
    pub failure: Option<String>,
    pub reconnecting: Option<String>,
    route: Resolved,
}
impl Forward {
    pub fn address(&self) -> String {
        format!("127.0.0.1:{}", self.local_port)
    }
    pub fn describe(&self) -> String {
        format!(
            "{} → {}/{}:{}",
            self.address(),
            self.namespace,
            self.pod,
            self.remote_port
        )
    }
}
struct Running {
    forward: Arc<Mutex<Forward>>,
    connections: Arc<AtomicUsize>,
    task: tokio::task::AbortHandle,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[derive(Default)]
pub struct Forwards {
    running: Mutex<HashMap<ForwardId, Arc<Running>>>,
    next_id: AtomicU64,
    stopped: AtomicBool,
}
impl Forwards {
    /// Compatibility entry point for a known Pod/port; new UI uses open_target.
    pub async fn open(
        &self,
        client: kube::Client,
        runtime: &tokio::runtime::Handle,
        namespace: String,
        pod: String,
        remote_port: u16,
        local_port: u16,
    ) -> Result<Forward> {
        let target = Target {
            kind: TargetKind::Pod,
            namespace,
            name: pod.clone(),
            port: remote_port,
        };
        target.validate()?;
        self.listen(
            client,
            runtime,
            target,
            Resolved {
                pod,
                uid: None,
                port: remote_port,
            },
            local_port,
        )
        .await
    }
    pub async fn open_target(
        &self,
        client: kube::Client,
        runtime: &tokio::runtime::Handle,
        target: Target,
        local_port: u16,
    ) -> Result<Forward> {
        let route = resolve(&target, &client).await?;
        self.listen(client, runtime, target, route, local_port)
            .await
    }
    async fn listen(
        &self,
        client: kube::Client,
        runtime: &tokio::runtime::Handle,
        target: Target,
        route: Resolved,
        local_port: u16,
    ) -> Result<Forward> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, local_port))).await.map_err(|error| Error::Forward {
            what: format!("could not listen on 127.0.0.1:{local_port}"),
            cause: if error.kind() == std::io::ErrorKind::AddrInUse { "Port is already in use. Stop its current listener or choose another local port (0 selects a free port).".into() } else { error.to_string() },
        })?;
        let bound = listener
            .local_addr()
            .map_err(|error| Error::Forward {
                what: "could not read local address".into(),
                cause: error.to_string(),
            })?
            .port();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let connections = Arc::new(AtomicUsize::new(0));
        let forward = Forward {
            id,
            namespace: target.namespace.clone(),
            target,
            pod: route.pod.clone(),
            remote_port: route.port,
            local_port: bound,
            connections: 0,
            failure: None,
            reconnecting: None,
            route,
        };
        let state = Arc::new(Mutex::new(forward.clone()));
        let task = runtime.spawn(accept_loop(
            listener,
            client,
            state.clone(),
            connections.clone(),
        ));
        let running = Arc::new(Running {
            forward: state,
            connections,
            task: task.abort_handle(),
        });
        let mut forwards = self.running.lock().expect("forwards");
        if self.stopped.load(Ordering::Acquire) {
            return Err(Error::Forward {
                what: "could not start forwarding".into(),
                cause: "cluster was disconnected".into(),
            });
        }
        tracing::info!(forward = %forward.describe(), "forwarding");
        forwards.insert(id, running);
        Ok(forward)
    }
    pub fn list(&self) -> Vec<Forward> {
        let mut forwards: Vec<_> = self
            .running
            .lock()
            .expect("forwards")
            .values()
            .map(|running| {
                let mut forward = running.forward.lock().expect("forward").clone();
                forward.connections = running.connections.load(Ordering::Relaxed);
                forward
            })
            .collect();
        forwards.sort_by_key(|f| f.id);
        forwards
    }
    pub fn is_empty(&self) -> bool {
        self.running.lock().expect("forwards").is_empty()
    }
    pub fn stop_all(&self) {
        let mut forwards = self.running.lock().expect("forwards");
        self.stopped.store(true, Ordering::Release);
        forwards.clear();
    }
    pub fn close(&self, id: ForwardId) {
        self.running.lock().expect("forwards").remove(&id);
    }
    pub fn close_for_pod(&self, namespace: &str, pod: &str) {
        // A Service/Deployment listener must survive deletion of its current Pod.
        for forward in self.list().into_iter().filter(|f| {
            f.target.kind == TargetKind::Pod && f.namespace == namespace && f.pod == pod
        }) {
            self.close(forward.id);
        }
    }
}
async fn resolve(target: &Target, client: &kube::Client) -> Result<Resolved> {
    tokio::time::timeout(Duration::from_secs(15), target.resolve(client.clone()))
        .await
        .map_err(|_| Error::Forward {
            what: "could not resolve forwarding target".into(),
            cause: "API request timed out; check the connection and retry.".into(),
        })?
}
fn update_route(state: &Mutex<Forward>, result: &Result<Resolved>) {
    let mut state = state.lock().expect("forward");
    match result {
        Ok(route) => {
            if state.route != *route {
                state.failure = None;
            }
            state.pod = route.pod.clone();
            state.remote_port = route.port;
            state.route = route.clone();
            state.reconnecting = None;
        }
        Err(error) => state.reconnecting = Some(error.user_message()),
    }
}
async fn accept_loop(
    listener: TcpListener,
    client: kube::Client,
    state: Arc<Mutex<Forward>>,
    connections: Arc<AtomicUsize>,
) {
    let target = state.lock().expect("forward").target.clone();
    let mut tunnels = JoinSet::new();
    let mut refresh = tokio::time::interval(Duration::from_secs(3));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pending = Box::pin(resolve(&target, &client));
    let mut resolving = true;
    loop {
        tokio::select! {
            result = &mut pending, if resolving => { update_route(&state, &result); resolving = false; }
            _ = refresh.tick(), if !resolving => { pending = Box::pin(resolve(&target, &client)); resolving = true; }
            _ = tunnels.join_next(), if !tunnels.is_empty() => {}
            accepted = listener.accept() => {
                let Ok((mut socket, _peer)) = accepted else { return; };
                let (client, target, state, connections) = (client.clone(), target.clone(), state.clone(), connections.clone());
                tunnels.spawn(async move {
                    connections.fetch_add(1, Ordering::Relaxed);
                    let _guard = Counted(connections);
                    let result = resolve(&target, &client).await;
                    update_route(&state, &result);
                    let Ok(route) = result else { return; };
                    let api: Api<Pod> = Api::namespaced(client, &target.namespace);
                    let mut forwarder = match tokio::time::timeout(Duration::from_secs(15), api.portforward(&route.pod, &[route.port])).await {
                        Ok(Ok(forwarder)) => Tunnel(forwarder),
                        result => {
                            let message = match result { Ok(Err(error)) => crate::Error::from(error).user_message(), Err(_) => "Opening the tunnel timed out.".into(), _ => unreachable!() };
                            state.lock().expect("forward").failure = Some(message); return;
                        }
                    };
                    let Some(mut tunnel) = forwarder.0.take_stream(route.port) else {
                        state.lock().expect("forward").failure = Some("The Pod did not open the requested port.".into()); return;
                    };
                    state.lock().expect("forward").failure = None;
                    let remote_error = forwarder.0.take_error(route.port);
                    tokio::select! {
                        result = tokio::io::copy_bidirectional(&mut socket, &mut tunnel) => {
                            if let Err(error) = result { tracing::debug!(%error, "forward connection closed"); }
                        }
                        error = async { match remote_error { Some(error) => error.await, None => std::future::pending().await } } => {
                            if let Some(error) = error { state.lock().expect("forward").failure = Some(error); }
                        }
                    }
                });
            }
        }
    }
}
// kube's Portforwarder owns a detached task; dropping it alone does not abort.
struct Tunnel(kube::api::Portforwarder);
impl Drop for Tunnel {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Counted(Arc<AtomicUsize>);
impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forward(local: u16, namespace: &str, pod: &str, remote: u16) -> Forward {
        Forward {
            id: 0,
            namespace: namespace.to_string(),
            target: Target {
                kind: TargetKind::Pod,
                namespace: namespace.into(),
                name: pod.into(),
                port: remote,
            },
            route: Resolved {
                pod: pod.into(),
                uid: None,
                port: remote,
            },
            reconnecting: None,
            pod: pod.to_string(),
            remote_port: remote,
            local_port: local,
            connections: 0,
            failure: None,
        }
    }

    /// The local port is the whole point of the row: it is what you paste into
    /// a browser, and with port 0 it is the only place the answer exists.
    #[test]
    fn a_forward_reads_as_an_address_and_a_destination() {
        let forward = forward(52310, "argocd", "argocd-server-abc", 8080);

        assert_eq!(forward.address(), "127.0.0.1:52310");
        assert_eq!(
            forward.describe(),
            "127.0.0.1:52310 → argocd/argocd-server-abc:8080"
        );
    }

    /// The connection counter must come back down however a tunnel ends, or
    /// the list slowly reports connections that closed long ago.
    #[test]
    fn the_connection_count_is_released_on_drop() {
        let connections = Arc::new(AtomicUsize::new(0));

        {
            connections.fetch_add(1, Ordering::Relaxed);
            let _guard = Counted(connections.clone());
            assert_eq!(connections.load(Ordering::Relaxed), 1);
        }

        assert_eq!(connections.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn forwards_are_listed_in_the_order_they_were_opened() {
        let forwards = Forwards::default();
        assert!(forwards.is_empty());

        // `open` needs a cluster, so the registry is exercised directly.
        for (index, port) in [8080u16, 5432, 6379].into_iter().enumerate() {
            let id = forwards.next_id.fetch_add(1, Ordering::Relaxed);
            let task = tokio::spawn(async { std::future::pending::<()>().await });
            forwards.running.lock().expect("forwards").insert(
                id,
                Arc::new(Running {
                    forward: Arc::new(Mutex::new(Forward {
                        id,
                        ..forward(10000 + index as u16, "default", "api", port)
                    })),
                    connections: Arc::new(AtomicUsize::new(0)),
                    task: task.abort_handle(),
                }),
            );
        }

        let listed = forwards.list();
        assert_eq!(listed.len(), 3);
        assert_eq!(
            listed.iter().map(|f| f.remote_port).collect::<Vec<_>>(),
            [8080, 5432, 6379]
        );

        forwards.close(listed[1].id);
        assert_eq!(
            forwards
                .list()
                .iter()
                .map(|f| f.remote_port)
                .collect::<Vec<_>>(),
            [8080, 6379]
        );
    }

    #[tokio::test]
    async fn stopping_a_session_closes_listeners_and_rejects_late_opens() {
        let forwards = Forwards::default();
        let config = kube::Config::new("http://127.0.0.1:9".parse().unwrap());
        let client = kube::Client::try_from(config).unwrap();
        let runtime = tokio::runtime::Handle::current();
        let opened = forwards
            .open(
                client.clone(),
                &runtime,
                "default".into(),
                "test".into(),
                80,
                0,
            )
            .await
            .unwrap();
        assert_eq!(forwards.list().len(), 1);
        forwards.stop_all();
        tokio::task::yield_now().await;
        assert!(forwards.is_empty());
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, opened.local_port))
            .await
            .unwrap();
        drop(listener);
        let late = forwards
            .open(client, &runtime, "default".into(), "test".into(), 80, 0)
            .await;
        assert!(late.is_err());
        assert!(forwards.is_empty());
    }

    /// A forward to a pod that no longer exists accepts connections and fails
    /// them, which looks like a broken service rather than a stale forward.
    #[tokio::test]
    async fn forwards_to_a_gone_pod_are_closed() {
        let forwards = Forwards::default();

        for (pod, port) in [("api-1", 8080u16), ("api-1", 9090), ("api-2", 8080)] {
            let id = forwards.next_id.fetch_add(1, Ordering::Relaxed);
            let task = tokio::spawn(async { std::future::pending::<()>().await });
            forwards.running.lock().expect("forwards").insert(
                id,
                Arc::new(Running {
                    forward: Arc::new(Mutex::new(Forward {
                        id,
                        ..forward(10000, "default", pod, port)
                    })),
                    connections: Arc::new(AtomicUsize::new(0)),
                    task: task.abort_handle(),
                }),
            );
        }

        forwards.close_for_pod("default", "api-1");

        let left = forwards.list();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].pod, "api-2");
    }
}

#[cfg(all(test, unix))]
mod native_test;
#[cfg(test)]
mod target_tests;
