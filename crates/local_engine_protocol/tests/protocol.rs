use axiusflow_local_engine_protocol::{
    ActivateExistingUi, CatalogEntry, CatalogReassembler, CatalogSnapshot, ChartDelta,
    ChartProvenance, ChartSnapshot, ClientHello, ClientKind, DomBookState, DomLevel, DomRow,
    DomSnapshot, EngineFaultCode, EngineHeartbeat, EngineReady, Envelope, EnvelopeDecoder, Fault,
    Fence, Goodbye, HotSeries, MAX_FRAME_BYTES, PROTOCOL_VERSION, ProtocolError,
    ProviderConnectionState, ProviderState, ResourceMode, RestoreWorkspace, RithmicCatalog,
    RithmicChart, RithmicHistory, RithmicSearch, RithmicSelect, RithmicSymbol, SetSelection,
    SetViewport, SetWatchlist, SubscribeView, UnsubscribeView, ViewKind, WorkspaceState,
    encode_envelope, envelope, split_catalog,
};
use axiusflow_transport::encode_binary_frame;
use std::num::NonZeroUsize;

fn all_payloads() -> Vec<envelope::Payload> {
    let mut payloads = vec![
        envelope::Payload::ClientHello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            installation_token: vec![1, 2, 3, 4],
            client_kind: ClientKind::Diagnostics as i32,
        }),
        envelope::Payload::EngineReady(EngineReady {
            protocol_version: PROTOCOL_VERSION,
            engine_epoch: 7,
            workspace_revision: 42,
        }),
        envelope::Payload::EngineHeartbeat(EngineHeartbeat { engine_epoch: 7 }),
        envelope::Payload::RestoreWorkspace(RestoreWorkspace {}),
        envelope::Payload::WorkspaceState(WorkspaceState {
            provider: "coinbase".to_string(),
            market: "BTC-USD".to_string(),
            interval_seconds: 60,
            watchlist: vec!["BTC-USD".to_string(), "ETH-USD".to_string()],
            workspace_revision: 42,
            warm_mode_enabled: true,
            resource_mode: ResourceMode::Warm as i32,
            schema_revision: 1,
            cache_manifest_revision: 1,
            hot_series: vec![HotSeries {
                provider: "coinbase".to_string(),
                market: "BTC-USD".to_string(),
                interval_seconds: 60,
                score: 1,
                last_used_unix_seconds: 1,
                provider_watermark: 0,
                series_watermark: 0,
                viewport_start_unix_nanos: None,
                viewport_end_unix_nanos: None,
            }],
        }),
        envelope::Payload::SetSelection(SetSelection {
            market: "ETH-USD".to_string(),
            interval_seconds: 300,
            workspace_revision: 42,
            selection_generation: 9,
            provider: "coinbase".to_string(),
        }),
        envelope::Payload::SetWatchlist(SetWatchlist {
            markets: vec!["SOL-USD".to_string()],
            workspace_revision: 43,
        }),
        envelope::Payload::SetViewport(SetViewport {
            start_unix_nanos: 1_000,
            end_unix_nanos: 2_000,
            selection_generation: 9,
        }),
        envelope::Payload::SubscribeView(SubscribeView {
            view: ViewKind::Chart as i32,
        }),
        envelope::Payload::UnsubscribeView(UnsubscribeView {
            view: ViewKind::Dom as i32,
        }),
        envelope::Payload::CatalogSnapshot(CatalogSnapshot {
            revision: 5,
            chunk_index: 0,
            chunk_count: 1,
            entries: vec![CatalogEntry {
                product_id: "BTC-USD".to_string(),
                base_currency: "BTC".to_string(),
                quote_currency: "USD".to_string(),
                price_scale: 2,
                quantity_scale: 8,
            }],
        }),
        envelope::Payload::ChartSnapshot(ChartSnapshot {
            market: "BTC-USD".to_string(),
            interval_seconds: 60,
            engine_epoch: 7,
            selection_generation: 9,
            provider_generation: 3,
            payload: vec![9, 9, 9],
            provenance: ChartProvenance::LiveProvider as i32,
        }),
        envelope::Payload::ChartDelta(ChartDelta {
            market: "BTC-USD".to_string(),
            interval_seconds: 60,
            engine_epoch: 7,
            selection_generation: 9,
            provider_generation: 3,
            payload: vec![8, 8],
            provenance: ChartProvenance::LiveProvider as i32,
        }),
        dom_payload(),
    ];
    payloads.extend(lifecycle_payloads());
    payloads.extend(rithmic_payloads());
    payloads
}

fn lifecycle_payloads() -> Vec<envelope::Payload> {
    vec![
        envelope::Payload::ProviderState(ProviderState {
            state: ProviderConnectionState::Connected as i32,
            generation: 3,
        }),
        envelope::Payload::Fault(Fault {
            code: EngineFaultCode::Retryable as i32,
            redacted_detail: "temporary upstream timeout".to_string(),
        }),
        envelope::Payload::ActivateExistingUi(ActivateExistingUi {}),
        envelope::Payload::Goodbye(Goodbye {
            reason: "engine shutting down".to_string(),
        }),
    ]
}

fn rithmic_payloads() -> Vec<envelope::Payload> {
    vec![
        envelope::Payload::RithmicSearch(RithmicSearch {
            generation: 11,
            query: "MNQ".to_string(),
            exchange: Some("CME".to_string()),
            product_code: None,
            instrument_type: Some("FUTURE".to_string()),
            contains: true,
            maximum_results: 24,
        }),
        envelope::Payload::RithmicSelect(RithmicSelect {
            selection_generation: 12,
            search_generation: 11,
            symbol: "MNQU6".to_string(),
            exchange: "CME".to_string(),
            entitlement_id: "test".to_string(),
            trades: true,
            quotes: true,
            order_book: true,
        }),
        envelope::Payload::RithmicHistory(RithmicHistory {
            selection_generation: 12,
            series_generation: 13,
            series: "1m".to_string(),
        }),
        envelope::Payload::RithmicCatalog(RithmicCatalog {
            kind: 0,
            session_generation: Some(4),
            command_generation: 11,
            symbols: vec![RithmicSymbol {
                symbol: "MNQU6".to_string(),
                exchange: "CME".to_string(),
                name: Some("Micro E-mini Nasdaq-100".to_string()),
                product_code: Some("MNQ".to_string()),
                instrument_type: Some("FUTURE".to_string()),
                expiration_date: Some("2026-09-18".to_string()),
            }],
            instrument_id: None,
            provider_symbol: None,
            display_symbol: None,
            venue_id: None,
            price_scale: None,
            quantity_scale: None,
            entitlement_id: None,
            rejection: None,
        }),
        envelope::Payload::RithmicChart(RithmicChart {
            selection_generation: 12,
            series_generation: 13,
            live: false,
            payload: vec![7, 8, 9],
        }),
    ]
}

fn dom_payload() -> envelope::Payload {
    envelope::Payload::DomSnapshot(DomSnapshot {
        market: "BTC-USD".to_string(),
        engine_epoch: 7,
        selection_generation: 9,
        provider_generation: 3,
        payload: Vec::new(),
        provider_id: "coinbase".to_string(),
        instrument_id: "coinbase:BTC-USD".to_string(),
        entitlement_id: "public".to_string(),
        revision: 12,
        source_watermark: 42,
        state: DomBookState::Ready as i32,
        recovery_reason: None,
        rows: vec![DomRow {
            bid: Some(DomLevel {
                price: 10,
                quantity: 2,
                order_count: None,
                price_text: "10".to_string(),
                quantity_text: "2".to_string(),
                relative_size_bps: 10_000,
            }),
            ask: None,
        }],
    })
}

fn wrap(payload: envelope::Payload) -> Envelope {
    Envelope {
        protocol_version: PROTOCOL_VERSION,
        payload: Some(payload),
    }
}

#[test]
fn every_payload_variant_roundtrips() {
    for payload in all_payloads() {
        let envelope = wrap(payload.clone());
        let framed = encode_envelope(&envelope).expect("envelope encodes");
        let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
        let received = decoder.push(&framed).expect("frame decodes");
        assert_eq!(received, vec![envelope]);
    }
}

#[test]
fn byte_by_byte_fragmentation_roundtrips() {
    for payload in all_payloads() {
        let envelope = wrap(payload);
        let framed = encode_envelope(&envelope).expect("envelope encodes");
        let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
        let mut received = Vec::new();
        for byte in &framed {
            received.extend(decoder.push(&[*byte]).expect("fragment decodes"));
        }
        assert_eq!(received, vec![envelope]);
    }
}

#[test]
fn coalesced_frames_decode_in_order() {
    let envelopes: Vec<Envelope> = all_payloads().into_iter().map(wrap).collect();
    let mut coalesced = Vec::new();
    for envelope in &envelopes {
        coalesced.extend_from_slice(&encode_envelope(envelope).expect("envelope encodes"));
    }
    let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
    let received = decoder.push(&coalesced).expect("frames decode");
    assert_eq!(received, envelopes);
}

#[test]
fn oversized_encode_is_rejected_as_oversized_frame() {
    let envelope = wrap(envelope::Payload::ChartSnapshot(ChartSnapshot {
        market: "BTC-USD".to_string(),
        interval_seconds: 60,
        engine_epoch: 7,
        selection_generation: 9,
        provider_generation: 3,
        payload: vec![0_u8; MAX_FRAME_BYTES],
        provenance: ChartProvenance::LiveProvider as i32,
    }));
    let error = encode_envelope(&envelope).expect_err("oversized payload must fail");
    assert_eq!(error.fault_code(), EngineFaultCode::OversizedFrame);
}

#[test]
fn oversized_declared_length_is_rejected_by_decoder() {
    let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
    let declared = u32::try_from(MAX_FRAME_BYTES + 1)
        .expect("limit fits in u32")
        .to_be_bytes();
    let error = decoder
        .push(&declared)
        .expect_err("oversized declared length must fail");
    assert_eq!(error.fault_code(), EngineFaultCode::OversizedFrame);
}

#[test]
fn version_mismatch_is_rejected() {
    let envelope = Envelope {
        protocol_version: PROTOCOL_VERSION + 1,
        payload: Some(envelope::Payload::Goodbye(Goodbye {
            reason: "future peer".to_string(),
        })),
    };
    let framed = encode_envelope(&envelope).expect("envelope encodes");
    let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
    let error = decoder
        .push(&framed)
        .expect_err("mismatched version must fail");
    assert!(matches!(
        error,
        ProtocolError::VersionMismatch {
            expected: PROTOCOL_VERSION,
            found
        } if found == PROTOCOL_VERSION + 1
    ));
    assert_eq!(error.fault_code(), EngineFaultCode::VersionMismatch);
}

#[test]
fn missing_payload_is_rejected() {
    let framed = encode_envelope(&Envelope {
        protocol_version: PROTOCOL_VERSION,
        payload: None,
    })
    .expect("envelope encodes");
    let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
    let error = decoder
        .push(&framed)
        .expect_err("missing payload must fail");
    assert!(matches!(error, ProtocolError::MissingPayload));
    assert_eq!(error.fault_code(), EngineFaultCode::MalformedMessage);
}

#[test]
fn malformed_protobuf_is_rejected() {
    let limit = NonZeroUsize::new(MAX_FRAME_BYTES).expect("non-zero limit");
    let framed = encode_binary_frame(&[0xFF, 0xFF, 0xFF, 0x07], limit).expect("frame encodes");
    let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
    let error = decoder
        .push(&framed)
        .expect_err("malformed protobuf must fail");
    assert!(matches!(error, ProtocolError::Decode(_)));
    assert_eq!(error.fault_code(), EngineFaultCode::MalformedMessage);
}

fn catalog_entry(index: usize) -> CatalogEntry {
    CatalogEntry {
        product_id: format!("PROD-{index:06}"),
        base_currency: "BASE".to_string(),
        quote_currency: "QUOTE".to_string(),
        price_scale: 2,
        quantity_scale: 8,
    }
}

#[test]
fn split_catalog_respects_budget_and_reassembles() {
    let entries: Vec<CatalogEntry> = (0..20_000).map(catalog_entry).collect();
    let expected_len = entries.len();
    let chunks = split_catalog(entries, 11);
    assert!(chunks.len() > 1, "a large catalog must be chunked");
    assert_eq!(
        u32::try_from(chunks.len()).expect("chunk count fits"),
        chunks[0].chunk_count
    );
    for (index, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.revision, 11);
        assert_eq!(
            usize::try_from(chunk.chunk_index).expect("index fits"),
            index
        );
        assert_eq!(chunk.chunk_count, chunks[0].chunk_count);
        let framed = encode_envelope(&wrap(envelope::Payload::CatalogSnapshot(chunk.clone())))
            .expect("chunk encodes");
        assert!(
            framed.len() <= MAX_FRAME_BYTES + 4,
            "every chunk must fit in one frame"
        );
    }

    let mut reassembler = CatalogReassembler::new();
    let mut result = None;
    for chunk in chunks.into_iter().rev() {
        result = reassembler.push(chunk).expect("chunk accepted");
    }
    let assembled = result.expect("all chunks arrived");
    assert_eq!(assembled.len(), expected_len);
    assert_eq!(assembled[0].product_id, "PROD-000000");
    assert_eq!(assembled[expected_len - 1].product_id, "PROD-019999");
}

#[test]
fn reassembler_rejects_interleaved_revision() {
    let entries: Vec<CatalogEntry> = (0..20_000).map(catalog_entry).collect();
    let chunks = split_catalog(entries, 7);
    assert!(chunks.len() > 1, "transfer must span multiple chunks");
    let mut reassembler = CatalogReassembler::new();
    assert!(
        reassembler
            .push(chunks[0].clone())
            .expect("first chunk accepted")
            .is_none(),
        "transfer must remain incomplete"
    );
    let interloper = split_catalog(vec![catalog_entry(0)], 2).remove(0);
    let error = reassembler
        .push(interloper)
        .expect_err("interleaved revision must fail");
    assert!(matches!(
        error,
        ProtocolError::CatalogRevisionConflict {
            expected: 7,
            found: 2
        }
    ));
    assert_eq!(error.fault_code(), EngineFaultCode::MalformedMessage);
}

#[test]
fn reassembler_rejects_out_of_range_chunk_index() {
    let mut reassembler = CatalogReassembler::new();
    let error = reassembler
        .push(CatalogSnapshot {
            revision: 1,
            chunk_index: 2,
            chunk_count: 1,
            entries: Vec::new(),
        })
        .expect_err("out-of-range index must fail");
    assert!(matches!(
        error,
        ProtocolError::CatalogChunkOutOfRange { index: 2, count: 1 }
    ));
}

#[test]
fn split_catalog_empty_yields_single_empty_chunk() {
    let chunks = split_catalog(Vec::new(), 9);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].chunk_count, 1);
    assert!(chunks[0].entries.is_empty());
    let mut reassembler = CatalogReassembler::new();
    let result = reassembler
        .push(chunks.into_iter().next().expect("one chunk"))
        .expect("chunk accepted");
    assert_eq!(result, Some(Vec::new()));
}

#[test]
fn fence_accepts_only_exact_match() {
    let current = Fence {
        engine_epoch: 7,
        workspace_revision: 42,
        selection_generation: 9,
        provider_generation: 3,
    };
    assert!(current.accepts(current));
    for stale in [
        Fence {
            engine_epoch: 8,
            ..current
        },
        Fence {
            workspace_revision: 43,
            ..current
        },
        Fence {
            selection_generation: 10,
            ..current
        },
        Fence {
            provider_generation: 4,
            ..current
        },
    ] {
        assert!(!current.accepts(stale), "any stale dimension must reject");
    }
}
