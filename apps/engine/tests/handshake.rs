use std::{
    cell::RefCell,
    fs,
    io::{Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use axiusflow_engine::{
    EngineState, SessionPairer, bind_listener, serve_client, serve_client_with_state,
};
use axiusflow_engine_protocol::{
    ClientHello, ClientKind, EngineFaultCode, Envelope, EnvelopeDecoder, PROTOCOL_VERSION,
    ResourceMode, StreamRole, WorkspaceLayoutState, WorkspaceSplitAxis, WorkspaceTabState,
    encode_envelope, envelope,
};
use axiusflow_local_engine_client::{EngineClient, load_or_create_installation_token};
use axiusflow_platform_runtime::CredentialVault;
use interprocess::local_socket::{GenericNamespaced, ToNsName as _, prelude::*};

static NEXT_NAME: AtomicU64 = AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(unique_name());
        fs::create_dir(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct MemoryVault(RefCell<Option<Vec<u8>>>);

impl CredentialVault for MemoryVault {
    type Error = ();

    fn store(&self, _key: &str, secret: &[u8]) -> Result<(), Self::Error> {
        self.0.replace(Some(secret.to_vec()));
        Ok(())
    }

    fn load(&self, _key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.0.borrow().clone())
    }

    fn delete(&self, _key: &str) -> Result<(), Self::Error> {
        self.0.replace(None);
        Ok(())
    }
}

fn unique_name() -> String {
    format!(
        "axiusflow-engine-test-{}-{}",
        std::process::id(),
        NEXT_NAME.fetch_add(1, Ordering::Relaxed)
    )
}

fn hello(token: &[u8], nonce: u64, role: StreamRole) -> Vec<u8> {
    let release = axiusflow_platform_runtime::current_release_identity();
    hello_for(
        token,
        &release.release_identity,
        release.install_generation,
        nonce,
        role,
    )
}

fn hello_for(
    token: &[u8],
    release_identity: &str,
    install_generation: u64,
    nonce: u64,
    role: StreamRole,
) -> Vec<u8> {
    encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: Some(envelope::Payload::ClientHello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            installation_token: token.to_vec(),
            client_kind: ClientKind::Ui as i32,
            release_identity: release_identity.to_string(),
            install_generation,
            session_nonce: nonce,
            stream_role: role as i32,
        })),
    })
    .expect("encode hello")
}

fn connect_pair(name: &str) -> (LocalSocketStream, LocalSocketStream) {
    let socket_name = name
        .to_ns_name::<GenericNamespaced>()
        .expect("create socket name");
    let command = LocalSocketStream::connect(socket_name).expect("connect command stream");
    let socket_name = name
        .to_ns_name::<GenericNamespaced>()
        .expect("create socket name");
    let event = LocalSocketStream::connect(socket_name).expect("connect event stream");
    (command, event)
}

/// Serves one paired session on a bound listener: accepts both streams in any
/// arrival order, pairs them by nonce, and serves the completed session.
fn serve_one_pair(listener: &LocalSocketListener, token: &[u8], epoch: u64) {
    serve_one_pair_with_state(listener, token, epoch, &EngineState::default());
}

fn serve_one_pair_with_state(
    listener: &LocalSocketListener,
    token: &[u8],
    epoch: u64,
    state: &EngineState,
) {
    let pairer = SessionPairer::new();
    let mut pair = None;
    for _ in 0..2 {
        let stream = listener.accept().expect("accept client stream");
        if let Some(completed) = pairer
            .accept_one(stream, token)
            .expect("pair client stream")
        {
            pair = Some(completed);
            break;
        }
    }
    let pair = pair.expect("session paired");
    serve_client_with_state(pair, epoch, state).expect("serve client");
}

fn exchange(token: &[u8], client_token: &[u8]) -> envelope::Payload {
    let nonce = NEXT_NAME.fetch_add(1, Ordering::Relaxed);
    exchange_hello(
        token,
        &hello(client_token, nonce, StreamRole::Command),
        &hello(client_token, nonce, StreamRole::Event),
    )
}

fn exchange_hello(token: &[u8], command_hello: &[u8], event_hello: &[u8]) -> envelope::Payload {
    let name = unique_name();
    let listener = bind_listener(&name).expect("bind engine listener");
    let expected = token.to_vec();
    let server = thread::spawn(move || {
        let pairer = SessionPairer::new();
        // Both arrivals are always drained so every offending stream receives
        // its fault regardless of arrival order; a completed pair is served.
        let mut pair = None;
        for _ in 0..2 {
            let stream = listener.accept().expect("accept client stream");
            if let Ok(Some(completed)) = pairer.accept_one(stream, &expected) {
                pair = Some(completed);
                break;
            }
        }
        if let Some(pair) = pair {
            serve_client(pair, 41).expect("serve client");
        }
    });
    let (mut command, mut event) = connect_pair(&name);
    command.write_all(command_hello).expect("write hello");
    event.write_all(event_hello).expect("write hello");
    let mut bytes = [0_u8; 4096];
    let count = command.read(&mut bytes).expect("read reply");
    drop(command);
    drop(event);
    server.join().expect("join server");
    let mut decoder = EnvelopeDecoder::try_new().expect("create decoder");
    decoder
        .push(&bytes[..count])
        .expect("decode reply")
        .pop()
        .and_then(|envelope| envelope.payload)
        .expect("reply payload")
}

#[test]
fn valid_installation_token_receives_engine_readiness() {
    let token = [7_u8; 32];
    let envelope::Payload::EngineReady(ready) = exchange(&token, &token) else {
        panic!("expected engine readiness");
    };
    assert_eq!(ready.protocol_version, PROTOCOL_VERSION);
    assert_eq!(ready.engine_epoch, 41);
}

#[test]
fn invalid_installation_token_is_rejected_without_readiness() {
    let envelope::Payload::Fault(fault) = exchange(&[7_u8; 32], &[8_u8; 32]) else {
        panic!("expected authentication fault");
    };
    assert_eq!(fault.code, EngineFaultCode::Unauthenticated as i32);
}

#[test]
fn mismatched_release_identity_is_rejected_before_readiness() {
    let token = [7_u8; 32];
    let nonce = NEXT_NAME.fetch_add(1, Ordering::Relaxed);
    let envelope::Payload::Fault(fault) = exchange_hello(
        &token,
        &hello_for(&token, "superseded-release", 99, nonce, StreamRole::Command),
        &hello_for(&token, "superseded-release", 99, nonce, StreamRole::Event),
    ) else {
        panic!("expected release identity fault");
    };
    assert_eq!(fault.code, EngineFaultCode::VersionMismatch as i32);
    assert!(fault.redacted_detail.contains("release identities"));
}

#[test]
fn listener_name_has_exactly_one_owner() {
    let name = unique_name();
    let _first = bind_listener(&name).expect("bind first owner");
    assert!(matches!(
        bind_listener(&name)
            .expect_err("reject second owner")
            .kind(),
        std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
    ));
}

#[test]
fn installation_token_is_created_once_and_reused() {
    let vault = MemoryVault::default();
    let first = load_or_create_installation_token(&vault).expect("create token");
    let second = load_or_create_installation_token(&vault).expect("reload token");
    assert_eq!(first.len(), 32);
    assert_eq!(first.as_slice(), second.as_slice());
    assert!(first.iter().any(|byte| *byte != 0));
}

#[test]
fn malformed_stored_token_fails_closed() {
    let vault = MemoryVault(RefCell::new(Some(vec![7_u8; 31])));
    assert_eq!(
        load_or_create_installation_token(&vault).expect_err("reject malformed token"),
        "native engine credential has an invalid length"
    );
}

#[test]
fn authenticated_client_restores_engine_owned_workspace() {
    let name = unique_name();
    let listener = bind_listener(&name).expect("bind engine listener");
    let token = [9_u8; 32];
    let server = thread::spawn(move || {
        serve_one_pair(&listener, &token, 73);
    });
    let mut client = EngineClient::connect(&name, &token).expect("connect engine client");
    assert_eq!(client.ready().engine_epoch, 73);
    let workspace = client.restore_workspace().expect("restore workspace");
    assert_eq!(workspace.provider, "hyperliquid");
    assert_eq!(workspace.market, "BTC-PERP");
    assert_eq!(workspace.interval_seconds, 60);
    let instrument = workspace.workspace_tabs[0].panes[0]
        .instrument
        .as_ref()
        .expect("fresh workspace has a canonical chart instrument");
    assert_eq!(instrument.price_scale, 8);
    assert_eq!(instrument.quantity_scale, 8);
    drop(client);
    server.join().expect("join server");
}

#[test]
fn framed_session_survives_repeated_handshake_burst_and_reconnect() {
    // Windows transport contract: the framed session must survive repeated
    // handshakes, duplex command/reply bursts, disconnect, and reconnect
    // without interpreting temporary no-data as peer closure.
    for round in 0..5_u64 {
        let name = unique_name();
        let listener = bind_listener(&name).expect("bind engine listener");
        let token = [9_u8; 32];
        let server = thread::spawn(move || {
            serve_one_pair(&listener, &token, 73 + round);
        });
        let mut client = EngineClient::connect(&name, &token).expect("connect engine client");
        assert_eq!(client.ready().engine_epoch, 73 + round);
        for _ in 0..16 {
            let workspace = client
                .restore_workspace()
                .expect("restore workspace in burst");
            assert_eq!(workspace.provider, "hyperliquid");
            assert_eq!(workspace.market, "BTC-PERP");
        }
        drop(client);
        server.join().expect("join server");
    }
}
#[test]
fn operational_resource_mode_updates_without_revising_user_workspace() {
    let state = EngineState::default();
    let before = state.workspace();
    let interactive = state.set_resource_mode(ResourceMode::Interactive);
    assert_eq!(interactive.resource_mode, ResourceMode::Interactive as i32);
    assert_eq!(interactive.workspace_revision, before.workspace_revision);
    let warm = state.set_resource_mode(ResourceMode::Warm);
    assert_eq!(warm.resource_mode, ResourceMode::Warm as i32);
    assert_eq!(warm.workspace_revision, before.workspace_revision);
}

#[test]
fn workspace_selection_is_durable_across_engine_restart() {
    let directory = TestDirectory::new();
    let state = EngineState::open(&directory.0).expect("open persistent state");
    let name = unique_name();
    let listener = bind_listener(&name).expect("bind engine listener");
    let token = [11_u8; 32];
    let server = thread::spawn(move || {
        serve_one_pair_with_state(&listener, &token, 91, &state);
    });
    let mut client = EngineClient::connect(&name, &token).expect("connect engine client");
    let restored = client.restore_workspace().expect("restore workspace");
    let updated = client
        .set_provider_selection(
            "rithmic".to_string(),
            "MNQU6".to_string(),
            300,
            restored.workspace_revision,
            1,
        )
        .expect("persist selection");
    assert_eq!(updated.workspace_revision, 1);
    drop(client);
    server.join().expect("join server");

    let reopened = EngineState::open(&directory.0).expect("reopen persistent state");
    assert_eq!(reopened.workspace().provider, "rithmic");
    assert_eq!(reopened.workspace().market, "MNQU6");
    assert_eq!(reopened.workspace().interval_seconds, 300);
    assert_eq!(reopened.workspace().workspace_revision, 1);
    assert_eq!(reopened.workspace().schema_revision, 5);
    assert!(reopened.workspace().workspace_tabs[0].layout.is_some());
    assert_eq!(reopened.workspace().cache_manifest_revision, 1);
    assert_eq!(reopened.workspace().hot_series[0].provider, "rithmic");
}

#[test]
fn chart_viewport_is_generation_fenced_and_persisted_independently() {
    let directory = TestDirectory::new();
    let state = EngineState::open(&directory.0).expect("open persistent state");
    let name = unique_name();
    let listener = bind_listener(&name).expect("bind engine listener");
    let token = [12_u8; 32];
    let server = thread::spawn(move || {
        serve_one_pair_with_state(&listener, &token, 92, &state);
    });
    let mut client = EngineClient::connect(&name, &token).expect("connect engine client");
    let restored = client.restore_workspace().expect("restore workspace");
    let selected = client
        .set_selection(
            restored.market,
            restored.interval_seconds,
            restored.workspace_revision,
            7,
        )
        .expect("install selection generation");
    assert_eq!(
        client
            .set_viewport(1_000, 2_000, 6)
            .expect_err("reject stale viewport"),
        "chart viewport selection is stale"
    );
    let first = client
        .set_viewport(1_000, 2_000, 7)
        .expect("persist first viewport");
    let second = client
        .set_viewport(2_000, 3_000, 7)
        .expect("persist second viewport");
    let latest = client
        .set_viewport(3_000, 4_000, 7)
        .expect("persist latest viewport");
    assert_eq!(latest.workspace_revision, selected.workspace_revision);
    assert_eq!(
        latest.cache_manifest_revision,
        first.cache_manifest_revision + 2
    );
    assert_eq!(
        second.cache_manifest_revision,
        first.cache_manifest_revision + 1
    );
    drop(client);
    server.join().expect("join server");

    let manifests = fs::read_dir(&directory.0)
        .expect("read workspace directory")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("hot-set-"))
        .count();
    assert_eq!(manifests, 2);
    let reopened = EngineState::open(&directory.0).expect("reopen persistent state");
    let active = reopened
        .workspace()
        .hot_series
        .into_iter()
        .find(|series| series.market == "BTC-PERP" && series.interval_seconds == 60)
        .expect("active hot series");
    assert_eq!(active.viewport_start_unix_nanos, Some(3_000));
    assert_eq!(active.viewport_end_unix_nanos, Some(4_000));
}

#[test]
fn shutdown_flush_preserves_the_latest_hot_set_and_fences_late_mutation() {
    let directory = TestDirectory::new();
    let state = EngineState::open(&directory.0).expect("open persistent state");
    let shutdown_state = state.clone();
    let name = unique_name();
    let listener = bind_listener(&name).expect("bind engine listener");
    let token = [14_u8; 32];
    let server = thread::spawn(move || {
        serve_one_pair_with_state(&listener, &token, 94, &state);
    });
    let mut client = EngineClient::connect(&name, &token).expect("connect engine client");
    let restored = client.restore_workspace().expect("restore workspace");
    let selected = client
        .set_selection(
            restored.market,
            restored.interval_seconds,
            restored.workspace_revision,
            7,
        )
        .expect("select initial series");
    let viewport = client
        .set_viewport(1_000, 2_000, 7)
        .expect("persist older hot-set manifest");
    let latest = client
        .set_provider_selection(
            "rithmic".to_string(),
            "MNQU6".to_string(),
            300,
            selected.workspace_revision,
            8,
        )
        .expect("select newer series without another viewport");
    assert_eq!(
        latest.cache_manifest_revision,
        viewport.cache_manifest_revision
    );
    drop(client);
    server.join().expect("join server");

    let flushed = shutdown_state
        .persist_shutdown_hot_set()
        .expect("persist final hot set");
    assert_eq!(
        flushed.cache_manifest_revision,
        latest.cache_manifest_revision + 1
    );

    let name = unique_name();
    let listener = bind_listener(&name).expect("bind shutdown listener");
    let blocked_state = shutdown_state.clone();
    let server = thread::spawn(move || {
        serve_one_pair_with_state(&listener, &token, 95, &blocked_state);
    });
    let mut client = EngineClient::connect(&name, &token).expect("connect shutdown client");
    let current = client
        .restore_workspace()
        .expect("restore flushed workspace");
    assert_eq!(
        client
            .set_selection(
                current.market,
                current.interval_seconds,
                current.workspace_revision,
                9,
            )
            .expect_err("late workspace mutation is fenced"),
        "engine workspace is shutting down"
    );
    drop(client);
    server.join().expect("join shutdown server");

    let reopened = EngineState::open(&directory.0).expect("reopen flushed state");
    let workspace = reopened.workspace();
    assert_eq!(
        workspace.cache_manifest_revision,
        flushed.cache_manifest_revision
    );
    assert_eq!(workspace.provider, "rithmic");
    assert_eq!(workspace.market, "MNQU6");
    assert!(
        workspace
            .hot_series
            .iter()
            .any(|series| series.provider == "hyperliquid"),
        "the persisted hot set retains the Hyperliquid default"
    );
    assert!(
        workspace
            .hot_series
            .iter()
            .any(|series| series.provider == "rithmic" && series.market == "MNQU6"),
        "the persisted hot set retains the Rithmic selection"
    );
    let manifests = fs::read_dir(&directory.0)
        .expect("read workspace directory")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("hot-set-"))
        .count();
    assert_eq!(manifests, 2);
}

#[test]
fn corrupt_latest_workspace_is_quarantined_and_falls_back() {
    let directory = TestDirectory::new();
    fs::write(
        directory.0.join("workspace-00000000000000000007.frame"),
        b"corrupt",
    )
    .expect("write corrupt state");
    let state = EngineState::open(&directory.0).expect("recover workspace state");
    assert_eq!(state.workspace().workspace_revision, 0);
    assert!(
        directory
            .0
            .join("workspace-00000000000000000007.corrupt-0")
            .exists()
    );
}
#[test]
fn workspace_layout_order_sizes_and_consumer_ids_survive_restart_and_stale_writes_fail() {
    let directory = TestDirectory::new();
    let state = EngineState::open(&directory.0).expect("open persistent state");
    let name = unique_name();
    let listener = bind_listener(&name).expect("bind engine listener");
    let token = [15_u8; 32];
    let server = thread::spawn(move || {
        serve_one_pair_with_state(&listener, &token, 96, &state);
    });
    let mut client = EngineClient::connect(&name, &token).expect("connect engine client");
    let restored = client.restore_workspace().expect("restore workspace");
    let first = restored.workspace_tabs[0].panes[0].clone();
    let mut second = first.clone();
    second.pane_id = 7;
    second.consumer_id = 41;
    second.size_basis_points = 3_500;
    second.generation = 4;
    let mut first = first;
    first.size_basis_points = 6_500;
    first.generation = 3;
    let mut third = first.clone();
    third.pane_id = 9;
    third.consumer_id = 43;
    third.size_basis_points = 10_000;
    third.generation = 6;
    let layout = vec![
        WorkspaceTabState {
            workspace_id: 5,
            label: "Rates".to_string(),
            split_axis: WorkspaceSplitAxis::Vertical as i32,
            panes: vec![first, second],
            active_pane_id: 7,
            generation: 8,
            layout: Some(WorkspaceLayoutState {
                pane_id: 0,
                split_axis: WorkspaceSplitAxis::Vertical as i32,
                ratio_basis_points: 6_500,
                first: Some(Box::new(WorkspaceLayoutState {
                    pane_id: 1,
                    ..WorkspaceLayoutState::default()
                })),
                second: Some(Box::new(WorkspaceLayoutState {
                    pane_id: 7,
                    ..WorkspaceLayoutState::default()
                })),
            }),
        },
        WorkspaceTabState {
            workspace_id: 3,
            label: "Futures".to_string(),
            split_axis: WorkspaceSplitAxis::Horizontal as i32,
            panes: vec![third],
            active_pane_id: 9,
            generation: 11,
            layout: Some(WorkspaceLayoutState {
                pane_id: 9,
                ..WorkspaceLayoutState::default()
            }),
        },
    ];
    let updated = client
        .set_workspace_layout(
            restored.workspace_revision,
            restored.layout_generation + 1,
            3,
            layout.clone(),
        )
        .expect("persist multi-workspace layout");
    assert_eq!(updated.workspace_tabs, layout);
    assert_eq!(updated.active_workspace_id, 3);
    assert_eq!(
        client
            .set_workspace_layout(
                updated.workspace_revision,
                updated.layout_generation,
                5,
                updated.workspace_tabs.clone(),
            )
            .expect_err("reject stale layout generation"),
        "workspace revision is stale"
    );
    drop(client);
    server.join().expect("join server");

    let reopened = EngineState::open(&directory.0).expect("reopen persistent state");
    let workspace = reopened.workspace();
    assert_eq!(workspace.workspace_tabs, layout);
    assert_eq!(workspace.active_workspace_id, 3);
    assert_eq!(workspace.workspace_tabs[0].panes[0].consumer_id, 1);
    assert_eq!(workspace.workspace_tabs[0].panes[1].consumer_id, 41);
    assert_eq!(workspace.workspace_tabs[1].panes[0].consumer_id, 43);
}
