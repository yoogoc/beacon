//! Beacon's Kubernetes domain layer.
//!
//! This crate owns everything that talks to a cluster: kubeconfig handling,
//! discovery, watches, the object store and the write operations. It has no
//! dependency on GPUI, and it must stay that way -- the UI is one consumer of
//! this crate, a headless CLI or an integration test is another.

pub mod config;
pub mod discovery;
pub mod error;
pub mod resources;
pub mod session;
pub mod shell_env;
pub mod store;
pub mod watch;

pub use discovery::{Discovery, Kind};
pub use error::{Error, Result};
/// Re-exported so that consumers can name and build the types this crate hands
/// them without taking their own `kube` dependency -- and without it being a
/// different `kube`, which is a whole class of confusing type errors.
pub use kube::api::{ApiResource, DynamicObject};
pub use kube::core::GroupVersionKind;
pub use session::{ClusterSession, Health};
pub use store::{Delta, DeltaBatch, ObjectRef, ResourceStore};
pub use watch::{Subscription, WatchKey};

use std::fmt;

/// Identifies one cluster connection. This is the kubeconfig context name,
/// which is the only identifier the user actually recognises.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClusterId(String);

impl ClusterId {
    pub fn new(context_name: impl Into<String>) -> Self {
        Self(context_name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClusterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
