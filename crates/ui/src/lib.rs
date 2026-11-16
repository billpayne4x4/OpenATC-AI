//! Shared ATC interface for the simulator plugin and desktop preview.

/// Plugin build version with the compile timestamp.
pub const VERSION: &str = concat!(
    env!("OPENATC_RELEASE"),
    " (",
    env!("CARGO_PKG_VERSION"),
    " · ",
    env!("OPENATC_BUILD"),
    ")"
);

mod arrival;
pub mod client;
pub mod clipboard;
mod debug;
pub mod interface;
pub mod maps;
mod missions;
pub mod pages;
mod sectors;
mod setup;
mod theme;
mod traffic;
pub mod widgets;
