//! Beacon's GPUI layer.
//!
//! Everything here runs on the GPUI foreground thread. Nothing here may block:
//! network work belongs to [`bridge::Bridge`], which owns the tokio runtime.

pub mod actions;
mod activity;
mod aggregate_logs;
pub mod app;
mod app_logs;
mod argo;
mod argo_node;
pub mod bridge;
pub mod catalog;
pub mod cluster;
mod cluster_overview;
mod configuration_usage;
mod connections;
mod context_sidebar;
mod copyable_text;
mod create;
mod data_syntax;
mod debug_container;
pub mod detail;
#[cfg(all(test, feature = "ui-tests"))]
mod feature_test_support;
mod filters;
mod global_search;
mod helm_details;
mod icons;
mod ingress_links;
mod network;
mod node_pods;
mod overview;
pub mod palette;
mod pod_files;
mod pod_logs;
mod pod_tools;
mod preferences;
pub mod prompt;
mod related;
mod resource_compare;
mod rollout;
mod selection;
mod settings;
mod shortcuts;
mod sources;
pub mod status;
pub mod table;
mod table_preferences;
pub mod terminal;
pub mod theme;
mod tls;
pub mod updates;
mod web_links;
mod workspace;
mod yaml_folding;
mod yaml_review;

pub use app::BeaconApp;
pub use bridge::Bridge;
pub use cluster::ClusterView;
pub use icons::Assets;
pub use table::ResourceTable;
