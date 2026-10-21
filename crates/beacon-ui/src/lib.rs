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
mod argo_canvas;
mod argo_node;
pub mod bridge;
mod bulk_restart;
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
mod debug_stop;
pub mod detail;
#[cfg(all(test, feature = "ui-tests"))]
mod feature_test_support;
mod filters;
mod forward_dialog;
mod global_search;
mod helm_details;
mod i18n;
mod icons;
mod ingress_links;
mod job_actions;
mod log_workbench;
mod network;
mod node_actions;
mod node_pods;
mod overview;
pub mod palette;
mod permissions;
mod pod_files;
mod pod_logs;
mod pod_tools;
mod preferences;
pub mod prompt;
mod related;
mod resource_compare;
mod resource_menu;
mod resource_metrics;
mod rollout;
mod selection;
mod service_account_secret;
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
pub mod window_state;
mod workflow_actions;
mod workspace;
mod yaml_assistance;
mod yaml_folding;
mod yaml_language;
mod yaml_review;

pub use app::BeaconApp;
pub use bridge::Bridge;
pub use cluster::ClusterView;
pub use icons::Assets;
pub use table::ResourceTable;

rust_i18n::i18n!("locales", fallback = "en");
