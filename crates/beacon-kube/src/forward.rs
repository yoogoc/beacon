//! Port forwarding.
//!
//! A forward is a local TCP listener that hands every connection its own
//! tunnel to a pod. It is the one thing in Beacon that outlives what you are
//! looking at: you set one up in order to point a browser or a database client
//! at it, and switching to another resource — or another cluster — must not
//! tear it down. So forwards live in the session, not in a view, and they are
//! listed by the session for as long as they run.
//!
//! One connection, one tunnel. `kubectl port-forward` works the same way, and
//! it matters: a tunnel is a WebSocket to the API server, and multiplexing
//! several client connections onto one would tangle their streams.

use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use tokio::net::TcpListener;

use crate::{Error, Result};

/// Identifies one forward for as long as it runs.
pub type ForwardId = u64;

/// A running forward, as the UI lists it.
#[derive(Debug, Clone)]
pub struct Forward {
    pub id: ForwardId,
    pub namespace: String,
    pub pod: String,
    pub remote_port: u16,
    /// The port that was actually bound, which is the one to connect to. Asking
    /// for 0 means "any free port", and then this is the only place the answer
    /// exists.
    pub local_port: u16,
    /// How many client connections are currently tunnelled.
    pub connections: usize,
    /// Why it stopped, if it did.
    pub failure: Option<String>,
}

impl Forward {
    /// What to paste into a browser.
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

/// The shared, mutable part of a running forward.
struct Running {
    forward: Mutex<Forward>,
    connections: Arc<AtomicUsize>,
    task: tokio::task::AbortHandle,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Every forward a cluster session is running.
#[derive(Default)]
pub struct Forwards {
    running: Mutex<HashMap<ForwardId, Arc<Running>>>,
    next_id: AtomicU64,
}

impl Forwards {
    /// Opens a local listener and tunnels everything that connects to it.
    ///
    /// `local_port` of 0 asks the operating system for a free one, which is
    /// what the UI offers by default: a port that is already taken is a
    /// failure at bind time and a confusing one.
    pub async fn open(
        &self,
        client: kube::Client,
        runtime: &tokio::runtime::Handle,
        namespace: String,
        pod: String,
        remote_port: u16,
        local_port: u16,
    ) -> Result<Forward> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, local_port)))
            .await
            .map_err(|error| Error::Forward {
                what: format!("could not listen on 127.0.0.1:{local_port}"),
                cause: error.to_string(),
            })?;

        let bound = listener
            .local_addr()
            .map_err(|error| Error::Forward {
                what: "could not read the local address".to_string(),
                cause: error.to_string(),
            })?
            .port();

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let connections = Arc::new(AtomicUsize::new(0));

        let forward = Forward {
            id,
            namespace: namespace.clone(),
            pod: pod.clone(),
            remote_port,
            local_port: bound,
            connections: 0,
            failure: None,
        };

        let task = runtime.spawn(accept_loop(
            listener,
            client,
            namespace,
            pod,
            remote_port,
            connections.clone(),
        ));

        tracing::info!(forward = %forward.describe(), "forwarding");

        self.running.lock().expect("forwards").insert(
            id,
            Arc::new(Running {
                forward: Mutex::new(forward.clone()),
                connections,
                task: task.abort_handle(),
            }),
        );

        Ok(forward)
    }

    /// Every forward, with its live connection count.
    pub fn list(&self) -> Vec<Forward> {
        let mut forwards: Vec<Forward> = self
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

        forwards.sort_by_key(|forward| forward.id);
        forwards
    }

    pub fn is_empty(&self) -> bool {
        self.running.lock().expect("forwards").is_empty()
    }

    /// Stops one forward. Dropping the handle aborts its task, which closes
    /// the listener and every tunnel through it.
    pub fn close(&self, id: ForwardId) {
        if let Some(running) = self.running.lock().expect("forwards").remove(&id) {
            tracing::info!(
                forward = %running.forward.lock().expect("forward").describe(),
                "stopped forwarding"
            );
        }
    }

    /// Stops the forwards pointing at a pod that no longer exists.
    ///
    /// A forward to a dead pod accepts connections and then fails each one,
    /// which looks like a broken service rather than a stale forward.
    pub fn close_for_pod(&self, namespace: &str, pod: &str) {
        let stale: Vec<ForwardId> = self
            .list()
            .into_iter()
            .filter(|forward| forward.namespace == namespace && forward.pod == pod)
            .map(|forward| forward.id)
            .collect();

        for id in stale {
            self.close(id);
        }
    }
}

/// Accepts local connections and gives each one its own tunnel.
async fn accept_loop(
    listener: TcpListener,
    client: kube::Client,
    namespace: String,
    pod: String,
    remote_port: u16,
    connections: Arc<AtomicUsize>,
) {
    let api: Api<Pod> = Api::namespaced(client, &namespace);

    loop {
        let Ok((mut socket, peer)) = listener.accept().await else {
            // The listener is gone; so is this forward.
            return;
        };

        let api = api.clone();
        let pod = pod.clone();
        let connections = connections.clone();

        tokio::spawn(async move {
            connections.fetch_add(1, Ordering::Relaxed);
            // The count has to come back down however this ends, including a
            // panic in the copy below.
            let _guard = Counted(connections);

            let mut forwarder = match api.portforward(&pod, &[remote_port]).await {
                Ok(forwarder) => forwarder,
                Err(error) => {
                    tracing::warn!(%pod, %peer, %error, "could not open the tunnel");
                    return;
                }
            };

            let Some(mut tunnel) = forwarder.take_stream(remote_port) else {
                tracing::warn!(%pod, port = remote_port, "the pod did not open that port");
                return;
            };

            if let Err(error) = tokio::io::copy_bidirectional(&mut socket, &mut tunnel).await {
                // A client closing its end mid-request is ordinary, not an
                // incident.
                tracing::debug!(%pod, %peer, %error, "tunnel closed");
            }
        });
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
                    forward: Mutex::new(Forward {
                        id,
                        ..forward(10000 + index as u16, "default", "api", port)
                    }),
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
                    forward: Mutex::new(Forward {
                        id,
                        ..forward(10000, "default", pod, port)
                    }),
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
