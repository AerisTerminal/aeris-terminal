//! Resident engine process boundary and authenticated local sessions.

use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    sync::{Arc, Mutex},
};

use axiusflow_local_engine_protocol::{
    ClientHello, ClientKind, EngineFaultCode, EngineReady, Envelope, EnvelopeDecoder, Fault,
    Goodbye, PROTOCOL_VERSION, ResourceMode, RestoreWorkspace, SetSelection, SetWatchlist,
    WorkspaceState, encode_envelope, envelope,
};
use axiusflow_platform_runtime::{CredentialVault, NativeCredentialVault};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName as _, prelude::*};
use zeroize::Zeroizing;

/// Stable per-user local socket name for protocol version one.
pub const ENGINE_SOCKET_NAME: &str = "axiusflow-engine-v1";
/// Exact entropy required for the installation credential.
pub const INSTALLATION_TOKEN_BYTES: usize = 32;

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

/// Shared resident-engine state visible to authenticated clients.
#[derive(Clone, Debug)]
pub struct EngineState {
    workspace: Arc<Mutex<WorkspaceState>>,
}

impl Default for EngineState {
    fn default() -> Self {
        Self {
            workspace: Arc::new(Mutex::new(WorkspaceState {
                provider: "coinbase".to_string(),
                market: "BTC-USD".to_string(),
                interval_seconds: 60,
                watchlist: vec!["BTC-USD".to_string(), "ETH-USD".to_string()],
                workspace_revision: 0,
                warm_mode_enabled: true,
                resource_mode: ResourceMode::Warm as i32,
            })),
        }
    }
}

impl EngineState {
    /// Returns a consistent copy of the current workspace state.
    #[must_use]
    pub fn workspace(&self) -> WorkspaceState {
        self.workspace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Authenticated local-engine client connection.
pub struct EngineClient {
    connection: FramedConnection,
    ready: EngineReady,
}

impl EngineClient {
    /// Connects and authenticates against a named engine endpoint.
    ///
    /// # Errors
    /// Returns an error when connection, framing, authentication, or negotiation fails.
    pub fn connect(name: &str, installation_token: &[u8]) -> Result<Self, String> {
        let name = name
            .to_ns_name::<GenericNamespaced>()
            .map_err(|error| error.to_string())?;
        let stream = LocalSocketStream::connect(name).map_err(|error| error.to_string())?;
        let mut connection = FramedConnection::new(stream)?;
        connection.send(envelope::Payload::ClientHello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            installation_token: installation_token.to_vec(),
            client_kind: ClientKind::Ui as i32,
        }))?;
        let ready = match connection.receive()? {
            envelope::Payload::EngineReady(ready) => ready,
            envelope::Payload::Fault(fault) => return Err(fault.redacted_detail),
            _ => return Err("engine did not complete readiness negotiation".to_string()),
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
        self.connection
            .send(envelope::Payload::SetSelection(SetSelection {
                market,
                interval_seconds,
                workspace_revision,
                selection_generation,
            }))?;
        self.receive_workspace()
    }

    /// Replaces the engine-owned watchlist under workspace revision fencing.
    ///
    /// # Errors
    /// Returns an error when the revision is stale, transport fails, or the reply is invalid.
    pub fn set_watchlist(
        &mut self,
        markets: Vec<String>,
        workspace_revision: u64,
    ) -> Result<WorkspaceState, String> {
        self.connection
            .send(envelope::Payload::SetWatchlist(SetWatchlist {
                markets,
                workspace_revision,
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
}

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

/// Creates the engine's single-instance local listener.
///
/// # Errors
/// Returns an I/O error if another engine owns the name or the OS rejects it.
pub fn bind_listener(name: &str) -> io::Result<LocalSocketListener> {
    let name = name.to_ns_name::<GenericNamespaced>()?;
    ListenerOptions::new().name(name).create_sync()
}

/// Handles one client against isolated default state. Intended for probes and tests.
///
/// # Errors
/// Returns an error for I/O, framing, or malformed-message failures.
pub fn serve_client(
    stream: LocalSocketStream,
    installation_token: &[u8],
    engine_epoch: u64,
) -> Result<(), String> {
    serve_client_with_state(
        stream,
        installation_token,
        engine_epoch,
        &EngineState::default(),
    )
}

/// Serves one authenticated client against shared resident-engine state.
///
/// # Errors
/// Returns an error for I/O, framing, authentication setup, or malformed requests.
pub fn serve_client_with_state(
    stream: LocalSocketStream,
    installation_token: &[u8],
    engine_epoch: u64,
    state: &EngineState,
) -> Result<(), String> {
    if installation_token.len() != INSTALLATION_TOKEN_BYTES {
        return Err("installation credential has an invalid length".to_string());
    }
    let mut connection = FramedConnection::new(stream)?;
    let envelope::Payload::ClientHello(hello) = connection.receive()? else {
        return Err("client hello must be the first engine message".to_string());
    };
    if ClientKind::try_from(hello.client_kind).is_err() {
        return Err("client kind is invalid".to_string());
    }
    if !constant_time_equals(&hello.installation_token, installation_token) {
        connection.send(envelope::Payload::Fault(Fault {
            code: EngineFaultCode::Unauthenticated as i32,
            redacted_detail: "local engine authentication failed".to_string(),
        }))?;
        return Ok(());
    }
    connection.send(envelope::Payload::EngineReady(EngineReady {
        protocol_version: PROTOCOL_VERSION,
        engine_epoch,
        workspace_revision: state.workspace().workspace_revision,
    }))?;
    serve_authenticated_session(&mut connection, state)
}

fn serve_authenticated_session(
    connection: &mut FramedConnection,
    state: &EngineState,
) -> Result<(), String> {
    loop {
        let payload = match connection.receive() {
            Ok(payload) => payload,
            Err(error) if error == "local engine connection closed" => return Ok(()),
            Err(error) => return Err(error),
        };
        match payload {
            envelope::Payload::RestoreWorkspace(_) => {
                connection.send(envelope::Payload::WorkspaceState(state.workspace()))?;
            }
            envelope::Payload::SetSelection(selection) => {
                apply_selection(state, selection, connection)?;
            }
            envelope::Payload::SetWatchlist(watchlist) => {
                apply_watchlist(state, watchlist, connection)?;
            }
            envelope::Payload::Goodbye(_) => {
                connection.send(envelope::Payload::Goodbye(Goodbye {
                    reason: "client session closed".to_string(),
                }))?;
                return Ok(());
            }
            _ => connection.send(envelope::Payload::Fault(Fault {
                code: EngineFaultCode::MalformedMessage as i32,
                redacted_detail: "message is invalid in the current engine state".to_string(),
            }))?,
        }
    }
}

fn apply_selection(
    state: &EngineState,
    selection: SetSelection,
    connection: &mut FramedConnection,
) -> Result<(), String> {
    let mut workspace = state
        .workspace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if selection.workspace_revision != workspace.workspace_revision {
        return connection.send(stale_workspace_fault());
    }
    workspace.market = selection.market;
    workspace.interval_seconds = selection.interval_seconds;
    workspace.workspace_revision = workspace.workspace_revision.saturating_add(1);
    connection.send(envelope::Payload::WorkspaceState(workspace.clone()))
}

fn apply_watchlist(
    state: &EngineState,
    watchlist: SetWatchlist,
    connection: &mut FramedConnection,
) -> Result<(), String> {
    let mut workspace = state
        .workspace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if watchlist.workspace_revision != workspace.workspace_revision {
        return connection.send(stale_workspace_fault());
    }
    workspace.watchlist = watchlist.markets;
    workspace.workspace_revision = workspace.workspace_revision.saturating_add(1);
    connection.send(envelope::Payload::WorkspaceState(workspace.clone()))
}

fn stale_workspace_fault() -> envelope::Payload {
    envelope::Payload::Fault(Fault {
        code: EngineFaultCode::Cancelled as i32,
        redacted_detail: "workspace revision is stale".to_string(),
    })
}

fn constant_time_equals(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}
