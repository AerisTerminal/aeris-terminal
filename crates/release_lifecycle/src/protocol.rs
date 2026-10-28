//! Update-restart handshake tokens shared by the stable launcher and the desktop.
//!
//! The launcher prints [`UPDATE_RESTART_READY`] once its update is prepared, and installs
//! only after the desktop answers with [`UPDATE_RESTART_COMMIT`]. Each token is one exact
//! line; any other input aborts the restart.

/// The launcher's line announcing a prepared update.
pub const UPDATE_RESTART_READY: &str = "AERIS_UPDATE_RESTART_READY_V1\n";

/// The desktop's line committing the restart after its state is saved.
pub const UPDATE_RESTART_COMMIT: &str = "AERIS_UPDATE_RESTART_COMMIT_V1\n";
