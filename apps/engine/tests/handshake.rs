use std::{
    cell::RefCell,
    fs,
    io::{Read, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread,
};

use axiusflow_coinbase_market_adapter::{CoinbaseSpotProduct, coinbase_instrument_id};
use axiusflow_engine::{
    EngineClient, EnginePublicationHub, EngineState, bind_listener, load_coinbase_catalog,
    load_or_create_installation_token, persist_coinbase_catalog, serve_client,
    serve_client_with_publications, serve_client_with_state,
};
use axiusflow_local_engine_protocol::{
    ChartProvenance, ChartSnapshot, ClientHello, ClientKind, EngineFaultCode, Envelope,
    EnvelopeDecoder, PROTOCOL_VERSION, ResourceMode, ViewKind, WorkspaceState, encode_envelope,
    envelope,
};
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
    encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        payload: Some(envelope::Payload::ClientHello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            installation_token: token.to_vec(),
            client_kind: ClientKind::Ui as i32,
        })),
    })
    .expect("encode hello")
}

fn exchange(token: &[u8], client_token: &[u8]) -> envelope::Payload {
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
    stream.write_all(&hello(client_token)).expect("write hello");
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
    drop(client);
    server.join().expect("join server");
}

#[test]
fn session_subscription_receives_retained_covering_chart_state() {
    let name = unique_name();
    let listener = bind_listener(&name).expect("bind engine listener");
    let token = [13_u8; 32];
    let state = EngineState::default();
    let publications = EnginePublicationHub::default();
    let covering = envelope::Payload::ChartSnapshot(ChartSnapshot {
        market: "BTC-USD".to_string(),
        interval_seconds: 60,
        engine_epoch: 101,
        selection_generation: 1,
        provider_generation: 2,
        payload: vec![1, 2, 3],
        provenance: ChartProvenance::LocalCache as i32,
    });
    publications.publish_covering(ViewKind::Chart, std::slice::from_ref(&covering));
    let server_publications = publications.clone();
    let server = thread::spawn(move || {
        let stream = listener.accept().expect("accept client");
        let _ = serve_client_with_publications(stream, &token, 101, &state, &server_publications);
    });
    let client = EngineClient::connect(&name, &token).expect("connect engine client");
    let mut stream = client
        .subscribe_view(ViewKind::Session)
        .expect("subscribe session");
    assert_eq!(stream.receive().expect("receive retained chart"), covering);
    drop(stream);
    publications.publish_transient(
        ViewKind::Chart,
        &envelope::Payload::ChartSnapshot(ChartSnapshot {
            market: "BTC-USD".to_string(),
            interval_seconds: 60,
            engine_epoch: 101,
            selection_generation: 1,
            provider_generation: 3,
            payload: vec![4],
            provenance: ChartProvenance::LiveProvider as i32,
        }),
    );
    server.join().expect("join server");
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
        .set_selection("ETH-USD".to_string(), 300, restored.workspace_revision, 1)
        .expect("persist selection");
    assert_eq!(updated.workspace_revision, 1);
    drop(client);
    server.join().expect("join server");

    let reopened = EngineState::open(&directory.0).expect("reopen persistent state");
    assert_eq!(reopened.workspace().market, "ETH-USD");
    assert_eq!(reopened.workspace().interval_seconds, 300);
    assert_eq!(reopened.workspace().workspace_revision, 1);
    assert_eq!(reopened.workspace().schema_revision, 1);
    assert_eq!(reopened.workspace().cache_manifest_revision, 1);
    assert_eq!(reopened.workspace().hot_series[0].market, "ETH-USD");
    assert_eq!(reopened.workspace().hot_series[0].interval_seconds, 300);
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
    };
    let bytes = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
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
    assert_eq!(workspace.schema_revision, 1);
    assert_eq!(workspace.cache_manifest_revision, 1);
    assert_eq!(workspace.hot_series.len(), 1);
    assert_eq!(workspace.hot_series[0].market, "ETH-USD");
    assert!(
        directory
            .0
            .join("workspace-00000000000000000008.frame")
            .exists()
    );
}

#[test]
fn resident_catalog_roundtrips_exact_product_precision() {
    let directory = TestDirectory::new();
    let products = vec![coinbase_product("BTC", "USD", 2, 8)];
    persist_coinbase_catalog(&directory.0, &products).expect("persist catalog");
    assert_eq!(
        load_coinbase_catalog(&directory.0).expect("load catalog"),
        Some(products)
    );
}

#[test]
fn corrupt_latest_catalog_is_quarantined_and_falls_back() {
    let directory = TestDirectory::new();
    let first = vec![coinbase_product("BTC", "USD", 2, 8)];
    persist_coinbase_catalog(&directory.0, &first).expect("persist first catalog");
    persist_coinbase_catalog(&directory.0, &[coinbase_product("ETH", "USD", 2, 8)])
        .expect("persist second catalog");
    fs::write(
        directory
            .0
            .join("coinbase-catalog-00000000000000000002.frame"),
        b"corrupt",
    )
    .expect("corrupt latest catalog");
    assert_eq!(
        load_coinbase_catalog(&directory.0).expect("load fallback catalog"),
        Some(first)
    );
    assert!(
        directory
            .0
            .join("coinbase-catalog-00000000000000000002.corrupt-0")
            .exists()
    );
}

fn coinbase_product(
    base: &str,
    quote: &str,
    price_scale: u8,
    quantity_scale: u8,
) -> CoinbaseSpotProduct {
    let product_id = format!("{base}-{quote}");
    CoinbaseSpotProduct {
        instrument_id: coinbase_instrument_id(&product_id).expect("valid product identity"),
        display_symbol: format!("{base}/{quote}"),
        product_id,
        base_currency: base.to_string(),
        quote_currency: quote.to_string(),
        price_scale,
        quantity_scale,
    }
}
