//! Off-GPUI commands for the authenticated resident-engine control plane.

use std::{
    sync::mpsc::{self, SyncSender, TrySendError},
    thread,
};

use axiusflow_local_engine_client::{connect_or_start_engine, sibling_engine_executable};
use axiusflow_local_engine_protocol::InstallProviderInstrument;

const COMMAND_CAPACITY: usize = 4;

/// Bounded nonblocking sender for canonical catalog installs.
#[derive(Clone)]
pub(super) struct EngineCatalogClient {
    commands: SyncSender<InstallProviderInstrument>,
}

impl EngineCatalogClient {
    /// Starts the blocking engine IPC owner away from the GPUI foreground thread.
    pub(super) fn start() -> Result<Self, String> {
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        thread::Builder::new()
            .name("axiusflow-engine-catalog-client".to_string())
            .spawn(move || {
                while let Ok(instrument) = receiver.recv() {
                    let result = sibling_engine_executable()
                        .and_then(|executable| connect_or_start_engine(&executable))
                        .and_then(|mut client| {
                            client.install_provider_instrument(instrument).map(|_| ())
                        });
                    if result.is_err() {
                        eprintln!("resident engine instrument install failed");
                    }
                }
            })
            .map_err(|_| "resident engine catalog client is unavailable".to_string())?;
        Ok(Self { commands })
    }

    /// Queues one canonical instrument without blocking GPUI.
    pub(super) fn try_install(&self, instrument: InstallProviderInstrument) -> Result<(), String> {
        self.commands
            .try_send(instrument)
            .map_err(|error| match error {
                TrySendError::Full(_) => "resident engine catalog queue is busy".to_string(),
                TrySendError::Disconnected(_) => {
                    "resident engine catalog client stopped".to_string()
                }
            })
    }
}
