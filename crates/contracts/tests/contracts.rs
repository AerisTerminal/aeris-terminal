use axiusflow_contracts::{
    AccountSessionState, AccountView, InstallProviderInstrument, ProviderConnectionState,
    ProviderState, SeriesCadence, SeriesKey, WorkspaceChartState, WorkspaceChartStudyState,
    WorkspaceLayoutState, WorkspacePaneKind, WorkspacePaneState, WorkspaceSplitAxis,
    WorkspaceState, WorkspaceStudyDependencyKind, WorkspaceStudyDependencyState,
    WorkspaceStudyMarketStream, WorkspaceStudySettingState, WorkspaceTabState,
    workspace_study_setting_state,
};
use prost::Message as _;

fn workspace_tab() -> WorkspaceTabState {
    let instrument = InstallProviderInstrument {
        provider: "rithmic".into(),
        session_generation: 2,
        selection_generation: 3,
        instrument_id: "instrument:rithmic:CME:MNQU6".into(),
        provider_symbol: "MNQU6".into(),
        display_symbol: "MNQ".into(),
        venue_id: "CME".into(),
        price_scale: 2,
        quantity_scale: 0,
        entitlement_id: "rithmic-test:CME-Delayed:MNQU6".into(),
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
        label: "Futures".into(),
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
            chart: Some(WorkspaceChartState {
                studies: vec![WorkspaceChartStudyState {
                    local_id: 1,
                    identifier: "builtin.sma".into(),
                    implementation_revision: 1,
                    settings: vec![WorkspaceStudySettingState {
                        identifier: "period".into(),
                        value: Some(workspace_study_setting_state::Value::Integer(20)),
                    }],
                    dependencies: vec![WorkspaceStudyDependencyState {
                        kind: WorkspaceStudyDependencyKind::CurrentChartSeries as i32,
                        streams: vec![WorkspaceStudyMarketStream::Bars as i32],
                        ..WorkspaceStudyDependencyState::default()
                    }],
                    visible: true,
                    output_identifiers: vec!["sma".into()],
                }],
                ..WorkspaceChartState::default()
            }),
        }],
        active_pane_id: 7,
        generation: 6,
        layout: Some(WorkspaceLayoutState {
            pane_id: 7,
            ..WorkspaceLayoutState::default()
        }),
    }
}

#[test]
fn provider_state_is_a_strongly_typed_in_process_contract() {
    let measured = ProviderState {
        provider: "rithmic".into(),
        state: ProviderConnectionState::Online,
        generation: 3,
        detail: None,
        transport_rtt_nanos: Some(18_400_000),
    };
    assert_eq!(measured.state, ProviderConnectionState::Online);
    assert_eq!(measured.transport_rtt_nanos, Some(18_400_000));
}

#[test]
fn account_view_is_a_strongly_typed_in_process_contract() {
    let view = AccountView {
        state: AccountSessionState::Active,
        account_id: "acct_01".into(),
        plan_id: "pro".into(),
        detail: "active".into(),
        request_generation: 3,
        display_name: "Ada Trader".into(),
        email: "ada@example.com".into(),
        photo_url: "https://auth.axiusflow.com/photo/ada.png".into(),
    };
    assert_eq!(view.state, AccountSessionState::Active);
    assert_eq!(view.display_name, "Ada Trader");
    assert_eq!(view.account_id, "acct_01");
}
#[test]
fn account_session_states_have_stable_values() {
    assert_eq!(AccountSessionState::SignedOut as i32, 0);
    assert_eq!(AccountSessionState::Authorizing as i32, 1);
    assert_eq!(AccountSessionState::Active as i32, 2);
    assert_eq!(AccountSessionState::OfflineLease as i32, 3);
    assert_eq!(AccountSessionState::ReauthenticationRequired as i32, 4);
    assert_eq!(AccountSessionState::LeaseExpired as i32, 5);
    assert_eq!(AccountSessionState::TerminalError as i32, 6);
}

#[test]
fn retired_process_lifecycle_workspace_fields_are_ignored_when_loading_old_bytes() {
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
        provider: "rithmic".into(),
        warm_mode_enabled: true,
        resource_mode: 4,
        schema_revision: 7,
        cache_manifest_revision: 9,
        lifetime_mode: 2,
        autostart_enabled: true,
        markets_live_permitted: true,
    };
    let decoded = WorkspaceState::decode(legacy.encode_to_vec().as_slice())
        .expect("retired tags decode as unknown fields");
    assert_eq!(decoded.provider, "rithmic");
    assert_eq!(decoded.schema_revision, 7);
}

#[test]
fn workspace_contract_round_trips_directly_as_protobuf() {
    let tab = workspace_tab();
    let workspace = WorkspaceState {
        provider: "rithmic".into(),
        market: "MNQ".into(),
        interval_seconds: 60,
        watchlist: vec!["MNQ".into(), "ES".into()],
        workspace_revision: 3,
        schema_revision: 1,
        layout_generation: 9,
        active_workspace_id: tab.workspace_id,
        workspace_tabs: vec![tab],
    };
    let decoded =
        WorkspaceState::decode(workspace.encode_to_vec().as_slice()).expect("workspace decodes");
    assert_eq!(decoded, workspace);
}

#[test]
fn every_series_cadence_round_trips_with_entitlement_identity() {
    for (cadence, value) in [
        (SeriesCadence::FixedSeconds, 180),
        (SeriesCadence::Trades, 100),
        (SeriesCadence::SessionDays, 3),
        (SeriesCadence::CalendarWeeks, 1),
        (SeriesCadence::CalendarMonths, 1),
    ] {
        let series = SeriesKey {
            provider: "rithmic".into(),
            instrument_id: "instrument:rithmic:CME:MNQU6".into(),
            cadence_value: value,
            definition_revision: 1,
            entitlement_id: "rithmic-test:CME-Delayed:MNQU6".into(),
            cadence: cadence as i32,
        };
        let decoded = SeriesKey::decode(series.encode_to_vec().as_slice()).expect("series decodes");
        assert_eq!(decoded, series);
    }
}
