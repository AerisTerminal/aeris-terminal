//! Safe, leaf-level operating-system primitives.
//!
//! Platform FFI is isolated below target-specific modules. Consumers never
//! need to use `unsafe` to access these capabilities.

pub mod fs;
pub mod network;
pub mod power;
pub mod process;
#[cfg(target_os = "windows")]
pub mod session;
#[cfg(target_os = "windows")]
pub mod trust;
pub mod ui;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
