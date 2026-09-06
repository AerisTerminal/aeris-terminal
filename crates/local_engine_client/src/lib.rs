//! Blocking authenticated client for the resident Axiusflow engine.
//!
//! The desktop runs this boundary only on background workers. It owns local
//! process discovery, installation credentials, framing, and typed commands;
//! it contains no provider or GPUI behavior.

use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

use axiusflow_engine_protocol::{
    AccountView, AttachClient, BeginLogin, CancelLogin, ClientHello, ClientKind,
    ConsumerResourceClass, DetachClient, EngineLifetimeMode, EngineReady, EngineStatus, Envelope,
    EnvelopeDecoder, GetAccountStatus, GetEngineStatus, InstallProviderInstrument,
    LIFECYCLE_CONTRACT_REVISION, LoginAuthorization, PROTOCOL_VERSION, ProviderInstrumentInstalled,
    RegisterConsumer, RemoveConsumer, ResourceMode, RestoreWorkspace, SearchProviderInstruments,
    SelectProviderInstrument, SeriesDemand, SeriesKey, SetEngineLifecycle, SetEngineResourceMode,
    SetSelection, SetViewport, SetWorkspaceLayout, ShutdownEngine, SignOut, StreamRole,
    ViewportDemand, VisibilityDemand, WorkspaceState, WorkspaceTabState, encode_envelope, envelope,
};
use axiusflow_platform_runtime::{
    BackgroundService, CredentialVault, NativeCredentialVault, current_release_identity,
};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, ToNsName as _, prelude::*};
use zeroize::Zeroizing;

/// Stable per-user local socket endpoint generation.
pub const ENGINE_SOCKET_NAME: &str = "axiusflow-engine-v10";
/// Exact entropy required for the installation credential.
pub const INSTALLATION_TOKEN_BYTES: usize = 32;
/// Maximum time allowed for a newly spawned engine to publish readiness.
pub const ENGINE_START_TIMEOUT: Duration = Duration::from_secs(3);
/// Maximum time allowed for one local handshake round trip (hello, readiness,
/// legacy shutdown). A live resident answers in milliseconds; expiry means
/// the endpoint is wedged or gone.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
/// Maximum time allowed for one session frame write. Frames are small and a
/// live resident always drains its command stream; expiry means it is wedged.
const SESSION_SEND_TIMEOUT: Duration = Duration::from_secs(2);

const ENGINE_VAULT_SERVICE: &str = "com.axiusflow.engine";
const IPC_INBOX_CAPACITY: usize = 256;
const MAX_PENDING_MARKET_RESPONSES: usize = IPC_INBOX_CAPACITY;
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
    pending_market_responses: BTreeMap<u64, VecDeque<envelope::Payload>>,
    pending_market_response_count: usize,
}

struct EngineConnectionFailure {
    detail: String,
    endpoint_reached: bool,
    legacy_stopped: bool,
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
        let ns_name =
            name.to_ns_name::<GenericNamespaced>()
                .map_err(|error| EngineConnectionFailure {
                    detail: error.to_string(),
                    endpoint_reached: false,
                    legacy_stopped: false,
                })?;
        let release = current_release_identity();
        let session_nonce = fresh_session_nonce()?;
        // The command stream is required: a missing endpoint means no resident
        // engine is listening at all.
        let mut command = open_session_stream(
            ns_name.clone(),
            installation_token,
            &release,
            session_nonce,
            StreamRole::Command,
            false,
        )?
        .expect("command stream is required");
        // The event stream is optional at this point: a legacy single-stream
        // resident never accepts it, and the readiness reply below reveals
        // which generation answered. A few attempts cover transient pipe
        // instance exhaustion; a persistent refusal means a legacy resident.
        let mut event = None;
        for _ in 0..3 {
            match open_session_stream(
                ns_name.clone(),
                installation_token,
                &release,
                session_nonce,
                StreamRole::Event,
                true,
            )? {
                Some(stream) => {
                    event = Some(stream);
                    break;
                }
                None => thread::sleep(Duration::from_millis(20)),
            }
        }
        // Readiness always arrives on the command stream, for both paired and
        // legacy sessions. Handshake I/O polls the nonblocking command stream
        // with a deadline so a wedged endpoint fails bounded; no reader thread
        // exists yet, so no transport handle is ever shared.
        let handshake_deadline =
            Instant::now()
                .checked_add(HANDSHAKE_TIMEOUT)
                .ok_or_else(|| EngineConnectionFailure {
                    detail: "handshake deadline overflowed".to_string(),
                    endpoint_reached: true,
                    legacy_stopped: false,
                })?;
        let mut handshake_decoder =
            EnvelopeDecoder::try_new().map_err(|error| EngineConnectionFailure {
                detail: error.to_string(),
                endpoint_reached: true,
                legacy_stopped: false,
            })?;
        let ready = read_session_ready(
            &mut command,
            &mut handshake_decoder,
            &release,
            handshake_deadline,
        )?;
        if ready.lifecycle_contract_revision == 0 {
            // A legacy single-stream resident answered. Shut it down over the
            // same strictly sequential command stream, then report replacement
            // so the caller starts the paired generation.
            drop(event);
            shutdown_legacy_engine(&mut command, &mut handshake_decoder, handshake_deadline)
                .map_err(|detail| EngineConnectionFailure {
                    detail,
                    endpoint_reached: true,
                    legacy_stopped: false,
                })?;
            return Err(EngineConnectionFailure {
                detail: "resident engine is stopping for a compatible replacement".to_string(),
                endpoint_reached: true,
                legacy_stopped: true,
            });
        }
        if !compatible_lifecycle_contract_ready(&ready) {
            return Err(EngineConnectionFailure {
                detail: "resident engine lifecycle contract is newer than this desktop".to_string(),
                endpoint_reached: true,
                legacy_stopped: false,
            });
        }
        let Some(event) = event else {
            return Err(EngineConnectionFailure {
                detail: "resident engine did not pair the event stream".to_string(),
                endpoint_reached: true,
                legacy_stopped: false,
            });
        };
        let connection = FramedConnection::new(command, event).map_err(reached_failure)?;
        Ok(Self {
            connection,
            ready,
            pending_market_responses: BTreeMap::new(),
            pending_market_response_count: 0,
        })
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
        match self.receive_reply()? {
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

    /// Persists revision-fenced engine lifetime, autostart, and markets-live permission settings.
    ///
    /// # Errors
    /// Returns an error when the authenticated mutation or native autostart update fails.
    pub fn set_engine_lifecycle(
        &mut self,
        workspace_revision: u64,
        lifetime_mode: EngineLifetimeMode,
        autostart_enabled: bool,
        markets_live_permitted: bool,
    ) -> Result<WorkspaceState, String> {
        self.connection
            .send(envelope::Payload::SetEngineLifecycle(SetEngineLifecycle {
                workspace_revision,
                lifetime_mode: lifetime_mode as i32,
                autostart_enabled,
                markets_live_permitted,
            }))?;
        self.receive_workspace()
    }

    /// Returns one bounded engine lifecycle and resource status snapshot.
    ///
    /// # Errors
    /// Returns an error when the authenticated request fails or the reply is invalid.
    pub fn engine_status(&mut self) -> Result<EngineStatus, String> {
        self.connection
            .send(envelope::Payload::GetEngineStatus(GetEngineStatus {}))?;
        match self.receive_reply()? {
            envelope::Payload::EngineStatus(status) => Ok(status),
            envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
            _ => Err("engine returned an unexpected status reply".to_string()),
        }
    }

    /// Requests complete resident-engine shutdown and consumes this connection.
    ///
    /// # Errors
    /// Returns an error when the authenticated command fails or is not acknowledged.
    pub fn shutdown_engine(mut self) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::ShutdownEngine(ShutdownEngine {}))?;
        match self.receive_reply()? {
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

    /// Persists the complete bounded workspace/tab/pane composition.
    ///
    /// # Errors
    /// Returns an error when the revision/generation is stale or persistence fails.
    pub fn set_workspace_layout(
        &mut self,
        workspace_revision: u64,
        layout_generation: u64,
        active_workspace_id: u64,
        workspace_tabs: Vec<WorkspaceTabState>,
    ) -> Result<WorkspaceState, String> {
        self.connection
            .send(envelope::Payload::SetWorkspaceLayout(SetWorkspaceLayout {
                workspace_revision,
                layout_generation,
                active_workspace_id,
                workspace_tabs,
            }))?;
        self.receive_workspace()
    }

    fn receive_workspace(&mut self) -> Result<WorkspaceState, String> {
        match self.receive_reply()? {
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
        match self.receive_reply()? {
            envelope::Payload::ProviderInstrumentInstalled(installed) => Ok(installed),
            envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
            _ => Err("engine returned an unexpected instrument install reply".to_string()),
        }
    }

    /// Schedules one bounded provider-neutral instrument search.
    ///
    /// Results are delivered by [`Self::receive_market_event_timeout`].
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
    /// Results are delivered by [`Self::receive_market_event_timeout`].
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
        self.set_market_resource_class(
            consumer_id,
            if visible {
                ConsumerResourceClass::Foreground
            } else {
                ConsumerResourceClass::Background
            },
        )
    }

    /// Updates one consumer's exact foreground/background/warm/detached class.
    ///
    /// # Errors
    /// Returns an error when the authenticated local connection cannot send the command.
    pub fn set_market_resource_class(
        &mut self,
        consumer_id: u64,
        resource_class: ConsumerResourceClass,
    ) -> Result<(), String> {
        self.connection
            .send(envelope::Payload::VisibilityDemand(VisibilityDemand {
                consumer_id,
                visible: resource_class == ConsumerResourceClass::Foreground,
                resource_class: resource_class as i32,
            }))
    }

    /// Receives the next market response from the authenticated engine session.
    ///
    /// # Errors
    /// Returns an error for connection, framing, protocol-version, or payload failure.
    pub fn receive_market_event(&mut self) -> Result<envelope::Payload, String> {
        let (_, payload) = self.connection.receive_routed()?;
        Ok(payload)
    }

    /// Receives the next pushed market event and its target consumer.
    ///
    /// A timeout means no publication arrived; it does not perform an IPC poll.
    ///
    /// # Errors
    /// Returns an error for connection, framing, protocol-version, or routing failure.
    pub fn receive_market_event_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<(u64, envelope::Payload)>, String> {
        if let Some((&consumer_id, _)) = self.pending_market_responses.first_key_value()
            && let Some(payload) = self.take_pending_market_response(consumer_id)
        {
            return Ok(Some((consumer_id, payload)));
        }
        let Some((consumer_id, payload)) = self.connection.receive_routed_timeout(timeout)? else {
            return Ok(None);
        };
        if consumer_id == 0 {
            return match payload {
                payload @ envelope::Payload::Fault(_) => Ok(Some((0, payload))),
                _ => Err("engine pushed an unrouted market event".to_string()),
            };
        }
        if market_response_consumer_id(&payload).is_some_and(|id| id != consumer_id) {
            return Err("engine market event routing identity mismatched".to_string());
        }
        Ok(Some((consumer_id, payload)))
    }

    fn take_pending_market_response(&mut self, consumer_id: u64) -> Option<envelope::Payload> {
        let pending = self.pending_market_responses.get_mut(&consumer_id)?;
        let payload = pending.pop_front()?;
        self.pending_market_response_count = self.pending_market_response_count.saturating_sub(1);
        if pending.is_empty() {
            self.pending_market_responses.remove(&consumer_id);
        }
        Some(payload)
    }

    fn buffer_market_response(
        &mut self,
        consumer_id: u64,
        payload: envelope::Payload,
    ) -> Result<(), String> {
        if self.pending_market_response_count >= MAX_PENDING_MARKET_RESPONSES {
            return Err("engine market response realignment exceeded its bound".to_string());
        }
        self.pending_market_responses
            .entry(consumer_id)
            .or_default()
            .push_back(payload);
        self.pending_market_response_count += 1;
        Ok(())
    }

    fn receive_reply(&mut self) -> Result<envelope::Payload, String> {
        loop {
            let (consumer_id, payload) = self.connection.receive_routed()?;
            if consumer_id == 0 {
                return Ok(payload);
            }
            if market_response_consumer_id(&payload).is_some_and(|id| id != consumer_id) {
                return Err("engine market event routing identity mismatched".to_string());
            }
            self.buffer_market_response(consumer_id, payload)?;
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

    /// Starts one engine-owned login transaction and returns the browser
    /// authorization address for the desktop to open in the system browser.
    ///
    /// # Errors
    /// Returns an error when the transaction cannot start or the reply is invalid.
    pub fn begin_login(
        &mut self,
        client_id: u64,
        request_generation: u64,
    ) -> Result<LoginAuthorization, String> {
        self.connection
            .send(envelope::Payload::BeginLogin(BeginLogin {
                client_id,
                request_generation,
            }))?;
        match self.receive_reply()? {
            envelope::Payload::LoginAuthorization(authorization) => Ok(authorization),
            envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
            _ => Err("engine returned an unexpected login reply".to_string()),
        }
    }

    /// Cancels one pending engine-owned login transaction.
    ///
    /// # Errors
    /// Returns an error when no matching transaction is pending or the reply is invalid.
    pub fn cancel_login(&mut self, request_generation: u64) -> Result<AccountView, String> {
        self.connection
            .send(envelope::Payload::CancelLogin(CancelLogin {
                request_generation,
            }))?;
        self.receive_account_view()
    }

    /// Returns the current sanitized engine-owned account view.
    ///
    /// # Errors
    /// Returns an error when the request fails or the reply is invalid.
    pub fn account_status(&mut self) -> Result<AccountView, String> {
        self.connection
            .send(envelope::Payload::GetAccountStatus(GetAccountStatus {}))?;
        self.receive_account_view()
    }

    /// Signs out the shared engine-owned account session.
    ///
    /// # Errors
    /// Returns an error when the request fails or the reply is invalid.
    pub fn sign_out(&mut self) -> Result<AccountView, String> {
        self.connection
            .send(envelope::Payload::SignOut(SignOut {}))?;
        self.receive_account_view()
    }

    fn receive_account_view(&mut self) -> Result<AccountView, String> {
        match self.receive_reply()? {
            envelope::Payload::AccountView(view) => Ok(view),
            envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
            _ => Err("engine returned an unexpected account reply".to_string()),
        }
    }
}

fn market_response_consumer_id(payload: &envelope::Payload) -> Option<u64> {
    match payload {
        envelope::Payload::SeriesState(state) => Some(state.consumer_id),
        envelope::Payload::SeriesSnapshot(snapshot) => Some(snapshot.consumer_id),
        envelope::Payload::SeriesUpdate(update) => Some(update.consumer_id),
        envelope::Payload::DemandError(error) => Some(error.consumer_id),
        envelope::Payload::OrderBookSnapshot(snapshot) => Some(snapshot.consumer_id),
        envelope::Payload::OrderFlowSnapshot(snapshot) => Some(snapshot.consumer_id),
        envelope::Payload::OrderFlowUpdate(update) => Some(update.consumer_id),
        envelope::Payload::ProviderInstrumentSearchResult(result) => Some(result.consumer_id),
        envelope::Payload::ProviderCatalogRejected(rejection) => Some(rejection.consumer_id),
        envelope::Payload::ProviderInstrumentSelection(selection) => Some(selection.consumer_id),
        _ => None,
    }
}

fn reached_failure(detail: String) -> EngineConnectionFailure {
    EngineConnectionFailure {
        detail,
        endpoint_reached: true,
        legacy_stopped: false,
    }
}

fn unreached_failure(detail: String) -> EngineConnectionFailure {
    EngineConnectionFailure {
        detail,
        endpoint_reached: false,
        legacy_stopped: false,
    }
}

fn fresh_session_nonce() -> Result<u64, EngineConnectionFailure> {
    let mut nonce_bytes = [0_u8; 8];
    getrandom::fill(&mut nonce_bytes)
        .map_err(|_| unreached_failure("system CSPRNG unavailable".to_string()))?;
    Ok(u64::from_le_bytes(nonce_bytes).max(1))
}

/// Opens one directed session stream and sends its hello.
///
/// The command stream is nonblocking from birth: handshake reads poll it with
/// a deadline because a wedged endpoint must fail bounded instead of hanging
/// a blocking read. The event stream stays blocking for prompt close
/// detection once its reader thread owns it; its hello is written before any
/// thread shares the handle.
///
/// When `optional` is set, a refused connection resolves to `None` instead of
/// an error so a legacy single-stream resident can still be replaced.
fn open_session_stream(
    name: interprocess::local_socket::Name<'_>,
    installation_token: &[u8],
    release: &axiusflow_platform_runtime::ReleaseIdentity,
    session_nonce: u64,
    stream_role: StreamRole,
    optional: bool,
) -> Result<Option<LocalSocketStream>, EngineConnectionFailure> {
    let mut stream = match LocalSocketStream::connect(name) {
        Ok(stream) => stream,
        Err(_) if optional => return Ok(None),
        Err(error) => return Err(unreached_failure(error.to_string())),
    };
    if stream_role == StreamRole::Command {
        stream
            .set_nonblocking(true)
            .map_err(|error| reached_failure(error.to_string()))?;
    }
    let deadline = Instant::now()
        .checked_add(HANDSHAKE_TIMEOUT)
        .ok_or_else(|| reached_failure("handshake deadline overflowed".to_string()))?;
    send_hello(
        &mut stream,
        installation_token,
        release,
        session_nonce,
        stream_role,
        deadline,
    )
    .map_err(reached_failure)?;
    Ok(Some(stream))
}

/// Reads the readiness reply on the command stream and checks release identity.
fn read_session_ready(
    command: &mut LocalSocketStream,
    decoder: &mut EnvelopeDecoder,
    release: &axiusflow_platform_runtime::ReleaseIdentity,
    deadline: Instant,
) -> Result<EngineReady, EngineConnectionFailure> {
    let ready = match read_one_payload(command, decoder, deadline).map_err(reached_failure)? {
        envelope::Payload::EngineReady(ready) => ready,
        envelope::Payload::Fault(fault) => {
            return Err(EngineConnectionFailure {
                detail: fault.redacted_detail,
                endpoint_reached: true,
                legacy_stopped: false,
            });
        }
        _ => {
            return Err(EngineConnectionFailure {
                detail: "engine did not complete readiness negotiation".to_string(),
                endpoint_reached: true,
                legacy_stopped: false,
            });
        }
    };
    if ready.release_identity != release.release_identity
        || ready.install_generation != release.install_generation
    {
        return Err(EngineConnectionFailure {
            detail: "resident engine release identity does not match the active desktop"
                .to_string(),
            endpoint_reached: true,
            legacy_stopped: false,
        });
    }
    Ok(ready)
}

fn send_hello(
    stream: &mut LocalSocketStream,
    installation_token: &[u8],
    release: &axiusflow_platform_runtime::ReleaseIdentity,
    session_nonce: u64,
    stream_role: StreamRole,
    deadline: Instant,
) -> Result<(), String> {
    let frame = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: Some(envelope::Payload::ClientHello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            installation_token: installation_token.to_vec(),
            client_kind: ClientKind::Ui as i32,
            release_identity: release.release_identity.clone(),
            install_generation: release.install_generation,
            session_nonce,
            stream_role: stream_role as i32,
        })),
    })
    .map_err(|_| "ipc_send failed: local message encoding failed".to_string())?;
    write_frame(stream, &frame, deadline)
}

/// Writes one complete frame without ever blocking the transport handle:
/// temporary backpressure is retried until the deadline, anything else fails.
fn write_frame(
    stream: &mut LocalSocketStream,
    frame: &[u8],
    deadline: Instant,
) -> Result<(), String> {
    let mut written = 0;
    while written < frame.len() {
        match stream.write(&frame[written..]) {
            Ok(0) => {}
            Ok(count) => written += count,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                return Err("ipc_send failed: local transport is unavailable".to_string());
            }
        }
        if written < frame.len() {
            if Instant::now() >= deadline {
                return Err("ipc_send failed: local transport is busy".to_string());
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
    stream
        .flush()
        .map_err(|_| "ipc_send failed: local transport is unavailable".to_string())
}

/// Reads one complete payload, polling a nonblocking handshake stream until
/// the deadline. A zero-byte read here is temporary no-data, not peer
/// closure: the deadline bounds a wedged or departed peer.
fn read_one_payload(
    stream: &mut LocalSocketStream,
    decoder: &mut EnvelopeDecoder,
    deadline: Instant,
) -> Result<envelope::Payload, String> {
    loop {
        let mut chunk = [0_u8; 16 * 1024];
        match stream.read(&mut chunk) {
            Ok(0) => {}
            Ok(count) => {
                let mut envelopes = decoder
                    .push(&chunk[..count])
                    .map_err(|_| "ipc_receive failed: local message is invalid".to_string())?;
                if let Some(envelope) = envelopes.pop() {
                    return envelope.payload.ok_or_else(|| {
                        "ipc_receive failed: engine message has no payload".to_string()
                    });
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                return Err("ipc_receive failed: local engine connection closed".to_string());
            }
        }
        if Instant::now() >= deadline {
            return Err("ipc_receive failed: local engine connection closed".to_string());
        }
        thread::sleep(Duration::from_millis(1));
    }
}

/// Shuts down a legacy single-stream resident over an already-authenticated
/// command stream: the readiness reply was already consumed by the caller.
fn shutdown_legacy_engine(
    command: &mut LocalSocketStream,
    decoder: &mut EnvelopeDecoder,
    deadline: Instant,
) -> Result<(), String> {
    let frame = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: Some(envelope::Payload::ShutdownEngine(ShutdownEngine {})),
    })
    .map_err(|_| "ipc_send failed: local message encoding failed".to_string())?;
    write_frame(command, &frame, deadline)?;
    match read_one_payload(command, decoder, deadline)? {
        envelope::Payload::Goodbye(_) => Ok(()),
        envelope::Payload::Fault(fault) => Err(fault.redacted_detail),
        _ => Err("engine returned an unexpected shutdown reply".to_string()),
    }
}

fn compatible_lifecycle_contract_ready(ready: &EngineReady) -> bool {
    ready.lifecycle_contract_revision == LIFECYCLE_CONTRACT_REVISION
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
    let deadline = Instant::now() + start_timeout;
    let mut last_error =
        match EngineClient::connect_with_reachability(socket_name, installation_token) {
            Ok(client) => return Ok(client),
            Err(failure) if failure.legacy_stopped => {
                replace_legacy_engine(
                    socket_name,
                    engine_executable,
                    &mut engine_started,
                    deadline,
                )?;
                failure.detail
            }
            Err(failure) => {
                if !failure.endpoint_reached {
                    start_engine_process(engine_executable)?;
                    engine_started = true;
                }
                failure.detail
            }
        };
    while Instant::now() < deadline {
        match EngineClient::connect_with_reachability(socket_name, installation_token) {
            Ok(client) => return Ok(client),
            Err(failure) if failure.legacy_stopped => {
                replace_legacy_engine(
                    socket_name,
                    engine_executable,
                    &mut engine_started,
                    deadline,
                )?;
                last_error = failure.detail;
            }
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

/// Waits for a stopped legacy engine to release its endpoint, then starts the
/// installed replacement once the endpoint is free.
///
/// Probing the endpoint by binding avoids blind reconnects against a dying
/// resident whose accepted connections no longer answer.
fn replace_legacy_engine(
    socket_name: &str,
    engine_executable: &Path,
    engine_started: &mut bool,
    deadline: Instant,
) -> Result<(), String> {
    let mut released = false;
    while Instant::now() < deadline {
        let probe = socket_name
            .to_ns_name::<GenericNamespaced>()
            .map(|name| ListenerOptions::new().name(name).create_sync());
        match probe {
            Ok(listener) => {
                drop(listener);
                released = true;
                break;
            }
            Err(_) => thread::sleep(Duration::from_millis(20)),
        }
    }
    if released && !*engine_started {
        start_engine_process(engine_executable)?;
        *engine_started = true;
    }
    Ok(())
}

fn start_engine_process(engine_executable: &Path) -> Result<(), String> {
    BackgroundService::new(engine_executable.to_path_buf())
        .and_then(|service| service.start())
        .map_err(|_| {
            "resident engine could not be started from the installation directory".to_string()
        })
}

struct FramedConnection {
    command: LocalSocketStream,
    incoming: Receiver<Result<Envelope, String>>,
    stop: Arc<AtomicBool>,
}

impl FramedConnection {
    /// Forms a paired session from a write-only command stream and a
    /// read-only event stream.
    ///
    /// Neither stream is ever split: each transport handle has exactly one
    /// owner and one direction, so a blocking read can never stall a
    /// concurrent write on any platform.
    fn new(command: LocalSocketStream, event: LocalSocketStream) -> Result<Self, String> {
        // Where the platform supports receive timeouts the event reader wakes
        // periodically to observe `stop`; elsewhere the blocked read unblocks
        // on data or peer closure. `set_recv_timeout` is unsupported on
        // Windows named pipes, so it is only attempted where it exists.
        #[cfg(unix)]
        let _ = event.set_recv_timeout(Some(Duration::from_millis(100)));
        let (incoming_tx, incoming) = mpsc::sync_channel(IPC_INBOX_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        thread::Builder::new()
            .name("axiusflow-desktop-ipc-reader".to_string())
            .spawn(move || read_ipc_messages(event, &incoming_tx, &reader_stop))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            command,
            incoming,
            stop,
        })
    }

    fn send(&mut self, payload: envelope::Payload) -> Result<(), String> {
        let frame = encode_envelope(&Envelope {
            protocol_version: PROTOCOL_VERSION,
            target_consumer_id: 0,
            payload: Some(payload),
        })
        .map_err(|_| "ipc_send failed: local message encoding failed".to_string())?;
        let deadline = Instant::now()
            .checked_add(SESSION_SEND_TIMEOUT)
            .ok_or_else(|| "ipc_send failed: local transport is busy".to_string())?;
        write_frame(&mut self.command, &frame, deadline)
    }

    fn receive_routed(&mut self) -> Result<(u64, envelope::Payload), String> {
        self.incoming
            .recv()
            .map_err(|_| "ipc_receive failed: local engine connection closed".to_string())?
            .and_then(routed_payload)
    }

    fn receive_routed_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<(u64, envelope::Payload)>, String> {
        match self.incoming.recv_timeout(timeout) {
            Ok(envelope) => envelope.and_then(routed_payload).map(Some),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => {
                Err("ipc_receive failed: local engine connection closed".to_string())
            }
        }
    }
}

impl Drop for FramedConnection {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn routed_payload(message: Envelope) -> Result<(u64, envelope::Payload), String> {
    let payload = message
        .payload
        .ok_or_else(|| "ipc_receive failed: engine message has no payload".to_string())?;
    Ok((message.target_consumer_id, payload))
}

fn read_ipc_messages(
    mut reader: LocalSocketStream,
    incoming: &mpsc::SyncSender<Result<Envelope, String>>,
    stop: &AtomicBool,
) {
    let mut decoder = match EnvelopeDecoder::try_new() {
        Ok(decoder) => decoder,
        Err(error) => {
            let _ = incoming.send(Err(error.to_string()));
            return;
        }
    };
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let mut chunk = [0_u8; 16 * 1024];
        let count = match reader.read(&mut chunk) {
            // The reader is always blocking (see `FramedConnection::new`), so
            // a zero-byte read genuinely means the peer closed, on every
            // platform. Never treat it as temporary no-data.
            Ok(0) => {
                let _ = incoming.send(Err(
                    "ipc_receive failed: local engine connection closed".to_string()
                ));
                return;
            }
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(_) => {
                let _ = incoming.send(Err(
                    "ipc_receive failed: local transport is unavailable".to_string()
                ));
                return;
            }
        };
        let Ok(envelopes) = decoder.push(&chunk[..count]) else {
            let _ = incoming.send(Err(
                "ipc_receive failed: local message is invalid".to_string()
            ));
            return;
        };
        for envelope in envelopes {
            if incoming.send(Ok(envelope)).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    use interprocess::local_socket::{
        GenericNamespaced, ListenerNonblockingMode, ListenerOptions, ToNsName as _, prelude::*,
    };

    #[cfg(target_os = "windows")]
    use axiusflow_engine_protocol::{
        EngineLifetimeMode, InstallProviderInstrument, OrderBookState,
        ProviderCatalogRejectionReason, ResourceMode, SearchProviderInstruments,
        SelectProviderInstrument, SeriesCadence, SeriesKey, WorkspaceState,
    };
    use axiusflow_engine_protocol::{
        EngineReady, Envelope, EnvelopeDecoder, PROTOCOL_VERSION, StreamRole, encode_envelope,
        envelope,
    };

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    use super::native_installation_token;
    use super::{ENGINE_SOCKET_NAME, EngineClient, connect_or_start_engine_named};

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn protocol_socket_name_tracks_the_active_version() {
        assert_eq!(ENGINE_SOCKET_NAME, "axiusflow-engine-v10");
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

    /// Reads one frame from a stream accepted off a non-blocking listener.
    /// Accepted streams can inherit non-blocking mode (macOS), so temporary
    /// no-data is polled with a bound instead of being mistaken for a closed
    /// peer. An orderly shutdown still returns empty immediately so the
    /// caller's decode assertion fires without waiting out the deadline.
    fn read_arrival_frame(stream: &mut LocalSocketStream, what: &str) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut bytes = [0_u8; 4096];
        loop {
            match stream.read(&mut bytes) {
                Ok(0) => return Vec::new(),
                Ok(count) => return bytes[..count].to_vec(),
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => panic!("{what}: {error}"),
            }
            assert!(Instant::now() < deadline, "{what} timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Drains one legacy fixture listener and returns its command-role
    /// arrival. Arrival order is not deterministic under load, and a legacy
    /// client may only ever open its command stream, so arrivals are polled
    /// with a bound instead of being awaited unconditionally.
    fn accept_legacy_command_stream(listener: &LocalSocketListener) -> LocalSocketStream {
        listener
            .set_nonblocking(ListenerNonblockingMode::Accept)
            .expect("poll replacement arrivals");
        let mut command = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while command.is_none() && Instant::now() < deadline {
            match listener.accept() {
                Ok(mut stream) => {
                    let mut decoder = EnvelopeDecoder::try_new().expect("decoder");
                    let frame = read_arrival_frame(&mut stream, "read client hello");
                    let hello = decoder.push(&frame).expect("decode hello");
                    let payload = hello.first().and_then(|message| message.payload.as_ref());
                    assert!(
                        matches!(payload, Some(envelope::Payload::ClientHello(_))),
                        "legacy resident expects a client hello"
                    );
                    if matches!(
                        payload,
                        Some(envelope::Payload::ClientHello(hello))
                            if hello.stream_role == StreamRole::Command as i32
                    ) {
                        assert!(command.replace(stream).is_none());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept replacement probe: {error}"),
            }
        }
        command.expect("legacy command stream arrives")
    }

    #[test]
    fn lifecycle_revision_zero_resident_is_shutdown_before_replacement_start() {
        let socket_name = format!(
            "axiusflow-engine-client-replacement-test-{}-{}",
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
            .expect("bind legacy endpoint");
        let shutdown_received = Arc::new(AtomicBool::new(false));
        let server_shutdown = Arc::clone(&shutdown_received);
        let server = thread::spawn(move || {
            let mut stream = accept_legacy_command_stream(&listener);
            let mut decoder = EnvelopeDecoder::try_new().expect("decoder");
            let ready = encode_envelope(&Envelope {
                protocol_version: PROTOCOL_VERSION,
                target_consumer_id: 0,
                payload: Some(envelope::Payload::EngineReady(EngineReady {
                    protocol_version: PROTOCOL_VERSION,
                    engine_epoch: 1,
                    workspace_revision: 0,
                    lifecycle_contract_revision: 0,
                    release_identity: axiusflow_platform_runtime::current_release_identity()
                        .release_identity,
                    install_generation: axiusflow_platform_runtime::current_release_identity()
                        .install_generation,
                })),
            })
            .expect("encode legacy readiness");
            stream.write_all(&ready).expect("send legacy readiness");
            let frame = read_arrival_frame(&mut stream, "read shutdown command");
            let shutdown = decoder.push(&frame).expect("decode shutdown");
            assert!(matches!(
                shutdown
                    .first()
                    .and_then(|message| message.payload.as_ref()),
                Some(envelope::Payload::ShutdownEngine(_))
            ));
            server_shutdown.store(true, Ordering::Release);
            let goodbye = encode_envelope(&Envelope {
                protocol_version: PROTOCOL_VERSION,
                target_consumer_id: 0,
                payload: Some(envelope::Payload::Goodbye(
                    axiusflow_engine_protocol::Goodbye {
                        reason: "legacy resident stopping".to_string(),
                    },
                )),
            })
            .expect("encode shutdown acknowledgement");
            stream.write_all(&goodbye).expect("acknowledge shutdown");
        });

        let Err(error) = connect_or_start_engine_named(
            &socket_name,
            Path::new("engine-executable-that-does-not-exist"),
            &[9_u8; 32],
            Duration::from_millis(250),
        ) else {
            panic!("replacement executable is intentionally absent");
        };
        server.join().expect("join legacy resident");
        assert!(shutdown_received.load(Ordering::Acquire));
        assert_eq!(
            error,
            "resident engine could not be started from the installation directory"
        );
    }

    #[test]
    fn mismatched_engine_release_is_never_returned_as_ready() {
        let socket_name = format!(
            "axiusflow-engine-client-release-test-{}-{}",
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
            .expect("bind mismatched endpoint");
        let server = thread::spawn(move || {
            // Answer every arrival: arrival order is not deterministic under
            // load, and the client reads its verdict on the command stream.
            // Arrivals are polled with a bound because a legacy client may
            // only ever open one stream.
            listener
                .set_nonblocking(ListenerNonblockingMode::Accept)
                .expect("poll release arrivals");
            let mut answered = 0_usize;
            let mut quiet_since = None;
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok(mut stream) => {
                        let frame = read_arrival_frame(&mut stream, "read client hello");
                        assert!(!frame.is_empty());
                        let ready = encode_envelope(&Envelope {
                            protocol_version: PROTOCOL_VERSION,
                            target_consumer_id: 0,
                            payload: Some(envelope::Payload::EngineReady(EngineReady {
                                protocol_version: PROTOCOL_VERSION,
                                engine_epoch: 1,
                                workspace_revision: 0,
                                lifecycle_contract_revision:
                                    axiusflow_engine_protocol::LIFECYCLE_CONTRACT_REVISION,
                                release_identity: "superseded-release".to_string(),
                                install_generation: 99,
                            })),
                        })
                        .expect("encode mismatched readiness");
                        let _ = stream.write_all(&ready);
                        answered += 1;
                        quiet_since = None;
                        if answered >= 2 {
                            return;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        let quiet = *quiet_since.get_or_insert_with(Instant::now);
                        if answered > 0 && quiet.elapsed() > Duration::from_millis(200) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept release probe: {error}"),
                }
            }
            assert!(answered > 0, "mismatched probe arrives");
        });
        assert_eq!(
            EngineClient::connect(&socket_name, &[9_u8; 32])
                .err()
                .as_deref(),
            Some("resident engine release identity does not match the active desktop")
        );
        server.join().expect("join mismatched endpoint");
    }

    #[cfg(target_os = "windows")]
    fn assert_native_autostart_enabled(executable: &Path) {
        let query = std::process::Command::new("reg.exe")
            .args([
                "QUERY",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "Axiusflow Engine",
            ])
            .output()
            .expect("query Windows autostart value");
        assert!(query.status.success());
        let expected = format!("\"{}\"", executable.display());
        assert!(String::from_utf8_lossy(&query.stdout).contains(&expected));
    }

    #[cfg(target_os = "linux")]
    fn assert_native_autostart_enabled(executable: &Path) {
        let config_root = std::path::PathBuf::from(
            std::env::var_os("XDG_CONFIG_HOME").expect("XDG_CONFIG_HOME is required"),
        );
        let entry = config_root.join("autostart/axiusflow-engine.desktop");
        let escaped = executable
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let expected = format!(
            "[Desktop Entry]\nType=Application\nName=Axiusflow Engine\nExec=\"{escaped}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        );
        assert_eq!(
            std::fs::read_to_string(entry).expect("read Linux autostart entry"),
            expected
        );
    }

    #[cfg(target_os = "windows")]
    fn assert_native_autostart_disabled() {
        let query = std::process::Command::new("reg.exe")
            .args([
                "QUERY",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "Axiusflow Engine",
            ])
            .output()
            .expect("query removed Windows autostart value");
        assert!(!query.status.success());
    }

    #[cfg(target_os = "linux")]
    fn assert_native_autostart_disabled() {
        let config_root = std::path::PathBuf::from(
            std::env::var_os("XDG_CONFIG_HOME").expect("XDG_CONFIG_HOME is required"),
        );
        assert!(
            !config_root
                .join("autostart/axiusflow-engine.desktop")
                .exists()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    #[ignore = "requires the optimized resident engine and mutates then restores native per-user autostart"]
    fn native_release_status_and_autostart_round_trip() {
        use std::path::PathBuf;

        use axiusflow_engine_protocol::{EngineLifetimeMode, EngineShutdownState, ResourceMode};

        struct LifecycleRestore {
            lifetime_mode: EngineLifetimeMode,
            autostart_enabled: bool,
            markets_live_permitted: bool,
        }

        impl Drop for LifecycleRestore {
            fn drop(&mut self) {
                let Ok(token) = native_installation_token() else {
                    return;
                };
                let Ok(mut client) = EngineClient::connect(ENGINE_SOCKET_NAME, token.as_slice())
                else {
                    return;
                };
                let Ok(workspace) = client.restore_workspace() else {
                    return;
                };
                let _ = client.set_engine_lifecycle(
                    workspace.workspace_revision,
                    self.lifetime_mode,
                    self.autostart_enabled,
                    self.markets_live_permitted,
                );
            }
        }

        let executable = PathBuf::from(
            std::env::var_os("AXIUSFLOW_NATIVE_ENGINE_EXE")
                .expect("AXIUSFLOW_NATIVE_ENGINE_EXE is required"),
        );
        assert!(executable.is_absolute());
        assert!(executable.is_file());
        let token = native_installation_token().expect("load native installation token");
        let mut client = EngineClient::connect(ENGINE_SOCKET_NAME, token.as_slice())
            .expect("connect optimized resident engine");
        let original = client.restore_workspace().expect("restore lifecycle state");
        let original_mode = EngineLifetimeMode::try_from(original.lifetime_mode)
            .expect("persisted lifetime mode is valid");
        let _restore = LifecycleRestore {
            lifetime_mode: original_mode,
            autostart_enabled: original.autostart_enabled,
            markets_live_permitted: original.markets_live_permitted,
        };
        let enabled = client
            .set_engine_lifecycle(
                original.workspace_revision,
                EngineLifetimeMode::KeepEngineWarm,
                true,
                false,
            )
            .expect("enable native autostart");
        let status = client.engine_status().expect("read engine status");
        assert_ne!(status.process_id, 0);
        assert_eq!(
            status.lifetime_mode,
            EngineLifetimeMode::KeepEngineWarm as i32
        );
        assert_eq!(status.resource_mode, ResourceMode::Warm as i32);
        assert_eq!(status.shutdown_state, EngineShutdownState::Running as i32);
        assert!(status.autostart_enabled);

        assert_native_autostart_enabled(&executable);

        let disabled = client
            .set_engine_lifecycle(
                enabled.workspace_revision,
                EngineLifetimeMode::KeepEngineWarm,
                false,
                false,
            )
            .expect("disable native autostart");
        assert!(!disabled.autostart_enabled);
        assert_native_autostart_disabled();
        println!(
            "native_engine_status pid={} clients={} retained_series={} retained_bars={} bytes={}",
            status.process_id,
            status.connected_desktop_clients,
            status.retained_series,
            status.retained_bars,
            status.approximate_series_bytes
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires the optimized resident engine"]
    fn native_release_status_probe() {
        let token = native_installation_token().expect("load native installation token");
        let mut client = EngineClient::connect(ENGINE_SOCKET_NAME, token.as_slice())
            .expect("connect optimized resident engine");
        let status = client.engine_status().expect("read engine status");
        assert_ne!(status.process_id, 0);
        let providers = status
            .providers
            .iter()
            .map(|provider| {
                format!(
                    "{}:{}:gen{}:{}",
                    provider.provider,
                    provider.state,
                    provider.generation,
                    provider.detail.as_deref().unwrap_or("-")
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        println!(
            "native_engine_probe pid={} clients={} providers={} retained_series={} retained_bars={} bytes={} shutdown_state={}",
            status.process_id,
            status.connected_desktop_clients,
            providers,
            status.retained_series,
            status.retained_bars,
            status.approximate_series_bytes,
            status.shutdown_state
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires the optimized resident engine and intentionally changes lifecycle mode"]
    fn native_release_configure_lifecycle() {
        use axiusflow_engine_protocol::EngineLifetimeMode;

        let requested = std::env::var("AXIUSFLOW_NATIVE_LIFETIME_MODE")
            .expect("AXIUSFLOW_NATIVE_LIFETIME_MODE is required");
        let (mode, markets_live_permitted) = match requested.as_str() {
            "exit" => (EngineLifetimeMode::ExitCompletely, false),
            "warm" => (EngineLifetimeMode::KeepEngineWarm, false),
            "live" => (EngineLifetimeMode::KeepMarketsLive, true),
            _ => panic!("AXIUSFLOW_NATIVE_LIFETIME_MODE must be exit, warm, or live"),
        };
        let token = native_installation_token().expect("load native installation token");
        let mut client = EngineClient::connect(ENGINE_SOCKET_NAME, token.as_slice())
            .expect("connect optimized resident engine");
        let workspace = client.restore_workspace().expect("restore lifecycle state");
        let updated = client
            .set_engine_lifecycle(
                workspace.workspace_revision,
                mode,
                workspace.autostart_enabled,
                markets_live_permitted,
            )
            .expect("configure native lifecycle mode");
        assert_eq!(updated.lifetime_mode, mode as i32);
        println!(
            "native_lifecycle_mode={} workspace_revision={}",
            requested, updated.workspace_revision
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires the optimized resident engine and a live market provider"]
    fn native_release_market_snapshot_probe() {
        use axiusflow_engine_protocol::SeriesKey;

        let token = native_installation_token().expect("load native installation token");
        let mut first = EngineClient::connect(ENGINE_SOCKET_NAME, token.as_slice())
            .expect("connect first optimized resident-engine client");
        let workspace = first.restore_workspace().expect("restore hot metadata");
        let hot = workspace
            .hot_series
            .iter()
            .find(|series| series.provider == "hyperliquid")
            .or_else(|| {
                workspace
                    .hot_series
                    .iter()
                    .find(|series| series.provider == "rithmic")
            })
            .expect("live-provider hot series is available");
        let series = SeriesKey {
            provider: hot.provider.clone(),
            instrument_id: hot.instrument_id.clone(),
            cadence_value: hot.cadence_value,
            definition_revision: hot.definition_revision,
            entitlement_id: hot.entitlement_id.clone(),
            cadence: hot.cadence,
        };
        let first_client_id = u64::from(std::process::id());
        let first_consumer_id = first_client_id;
        let second_client_id = first_client_id + 1;
        let second_consumer_id = second_client_id;
        attach_native_market_probe(&mut first, first_client_id, first_consumer_id, &series);
        let mut second = EngineClient::connect(ENGINE_SOCKET_NAME, token.as_slice())
            .expect("connect second optimized resident-engine client");
        attach_native_market_probe(&mut second, second_client_id, second_consumer_id, &series);

        let first_observation = observe_native_market(&mut first, first_consumer_id);
        let second_observation = observe_native_market(&mut second, second_consumer_id);
        assert_eq!(
            first_observation.snapshot_bars, second_observation.snapshot_bars,
            "both clients receive the same canonical retained depth"
        );
        assert_eq!(
            first_observation.snapshot_sequence, second_observation.snapshot_sequence,
            "both clients receive the same canonical retained edge"
        );
        println!(
            "native_market_shared instrument={} clients=2 bars={} sequence={} first_publication={} second_publication={}",
            hot.instrument_id,
            first_observation.snapshot_bars,
            first_observation.snapshot_sequence,
            first_observation.live_publication,
            second_observation.live_publication
        );

        first
            .remove_market_consumer(first_consumer_id)
            .expect("remove first native probe consumer");
        second
            .remove_market_consumer(second_consumer_id)
            .expect("remove second native probe consumer");
    }

    #[cfg(target_os = "windows")]
    struct NativeMarketObservation {
        snapshot_bars: usize,
        snapshot_sequence: u64,
        live_publication: u64,
    }

    #[cfg(target_os = "windows")]
    fn attach_native_market_probe(
        client: &mut EngineClient,
        client_id: u64,
        consumer_id: u64,
        series: &axiusflow_engine_protocol::SeriesKey,
    ) {
        client
            .attach_client(client_id)
            .expect("attach native probe client");
        client
            .register_consumer(client_id, 1, consumer_id)
            .expect("register native probe consumer");
        client
            .set_series_demand(consumer_id, 1, series.clone())
            .expect("demand native hot series");
    }

    #[cfg(target_os = "windows")]
    fn observe_native_market(
        client: &mut EngineClient,
        consumer_id: u64,
    ) -> NativeMarketObservation {
        use axiusflow_engine_protocol::envelope;

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut snapshot = None;
        loop {
            let event = receive_native_event(client, consumer_id)
                .expect("receive native market publication");
            match event {
                Some(envelope::Payload::SeriesSnapshot(candidate)) if candidate.generation == 1 => {
                    let last = candidate.bars.last().expect("snapshot contains bars");
                    snapshot = Some((
                        candidate.bars.len(),
                        last.source_sequence,
                        candidate.publication_generation,
                    ));
                }
                Some(envelope::Payload::SeriesUpdate(update))
                    if update.generation == 1
                        && snapshot.is_some_and(|(_, _, publication)| {
                            update.publication_generation > publication
                        }) =>
                {
                    let (snapshot_bars, snapshot_sequence, _) =
                        snapshot.expect("snapshot precedes live update");
                    return NativeMarketObservation {
                        snapshot_bars,
                        snapshot_sequence,
                        live_publication: update.publication_generation,
                    };
                }
                Some(envelope::Payload::DemandError(error)) if error.generation == 1 => {
                    panic!("native market demand failed: {error:?}");
                }
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "native snapshot and advancing live publication timed out"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "windows")]
    fn receive_native_event(
        client: &mut EngineClient,
        consumer_id: u64,
    ) -> Result<Option<envelope::Payload>, String> {
        Ok(client
            .receive_market_event_timeout(Duration::from_millis(20))?
            .and_then(|(target, event)| (target == consumer_id || target == 0).then_some(event)))
    }

    #[cfg(target_os = "windows")]
    fn select_native_rithmic_instrument(
        client: &mut EngineClient,
        consumer_id: u64,
        requested_symbol: &str,
    ) -> Result<InstallProviderInstrument, String> {
        const SEARCH_GENERATION: u64 = 1;
        const SELECTION_GENERATION: u64 = 1;
        client.search_provider_instruments(SearchProviderInstruments {
            consumer_id,
            search_generation: SEARCH_GENERATION,
            provider: "rithmic".to_string(),
            query: requested_symbol.to_string(),
            maximum_results: 16,
        })?;
        let search_deadline = Instant::now() + Duration::from_secs(20);
        let selected = loop {
            if Instant::now() >= search_deadline {
                return Err("Rithmic native search timed out".to_string());
            }
            match receive_native_event(client, consumer_id)? {
                Some(envelope::Payload::ProviderInstrumentSearchResult(result))
                    if result.search_generation == SEARCH_GENERATION =>
                {
                    break result
                        .instruments
                        .into_iter()
                        .find(|instrument| instrument.symbol == requested_symbol)
                        .ok_or_else(|| {
                            "Rithmic native search returned no exact requested contract".to_string()
                        })?;
                }
                Some(envelope::Payload::ProviderCatalogRejected(rejection))
                    if rejection.command_generation == SEARCH_GENERATION =>
                {
                    let reason = ProviderCatalogRejectionReason::try_from(rejection.reason)
                        .unwrap_or(ProviderCatalogRejectionReason::Unspecified);
                    return Err(format!("Rithmic native search was rejected: {reason:?}"));
                }
                _ => {}
            }
            thread::sleep(Duration::from_millis(20));
        };
        client.select_provider_instrument(SelectProviderInstrument {
            consumer_id,
            selection_generation: SELECTION_GENERATION,
            search_generation: SEARCH_GENERATION,
            provider: "rithmic".to_string(),
            entitlement_id: format!("rithmic-test:{}:{}", selected.exchange, selected.symbol),
            symbol: selected.symbol,
            exchange: selected.exchange,
        })?;
        let selection_deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if Instant::now() >= selection_deadline {
                return Err("Rithmic native selection timed out".to_string());
            }
            match receive_native_event(client, consumer_id)? {
                Some(envelope::Payload::ProviderInstrumentSelection(selection))
                    if selection.consumer_id == consumer_id =>
                {
                    return selection
                        .instrument
                        .ok_or_else(|| "Rithmic native selection omitted identity".to_string());
                }
                Some(envelope::Payload::ProviderCatalogRejected(rejection))
                    if rejection.command_generation == SELECTION_GENERATION =>
                {
                    let reason = ProviderCatalogRejectionReason::try_from(rejection.reason)
                        .unwrap_or(ProviderCatalogRejectionReason::Unspecified);
                    return Err(format!("Rithmic native selection was rejected: {reason:?}"));
                }
                _ => {}
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(target_os = "windows")]
    fn await_native_rithmic_baseline(
        client: &mut EngineClient,
        consumer_id: u64,
    ) -> Result<(u64, u64), String> {
        const SERIES_GENERATION: u64 = 1;
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut bar_sequence = None;
        let mut book_watermark = None;
        let mut provider_state = None;
        let mut series_state = None;
        let mut demand_error = None;
        while Instant::now() < deadline && (bar_sequence.is_none() || book_watermark.is_none()) {
            match receive_native_event(client, consumer_id)? {
                Some(envelope::Payload::SeriesSnapshot(snapshot))
                    if snapshot.generation == SERIES_GENERATION =>
                {
                    bar_sequence = snapshot.bars.last().map(|bar| bar.source_sequence);
                }
                Some(envelope::Payload::SeriesUpdate(update))
                    if update.generation == SERIES_GENERATION =>
                {
                    bar_sequence = update.bar.map(|bar| bar.source_sequence);
                }
                Some(envelope::Payload::OrderBookSnapshot(book))
                    if book.generation == SERIES_GENERATION
                        && book.state == OrderBookState::Ready as i32 =>
                {
                    book_watermark = Some(book.source_watermark);
                }
                Some(envelope::Payload::ProviderState(state)) if state.provider == "rithmic" => {
                    provider_state = Some(state.state);
                }
                Some(envelope::Payload::SeriesState(state))
                    if state.generation == SERIES_GENERATION =>
                {
                    series_state = Some(state.state);
                }
                Some(envelope::Payload::DemandError(error))
                    if error.generation == SERIES_GENERATION =>
                {
                    demand_error = Some(error.detail);
                }
                _ => {}
            }
            thread::sleep(Duration::from_millis(20));
        }
        let detail = || {
            format!("provider={provider_state:?}, series={series_state:?}, demand={demand_error:?}")
        };
        Ok((
            bar_sequence
                .ok_or_else(|| format!("Rithmic native bar did not arrive ({})", detail()))?,
            book_watermark.ok_or_else(|| {
                format!("Rithmic native ready book did not arrive ({})", detail())
            })?,
        ))
    }

    #[cfg(target_os = "windows")]
    fn await_advanced_native_rithmic(
        client: &mut EngineClient,
        consumer_id: u64,
        baseline_bar_sequence: u64,
        baseline_book_watermark: u64,
    ) -> Result<(u64, u64), String> {
        const SERIES_GENERATION: u64 = 1;
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut bar_sequence = None;
        let mut book_watermark = None;
        while Instant::now() < deadline
            && (bar_sequence.is_none()
                || book_watermark.is_none_or(|watermark| watermark <= baseline_book_watermark))
        {
            match receive_native_event(client, consumer_id)? {
                Some(envelope::Payload::SeriesSnapshot(snapshot))
                    if snapshot.generation == SERIES_GENERATION =>
                {
                    bar_sequence = snapshot.bars.last().map(|bar| bar.source_sequence);
                }
                Some(envelope::Payload::SeriesUpdate(update))
                    if update.generation == SERIES_GENERATION =>
                {
                    bar_sequence = update.bar.map(|bar| bar.source_sequence);
                }
                Some(envelope::Payload::OrderBookSnapshot(book))
                    if book.generation == SERIES_GENERATION
                        && book.state == OrderBookState::Ready as i32 =>
                {
                    book_watermark = Some(book.source_watermark);
                }
                _ => {}
            }
            thread::sleep(Duration::from_millis(20));
        }
        let bar_sequence =
            bar_sequence.ok_or_else(|| "Rithmic native resumed bar did not arrive".to_string())?;
        let book_watermark = book_watermark
            .ok_or_else(|| "Rithmic native resumed book did not arrive".to_string())?;
        if bar_sequence < baseline_bar_sequence || book_watermark <= baseline_book_watermark {
            return Err(format!(
                "Rithmic state regressed or the book did not advance while the UI was detached (bar {baseline_bar_sequence}->{bar_sequence}, book {baseline_book_watermark}->{book_watermark})"
            ));
        }
        Ok((bar_sequence, book_watermark))
    }

    #[cfg(target_os = "windows")]
    fn restore_native_lifecycle(token: &[u8], original: &WorkspaceState) {
        let mut cleanup = EngineClient::connect(ENGINE_SOCKET_NAME, token)
            .expect("reconnect resident engine for lifecycle restoration");
        let current = cleanup
            .restore_workspace()
            .expect("read current lifecycle state");
        let original_mode = EngineLifetimeMode::try_from(original.lifetime_mode)
            .expect("persisted original lifecycle mode is valid");
        cleanup
            .set_engine_lifecycle(
                current.workspace_revision,
                original_mode,
                original.autostart_enabled,
                original.markets_live_permitted,
            )
            .expect("restore original lifecycle state");
        let original_resource_mode = match original_mode {
            EngineLifetimeMode::ExitCompletely => ResourceMode::OfflineSuspended,
            EngineLifetimeMode::KeepMarketsLive if original.markets_live_permitted => {
                ResourceMode::MarketsLive
            }
            EngineLifetimeMode::KeepEngineWarm | EngineLifetimeMode::KeepMarketsLive => {
                ResourceMode::Warm
            }
        };
        cleanup
            .set_engine_resource_mode(original_resource_mode)
            .expect("restore original engine resource mode");
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires the optimized resident engine and credentialed Rithmic Test access"]
    fn native_release_rithmic_markets_live_round_trip() {
        let token = native_installation_token().expect("load native installation token");
        let mut client = EngineClient::connect(ENGINE_SOCKET_NAME, token.as_slice())
            .expect("connect optimized resident engine");
        let original = client.restore_workspace().expect("restore lifecycle state");
        client
            .set_engine_resource_mode(ResourceMode::Interactive)
            .expect("enable interactive provider work for native probe");
        let engine_pid = client
            .engine_status()
            .expect("read initial engine status")
            .process_id;
        let client_id = u64::from(std::process::id()).saturating_mul(10) + 1;
        let consumer_id = client_id;
        client
            .attach_client(client_id)
            .expect("attach native probe");
        client
            .register_consumer(client_id, 1, consumer_id)
            .expect("register native probe consumer");
        let requested_symbol = std::env::var("AXIUSFLOW_NATIVE_RITHMIC_SYMBOL")
            .expect("AXIUSFLOW_NATIVE_RITHMIC_SYMBOL is required");

        let result = (|| -> Result<(String, u64, u64, u64, u64), String> {
            const SERIES_GENERATION: u64 = 1;
            let instrument =
                select_native_rithmic_instrument(&mut client, consumer_id, &requested_symbol)?;
            let series = SeriesKey {
                provider: "rithmic".to_string(),
                instrument_id: instrument.instrument_id.clone(),
                cadence_value: 100,
                definition_revision: 1,
                entitlement_id: instrument.entitlement_id.clone(),
                cadence: SeriesCadence::Trades as i32,
            };
            client.set_series_demand(consumer_id, SERIES_GENERATION, series.clone())?;
            client.set_market_visibility(consumer_id, true)?;
            let (baseline_bar_sequence, baseline_book_watermark) =
                await_native_rithmic_baseline(&mut client, consumer_id)?;

            let workspace = client.restore_workspace()?;
            client.set_engine_lifecycle(
                workspace.workspace_revision,
                EngineLifetimeMode::KeepMarketsLive,
                workspace.autostart_enabled,
                true,
            )?;
            client.set_engine_resource_mode(ResourceMode::MarketsLive)?;
            client.detach_client(client_id)?;
            drop(client);
            thread::sleep(Duration::from_secs(15));

            let mut reattached = EngineClient::connect(ENGINE_SOCKET_NAME, token.as_slice())?;
            let status = reattached.engine_status()?;
            if status.process_id != engine_pid {
                return Err("resident engine PID changed during markets-live detach".to_string());
            }
            let reattached_client_id = client_id + 1;
            let reattached_consumer_id = consumer_id + 1;
            reattached.attach_client(reattached_client_id)?;
            reattached.register_consumer(reattached_client_id, 1, reattached_consumer_id)?;
            reattached.set_series_demand(reattached_consumer_id, SERIES_GENERATION, series)?;
            reattached.set_market_visibility(reattached_consumer_id, true)?;
            let (resumed_bar_sequence, resumed_book_watermark) = await_advanced_native_rithmic(
                &mut reattached,
                reattached_consumer_id,
                baseline_bar_sequence,
                baseline_book_watermark,
            )?;
            reattached.remove_market_consumer(reattached_consumer_id)?;
            reattached.detach_client(reattached_client_id)?;
            Ok((
                instrument.display_symbol,
                baseline_bar_sequence,
                resumed_bar_sequence,
                baseline_book_watermark,
                resumed_book_watermark,
            ))
        })();

        restore_native_lifecycle(token.as_slice(), &original);

        let (symbol, bar_before, bar_after, book_before, book_after) =
            result.expect("credentialed Rithmic markets-live round trip");
        println!(
            "native_rithmic_markets_live pid={engine_pid} symbol={symbol} bar={bar_before}->{bar_after} book={book_before}->{book_after} detached_ms=15000"
        );
    }
}
