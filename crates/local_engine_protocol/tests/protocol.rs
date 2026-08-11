use std::num::NonZeroUsize;

use axiusflow_local_engine_protocol::{
    ActivateExistingUi, AttachClient, ClientHello, ClientKind, DemandError, DetachClient,
    EngineFaultCode, EngineReady, Envelope, EnvelopeDecoder, Fault, Goodbye, HotSeries,
    MAX_FRAME_BYTES, MarketBar, MarketEventIdle, PROTOCOL_VERSION, PersistenceState,
    PollMarketEvent, ProtocolError, ProviderConnectionState, ProviderState, RegisterConsumer,
    RemoveConsumer, ResourceMode, RestoreWorkspace, SeriesCadence, SeriesDemand, SeriesKey,
    SeriesLoadState, SeriesSnapshot, SeriesState, SeriesUpdate, SetEngineResourceMode,
    SetSelection, SetViewport, SetWatchlist, ShutdownEngine, ViewportDemand, VisibilityDemand,
    WorkspaceState, encode_envelope, envelope,
};
use axiusflow_transport::encode_binary_frame;

fn payloads() -> Vec<envelope::Payload> {
    let mut payloads = workspace_payloads();
    payloads.extend(market_payloads());
    payloads
}

fn workspace_payloads() -> Vec<envelope::Payload> {
    vec![
        envelope::Payload::ClientHello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            installation_token: vec![7; 32],
            client_kind: ClientKind::Ui as i32,
        }),
        envelope::Payload::EngineReady(EngineReady {
            protocol_version: PROTOCOL_VERSION,
            engine_epoch: 7,
            workspace_revision: 3,
        }),
        envelope::Payload::RestoreWorkspace(RestoreWorkspace {}),
        envelope::Payload::WorkspaceState(WorkspaceState {
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
            }],
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
        envelope::Payload::ActivateExistingUi(ActivateExistingUi {}),
        envelope::Payload::Goodbye(Goodbye {
            reason: "done".into(),
        }),
        envelope::Payload::SetViewport(SetViewport {
            start_unix_nanos: 1,
            end_unix_nanos: 2,
            selection_generation: 4,
        }),
    ]
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
        open: 100,
        high: 110,
        low: 90,
        close: 105,
        volume: 7,
    };
    vec![
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
        }),
        envelope::Payload::RemoveConsumer(RemoveConsumer { consumer_id: 13 }),
        envelope::Payload::PollMarketEvent(PollMarketEvent { consumer_id: 13 }),
        envelope::Payload::MarketEventIdle(MarketEventIdle { consumer_id: 13 }),
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
        }),
    ]
}

fn wrap(payload: envelope::Payload) -> Envelope {
    Envelope {
        protocol_version: PROTOCOL_VERSION,
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
fn version_missing_payload_and_malformed_protobuf_fail_closed() {
    let wrong = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION + 1,
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
