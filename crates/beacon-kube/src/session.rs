//! One connected cluster.
//!
//! A [`ClusterSession`] is everything that happens against a single kubeconfig
//! context: the client, the health of the connection, and the watches that are
//! currently running. Several sessions run side by side without knowing about
//! each other, and dropping one stops all of its work -- which is what makes
//! switching contexts safe rather than a slow leak.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use kube::{
    Client,
    api::ApiResource,
    config::{Config, KubeConfigOptions},
    runtime::watcher,
};
use serde_json::Value;
use tokio::sync::watch as watch_channel;

use crate::{
    ClusterId, Error, Result,
    discovery::{Discovery, PrinterColumns, fetch_printer_columns},
    watch::{Registry, Subscription, WatchKey},
};

/// What the status bar shows about a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// The client exists but has not proved it can reach the API server yet.
    Connecting,
    Connected,
    /// Reachable, but something is failing -- an expired token, a kind we are
    /// not allowed to watch, a flapping network.
    Degraded {
        reason: String,
    },
}

impl Health {
    /// A word for the status bar.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Connecting => "Connecting",
            Self::Connected => "Connected",
            Self::Degraded { .. } => "Degraded",
        }
    }

    /// The detail behind the label, if there is one.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Degraded { reason } => Some(reason),
            _ => None,
        }
    }
}

pub struct ClusterSession {
    id: ClusterId,
    /// The API server URL, which is what distinguishes two contexts that have
    /// confusingly similar names.
    server: String,
    client: Client,
    discovery: Discovery,
    /// Filled in one kind at a time, as somebody opens them.
    printer_columns: Mutex<PrinterColumns>,
    registry: Arc<Registry>,
    health: Arc<HealthState>,
}

impl ClusterSession {
    /// Builds a client for a kubeconfig context and proves it can reach the
    /// API server.
    ///
    /// Must be awaited on the tokio runtime -- it authenticates, which for an
    /// `exec` context means running a subprocess, and it then makes a request.
    /// The runtime it runs on is the one every watch will use.
    pub async fn connect(id: ClusterId) -> Result<Self> {
        let options = KubeConfigOptions {
            context: Some(id.as_str().to_string()),
            ..Default::default()
        };

        let config = Config::from_kubeconfig(&options)
            .await
            .map_err(|error| Error::connect(&id, &error))?;
        let server = config.cluster_url.to_string();

        let client = Client::try_from(config).map_err(|error| Error::connect(&id, &error))?;

        // Connecting has to mean something. Without this the first failure
        // surfaces as an empty table, attributed to the resource kind rather
        // than to the credentials or the network.
        let version = client
            .apiserver_version()
            .await
            .map_err(|error| Error::connect(&id, &error))?;

        tracing::info!(
            context = %id,
            %server,
            version = %version.git_version,
            "connected"
        );

        // Part of connecting, not of opening the first view: a window that
        // cannot say what the cluster contains is not connected in any sense
        // the user cares about.
        let discovery = Discovery::load(&client)
            .await
            .map_err(|error| Error::connect(&id, &error))?;

        let health = Arc::new(HealthState::new(Health::Connected));
        let registry = Arc::new(Registry::new(
            client.clone(),
            tokio::runtime::Handle::current(),
            health.clone(),
        ));

        Ok(Self {
            id,
            server,
            client,
            discovery,
            printer_columns: Mutex::new(PrinterColumns::default()),
            registry,
            health,
        })
    }

    /// Everything this cluster can show.
    pub fn discovery(&self) -> &Discovery {
        &self.discovery
    }

    /// The `additionalPrinterColumns` a kind publishes for itself.
    ///
    /// Read from the CRD the first time a kind is opened and remembered after
    /// that, including the very common answer of "there are none".
    pub async fn printer_columns(self: Arc<Self>, resource: ApiResource) -> Option<Arc<Value>> {
        let gvk =
            kube::core::GroupVersionKind::gvk(&resource.group, &resource.version, &resource.kind);

        // Scoped so that no lock is held across the request below.
        if let Some(known) = self
            .printer_columns
            .lock()
            .expect("printer column cache")
            .get(&gvk)
        {
            return known.cloned();
        }

        let columns = fetch_printer_columns(&self.client, &resource).await;
        self.printer_columns
            .lock()
            .expect("printer column cache")
            .insert(gvk, columns.clone());
        columns
    }

    pub fn id(&self) -> &ClusterId {
        &self.id
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    /// Observes the connection's health. The current value is available
    /// immediately; `changed()` waits for the next one.
    pub fn health(&self) -> watch_channel::Receiver<Health> {
        self.health.sender.subscribe()
    }

    /// Starts receiving one kind's objects. See [`Subscription`].
    pub fn subscribe(&self, key: WatchKey) -> Subscription {
        self.registry.subscribe(key)
    }

    /// How many watches this session is running. Shown in the status bar, and
    /// the thing to assert on when checking that a context switch cleaned up.
    pub fn active_watches(&self) -> usize {
        self.registry.active()
    }
}

impl Drop for ClusterSession {
    fn drop(&mut self) {
        // Dropping the registry drops every `WatchHandle`, and each of those
        // aborts its task. Nothing outlives the session it belongs to.
        tracing::info!(
            context = %self.id,
            watches = self.registry.active(),
            "disconnecting"
        );
    }
}

/// The health of one session, as reported by everything running under it.
///
/// A session is degraded while any of its watches is failing, and recovers when
/// the last of them does. Counting rather than latching matters because a watch
/// that fails for its own reasons -- a kind the user cannot list -- must not
/// leave the whole cluster looking broken once the user navigates away from it.
pub(crate) struct HealthState {
    sender: watch_channel::Sender<Health>,
    failing: AtomicUsize,
}

impl HealthState {
    fn new(initial: Health) -> Self {
        Self {
            sender: watch_channel::Sender::new(initial),
            failing: AtomicUsize::new(0),
        }
    }

    /// A reporter for one watch. `what` is the watch's description, which is
    /// what the user sees as the reason.
    pub(crate) fn reporter(state: &Arc<Self>, what: String) -> WatchReporter {
        WatchReporter {
            state: state.clone(),
            what,
            failing: false,
        }
    }

    fn enter_failure(&self, reason: String) {
        self.failing.fetch_add(1, Ordering::SeqCst);
        self.sender.send_replace(Health::Degraded { reason });
    }

    fn leave_failure(&self) {
        if self.failing.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.sender.send_replace(Health::Connected);
        }
    }
}

/// One watch's view of the session's health.
///
/// Holds whether *this* watch is currently failing, so that a watch retrying in
/// a tight loop counts once rather than once per attempt.
pub(crate) struct WatchReporter {
    state: Arc<HealthState>,
    what: String,
    failing: bool,
}

impl WatchReporter {
    pub(crate) fn healthy(&mut self) {
        if std::mem::replace(&mut self.failing, false) {
            tracing::info!(watch = %self.what, "watch recovered");
            self.state.leave_failure();
        }
    }

    pub(crate) fn failed(&mut self, error: &watcher::Error) {
        if self.failing {
            // Already counted. `watcher` retries continuously, so this is the
            // common path while a cluster is unreachable.
            tracing::debug!(watch = %self.what, %error, "watch still failing");
            return;
        }

        tracing::warn!(watch = %self.what, %error, "watch failed");
        self.failing = true;
        self.state.enter_failure(format!("{}: {error}", self.what));
    }
}

impl Drop for WatchReporter {
    fn drop(&mut self) {
        // A watch can be aborted mid-failure. Without this the session would
        // stay degraded because of a watch that no longer exists.
        if self.failing {
            self.state.leave_failure();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Arc<HealthState> {
        Arc::new(HealthState::new(Health::Connected))
    }

    fn error() -> watcher::Error {
        watcher::Error::NoResourceVersion
    }

    #[test]
    fn one_failing_watch_degrades_the_session() {
        let state = state();
        let health = state.sender.subscribe();
        let mut pods = HealthState::reporter(&state, "Pod".into());

        pods.failed(&error());
        assert_eq!(health.borrow().label(), "Degraded");
        assert!(health.borrow().reason().unwrap().starts_with("Pod: "));

        pods.healthy();
        assert_eq!(*health.borrow(), Health::Connected);
    }

    /// A watch retrying in a loop reports every attempt. Counting each one
    /// would leave the session permanently degraded after it recovered.
    #[test]
    fn repeated_failures_from_one_watch_count_once() {
        let state = state();
        let health = state.sender.subscribe();
        let mut pods = HealthState::reporter(&state, "Pod".into());

        pods.failed(&error());
        pods.failed(&error());
        pods.failed(&error());
        pods.healthy();

        assert_eq!(*health.borrow(), Health::Connected);
    }

    /// The session recovers only once every failing watch has.
    #[test]
    fn the_session_recovers_with_the_last_watch() {
        let state = state();
        let health = state.sender.subscribe();
        let mut pods = HealthState::reporter(&state, "Pod".into());
        let mut nodes = HealthState::reporter(&state, "Node".into());

        pods.failed(&error());
        nodes.failed(&error());

        pods.healthy();
        assert_eq!(health.borrow().label(), "Degraded");

        nodes.healthy();
        assert_eq!(*health.borrow(), Health::Connected);
    }

    /// Navigating away aborts a watch wherever it happens to be, including in
    /// the middle of failing.
    #[test]
    fn dropping_a_failing_watch_clears_its_failure() {
        let state = state();
        let health = state.sender.subscribe();

        let mut pods = HealthState::reporter(&state, "Pod".into());
        pods.failed(&error());
        assert_eq!(health.borrow().label(), "Degraded");

        drop(pods);
        assert_eq!(*health.borrow(), Health::Connected);
    }
}
