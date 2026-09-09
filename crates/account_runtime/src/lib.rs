//! In-process account runtime for the native desktop application.
//!
//! Account authentication is a background service inside the desktop process.
//! It deliberately has no secondary local transport, process supervisor, or
//! secondary-engine lifecycle.

mod account_service;

pub use account_service::{AccountService, AccountServiceConfig};
