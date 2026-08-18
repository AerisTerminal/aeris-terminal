//! Blocking authenticated client for the resident Axiusflow engine.
//!
//! The desktop runs this boundary only on background workers. It owns local
//! process discovery, installation credentials, framing, and typed commands;
//! it contains no provider or GPUI behavior.

use std::{
    collections::VecDeque,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use axiusflow_engine_protocol::{
    AttachClient, ClientHello, ClientKind, DetachClient, EngineReady, Envelope, EnvelopeDecoder,
    InstallProviderInstrument, PROTOCOL_VERSION, PollMarketEvent, ProviderInstrumentInstalled,
    RegisterConsumer, RemoveConsumer, ResourceMode, RestoreWorkspace, SearchProviderInstruments,
    SelectProviderInstrument, SeriesDemand, SeriesKey, SetEngineResourceMode, SetSelection,
    SetViewport, ShutdownEngine, ViewportDemand, VisibilityDemand, WorkspaceState, encode_envelope,
    envelope,
};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use interprocess::local_socket::{GenericNamespaced, ToNsName as _, prelude::*};
use zeroize::Zeroizing;

/// Stable per-user local socket endpoint generation.
pub const ENGINE_SOCKET_NAME: &str = "axiusflow-engine-v8";
/// Exact entropy required for the installation credential.
pub const INSTALLATION_TOKEN_BYTES: usize = 32;
/// Maximum time allowed for a newly spawned engine to publish readiness.
pub const ENGINE_START_TIMEOUT: Duration = Duration::from_secs(3);

const ENGINE_VAULT_SERVICE: &str = "com.axiusflow.engine";
const ENGINE_TOKEN_KEY: &str = "local-ipc-token-v1";

/// Loads the installation credential from the native vault, creating it once.
///
/// # Errors
/// Returns an error when the native credential store is unavailable or malformed.
pub fn native_installation_token() -> Result<Zeroizing<Vec<u8>>, String> {
    let vault = NativeCredentialVault::new(ENGINE_VAULT_SERVICE).map_err(redacted_vault_error)?;
    load_or_create_installation_token(&vault)
}

/// Authenticates to the existing resident engine and requests complete shutdown.
///
/// This never starts an absent engine and is blocking, so callers must keep it
/// away from presentation threads.
///
/// # Errors
/// Returns an error when credentials, connection, authentication, or shutdown fail.
pub fn shutdown_running_engine() -> Result<(), String> {
    let token = native_installation_token()?;
    EngineClient::connect(ENGINE_SOCKET_NAME, token.as_slice())?.shutdown_engine()
}

/// Loads or creates the installation credential in a supplied credential vault.
///
/// # Errors
/// Returns an error when vault access fails or a stored credential has the wrong length.
pub fn load_or_create_installation_token<V>(vault: &V) -> Result<Zeroizing<Vec<u8>>, String>
where
    V: CredentialVault,
{
    if let Some(token) = vault.load(ENGINE_TOKEN_KEY).map_err(redacted_vault_error)? {
        if token.len() != INSTALLATION_TOKEN_BYTES {
            return Err("native engine credential has an invalid length".to_string());
        }
        return Ok(Zeroizing::new(token));
    }
    let mut token = Zeroizing::new(vec![0_u8; INSTALLATION_TOKEN_BYTES]);
    getrandom::fill(token.as_mut_slice()).map_err(|_| "system CSPRNG unavailable".to_string())?;
    vault
        .store(ENGINE_TOKEN_KEY, token.as_slice())
        .map_err(redacted_vault_error)?;
    Ok(token)
}

fn redacted_vault_error<E>(_error: E) -> String {
    "native engine credential store is unavailable".to_string()
}

/// Authenticated local-engine client connection.
pub struct EngineClient {
    connection: FramedConnection,
    ready: EngineReady,
}

struct EngineConnectionFailure {
    detail: String,
    endpoint_reached: bool,
}

impl EngineClient {
    /// Connects and authenticates against a named engine endpoint.
    ///
    /// # Errors
    /// Returns an error when connection, framing, authentication, or negotiation fails.
    pub fn connect(name: &str, installation_token: &[u8]) -> Result<Self, String> {
        Self::connect_with_reachability(name, installation_token).map_err(|failure| failure.detail)
    }

    fn connect_with_reachability(
        name: &str,
        installation_token: &[u8],
    ) -> Result<Self, EngineConnectionFailure> {
        let name =
            name.to_ns_name::<GenericNamespaced>()
                .map_err(|error| EngineConnectionFailure {
                    detail: error.to_string(),
                    endpoint_reached: false,
                })?;
        let stream = LocalSocketStream::connect(name).map_err(|error| EngineConnectionFailure {
            detail: error.to_string(),
            endpoint_reached: false,
        })?;
        let mut connection = FramedConnection::new(stream).map_err(reached_failure)?;
        connection
            .send(envelope::Payload::ClientHello(ClientHello {
                protocol_version: PROTOCOL_VERSION,
                installation_token: installation_token.to_vec(),
                client_kind: ClientKind::Ui as i32,
            }))
            .map_err(reached_failure)?;
        let ready = match connection.receive().map_err(reached_failure)? {
            envelope::Payload::EngineReady(ready) => ready,
            envelope::Payload::Fault(fault) => {
                return Err(EngineConnectionFailure {
                    detail: fault.redacted_detail,
                    endpoint_reached: true,
                });
            }
            _ => {
                return Err(EngineConnectionFailure {
                    detail: "engine did not complete readiness negotiation".to_string(),
                    endpoint_reached: true,
                });
            }
        };
        Ok(Self { connection, ready })
    }

    /// Returns the negotiated engine readiness state.
    #[must_use]
    pub const fn ready(&self) -> &EngineReady {
        &self.ready
    }

    /// Requests the engine-owned workspace state.
    ///
    /// # Errors
    /// Returns an error when the connection fails or the reply is unexpected.
    pub fn restore_workspace(&mut self) -> Result<WorkspaceState, String> {
        self.connection.send(envelope::Payload::RestoreWorkspace(
            RestoreWorkspace::default(),
        ))?;
        match self.connection.receive()? {
            envelope::Payload::WorkspaceState(workspace) => Ok(workspace),
            envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
            _ => Err("engine returned an unexpected workspace reply".to_string()),
        }
    }

    /// Changes the resident engine's operational resource mode.
    ///
    /// # Errors
    /// Returns an error when the authenticated command fails or its reply is invalid.
    pub fn set_engine_resource_mode(
        &mut self,
        mode: ResourceMode,
    ) -> Result<WorkspaceState, String> {
        self.connection
            .send(envelope::Payload::SetEngineResourceMode(
                SetEngineResourceMode {
                    resource_mode: mode as i32,
                },
            ))?;
        self.receive_workspace()
    }

    /// Requests complete resident-engine shutdown and consumes this connection.
    ///
    /// # Errors
    /// Returns an error when the authenticated command fails or is not acknowledged.
    pub fn shutdown_engine(mut self) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::ShutdownEngine(ShutdownEngine {}))?;
        match self.connection.receive()? {
            envelope::Payload::Goodbye(_) => Ok(()),
            envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
            _ => Err("engine returned an unexpected shutdown reply".to_string()),
        }
    }

    /// Applies a revision-fenced market selection.
    ///
    /// # Errors
    /// Returns an error when the revision is stale, transport fails, or the reply is invalid.
    pub fn set_selection(
        &mut self,
        market: String,
        interval_seconds: u32,
        workspace_revision: u64,
        selection_generation: u64,
    ) -> Result<WorkspaceState, String> {
        self.set_provider_selection(
            String::new(),
            market,
            interval_seconds,
            workspace_revision,
            selection_generation,
        )
    }

    /// Applies a revision-fenced provider and market selection.
    ///
    /// # Errors
    /// Returns an error when the revision is stale, transport fails, or the reply is invalid.
    pub fn set_provider_selection(
        &mut self,
        provider: String,
        market: String,
        interval_seconds: u32,
        workspace_revision: u64,
        selection_generation: u64,
    ) -> Result<WorkspaceState, String> {
        self.connection
            .send(envelope::Payload::SetSelection(SetSelection {
                market,
                interval_seconds,
                workspace_revision,
                selection_generation,
                provider,
            }))?;
        self.receive_workspace()
    }

    /// Persists the stable viewport for the active selection without changing its revision.
    ///
    /// # Errors
    /// Returns an error when the selection generation is stale or persistence fails.
    pub fn set_viewport(
        &mut self,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
        selection_generation: u64,
    ) -> Result<WorkspaceState, String> {
        self.connection
            .send(envelope::Payload::SetViewport(SetViewport {
                start_unix_nanos,
                end_unix_nanos,
                selection_generation,
            }))?;
        self.receive_workspace()
    }

    fn receive_workspace(&mut self) -> Result<WorkspaceState, String> {
        match self.connection.receive()? {
            envelope::Payload::WorkspaceState(workspace) => Ok(workspace),
            envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
            _ => Err("engine returned an unexpected workspace reply".to_string()),
        }
    }

    /// Attaches one stable desktop lifetime to engine-owned market state.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn attach_client(&mut self, client_id: u64) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::AttachClient(AttachClient { client_id }))
    }

    /// Registers one independently generated chart consumer.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn register_consumer(
        &mut self,
        client_id: u64,
        workspace_id: u64,
        consumer_id: u64,
    ) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::RegisterConsumer(RegisterConsumer {
                client_id,
                workspace_id,
                consumer_id,
            }))
    }

    /// Replaces one consumer's authoritative bar-series demand.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn set_series_demand(
        &mut self,
        consumer_id: u64,
        generation: u64,
        series: SeriesKey,
    ) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::SeriesDemand(SeriesDemand {
                consumer_id,
                generation,
                series: Some(series),
            }))
    }

    /// Installs one adapter-resolved provider instrument in the resident engine catalog.
    ///
    /// # Errors
    /// Returns an error when the install is stale, invalid, or cannot cross the local IPC boundary.
    pub fn install_provider_instrument(
        &mut self,
        instrument: InstallProviderInstrument,
    ) -> Result<ProviderInstrumentInstalled, String> {
        self.connection
            .send(envelope::Payload::InstallProviderInstrument(instrument))?;
        match self.connection.receive()? {
            envelope::Payload::ProviderInstrumentInstalled(installed) => Ok(installed),
            envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
            _ => Err("engine returned an unexpected instrument install reply".to_string()),
        }
    }

    /// Schedules one bounded provider-neutral instrument search.
    ///
    /// Results are returned through [`Self::poll_market_event`].
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn search_provider_instruments(
        &mut self,
        request: SearchProviderInstruments,
    ) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::SearchProviderInstruments(request))
    }

    /// Schedules one exact provider-neutral instrument selection.
    ///
    /// Results are returned through [`Self::poll_market_event`].
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn select_provider_instrument(
        &mut self,
        request: SelectProviderInstrument,
    ) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::SelectProviderInstrument(request))
    }

    /// Updates the visible range for the exact current consumer generation.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn set_market_viewport(
        &mut self,
        consumer_id: u64,
        generation: u64,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::ViewportDemand(ViewportDemand {
                consumer_id,
                generation,
                start_unix_nanos,
                end_unix_nanos,
            }))
    }

    /// Updates one consumer's presentation priority without changing market demand.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn set_market_visibility(&mut self, consumer_id: u64, visible: bool) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::VisibilityDemand(VisibilityDemand {
                consumer_id,
                visible,
            }))
    }

    /// Receives the next market response from the authenticated engine session.
    ///
    /// # Errors
    /// Returns an error for connection, framing, protocol-version, or payload failure.
    pub fn receive_market_event(&mut self) -> Result<envelope::Payload, String> {
        self.connection.receive()
    }

    /// Polls at most one bounded publication for an attached market consumer.
    ///
    /// # Errors
    /// Returns an error when the authenticated exchange cannot complete.
    pub fn poll_market_event(
        &mut self,
        consumer_id: u64,
    ) -> Result<Option<envelope::Payload>, String> {
        self.connection
            .send(envelope::Payload::PollMarketEvent(PollMarketEvent {
                consumer_id,
            }))?;
        match self.connection.receive()? {
            envelope::Payload::MarketEventIdle(idle) if idle.consumer_id == consumer_id => Ok(None),
            envelope::Payload::MarketEventIdle(_) => {
                Err("engine returned market idle for another consumer".to_string())
            }
            payload => Ok(Some(payload)),
        }
    }

    /// Removes one consumer without affecting shared engine state.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn remove_market_consumer(&mut self, consumer_id: u64) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::RemoveConsumer(RemoveConsumer {
                consumer_id,
            }))
    }

    /// Releases all market demand owned by one desktop lifetime.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn detach_client(&mut self, client_id: u64) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::DetachClient(DetachClient { client_id }))
    }
}

fn reached_failure(detail: String) -> EngineConnectionFailure {
    EngineConnectionFailure {
        detail,
        endpoint_reached: true,
    }
}

/// Resolves the engine executable installed beside the current desktop binary.
///
/// # Errors
/// Returns an error when the current executable path has no parent directory.
pub fn sibling_engine_executable() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let parent = current
        .parent()
        .ok_or_else(|| "current executable has no installation directory".to_string())?;
    Ok(parent.join(format!("axiusflow_engine{}", std::env::consts::EXE_SUFFIX)))
}

/// Attaches to the resident engine or starts the installed sibling and retries readiness.
///
/// This function is blocking and must run away from the UI thread.
///
/// # Errors
/// Returns an error when credentials, process launch, or readiness negotiation fail.
pub fn connect_or_start_engine(engine_executable: &Path) -> Result<EngineClient, String> {
    let token = native_installation_token()?;
    connect_or_start_engine_named(
        ENGINE_SOCKET_NAME,
        engine_executable,
        token.as_slice(),
        ENGINE_START_TIMEOUT,
    )
}

fn connect_or_start_engine_named(
    socket_name: &str,
    engine_executable: &Path,
    installation_token: &[u8],
    start_timeout: Duration,
) -> Result<EngineClient, String> {
    let mut engine_started = false;
    let mut last_error =
        match EngineClient::connect_with_reachability(socket_name, installation_token) {
            Ok(client) => return Ok(client),
            Err(failure) => {
                if !failure.endpoint_reached {
                    start_engine_process(engine_executable)?;
                    engine_started = true;
                }
                failure.detail
            }
        };
    let deadline = Instant::now() + start_timeout;
    while Instant::now() < deadline {
        match EngineClient::connect_with_reachability(socket_name, installation_token) {
            Ok(client) => return Ok(client),
            Err(failure) => {
                if !failure.endpoint_reached && !engine_started {
                    start_engine_process(engine_executable)?;
                    engine_started = true;
                }
                last_error = failure.detail;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(last_error)
}

fn start_engine_process(engine_executable: &Path) -> Result<(), String> {
    let mut command = Command::new(engine_executable);
    configure_background_process(&mut command);
    command.spawn().map_err(|_| {
        "resident engine could not be started from the installation directory".to_string()
    })?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn configure_background_process(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn configure_background_process(_command: &mut Command) {}

struct FramedConnection {
    stream: LocalSocketStream,
    decoder: EnvelopeDecoder,
    pending: VecDeque<Envelope>,
}

impl FramedConnection {
    fn new(stream: LocalSocketStream) -> Result<Self, String> {
        Ok(Self {
            stream,
            decoder: EnvelopeDecoder::try_new().map_err(|error| error.to_string())?,
            pending: VecDeque::new(),
        })
    }

    fn send(&mut self, payload: envelope::Payload) -> Result<(), String> {
        let frame = encode_envelope(&Envelope {
            protocol_version: PROTOCOL_VERSION,
            payload: Some(payload),
        })
        .map_err(|error| error.to_string())?;
        self.stream
            .write_all(&frame)
            .map_err(|error| error.to_string())?;
        self.stream.flush().map_err(|error| error.to_string())
    }

    fn receive(&mut self) -> Result<envelope::Payload, String> {
        loop {
            if let Some(envelope) = self.pending.pop_front() {
                return envelope
                    .payload
                    .ok_or_else(|| "engine message has no payload".to_string());
            }
            let mut chunk = [0_u8; 16 * 1024];
            let count = self
                .stream
                .read(&mut chunk)
                .map_err(|error| error.to_string())?;
            if count == 0 {
                return Err("local engine connection closed".to_string());
            }
            self.pending.extend(
                self.decoder
                    .push(&chunk[..count])
                    .map_err(|error| error.to_string())?,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    use interprocess::local_socket::{
        GenericNamespaced, ListenerOptions, ToNsName as _, prelude::*,
    };

    use super::{ENGINE_SOCKET_NAME, connect_or_start_engine_named};

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn protocol_socket_name_tracks_the_active_version() {
        assert_eq!(ENGINE_SOCKET_NAME, "axiusflow-engine-v8");
    }

    #[test]
    fn reached_endpoint_is_retried_without_spawning_another_engine() {
        let socket_name = format!(
            "axiusflow-engine-client-test-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        );
        let name = socket_name
            .as_str()
            .to_ns_name::<GenericNamespaced>()
            .expect("create listener name");
        let listener = ListenerOptions::new()
            .name(name)
            .create_sync()
            .expect("bind occupied endpoint");
        let accepting = Arc::new(AtomicBool::new(true));
        let server_accepting = Arc::clone(&accepting);
        let server = thread::spawn(move || {
            while server_accepting.load(Ordering::Acquire) {
                let stream = listener.accept().expect("accept probe connection");
                drop(stream);
            }
        });

        let timeout = Duration::from_millis(80);
        let started = Instant::now();
        let error = connect_or_start_engine_named(
            &socket_name,
            Path::new("engine-executable-that-does-not-exist"),
            &[7_u8; 32],
            timeout,
        )
        .err()
        .expect("closed occupied endpoint cannot become ready");

        assert!(started.elapsed() >= timeout);
        assert_ne!(
            error,
            "resident engine could not be started from the installation directory"
        );
        accepting.store(false, Ordering::Release);
        let name = socket_name
            .as_str()
            .to_ns_name::<GenericNamespaced>()
            .expect("create socket name");
        drop(LocalSocketStream::connect(name).expect("wake accepting server"));
        server.join().expect("join accepting server");
    }
}
