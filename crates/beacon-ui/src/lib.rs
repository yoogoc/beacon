//! Beacon's GPUI layer.
//!
//! Everything here runs on the GPUI foreground thread. Nothing here may block:
//! network work belongs to [`bridge::Bridge`], which owns the tokio runtime.

pub mod actions;
pub mod app;
pub mod bridge;
pub mod catalog;
pub mod cluster;
pub mod detail;
pub mod palette;
pub mod prompt;
pub mod status;
pub mod table;
pub mod terminal;
pub mod theme;

pub use app::BeaconApp;
pub use bridge::Bridge;
pub use cluster::ClusterView;
pub use table::ResourceTable;
