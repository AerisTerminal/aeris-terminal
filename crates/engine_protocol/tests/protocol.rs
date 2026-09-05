use std::num::NonZeroUsize;

use axiusflow_engine_protocol::{
    AccountSessionState, AccountView, AttachClient, BeginLogin, CancelLogin, ClientHello,
    ClientKind, DemandError, DetachClient, EngineFaultCode, EngineLifetimeMode, EngineReady,
    EngineShutdownState, EngineStatus, Envelope, EnvelopeDecoder, Fault, GetAccountStatus,
    GetEngineStatus, Goodbye, HotSeries, InstallProviderInstrument, LIFECYCLE_CONTRACT_REVISION,
    LoginAuthorization, MAX_FRAME_BYTES, MarketBar, OrderBookLevel, OrderBookSnapshot,
    OrderBookState, OrderFlowAggressor, OrderFlowLevel, OrderFlowSnapshot, OrderFlowTrade,
    OrderFlowUpdate, PROTOCOL_VERSION, PersistenceState, ProtocolError, ProviderCatalogRejected,
    ProviderCatalogRejectionReason, ProviderConnectionState, ProviderInstrumentSearchResult,
    ProviderInstrumentSelection, ProviderInstrumentSummary, ProviderState, RegisterConsumer,
    RemoveConsumer, ResourceMode, RestoreWorkspace, SearchProviderInstruments,
    SelectProviderInstrument, SeriesCadence, SeriesDemand, SeriesKey, SeriesLoadState,
    SeriesSnapshot, SeriesState, SeriesUpdate, SetEngineLifecycle, SetEngineResourceMode,
    SetSelection, SetViewport, SetWatchlist, SetWorkspaceLayout, ShutdownEngine, SignOut,
    StreamRole, ViewportDemand, VisibilityDemand, WorkspaceLayoutState, WorkspacePaneKind,
    WorkspacePaneState, WorkspaceSplitAxis, WorkspaceState, WorkspaceTabState, encode_envelope,
    envelope,
};
use axiusflow_transport::encode_binary_frame;

fn payloads() -> Vec<envelope::Payload> {
    let mut payloads = workspace_payloads();
    payloads.extend(market_payloads());
    payloads.extend(account_payloads());
    payloads
}

#[test]
fn markets_live_resource_mode_has_a_stable_wire_value() {
    assert_eq!(ResourceMode::MarketsLive as i32, 4);
    assert_eq!(ResourceMode::try_from(4), Ok(ResourceMode::MarketsLive));
}

#[test]
fn protocol_version_tracks_the_three_mebibyte_frame_contract() {
    assert_eq!(PROTOCOL_VERSION, 18);
}

fn account_payloads() -> Vec<envelope::Payload> {
    vec![
        envelope::Payload::BeginLogin(BeginLogin {
            client_id: 11,
            request_generation: 3,
        }),
        envelope::Payload::CancelLogin(CancelLogin {
            request_generation: 3,
        }),
        envelope::Payload::GetAccountStatus(GetAccountStatus {}),
        envelope::Payload::LoginAuthorization(LoginAuthorization {
            request_generation: 3,
            authorization_url: "https://auth.axiusflow.com/authorize?request=3".into(),
            expires_unix_seconds: 1_800_000_003,
        }),
        envelope::Payload::AccountView(AccountView {
            state: AccountSessionState::Active as i32,
            account_id: "acct_01".into(),
            plan_id: "pro".into(),
            detail: "active".into(),
            request_generation: 3,
        }),
        envelope::Payload::SignOut(SignOut {}),
    ]
}

#[test]
fn account_session_states_have_stable_wire_values() {
    assert_eq!(AccountSessionState::SignedOut as i32, 0);
    assert_eq!(AccountSessionState::Authorizing as i32, 1);
    assert_eq!(AccountSessionState::Active as i32, 2);
    assert_eq!(AccountSessionState::OfflineLease as i32, 3);
    assert_eq!(AccountSessionState::ReauthenticationRequired as i32, 4);
    assert_eq!(AccountSessionState::LeaseExpired as i32, 5);
    assert_eq!(AccountSessionState::TerminalError as i32, 6);
}

fn workspace_payloads() -> Vec<envelope::Payload> {
    let tab = workspace_tab();
    vec![
        envelope::Payload::ClientHello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            installation_token: vec![7; 32],
            client_kind: ClientKind::Ui as i32,
            release_identity: "release-1".to_string(),
            install_generation: 1,
            session_nonce: 9,
            stream_role: StreamRole::Command as i32,
        }),
        envelope::Payload::EngineReady(EngineReady {
            protocol_version: PROTOCOL_VERSION,
            engine_epoch: 7,
            workspace_revision: 3,
            lifecycle_contract_revision: LIFECYCLE_CONTRACT_REVISION,
            release_identity: "release-1".to_string(),
            install_generation: 1,
        }),
        envelope::Payload::RestoreWorkspace(RestoreWorkspace {}),
        envelope::Payload::WorkspaceState(workspace_state()),
        envelope::Payload::SetWorkspaceLayout(SetWorkspaceLayout {
            workspace_revision: 3,
            layout_generation: 4,
            active_workspace_id: 5,
            workspace_tabs: vec![tab],
        }),
        envelope::Payload::SetSelection(SetSelection {
            market: "ETH-USD".into(),
            interval_seconds: 300,
            workspace_revision: 3,
            selection_generation: 4,
            provider: "coinbase".into(),
        }),
        envelope::Payload::SetWatchlist(SetWatchlist {
            markets: vec!["ETH-USD".into()],
            workspace_revision: 4,
        }),
        envelope::Payload::Fault(Fault {
            code: EngineFaultCode::Retryable as i32,
            redacted_detail: "retry".into(),
        }),
        envelope::Payload::Goodbye(Goodbye {
            reason: "done".into(),
        }),
        envelope::Payload::SetViewport(SetViewport {
            start_unix_nanos: 1,
            end_unix_nanos: 2,
            selection_generation: 4,
        }),
        envelope::Payload::SetEngineLifecycle(SetEngineLifecycle {
            workspace_revision: 3,
            lifetime_mode: EngineLifetimeMode::KeepMarketsLive as i32,
            autostart_enabled: true,
            markets_live_permitted: true,
        }),
        envelope::Payload::GetEngineStatus(GetEngineStatus {}),
        envelope::Payload::EngineStatus(EngineStatus {
            process_id: 42,
            lifetime_mode: EngineLifetimeMode::KeepMarketsLive as i32,
            resource_mode: ResourceMode::MarketsLive as i32,
            connected_desktop_clients: 2,
            providers: vec![ProviderState {
                provider: "coinbase".into(),
                state: ProviderConnectionState::Online as i32,
                generation: 3,
                detail: None,
            }],
            retained_series: 4,
            retained_bars: 350,
            approximate_series_bytes: 22_400,
            shutdown_state: EngineShutdownState::Running as i32,
            autostart_enabled: true,
            markets_live_permitted: true,
        }),
    ]
}

fn workspace_tab() -> WorkspaceTabState {
    let instrument = InstallProviderInstrument {
        provider: "coinbase".into(),
        session_generation: 2,
        selection_generation: 3,
        instrument_id: "coinbase:spot:BTC-USD".into(),
        provider_symbol: "BTC-USD".into(),
        display_symbol: "BTC/USD".into(),
        venue_id: "coinbase".into(),
        price_scale: 2,
        quantity_scale: 8,
        entitlement_id: "coinbase-public-market-data".into(),
    };
    let series = SeriesKey {
        provider: "coinbase".into(),
        instrument_id: instrument.instrument_id.clone(),
        cadence_value: 60,
        definition_revision: 1,
        entitlement_id: instrument.entitlement_id.clone(),
        cadence: SeriesCadence::FixedSeconds as i32,
    };
    WorkspaceTabState {
        workspace_id: 5,
        label: "Crypto".into(),
        split_axis: WorkspaceSplitAxis::Vertical as i32,
        panes: vec![WorkspacePaneState {
            pane_id: 7,
            consumer_id: 11,
            kind: WorkspacePaneKind::Chart as i32,
            instrument: Some(instrument),
            series: Some(series),
            viewport_start_unix_nanos: Some(1),
            viewport_end_unix_nanos: Some(2),
            size_basis_points: 10_000,
            generation: 4,
        }],
        active_pane_id: 7,
        generation: 6,
        layout: Some(WorkspaceLayoutState {
            pane_id: 7,
            ..WorkspaceLayoutState::default()
        }),
    }
}

fn workspace_state() -> WorkspaceState {
    WorkspaceState {
        provider: "coinbase".into(),
        market: "BTC-USD".into(),
        interval_seconds: 60,
        watchlist: vec!["BTC-USD".into()],
        workspace_revision: 3,
        warm_mode_enabled: true,
        resource_mode: ResourceMode::Warm as i32,
        schema_revision: 1,
        cache_manifest_revision: 1,
        hot_series: vec![HotSeries {
            provider: "coinbase".into(),
            market: "BTC-USD".into(),
            interval_seconds: 60,
            score: 1,
            last_used_unix_seconds: 1,
            provider_watermark: 2,
            series_watermark: 3,
            viewport_start_unix_nanos: Some(4),
            viewport_end_unix_nanos: Some(5),
            account_id: "coinbase-public".into(),
            instrument_id: "coinbase:spot:BTC-USD".into(),
            entitlement_id: "coinbase-public-market-data".into(),
            cadence: SeriesCadence::FixedSeconds as i32,
            cadence_value: 60,
            definition_revision: 1,
            pinned: false,
            workspace_ids: vec![1],
            coverage_start_unix_nanos: Some(1),
            coverage_end_unix_nanos: Some(2),
            provider_symbol: "BTC-USD".into(),
            venue_id: "coinbase".into(),
            display_symbol: "BTC-USD".into(),
            price_scale: 2,
            quantity_scale: 8,
        }],
        lifetime_mode: EngineLifetimeMode::KeepEngineWarm as i32,
        autostart_enabled: true,
        markets_live_permitted: false,
        ..WorkspaceState::default()
    }
}

fn market_payloads() -> Vec<envelope::Payload> {
    let series = SeriesKey {
        provider: "coinbase".into(),
        instrument_id: "coinbase:spot:BTC-USD".into(),
        cadence_value: 60,
        definition_revision: 1,
        entitlement_id: "coinbase-public-market-data".into(),
        cadence: SeriesCadence::FixedSeconds as i32,
    };
    let bar = MarketBar {
        source_sequence: 1,
        exchange_timestamp_seconds: 1_700_000_000,
        exchange_timestamp_unix_nanos: 1_700_000_000_123_456_000,
        open: 100,
        high: 110,
        low: 90,
        close: 105,
        volume: 7,
    };
    let mut payloads = vec![
        envelope::Payload::AttachClient(AttachClient { client_id: 11 }),
        envelope::Payload::DetachClient(DetachClient { client_id: 11 }),
        envelope::Payload::RegisterConsumer(RegisterConsumer {
            client_id: 11,
            workspace_id: 12,
            consumer_id: 13,
        }),
        envelope::Payload::SeriesDemand(SeriesDemand {
            consumer_id: 13,
            generation: 14,
            series: Some(series.clone()),
        }),
        envelope::Payload::ViewportDemand(ViewportDemand {
            consumer_id: 13,
            generation: 14,
            start_unix_nanos: 1,
            end_unix_nanos: 2,
        }),
        envelope::Payload::VisibilityDemand(VisibilityDemand {
            consumer_id: 13,
            visible: true,
            resource_class: axiusflow_engine_protocol::ConsumerResourceClass::Foreground as i32,
        }),
        envelope::Payload::RemoveConsumer(RemoveConsumer { consumer_id: 13 }),
        envelope::Payload::SetEngineResourceMode(SetEngineResourceMode {
            resource_mode: ResourceMode::Warm as i32,
        }),
        envelope::Payload::ShutdownEngine(ShutdownEngine {}),
        envelope::Payload::SeriesState(SeriesState {
            consumer_id: 13,
            generation: 14,
            series: Some(series.clone()),
            state: SeriesLoadState::Ready as i32,
            persistence: PersistenceState::Degraded as i32,
            detail: Some("cache write unavailable".into()),
        }),
        envelope::Payload::SeriesSnapshot(SeriesSnapshot {
            consumer_id: 13,
            generation: 14,
            series: Some(series.clone()),
            provider_generation: 2,
            price_scale: 2,
            quantity_scale: 8,
            bars: vec![bar],
            publication_generation: 1,
            forming: false,
        }),
        envelope::Payload::SeriesUpdate(SeriesUpdate {
            consumer_id: 13,
            generation: 14,
            series: Some(series),
            provider_generation: 2,
            bar: Some(bar),
            forming: true,
            publication_generation: 2,
            operation: axiusflow_engine_protocol::SeriesUpdateOperation::ReviseTail as i32,
        }),
        envelope::Payload::ProviderState(ProviderState {
            provider: "coinbase".into(),
            state: ProviderConnectionState::Online as i32,
            generation: 2,
            detail: None,
        }),
        envelope::Payload::DemandError(DemandError {
            consumer_id: 13,
            generation: 14,
            code: EngineFaultCode::Retryable as i32,
            stage: "provider_history".into(),
            detail: "request timed out".into(),
            series: None,
            stage_code: axiusflow_engine_protocol::FailureStage::ProviderHistory as i32,
            cause: "provider timeout".into(),
            elapsed_millis: Some(2_000),
        }),
    ];
    payloads.push(order_book_payload());
    payloads.extend(order_flow_payloads());
    payloads.extend(catalog_payloads());
    payloads
}

fn order_flow_payloads() -> Vec<envelope::Payload> {
    let series = SeriesKey {
        provider: "rithmic".into(),
        instrument_id: "rithmic:CME:MNQU6".into(),
        cadence_value: 100,
        definition_revision: 1,
        entitlement_id: "rithmic-test:CME:MNQU6".into(),
        cadence: SeriesCadence::Trades as i32,
    };
    let level = OrderFlowLevel {
        price: 20_000,
        bid_volume: 3,
        ask_volume: 5,
        trade_count: 4,
        time_at_price_count: 4,
    };
    let trade = OrderFlowTrade {
        source_sequence: 9,
        exchange_timestamp_unix_nanos: 10,
        price: 20_000,
        quantity: 2,
        aggressor: OrderFlowAggressor::Buy as i32,
    };
    vec![
        envelope::Payload::OrderFlowSnapshot(OrderFlowSnapshot {
            consumer_id: 13,
            generation: 14,
            series: Some(series.clone()),
            provider_generation: 2,
            publication_generation: 3,
            source_watermark: 9,
            cumulative_delta: 2,
            levels: vec![level],
            tape: vec![trade],
        }),
        envelope::Payload::OrderFlowUpdate(OrderFlowUpdate {
            consumer_id: 13,
            generation: 14,
            series: Some(series),
            provider_generation: 2,
            publication_generation: 4,
            cumulative_delta: 4,
            level: Some(level),
            trade: Some(trade),
        }),
    ]
}

fn catalog_payloads() -> Vec<envelope::Payload> {
    let instrument = InstallProviderInstrument {
        provider: "rithmic".into(),
        session_generation: 2,
        selection_generation: 3,
        instrument_id: "rithmic:CME:MNQU6".into(),
        provider_symbol: "MNQU6".into(),
        venue_id: "CME".into(),
        display_symbol: "MNQ Jun 2026".into(),
        price_scale: 2,
        quantity_scale: 0,
        entitlement_id: "rithmic-test:CME:MNQU6".into(),
    };
    vec![
        envelope::Payload::SearchProviderInstruments(SearchProviderInstruments {
            consumer_id: 13,
            search_generation: 21,
            provider: "rithmic".into(),
            query: "MNQU6".into(),
            maximum_results: 16,
        }),
        envelope::Payload::SelectProviderInstrument(SelectProviderInstrument {
            consumer_id: 13,
            selection_generation: 22,
            search_generation: 21,
            provider: "rithmic".into(),
            symbol: "MNQU6".into(),
            exchange: "CME".into(),
            entitlement_id: instrument.entitlement_id.clone(),
        }),
        envelope::Payload::ProviderInstrumentSearchResult(ProviderInstrumentSearchResult {
            consumer_id: 13,
            provider: "rithmic".into(),
            provider_generation: 2,
            search_generation: 21,
            instruments: vec![ProviderInstrumentSummary {
                symbol: "MNQU6".into(),
                exchange: "CME".into(),
                name: Some("Micro E-mini Nasdaq-100".into()),
                product_code: Some("MNQ".into()),
                instrument_type: Some("Future".into()),
                expiration_date: Some("2026-06-19".into()),
            }],
        }),
        envelope::Payload::ProviderCatalogRejected(ProviderCatalogRejected {
            consumer_id: 13,
            provider: "rithmic".into(),
            provider_generation: Some(2),
            command_generation: 21,
            reason: ProviderCatalogRejectionReason::SearchRejected as i32,
        }),
        envelope::Payload::ProviderInstrumentSelection(ProviderInstrumentSelection {
            consumer_id: 13,
            instrument: Some(instrument),
        }),
    ]
}

fn order_book_payload() -> envelope::Payload {
    envelope::Payload::OrderBookSnapshot(OrderBookSnapshot {
        consumer_id: 13,
        generation: 14,
        provider: "rithmic".into(),
        instrument_id: "rithmic:CME:MNQU6".into(),
        entitlement_id: "rithmic-test:CME:MNQU6".into(),
        provider_generation: 2,
        selection_generation: 3,
        revision: 4,
        source_watermark: 5,
        state: OrderBookState::Ready as i32,
        bids: vec![OrderBookLevel {
            price: 20_000,
            quantity: 7,
            order_count: Some(3),
            traded_volume: 11,
        }],
        asks: vec![OrderBookLevel {
            price: 20_025,
            quantity: 4,
            order_count: None,
            traded_volume: 9,
        }],
    })
}

fn wrap(payload: envelope::Payload) -> Envelope {
    Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: Some(payload),
    }
}

#[test]
fn every_payload_variant_roundtrips_under_fragmentation_and_coalescing() {
    let envelopes: Vec<_> = payloads().into_iter().map(wrap).collect();
    let mut coalesced = Vec::new();
    for envelope in &envelopes {
        let framed = encode_envelope(envelope).expect("envelope encodes");
        let mut fragmented = EnvelopeDecoder::try_new().expect("decoder builds");
        let mut decoded = Vec::new();
        for byte in &framed {
            decoded.extend(fragmented.push(&[*byte]).expect("fragment decodes"));
        }
        assert_eq!(decoded, vec![envelope.clone()]);
        coalesced.extend_from_slice(&framed);
    }
    let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
    assert_eq!(decoder.push(&coalesced).expect("frames decode"), envelopes);
}

#[test]
fn every_series_cadence_roundtrips_with_entitlement_identity() {
    for (cadence, value) in [
        (SeriesCadence::FixedSeconds, 180),
        (SeriesCadence::Trades, 100),
        (SeriesCadence::SessionDays, 3),
        (SeriesCadence::CalendarWeeks, 1),
        (SeriesCadence::CalendarMonths, 1),
    ] {
        let series = SeriesKey {
            provider: "rithmic".into(),
            instrument_id: "rithmic:CME:MNQU6".into(),
            cadence_value: value,
            definition_revision: 1,
            entitlement_id: "rithmic-test:CME-Delayed:MNQU6".into(),
            cadence: cadence as i32,
        };
        let envelope = wrap(envelope::Payload::SeriesDemand(SeriesDemand {
            consumer_id: 13,
            generation: 14,
            series: Some(series.clone()),
        }));
        let encoded = encode_envelope(&envelope).expect("cadence encodes");
        let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
        assert_eq!(decoder.push(&encoded).expect("cadence decodes"), [envelope]);
    }
}

#[test]
fn oversized_frames_fail_closed() {
    let oversized = wrap(envelope::Payload::Goodbye(Goodbye {
        reason: "x".repeat(MAX_FRAME_BYTES),
    }));
    assert_eq!(
        encode_envelope(&oversized)
            .expect_err("oversized payload fails")
            .fault_code(),
        EngineFaultCode::OversizedFrame
    );
    let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
    let declared = u32::try_from(MAX_FRAME_BYTES + 1)
        .expect("bound fits")
        .to_be_bytes();
    assert_eq!(
        decoder
            .push(&declared)
            .expect_err("oversized declaration fails")
            .fault_code(),
        EngineFaultCode::OversizedFrame
    );
}

#[test]
fn worst_case_working_window_snapshot_fits_one_bounded_frame() {
    const SNAPSHOT_BARS: usize = 32_769;
    let worst_case_bar = MarketBar {
        source_sequence: u64::MAX,
        exchange_timestamp_seconds: i64::MIN,
        exchange_timestamp_unix_nanos: i64::MIN,
        open: i64::MIN,
        high: i64::MIN,
        low: i64::MIN,
        close: i64::MIN,
        volume: i64::MIN,
    };
    let snapshot = wrap(envelope::Payload::SeriesSnapshot(SeriesSnapshot {
        consumer_id: u64::MAX,
        generation: u64::MAX,
        series: Some(SeriesKey {
            provider: "coinbase".into(),
            instrument_id: "instrument:coinbase:btc:usd".into(),
            cadence_value: u32::MAX,
            definition_revision: u32::MAX,
            entitlement_id: "crypto_public_realtime".into(),
            cadence: SeriesCadence::FixedSeconds as i32,
        }),
        provider_generation: u64::MAX,
        price_scale: u32::MAX,
        quantity_scale: u32::MAX,
        bars: vec![worst_case_bar; SNAPSHOT_BARS],
        publication_generation: u64::MAX,
        forming: true,
    }));

    let encoded = encode_envelope(&snapshot).expect("maximum working snapshot fits");
    assert!(encoded.len() > 1_048_576);
    assert!(encoded.len() <= MAX_FRAME_BYTES + size_of::<u32>());
    let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
    assert_eq!(
        decoder.push(&encoded).expect("snapshot decodes"),
        [snapshot]
    );
}

#[test]
fn version_missing_payload_and_malformed_protobuf_fail_closed() {
    let wrong = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION + 1,
        target_consumer_id: 0,
        payload: Some(envelope::Payload::Goodbye(Goodbye { reason: "x".into() })),
    })
    .expect("frame encodes");
    let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
    assert!(matches!(
        decoder.push(&wrong),
        Err(ProtocolError::VersionMismatch { .. })
    ));

    let missing = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        target_consumer_id: 0,
        payload: None,
    })
    .expect("frame encodes");
    let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
    assert!(matches!(
        decoder.push(&missing),
        Err(ProtocolError::MissingPayload)
    ));

    let limit = NonZeroUsize::new(MAX_FRAME_BYTES).expect("non-zero bound");
    let malformed = encode_binary_frame(&[0xff, 0xff, 0xff, 0x07], limit).expect("frame encodes");
    let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
    assert!(matches!(
        decoder.push(&malformed),
        Err(ProtocolError::Decode(_))
    ));
}
