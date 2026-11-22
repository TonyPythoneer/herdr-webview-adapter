//! Standalone Rust browser implementation for the Herdr desktop host.
//! Herdr owns Tab/Pane persistence, native approval authority, and app packaging.
pub mod app_home;
pub mod assets;
pub mod browser;
pub mod browser_backend;
#[cfg(target_os = "macos")]
pub mod chrome_cdp;
#[cfg(target_os = "macos")]
pub mod browser_api;
#[cfg(target_os = "macos")]
pub mod chrome_view;
#[cfg(target_os = "macos")]
pub mod wk_agent;
#[cfg(target_os = "macos")]
pub mod wk_input;
#[cfg(target_os = "macos")]
pub mod wk_native;

#[cfg(target_os = "macos")]
pub mod browser_mcp;

#[cfg(target_os = "macos")]
pub mod browser_job;
