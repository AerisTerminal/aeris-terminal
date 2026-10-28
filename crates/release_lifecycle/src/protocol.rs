//! Release wire-protocol versions and the update-restart handshake tokens shared by the
//! stable launcher and the desktop.
//!
//! The launcher finishes every check of the prepared update before it prints
//! [`UPDATE_RESTART_READY`], and installs only after the desktop answers with
//! [`UPDATE_RESTART_COMMIT`]. Each token is one exact line; any other input aborts the restart.

use std::time::Duration;

/// Release-handling protocol this launcher implements. A signed manifest whose
/// `minimum_launcher_protocol` is higher is refused as incompatible. Bump it only
/// when a release needs launcher behavior older launchers lack; it is independent of
/// the product version.
pub const LAUNCHER_PROTOCOL_VERSION: u32 = 1;

/// How long the desktop waits for [`UPDATE_RESTART_READY`] while the launcher verifies the
/// prepared update: a channel fetch at its 15 s bound plus hashing the prepared and the active
/// release on a slow disk. The desktop stays usable meanwhile.
pub const UPDATE_RESTART_VERIFICATION_TIMEOUT: Duration = Duration::from_mins(2);

/// The launcher's line announcing a prepared update.
pub const UPDATE_RESTART_READY: &str = "AERIS_UPDATE_RESTART_READY_V1\n";

/// The desktop's line committing the restart after its state is saved.
pub const UPDATE_RESTART_COMMIT: &str = "AERIS_UPDATE_RESTART_COMMIT_V1\n";
