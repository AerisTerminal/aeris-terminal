use std::num::NonZeroUsize;

use axiusflow_local_engine_protocol::{
    ActivateExistingUi, ClientHello, ClientKind, EngineFaultCode, EngineReady, Envelope,
    EnvelopeDecoder, Fault, Goodbye, HotSeries, MAX_FRAME_BYTES, PROTOCOL_VERSION, ProtocolError,
    ResourceMode, RestoreWorkspace, SetSelection, SetViewport, SetWatchlist, WorkspaceState,
    encode_envelope, envelope,
};
use axiusflow_transport::encode_binary_frame;

fn payloads() -> Vec<envelope::Payload> {
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
