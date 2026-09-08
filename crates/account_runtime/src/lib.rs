//! In-process account runtime for the native desktop application.
//!
//! Account authentication is a background service inside the desktop process.
//! It deliberately has no local socket, IPC protocol, process supervisor, or
//! resident-engine lifecycle.

mod account_service;

pub use account_service::{AccountService, AccountServiceConfig};
