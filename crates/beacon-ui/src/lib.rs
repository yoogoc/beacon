//! Beacon's GPUI layer.
//!
//! Everything here runs on the GPUI foreground thread. Nothing here may block:
//! network work belongs to [`bridge::Bridge`], which owns the tokio runtime.

pub mod app;
pub mod bridge;
pub mod catalog;
pub mod cluster;
pub mod status;
pub mod table;
pub mod theme;

pub use app::BeaconApp;
pub use bridge::Bridge;
pub use cluster::ClusterView;
pub use table::ResourceTable;
