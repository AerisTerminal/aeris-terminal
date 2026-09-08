use std::num::NonZeroUsize;

use axiusflow_engine_protocol::{
    AccountSessionState, AccountView, EngineFaultCode, Envelope, EnvelopeDecoder,
    InstallProviderInstrument, MAX_FRAME_BYTES, PROTOCOL_VERSION, ProtocolError,
    ProviderConnectionState, ProviderState, SeriesCadence, SeriesKey, WorkspaceLayoutState,
    WorkspacePaneKind, WorkspacePaneState, WorkspaceSplitAxis, WorkspaceState, WorkspaceTabState,
    encode_envelope, envelope,
};
use axiusflow_transport::encode_binary_frame;

fn payloads() -> Vec<envelope::Payload> {
    vec![envelope::Payload::WorkspaceState(workspace_state())]
}

#[test]
fn protocol_version_tracks_the_three_mebibyte_frame_contract() {
    assert_eq!(PROTOCOL_VERSION, 21);
}

#[test]
fn provider_state_transport_rtt_is_optional_and_legacy_safe() {
    use prost::Message as _;

    let measured = ProviderState {
        provider: "rithmic".into(),
        state: ProviderConnectionState::Online as i32,
        generation: 3,
        detail: None,
        transport_rtt_nanos: Some(18_400_000),
    };
    let decoded =
        ProviderState::decode(measured.encode_to_vec().as_slice()).expect("state decodes");
    assert_eq!(decoded.transport_rtt_nanos, Some(18_400_000));

    let legacy = ProviderState {
        transport_rtt_nanos: None,
        ..measured
    };
    let decoded =
        ProviderState::decode(legacy.encode_to_vec().as_slice()).expect("legacy state decodes");
    assert_eq!(decoded.transport_rtt_nanos, None);
}

#[test]
fn account_profile_fields_round_trip_and_default_empty() {
    use prost::Message as _;
    let view = AccountView {
        state: AccountSessionState::Active as i32,
        account_id: "acct_01".into(),
        plan_id: "pro".into(),
        detail: "active".into(),
        request_generation: 3,
        display_name: "Ada Trader".into(),
        email: "ada@example.com".into(),
        photo_url: "https://auth.axiusflow.com/photo/ada.png".into(),
    };
    let decoded = AccountView::decode(view.encode_to_vec().as_slice()).expect("view decodes");
    assert_eq!(decoded, view);
    // Pre-profile peers omit tags 6-8: profile defaults empty, identity
    // and plan still decode. The wire stays compatible at version 18.
    let legacy = AccountView {
        display_name: String::new(),
        email: String::new(),
        photo_url: String::new(),
        ..view.clone()
    };
    let legacy_bytes = legacy.encode_to_vec();
    assert!(legacy_bytes.len() < view.encode_to_vec().len());
    let relaid = AccountView::decode(legacy_bytes.as_slice()).expect("legacy view decodes");
    assert_eq!(relaid.display_name, "");
    assert_eq!(relaid.account_id, "acct_01");
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

fn workspace_tab() -> WorkspaceTabState {
    let instrument = InstallProviderInstrument {
        provider: "rithmic".into(),
        session_generation: 2,
        selection_generation: 3,
        instrument_id: "rithmic:spot:BTC-USD".into(),
        provider_symbol: "BTC-USD".into(),
        display_symbol: "BTC/USD".into(),
        venue_id: "rithmic".into(),
        price_scale: 2,
        quantity_scale: 8,
        entitlement_id: "rithmic-public-market-data".into(),
        price_increment: Some(25),
    };
    let series = SeriesKey {
        provider: "rithmic".into(),
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
        provider: "rithmic".into(),
        market: "BTC-USD".into(),
        interval_seconds: 60,
        watchlist: vec!["BTC-USD".into()],
        workspace_revision: 3,
        schema_revision: 1,
        ..WorkspaceState::default()
    }
}

#[test]
fn retired_resident_workspace_fields_are_ignored_when_loading_old_bytes() {
    use prost::Message as _;

    #[derive(Clone, PartialEq, prost::Message)]
    struct LegacyWorkspaceState {
        #[prost(string, tag = "1")]
        provider: String,
        #[prost(bool, tag = "6")]
        warm_mode_enabled: bool,
        #[prost(int32, tag = "7")]
        resource_mode: i32,
        #[prost(uint32, tag = "8")]
        schema_revision: u32,
        #[prost(uint32, tag = "9")]
        cache_manifest_revision: u32,
        #[prost(int32, tag = "11")]
        lifetime_mode: i32,
        #[prost(bool, tag = "12")]
        autostart_enabled: bool,
        #[prost(bool, tag = "13")]
        markets_live_permitted: bool,
    }

    let legacy = LegacyWorkspaceState {
        provider: "rithmic".to_string(),
        warm_mode_enabled: true,
        resource_mode: 4,
        schema_revision: 7,
        cache_manifest_revision: 9,
        lifetime_mode: 2,
        autostart_enabled: true,
        markets_live_permitted: true,
    };
    let decoded = WorkspaceState::decode(legacy.encode_to_vec().as_slice())
        .expect("legacy workspace fields remain forward-compatible unknown tags");
    assert_eq!(decoded.provider, "rithmic");
    assert_eq!(decoded.schema_revision, 7);
}

fn wrap(payload: envelope::Payload) -> Envelope {
    Envelope {
        protocol_version: PROTOCOL_VERSION,
        payload: Some(payload),
    }
}

#[test]
fn workspace_payload_roundtrips_under_fragmentation_and_coalescing() {
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
        let mut tab = workspace_tab();
        tab.panes[0].series = Some(series.clone());
        let envelope = wrap(envelope::Payload::WorkspaceState(WorkspaceState {
            workspace_revision: 8,
            layout_generation: 9,
            active_workspace_id: tab.workspace_id,
            workspace_tabs: vec![tab],
            ..WorkspaceState::default()
        }));
        let encoded = encode_envelope(&envelope).expect("cadence encodes");
        let mut decoder = EnvelopeDecoder::try_new().expect("decoder builds");
        assert_eq!(decoder.push(&encoded).expect("cadence decodes"), [envelope]);
    }
}

#[test]
fn oversized_frames_fail_closed() {
    let oversized = wrap(envelope::Payload::WorkspaceState(WorkspaceState {
        provider: "x".repeat(MAX_FRAME_BYTES),
        ..WorkspaceState::default()
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
        payload: Some(envelope::Payload::WorkspaceState(WorkspaceState::default())),
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
