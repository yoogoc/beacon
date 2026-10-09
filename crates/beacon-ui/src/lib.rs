//! Beacon's GPUI layer.
//!
//! Everything here runs on the GPUI foreground thread. Nothing here may block:
//! network work belongs to [`bridge::Bridge`], which owns the tokio runtime.

pub mod actions;
mod activity;
pub mod app;
mod app_logs;
mod argo;
mod argo_node;
pub mod bridge;
pub mod catalog;
pub mod cluster;
mod connections;
mod copyable_text;
mod create;
pub mod detail;
mod filters;
mod icons;
mod node_pods;
mod overview;
pub mod palette;
mod pod_tools;
mod preferences;
pub mod prompt;
mod settings;
mod shortcuts;
pub mod status;
pub mod table;
pub mod terminal;
pub mod theme;
mod tls;
pub mod updates;
mod workspace;
mod yaml_folding;
mod yaml_review;

pub use app::BeaconApp;
pub use bridge::Bridge;
pub use cluster::ClusterView;
pub use table::ResourceTable;
