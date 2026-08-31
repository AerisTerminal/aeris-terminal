use std::{
    cell::RefCell,
    fs,
    io::{Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use axiusflow_engine::{EngineState, bind_listener, serve_client, serve_client_with_state};
use axiusflow_engine_protocol::{
    ClientHello, ClientKind, EngineFaultCode, Envelope, EnvelopeDecoder, HotSeries,
    PROTOCOL_VERSION, ResourceMode, WorkspaceLayoutState, WorkspaceSplitAxis, WorkspaceState,
    WorkspaceTabState, encode_envelope, envelope,
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

fn hello(token: &[u8]) -> Vec<u8> {
    let release = axiusflow_platform_runtime::current_release_identity();
    hello_for(token, &release.release_identity, release.install_generation)
}

fn hello_for(token: &[u8], release_identity: &str, install_generation: u64) -> Vec<u8> {
    encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: Some(envelope::Payload::ClientHello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            installation_token: token.to_vec(),
            client_kind: ClientKind::Ui as i32,
            release_identity: release_identity.to_string(),
            install_generation,
        })),
    })
    .expect("encode hello")
}

fn exchange(token: &[u8], client_token: &[u8]) -> envelope::Payload {
    exchange_hello(token, &hello(client_token))
}

fn exchange_hello(token: &[u8], hello: &[u8]) -> envelope::Payload {
    let name = unique_name();
    let listener = bind_listener(&name).expect("bind engine listener");
    let expected = token.to_vec();
    let server = thread::spawn(move || {
        let stream = listener.accept().expect("accept client");
        serve_client(stream, &expected, 41).expect("serve client");
    });
    let socket_name = name
        .to_ns_name::<GenericNamespaced>()
        .expect("create socket name");
    let mut stream = LocalSocketStream::connect(socket_name).expect("connect client");
    stream.write_all(hello).expect("write hello");
    let mut bytes = [0_u8; 4096];
    let count = stream.read(&mut bytes).expect("read reply");
    drop(stream);
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
    let envelope::Payload::Fault(fault) =
        exchange_hello(&token, &hello_for(&token, "superseded-release", 99))
    else {
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
        let stream = listener.accept().expect("accept client");
        serve_client(stream, &token, 73).expect("serve client");
    });
    let mut client = EngineClient::connect(&name, &token).expect("connect engine client");
    assert_eq!(client.ready().engine_epoch, 73);
    let workspace = client.restore_workspace().expect("restore workspace");
    assert_eq!(workspace.provider, "coinbase");
    assert_eq!(workspace.market, "BTC-USD");
    assert_eq!(workspace.interval_seconds, 60);
    let instrument = workspace.workspace_tabs[0].panes[0]
        .instrument
        .as_ref()
        .expect("fresh workspace has a canonical chart instrument");
    assert_eq!(instrument.price_scale, 2);
    assert_eq!(instrument.quantity_scale, 8);
    drop(client);
    server.join().expect("join server");
}

#[test]
fn schema_four_coinbase_precision_is_repaired_before_workspace_restore() {
    let directory = TestDirectory::new();
    let mut stale = EngineState::default().workspace();
    stale.workspace_revision = 14;
    stale.hot_series[0].price_scale = 0;
    stale.hot_series[0].quantity_scale = 0;
    let instrument = stale.workspace_tabs[0].panes[0]
        .instrument
        .as_mut()
        .expect("default workspace has an instrument");
    instrument.price_scale = 0;
    instrument.quantity_scale = 0;
    let bytes = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: Some(envelope::Payload::WorkspaceState(stale)),
    })
    .expect("encode stale schema-four workspace");
    fs::write(
        directory.0.join("workspace-00000000000000000014.frame"),
        bytes,
    )
    .expect("write stale schema-four workspace");

    let repaired = EngineState::open(&directory.0).expect("repair workspace precision");
    let workspace = repaired.workspace();
    assert_eq!(workspace.workspace_revision, 15);
    assert_eq!(workspace.hot_series[0].price_scale, 2);
    assert_eq!(workspace.hot_series[0].quantity_scale, 8);
    let instrument = workspace.workspace_tabs[0].panes[0]
        .instrument
        .as_ref()
        .expect("repaired workspace has an instrument");
    assert_eq!(instrument.price_scale, 2);
    assert_eq!(instrument.quantity_scale, 8);
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
        let stream = listener.accept().expect("accept client");
        serve_client_with_state(stream, &token, 91, &state).expect("serve client");
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
    assert_eq!(reopened.workspace().hot_series[0].provider, "coinbase");
}

#[test]
fn chart_viewport_is_generation_fenced_and_persisted_independently() {
    let directory = TestDirectory::new();
    let state = EngineState::open(&directory.0).expect("open persistent state");
    let name = unique_name();
    let listener = bind_listener(&name).expect("bind engine listener");
    let token = [12_u8; 32];
    let server = thread::spawn(move || {
        let stream = listener.accept().expect("accept client");
        serve_client_with_state(stream, &token, 92, &state).expect("serve client");
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
        .find(|series| series.market == "BTC-USD" && series.interval_seconds == 60)
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
        let stream = listener.accept().expect("accept client");
        serve_client_with_state(stream, &token, 94, &state).expect("serve client");
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
        let stream = listener.accept().expect("accept shutdown client");
        serve_client_with_state(stream, &token, 95, &blocked_state).expect("serve shutdown client");
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
            .all(|series| series.provider == "coinbase"),
        "legacy selection without exact Rithmic metadata is not fabricated into the hot set"
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
fn legacy_workspace_migrates_to_a_revisioned_hot_set() {
    let directory = TestDirectory::new();
    let legacy = WorkspaceState {
        provider: "coinbase".to_string(),
        market: "ETH-USD".to_string(),
        interval_seconds: 300,
        watchlist: vec!["ETH-USD".to_string()],
        workspace_revision: 7,
        warm_mode_enabled: true,
        resource_mode: ResourceMode::Warm as i32,
        schema_revision: 0,
        cache_manifest_revision: 0,
        hot_series: Vec::new(),
        lifetime_mode: 0,
        autostart_enabled: false,
        markets_live_permitted: false,
        ..WorkspaceState::default()
    };
    let bytes = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: Some(envelope::Payload::WorkspaceState(legacy)),
    })
    .expect("encode legacy workspace");
    fs::write(
        directory.0.join("workspace-00000000000000000007.frame"),
        bytes,
    )
    .expect("write legacy workspace");
    let migrated = EngineState::open(&directory.0).expect("migrate workspace");
    let workspace = migrated.workspace();
    assert_eq!(workspace.workspace_revision, 8);
    assert_eq!(workspace.schema_revision, 5);
    assert!(workspace.workspace_tabs[0].layout.is_some());
    assert_eq!(workspace.cache_manifest_revision, 1);
    assert_eq!(workspace.hot_series.len(), 1);
    assert_eq!(workspace.hot_series[0].market, "ETH-USD");
    assert_eq!(
        workspace.hot_series[0].instrument_id,
        "instrument:coinbase:eth:usd"
    );
    assert_eq!(
        workspace.hot_series[0].entitlement_id,
        "crypto_public_realtime"
    );
    assert!(
        directory
            .0
            .join("workspace-00000000000000000008.frame")
            .exists()
    );
}

#[test]
fn schema_two_migration_keeps_supported_coinbase_and_discards_incomplete_rithmic() {
    let directory = TestDirectory::new();
    let legacy_series =
        |provider: &str, market: &str, interval_seconds: u32, score: u32| HotSeries {
            provider: provider.to_string(),
            market: market.to_string(),
            interval_seconds,
            score,
            last_used_unix_seconds: u64::from(score),
            provider_watermark: 9,
            series_watermark: 11,
            viewport_start_unix_nanos: Some(1_000),
            viewport_end_unix_nanos: Some(2_000),
            account_id: String::new(),
            instrument_id: String::new(),
            entitlement_id: String::new(),
            cadence: 0,
            cadence_value: 0,
            definition_revision: 0,
            pinned: false,
            workspace_ids: Vec::new(),
            coverage_start_unix_nanos: None,
            coverage_end_unix_nanos: None,
            provider_symbol: String::new(),
            venue_id: String::new(),
            display_symbol: String::new(),
            price_scale: 0,
            quantity_scale: 0,
        };
    let legacy = WorkspaceState {
        provider: "rithmic".to_string(),
        market: "MNQU6".to_string(),
        interval_seconds: 300,
        watchlist: vec!["BTC-USD".to_string(), "MNQU6".to_string()],
        workspace_revision: 12,
        warm_mode_enabled: true,
        resource_mode: ResourceMode::Warm as i32,
        schema_revision: 2,
        cache_manifest_revision: 3,
        hot_series: vec![
            legacy_series("coinbase", "BTC-USD", 300, 1),
            legacy_series("rithmic", "MNQU6", 300, 2),
        ],
        lifetime_mode: axiusflow_engine_protocol::EngineLifetimeMode::KeepEngineWarm as i32,
        autostart_enabled: false,
        markets_live_permitted: false,
        ..WorkspaceState::default()
    };
    let bytes = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: Some(envelope::Payload::WorkspaceState(legacy)),
    })
    .expect("encode schema-two workspace");
    fs::write(
        directory.0.join("workspace-00000000000000000012.frame"),
        bytes,
    )
    .expect("write schema-two workspace");

    let migrated = EngineState::open(&directory.0).expect("migrate schema-two workspace");
    let workspace = migrated.workspace();

    assert_eq!(workspace.schema_revision, 5);
    assert!(workspace.workspace_tabs[0].layout.is_some());
    assert_eq!(workspace.workspace_revision, 13);
    assert_eq!(workspace.hot_series.len(), 1);
    let series = &workspace.hot_series[0];
    assert_eq!(series.provider, "coinbase");
    assert_eq!(series.instrument_id, "instrument:coinbase:btc:usd");
    assert_eq!(series.account_id, "coinbase_public_market_data");
    assert_eq!(series.entitlement_id, "crypto_public_realtime");
    assert_eq!(series.provider_watermark, 9);
    assert_eq!(series.series_watermark, 11);
    assert_eq!(series.viewport_start_unix_nanos, Some(1_000));
    assert_eq!(series.viewport_end_unix_nanos, Some(2_000));
    assert_eq!(workspace.workspace_tabs.len(), 1);
    assert_eq!(workspace.workspace_tabs[0].panes[0].consumer_id, 1);
}

#[test]
fn workspace_layout_order_sizes_and_consumer_ids_survive_restart_and_stale_writes_fail() {
    let directory = TestDirectory::new();
    let state = EngineState::open(&directory.0).expect("open persistent state");
    let name = unique_name();
    let listener = bind_listener(&name).expect("bind engine listener");
    let token = [15_u8; 32];
    let server = thread::spawn(move || {
        let stream = listener.accept().expect("accept client");
        serve_client_with_state(stream, &token, 96, &state).expect("serve client");
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
            label: "Crypto".to_string(),
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
