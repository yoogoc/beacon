//! GPUI Kit 0.7.3 DataTable with the right-pinning change from upstream #3833.
//! Base revision: 67265a36e260ffe03c5aaebdcd8d654dbf892eab.
//! Right-pinning revision: b92bb66d148fca10e15d1e3fb77d565ac4d306ec.
//! Only the virtualized table is backported. The existing GPUI Kit supplies
//! keyboard bindings, scrolling, menus, theme, and shared types.
//! Beacon's external scrollbar uses the widths of both pinned regions.
mod column;
mod delegate;
mod loading;
mod state;
mod view;
pub use column::*;
pub use delegate::*;
pub use state::*;
pub use view::*;
