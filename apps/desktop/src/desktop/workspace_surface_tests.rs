#![cfg(test)]

use super::*;

#[test]
fn chart_countdown_stops_only_for_the_products_closed_session() {
    use aeris_contracts::{MarketSessionPhase, MarketSessionSource, MarketSessionStatus};
    let product = InstallProviderInstrument {
        instrument_id: "tastytrade:Future:/GCZ6".into(),
        ..InstallProviderInstrument::default()
    };
    let status = |instrument_id: &str, phase| MarketSessionStatus {
        instrument_id: instrument_id.into(),
        phase,
        source: MarketSessionSource::ProviderCalendar,
        session_start_unix_nanos: None,
        session_end_unix_nanos: None,
        next_open_unix_nanos: None,
    };
    let closed = status("tastytrade:Future:/GCZ6", MarketSessionPhase::Closed);
    assert!(!chart_market_trading(Some(&closed), Some(&product)));
    for phase in [
        MarketSessionPhase::Regular,
        MarketSessionPhase::Overnight,
        MarketSessionPhase::AlwaysOpen,
        MarketSessionPhase::Unknown,
    ] {
        assert!(
            chart_market_trading(
                Some(&status("tastytrade:Future:/GCZ6", phase)),
                Some(&product)
            ),
            "{phase:?} keeps the countdown"
        );
    }
    assert!(chart_market_trading(None, Some(&product)));
    assert!(
        chart_market_trading(
            Some(&status(
                "tastytrade:Future:/ESZ6",
                MarketSessionPhase::Closed
            )),
            Some(&product)
        ),
        "another product's closed session does not stop this chart"
    );
}

#[test]
fn surface_restores_the_persisted_chart_time_zone() {
    let state = |time_zone: &str| WorkspaceChartState {
        time_zone: time_zone.to_string(),
        ..WorkspaceChartState::default()
    };
    assert_eq!(
        restored_chart_time_zone(Some(&state("America/New_York"))),
        "America/New_York"
    );
    // Legacy workspaces without a zone, and fresh panes, use the chart default.
    assert_eq!(
        restored_chart_time_zone(Some(&state(""))),
        aeris_chart_integration::DEFAULT_TIME_ZONE
    );
    assert_eq!(
        restored_chart_time_zone(None),
        aeris_chart_integration::DEFAULT_TIME_ZONE
    );
}

#[test]
fn ema_studies_default_to_one_pixel_and_explicit_widths_are_kept() {
    let study = |identifier: &str, line_width: u32| WorkspaceChartStudyState {
        identifier: identifier.to_string(),
        line_width,
        ..WorkspaceChartStudyState::default()
    };
    assert_eq!(
        persisted_study_line_width(&study(aeris_study_sdk::BUILTIN_EMA_IDENTIFIER, 0)),
        1
    );
    assert_eq!(
        persisted_study_line_width(&study(aeris_study_sdk::BUILTIN_EMA_RIBBON_IDENTIFIER, 0)),
        1
    );
    assert_eq!(
        persisted_study_line_width(&study(aeris_study_sdk::BUILTIN_SMA_IDENTIFIER, 0)),
        DEFAULT_STUDY_LINE_WIDTH
    );
    assert_eq!(
        persisted_study_line_width(&study(aeris_study_sdk::BUILTIN_EMA_IDENTIFIER, 3)),
        3,
        "a width the user chose is never replaced by the default"
    );
}

#[test]
fn legacy_chart_trading_visibility_defaults_to_visible_and_explicit_choices_restore() {
    assert_eq!(
        restored_chart_trading_visibility(None),
        ChartTradingVisibilitySettings::default()
    );
    let legacy = WorkspaceChartState::default();
    assert_eq!(
        restored_chart_trading_visibility(Some(&legacy)),
        ChartTradingVisibilitySettings::default()
    );
    let explicit = WorkspaceChartState {
        show_order_management_lines: Some(false),
        show_execution_marks: Some(true),
        extend_order_lines_left: Some(false),
        ..WorkspaceChartState::default()
    };
    assert_eq!(
        restored_chart_trading_visibility(Some(&explicit)),
        ChartTradingVisibilitySettings {
            show_order_management_lines: false,
            show_execution_marks: true,
            extend_order_lines_left: false,
        }
    );
}

#[test]
fn legacy_default_on_order_flow_studies_migrate_to_opt_in() {
    let legacy = WorkspaceOrderFlowSettingsState {
        display_mode: 0,
        show_cumulative_delta: true,
        show_delta_histogram: true,
        ticks_per_row: 0,
        study_visibility_revision: 0,
        big_trades: None,
    };
    let restored = restored_order_flow_settings(&legacy).expect("legacy settings restore");
    assert!(!restored.show_cumulative_delta);
    assert!(!restored.show_delta_histogram);

    let persisted = persisted_order_flow_settings(OrderFlowSettings {
        show_cumulative_delta: true,
        show_delta_histogram: true,
        ..OrderFlowSettings::default()
    });
    assert_eq!(persisted.study_visibility_revision, 1);
    let restored = restored_order_flow_settings(&persisted).expect("current settings restore");
    assert!(restored.show_cumulative_delta);
    assert!(restored.show_delta_histogram);
}

#[test]
fn retired_footprint_trade_bubbles_restore_without_big_trades() {
    use prost::Message as _;
    let mut legacy = WorkspaceOrderFlowSettingsState {
        display_mode: 0,
        show_cumulative_delta: false,
        show_delta_histogram: false,
        ticks_per_row: 0,
        study_visibility_revision: 1,
        big_trades: None,
    }
    .encode_to_vec();
    // Tag 4 (varint) enabled the footprint trade bubbles; tag 5 (fixed64) held their threshold.
    legacy.extend_from_slice(&[0x20, 0x01, 0x29]);
    legacy.extend_from_slice(&25.0_f64.to_bits().to_le_bytes());
    let decoded = WorkspaceOrderFlowSettingsState::decode(legacy.as_slice())
        .expect("a workspace saved with trade bubbles still decodes");
    let restored = restored_order_flow_settings(&decoded).expect("legacy settings restore");
    assert_eq!(restored.big_trades, None, "big trades is opt-in");
}

#[test]
fn big_trades_settings_round_trip_and_invalid_state_is_rejected() {
    let filters = [
        BigTradesFilter::Auto {
            intensity: BigTradesIntensity::Weak,
        },
        BigTradesFilter::Auto {
            intensity: BigTradesIntensity::Medium,
        },
        BigTradesFilter::Auto {
            intensity: BigTradesIntensity::Strong,
        },
        BigTradesFilter::Fixed {
            minimum_volume: 12.5,
        },
    ];
    for filter in filters {
        for size in [
            BigTradesSize::Small,
            BigTradesSize::Medium,
            BigTradesSize::Large,
        ] {
            let settings = OrderFlowSettings {
                big_trades: Some(BigTradesSettings {
                    filter,
                    size,
                    show_volume: false,
                    visible: false,
                }),
                ..OrderFlowSettings::default()
            };
            let persisted = persisted_order_flow_settings(settings);
            assert_eq!(restored_order_flow_settings(&persisted), Some(settings));
        }
    }
    assert_eq!(
        persisted_order_flow_settings(OrderFlowSettings::default()).big_trades,
        None
    );
    let defaults = persisted_order_flow_settings(OrderFlowSettings {
        big_trades: Some(BigTradesSettings::default()),
        ..OrderFlowSettings::default()
    });
    assert!(
        !defaults.big_trades.expect("added").hidden,
        "an indicator is visible unless the trader hid it"
    );

    let valid = WorkspaceBigTradesState {
        filter: 3,
        minimum_volume_bits: 10.0_f64.to_bits(),
        size: 1,
        show_volume: true,
        hidden: false,
    };
    for invalid in [
        WorkspaceBigTradesState {
            filter: 4,
            ..valid.clone()
        },
        WorkspaceBigTradesState {
            size: 3,
            ..valid.clone()
        },
        WorkspaceBigTradesState {
            minimum_volume_bits: 0.0_f64.to_bits(),
            ..valid.clone()
        },
        WorkspaceBigTradesState {
            minimum_volume_bits: (-5.0_f64).to_bits(),
            ..valid.clone()
        },
        WorkspaceBigTradesState {
            minimum_volume_bits: f64::NAN.to_bits(),
            ..valid.clone()
        },
    ] {
        let state = WorkspaceOrderFlowSettingsState {
            study_visibility_revision: 1,
            big_trades: Some(invalid.clone()),
            ..WorkspaceOrderFlowSettingsState::default()
        };
        assert_eq!(restored_order_flow_settings(&state), None, "{invalid:?}");
    }
}

#[test]
fn big_trades_minimum_volume_text_is_validated_and_readable() {
    assert_eq!(parse_big_trades_minimum_volume(" 50 "), Ok(50.0));
    assert_eq!(parse_big_trades_minimum_volume("12.5"), Ok(12.5));
    for invalid in ["", "abc", "0", "-3", "NaN", "inf"] {
        assert!(
            parse_big_trades_minimum_volume(invalid).is_err(),
            "{invalid:?} is rejected"
        );
    }
    assert_eq!(big_trades_volume_text(37.0), "37");
    assert_eq!(big_trades_volume_text(12.5), "12.5");
    assert_eq!(big_trades_volume_text(0.1 + 0.2), "0.3");
    assert_eq!(big_trades_volume_text(0.000_000_01), "0.00000001");
}

fn custom_package_calculate(
    context: &mut aeris_study_sdk::StudyExecutionContext<'_>,
) -> Result<(), String> {
    context
        .output(0)
        .map(|_| ())
        .ok_or_else(|| "custom-package test output is unavailable".to_string())
}

fn custom_package_restore(
    revision: u32,
    dependencies: Vec<StudyDependency>,
    settings: std::collections::BTreeMap<String, StudySettingValue>,
) -> Result<NativeStudyRegistration, aeris_study_sdk::StudySdkError> {
    if revision != 1 {
        return Err(
            aeris_study_sdk::StudySdkError::UnsupportedImplementationRevision {
                identifier: "example.workspace_reconnect".to_string(),
                revision,
            },
        );
    }
    if settings.into_iter().next().is_some() {
        return Err(aeris_study_sdk::StudySdkError::InvalidDependencyContract(
            "example.workspace_reconnect".to_string(),
        ));
    }
    let definition = aeris_study_sdk::StudyDefinition {
        identifier: "example.workspace_reconnect".to_string(),
        dependencies,
        settings: Vec::new(),
        outputs: vec![aeris_study_sdk::StudyOutputSpec {
            identifier: "value".to_string(),
            title: "Workspace Reconnect Example".to_string(),
            legend_label: None,
            plot: StudyPlotKind::Line,
            pane: StudyPaneTarget::Price,
            scale: StudyScaleTarget::Primary,
            threshold_region: None,
            point_style: StudyPointStyle::Uniform,
        }],
        invalidation: aeris_study_sdk::StudyInvalidationPolicy::SameRange,
    };
    Ok(NativeStudyRegistration {
        settings: aeris_study_sdk::StudySettings::defaults(&definition.settings)?,
        definition,
        program: aeris_study_sdk::NativeStudyProgram::stateless(custom_package_calculate),
    })
}

fn custom_package_series(instrument: &str) -> BarSeriesKey {
    BarSeriesKey {
        provider_id: "rithmic".to_string(),
        instrument_id: instrument.to_string(),
        entitlement_id: "test".to_string(),
        period: aeris_market_data::BarPeriod::time(60).expect("minute series"),
        definition_version: 1,
    }
}

#[test]
fn trusted_custom_study_restores_across_workspace_reopen_and_current_series_rebind() {
    let packages = [aeris_study_sdk::TrustedStudyPackage::new(
        "example.workspace_reconnect",
        aeris_study_sdk::STUDY_SDK_COMPATIBILITY_EPOCH,
        1,
        custom_package_restore,
    )];
    let registry = aeris_study_sdk::TrustedStudyRegistry::from_packages(&packages)
        .expect("custom package registry");
    let persisted = WorkspaceChartStudyState {
        line_width: 0,
        local_id: 7,
        identifier: "example.workspace_reconnect".to_string(),
        implementation_revision: 1,
        settings: Vec::new(),
        dependencies: vec![WorkspaceStudyDependencyState {
            kind: WorkspaceStudyDependencyKind::CurrentChartSeries as i32,
            streams: vec![WorkspaceStudyMarketStream::Bars as i32],
            ..WorkspaceStudyDependencyState::default()
        }],
        visible: true,
        output_identifiers: vec!["value".to_string()],
    };
    let workspace = WorkspaceChartState {
        studies: vec![persisted.clone()],
        ..WorkspaceChartState::default()
    };
    let restored = persisted_runtime_studies(Some(&workspace));
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].persisted, persisted);

    let first_series = custom_package_series("instrument:rithmic:CME:MNQ");
    let first = runtime_study_registration_with_registry(
        &restored[0].persisted,
        &first_series,
        &[],
        &registry,
    )
    .expect("custom study restores after workspace reopen");
    assert_eq!(
        first.definition.dependencies,
        vec![StudyDependency::Market(StudyMarketInput {
            series: first_series,
            streams: StreamRequirements::BARS,
        })]
    );

    let rebound_series = custom_package_series("instrument:rithmic:CME:MES");
    let rebound = runtime_study_registration_with_registry(
        &restored[0].persisted,
        &rebound_series,
        &[],
        &registry,
    )
    .expect("custom study rebinds after selected series/reconnect recovery");
    assert_eq!(rebound.definition.identifier, "example.workspace_reconnect");
    assert_eq!(rebound.definition.outputs[0].identifier, "value");
    assert_eq!(
        rebound.definition.dependencies,
        vec![StudyDependency::Market(StudyMarketInput {
            series: rebound_series,
            streams: StreamRequirements::BARS,
        })]
    );
}

#[test]
fn missing_custom_package_does_not_starve_later_independent_study_restore() {
    let missing = WorkspaceChartStudyState {
        line_width: 0,
        local_id: 1,
        identifier: "example.missing".to_string(),
        implementation_revision: 1,
        settings: Vec::new(),
        dependencies: Vec::new(),
        visible: true,
        output_identifiers: vec!["value".to_string()],
    };
    let builtin = legacy_runtime_study(2, ChartIndicator::Sma, true).expect("SMA study");
    let studies = RuntimeStudiesState {
        deferred: vec![
            PendingRuntimeStudyState {
                persisted: missing,
                resolved_chart_series: None,
                blocked: true,
                remove_on_registration: false,
                persist_on_registration: false,
            },
            PendingRuntimeStudyState {
                persisted: builtin,
                resolved_chart_series: None,
                blocked: false,
                remove_on_registration: false,
                persist_on_registration: false,
            },
        ],
        ..RuntimeStudiesState::default()
    };
    assert_eq!(next_deferred_runtime_study_index(&studies), Some(1));
}

#[test]
fn study_decimal_editor_round_trips_exact_fixed_point_values() {
    for value in [
        StudyDecimal {
            mantissa: 25,
            scale: 1,
        },
        StudyDecimal {
            mantissa: -125,
            scale: 2,
        },
        StudyDecimal {
            mantissa: 42,
            scale: 0,
        },
    ] {
        let text = study_decimal_text(value);
        assert_eq!(parse_study_decimal(&text).expect("decimal parses"), value);
    }
    assert!(parse_study_decimal("1.2.3").is_err());
}

#[test]
fn failed_study_setting_reinitialization_preserves_durable_configuration() {
    let study_id = StudyInstanceId::try_from_u64(1).expect("study id");
    let original = legacy_runtime_study(1, ChartIndicator::Wma, true).expect("WMA study");
    let mut replacement = original.clone();
    replacement.settings = vec![WorkspaceStudySettingState {
        identifier: aeris_study_sdk::BUILTIN_WMA_PERIOD_SETTING.to_string(),
        value: Some(workspace_study_setting_state::Value::Integer(42)),
    }];
    let series = BarSeriesKey {
        provider_id: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
        entitlement_id: "test".to_string(),
        period: aeris_market_data::BarPeriod::time(60).expect("minute series"),
        definition_version: 1,
    };
    let mut studies = RuntimeStudiesState {
        active: vec![RuntimeStudyState {
            study_id,
            persisted: original.clone(),
            resolved_chart_series: Some(series.clone()),
        }],
        ..RuntimeStudiesState::default()
    };
    studies.begin_reinitialization(
        study_id,
        PendingStudyReinitialization {
            series,
            replacement_persisted: Some(replacement),
        },
    );

    // Runtime failure retires only the pending candidate. The active
    // durable configuration is not replaced until success is acknowledged.
    studies.cancel_reinitialization(study_id);
    assert_eq!(studies.active[0].persisted, original);
    assert!(!studies.suppressing_outputs.contains(&study_id));
}

fn reinitializing_dependency_chain() -> (
    RuntimeStudiesState,
    StudyInstanceId,
    StudyInstanceId,
    BarSeriesKey,
) {
    let root_id = StudyInstanceId::try_from_u64(1).expect("root study id");
    let downstream_id = StudyInstanceId::try_from_u64(2).expect("downstream study id");
    let old_series = custom_package_series("instrument:rithmic:CME:MNQ");
    let new_series = custom_package_series("instrument:rithmic:CME:MES");
    let root = legacy_runtime_study(1, ChartIndicator::Sma, true).expect("root study");
    let mut downstream =
        legacy_runtime_study(2, ChartIndicator::Sma, true).expect("downstream study");
    downstream.dependencies.push(WorkspaceStudyDependencyState {
        kind: WorkspaceStudyDependencyKind::StudyOutput as i32,
        study_local_id: 1,
        output_identifier: aeris_study_sdk::BUILTIN_SMA_OUTPUT_IDENTIFIER.to_string(),
        ..WorkspaceStudyDependencyState::default()
    });
    let mut studies = RuntimeStudiesState {
        active: vec![
            RuntimeStudyState {
                study_id: root_id,
                persisted: root,
                resolved_chart_series: Some(old_series.clone()),
            },
            RuntimeStudyState {
                study_id: downstream_id,
                persisted: downstream,
                resolved_chart_series: Some(old_series),
            },
        ],
        ..RuntimeStudiesState::default()
    };
    studies.begin_reinitialization(
        root_id,
        PendingStudyReinitialization {
            series: new_series.clone(),
            replacement_persisted: None,
        },
    );
    studies.begin_reinitialization(
        downstream_id,
        PendingStudyReinitialization {
            series: new_series.clone(),
            replacement_persisted: None,
        },
    );
    (studies, root_id, downstream_id, new_series)
}

fn dependent_output_descriptor() -> ChartStudyOutputDescriptor<'static> {
    ChartStudyOutputDescriptor {
        title: "Dependent",
        legend_label: None,
        plot: ChartStudyPlotKind::Line,
        pane: ChartStudyPaneTarget::Price,
        scale: ChartStudyScaleTarget::Primary,
        settings_available: false,
        threshold_region: None,
        point_style: ChartStudyPointStyle::Uniform,
        input_requirements: ChartStudyInputRequirements::BARS,
    }
}

fn install_dependent_output(
    chart: &mut AerisChartView,
    study_id: StudyInstanceId,
    generation: u64,
    value: f64,
) {
    chart
        .install_study_output(
            study_id.get(),
            0,
            dependent_output_descriptor(),
            generation,
            &[60_i64 * 1_000_000_000],
            &[Some(value)],
        )
        .expect("dependent presentation installs");
}

#[test]
fn dependency_chain_reinitialization_keeps_downstream_presentation_suppressed_until_own_invalidation()
 {
    let (mut studies, root_id, downstream_id, new_series) = reinitializing_dependency_chain();
    let mut chart = AerisChartView::empty();
    install_dependent_output(&mut chart, downstream_id, 1, 1.0);
    assert_eq!(chart.study_visible(downstream_id.get()), Some(true));

    assert_eq!(studies.complete_reinitialization(root_id), Some(false));
    assert!(!studies.reinitializing.contains_key(&root_id));
    assert!(studies.reinitializing.contains_key(&downstream_id));
    assert!(studies.suppresses_output(root_id));
    assert!(studies.suppresses_output(downstream_id));

    studies.invalidate_study_outputs(&[root_id, downstream_id]);
    chart.remove_study_outputs(&[root_id.get(), downstream_id.get()]);
    assert!(!studies.suppresses_output(root_id));
    assert!(studies.suppresses_output(downstream_id));
    assert!(studies.reinitializing.contains_key(&downstream_id));
    assert_eq!(chart.study_visible(downstream_id.get()), None);

    if !studies.suppresses_output(downstream_id) {
        install_dependent_output(&mut chart, downstream_id, 2, 2.0);
    }
    assert_eq!(
        chart.study_visible(downstream_id.get()),
        None,
        "upstream subtree output must stay hidden while downstream reinit is pending"
    );

    assert_eq!(
        studies.complete_reinitialization(downstream_id),
        Some(false)
    );
    assert!(studies.reinitializing.is_empty());
    assert!(studies.suppresses_output(downstream_id));
    studies.invalidate_study_outputs(&[downstream_id]);
    assert!(!studies.suppresses_output(downstream_id));
    install_dependent_output(&mut chart, downstream_id, 3, 3.0);
    assert_eq!(chart.study_visible(downstream_id.get()), Some(true));
    assert!(
        studies
            .active
            .iter()
            .all(|state| state.resolved_chart_series.as_ref() == Some(&new_series))
    );
}

fn dependent_legacy_study(local_id: u64, upstream_local_id: u64) -> WorkspaceChartStudyState {
    let mut study =
        legacy_runtime_study(local_id, ChartIndicator::Sma, true).expect("dependent study");
    study.dependencies = vec![WorkspaceStudyDependencyState {
        kind: WorkspaceStudyDependencyKind::StudyOutput as i32,
        study_local_id: upstream_local_id,
        output_identifier: aeris_study_sdk::BUILTIN_SMA_OUTPUT_IDENTIFIER.to_string(),
        ..WorkspaceStudyDependencyState::default()
    }];
    study
}

fn pending_runtime_study(
    persisted: WorkspaceChartStudyState,
    blocked: bool,
) -> PendingRuntimeStudyState {
    PendingRuntimeStudyState {
        persisted,
        resolved_chart_series: None,
        remove_on_registration: false,
        persist_on_registration: false,
        blocked,
    }
}

fn runtime_subtree_removal_fixture() -> (RuntimeStudiesState, StudyInstanceId, StudyInstanceId) {
    let root_id = StudyInstanceId::try_from_u64(1).expect("root study id");
    let downstream_id = StudyInstanceId::try_from_u64(2).expect("downstream study id");
    let unrelated_id = StudyInstanceId::try_from_u64(3).expect("unrelated study id");
    let mut studies = RuntimeStudiesState {
        active: vec![
            RuntimeStudyState {
                study_id: root_id,
                persisted: legacy_runtime_study(1, ChartIndicator::Sma, true).expect("root study"),
                resolved_chart_series: None,
            },
            RuntimeStudyState {
                study_id: downstream_id,
                persisted: dependent_legacy_study(2, 1),
                resolved_chart_series: None,
            },
            RuntimeStudyState {
                study_id: unrelated_id,
                persisted: legacy_runtime_study(3, ChartIndicator::Wma, true)
                    .expect("unrelated study"),
                resolved_chart_series: None,
            },
        ],
        pending: HashMap::from([(
            7,
            pending_runtime_study(dependent_legacy_study(4, 2), false),
        )]),
        deferred: vec![
            pending_runtime_study(dependent_legacy_study(5, 4), true),
            pending_runtime_study(
                legacy_runtime_study(6, ChartIndicator::Wma, true)
                    .expect("unrelated deferred study"),
                false,
            ),
        ],
        ..RuntimeStudiesState::default()
    };
    studies.removing.insert(root_id);
    studies.begin_reinitialization(
        downstream_id,
        PendingStudyReinitialization {
            series: custom_package_series("instrument:rithmic:CME:MES"),
            replacement_persisted: None,
        },
    );
    (studies, root_id, downstream_id)
}

#[test]
fn manual_runtime_subtree_removal_waits_for_authoritative_ack_before_durable_cleanup() {
    let (mut studies, root_id, downstream_id) = runtime_subtree_removal_fixture();

    let persisted_before_ack = persisted_runtime_study_states(&studies, |_| None)
        .into_iter()
        .map(|state| state.local_id)
        .collect::<Vec<_>>();
    assert_eq!(
        persisted_before_ack,
        vec![1, 2, 3, 4, 5, 6],
        "queuing manual runtime removal must retain the durable root and every descendant until runtime ACK"
    );

    assert!(!study_removal_failed(&mut studies, root_id));
    assert!(!studies.removing.contains(&root_id));
    assert_eq!(
        persisted_runtime_study_states(&studies, |_| None)
            .into_iter()
            .map(|state| state.local_id)
            .collect::<Vec<_>>(),
        persisted_before_ack,
        "manual removal failure leaves the ordinary durable graph unchanged"
    );

    studies.removing.insert(root_id);
    assert_eq!(
        persisted_runtime_study_states(&studies, |_| None)
            .into_iter()
            .map(|state| state.local_id)
            .collect::<Vec<_>>(),
        persisted_before_ack,
        "retrying manual removal still cannot advance durable cleanup ahead of runtime authority"
    );

    assert!(studies.remove_runtime_subtree(&[root_id, downstream_id]));
    assert_eq!(
        studies
            .active
            .iter()
            .map(|state| state.persisted.local_id)
            .collect::<Vec<_>>(),
        vec![3]
    );
    assert!(
        studies
            .pending
            .get(&7)
            .is_some_and(|state| state.remove_on_registration)
    );
    assert_eq!(
        studies
            .deferred
            .iter()
            .map(|state| state.persisted.local_id)
            .collect::<Vec<_>>(),
        vec![6]
    );
    assert_eq!(runtime_study_count(&studies), 2);
    assert_eq!(
        persisted_runtime_study_states(&studies, |_| None)
            .into_iter()
            .map(|state| state.local_id)
            .collect::<Vec<_>>(),
        vec![3, 6],
        "persisted graph excludes the removed runtime subtree and every pending/deferred durable descendant"
    );
    assert!(!studies.removing.contains(&root_id));
    assert!(!studies.reinitializing.contains_key(&downstream_id));
    assert!(!studies.suppressing_outputs.contains(&downstream_id));
}

#[test]
fn persisted_runtime_managed_indicators_are_restored_by_the_runtime_owner_only() {
    let state = WorkspaceChartState {
        indicators: vec![
            WorkspaceChartIndicatorState {
                kind: "sma".to_string(),
                visible: false,
            },
            WorkspaceChartIndicatorState {
                kind: "ema".to_string(),
                visible: true,
            },
            WorkspaceChartIndicatorState {
                kind: "ema_ribbon".to_string(),
                visible: false,
            },
            WorkspaceChartIndicatorState {
                kind: "wma".to_string(),
                visible: true,
            },
            WorkspaceChartIndicatorState {
                kind: "bollinger".to_string(),
                visible: false,
            },
            WorkspaceChartIndicatorState {
                kind: "vwap".to_string(),
                visible: true,
            },
            WorkspaceChartIndicatorState {
                kind: "atr".to_string(),
                visible: false,
            },
            WorkspaceChartIndicatorState {
                kind: "rsi".to_string(),
                visible: true,
            },
            WorkspaceChartIndicatorState {
                kind: "macd".to_string(),
                visible: false,
            },
            WorkspaceChartIndicatorState {
                kind: "stochastic".to_string(),
                visible: true,
            },
        ],
        ..WorkspaceChartState::default()
    };
    let restored = persisted_runtime_studies(Some(&state));
    assert_eq!(restored.len(), 10);
    let expected = [
        (aeris_study_sdk::BUILTIN_SMA_IDENTIFIER, false),
        (aeris_study_sdk::BUILTIN_EMA_IDENTIFIER, true),
        (aeris_study_sdk::BUILTIN_EMA_RIBBON_IDENTIFIER, false),
        (aeris_study_sdk::BUILTIN_WMA_IDENTIFIER, true),
        (aeris_study_sdk::BUILTIN_BOLLINGER_IDENTIFIER, false),
        (aeris_study_sdk::BUILTIN_VWAP_IDENTIFIER, true),
        (aeris_study_sdk::BUILTIN_ATR_IDENTIFIER, false),
        (aeris_study_sdk::BUILTIN_RSI_IDENTIFIER, true),
        (aeris_study_sdk::BUILTIN_MACD_IDENTIFIER, false),
        (aeris_study_sdk::BUILTIN_STOCHASTIC_IDENTIFIER, true),
    ];
    for (state, (identifier, visible)) in restored.iter().zip(expected) {
        assert_eq!(state.persisted.identifier, identifier);
        assert_eq!(state.persisted.visible, visible);
    }
    assert_eq!(
        restored[2].persisted.output_identifiers,
        aeris_study_sdk::BUILTIN_EMA_RIBBON_OUTPUT_IDENTIFIERS
            .iter()
            .map(|identifier| (*identifier).to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        restored[4].persisted.output_identifiers,
        vec![
            aeris_study_sdk::BUILTIN_BOLLINGER_UPPER_OUTPUT_IDENTIFIER.to_string(),
            aeris_study_sdk::BUILTIN_BOLLINGER_MIDDLE_OUTPUT_IDENTIFIER.to_string(),
            aeris_study_sdk::BUILTIN_BOLLINGER_LOWER_OUTPUT_IDENTIFIER.to_string(),
        ]
    );
    assert!(restored.iter().all(|state| {
        runtime_study_uses_current_chart(&state.persisted)
            && state.resolved_chart_series.is_none()
            && !state.remove_on_registration
            && !state.persist_on_registration
            && !state.blocked
    }));
}

#[test]
fn durable_study_graph_merges_missing_legacy_runtime_indicators_without_duplicates() {
    let durable_wma = legacy_runtime_study(7, ChartIndicator::Wma, true).expect("WMA study");
    let durable_rsi = legacy_runtime_study(8, ChartIndicator::Rsi, true).expect("RSI study");
    let state = WorkspaceChartState {
        studies: vec![durable_wma.clone(), durable_rsi.clone()],
        indicators: vec![
            WorkspaceChartIndicatorState {
                kind: "wma".to_string(),
                visible: false,
            },
            WorkspaceChartIndicatorState {
                kind: "rsi".to_string(),
                visible: true,
            },
            WorkspaceChartIndicatorState {
                kind: "rsi".to_string(),
                visible: false,
            },
            WorkspaceChartIndicatorState {
                kind: "bollinger".to_string(),
                visible: false,
            },
        ],
        ..WorkspaceChartState::default()
    };

    let restored = persisted_runtime_studies(Some(&state));

    assert_eq!(restored.len(), 4);
    assert_eq!(restored[0].persisted, durable_wma);
    assert_eq!(restored[1].persisted, durable_rsi);
    assert_eq!(restored[2].persisted.local_id, 1);
    assert_eq!(
        restored[2].persisted.identifier,
        aeris_study_sdk::BUILTIN_RSI_IDENTIFIER
    );
    assert!(!restored[2].persisted.visible);
    assert_eq!(restored[3].persisted.local_id, 2);
    assert_eq!(
        restored[3].persisted.identifier,
        aeris_study_sdk::BUILTIN_BOLLINGER_IDENTIFIER
    );
    assert!(!restored[3].persisted.visible);
}

#[test]
fn migrated_picker_indicators_are_runtime_managed() {
    assert!(runtime_managed_indicator(ChartIndicator::Sma));
    assert!(runtime_managed_indicator(ChartIndicator::Ema));
    assert!(runtime_managed_indicator(ChartIndicator::EmaRibbon));
    assert!(runtime_managed_indicator(ChartIndicator::Wma));
    assert!(runtime_managed_indicator(ChartIndicator::Bollinger));
    assert!(runtime_managed_indicator(ChartIndicator::Vwap));
    assert!(runtime_managed_indicator(ChartIndicator::Rsi));
    assert!(runtime_managed_indicator(ChartIndicator::Macd));
    assert!(runtime_managed_indicator(ChartIndicator::Stochastic));
    assert!(runtime_managed_indicator(ChartIndicator::Atr));
    assert!(!runtime_managed_indicator(ChartIndicator::Volume));
}

#[test]
fn runtime_managed_indicators_are_not_written_to_the_legacy_indicator_field() {
    let persisted = persisted_legacy_indicator_states([
        ChartIndicatorState {
            indicator: ChartIndicator::Sma,
            visible: true,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Ema,
            visible: true,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::EmaRibbon,
            visible: true,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Wma,
            visible: false,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Bollinger,
            visible: true,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Vwap,
            visible: true,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Atr,
            visible: true,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Rsi,
            visible: false,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Macd,
            visible: true,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Stochastic,
            visible: false,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Volume,
            visible: true,
        },
    ]);

    assert_eq!(
        persisted,
        vec![WorkspaceChartIndicatorState {
            kind: "volume".to_string(),
            visible: true,
        }]
    );
}

#[test]
fn runtime_study_plan_count_excludes_removing_and_cancelled_pending_instances() {
    let first = StudyInstanceId::try_from_u64(1).expect("first study id");
    let removing = StudyInstanceId::try_from_u64(2).expect("removing study id");
    let mut studies = RuntimeStudiesState {
        active: vec![
            RuntimeStudyState {
                study_id: first,
                persisted: legacy_runtime_study(1, ChartIndicator::Wma, true).expect("WMA study"),
                resolved_chart_series: None,
            },
            RuntimeStudyState {
                study_id: removing,
                persisted: legacy_runtime_study(2, ChartIndicator::Bollinger, true)
                    .expect("Bollinger study"),
                resolved_chart_series: None,
            },
        ],
        ..RuntimeStudiesState::default()
    };
    studies.removing.insert(removing);
    studies.pending.insert(
        3,
        PendingRuntimeStudyState {
            persisted: legacy_runtime_study(3, ChartIndicator::Wma, true).expect("WMA study"),
            resolved_chart_series: None,
            remove_on_registration: false,
            persist_on_registration: true,
            blocked: false,
        },
    );
    studies.pending.insert(
        4,
        PendingRuntimeStudyState {
            persisted: legacy_runtime_study(4, ChartIndicator::Bollinger, true)
                .expect("Bollinger study"),
            resolved_chart_series: None,
            remove_on_registration: true,
            persist_on_registration: true,
            blocked: false,
        },
    );
    studies.deferred.push(PendingRuntimeStudyState {
        persisted: legacy_runtime_study(5, ChartIndicator::Bollinger, true)
            .expect("Bollinger study"),
        resolved_chart_series: None,
        remove_on_registration: false,
        persist_on_registration: false,
        blocked: false,
    });

    assert_eq!(runtime_study_count(&studies), 3);
}

#[test]
fn automatic_study_removal_retries_after_full_without_resurrecting_presentation_or_persistence() {
    let study_id = StudyInstanceId::try_from_u64(9).expect("study id");
    let mut studies = RuntimeStudiesState {
        active: vec![RuntimeStudyState {
            study_id,
            persisted: legacy_runtime_study(9, ChartIndicator::Sma, true).expect("SMA study"),
            resolved_chart_series: None,
        }],
        ..RuntimeStudiesState::default()
    };
    studies.automatic_removals.insert(study_id);

    let mut attempts = 0;
    assert!(!dispatch_automatic_study_removals(
        &mut studies,
        |received| {
            attempts += 1;
            Err(TrySendError::Full(received))
        }
    ));
    assert_eq!(attempts, 1);
    assert!(studies.automatic_removals.contains(&study_id));
    assert!(!studies.removing.contains(&study_id));
    assert!(studies.suppresses_output(study_id));
    assert_eq!(runtime_study_count(&studies), 0);
    assert_eq!(
        persisted_runtime_study_states(&studies, |_| None),
        [] as [aeris_contracts::WorkspaceChartStudyState; 0]
    );

    assert!(dispatch_automatic_study_removals(
        &mut studies,
        |received| {
            attempts += 1;
            assert_eq!(received, study_id);
            Ok(())
        }
    ));
    assert_eq!(attempts, 2);
    assert!(studies.removing.contains(&study_id));
    assert!(dispatch_automatic_study_removals(&mut studies, |_| panic!(
        "an in-flight automatic removal must not be queued twice"
    )));

    assert!(studies.remove_runtime_subtree(&[study_id]));
    assert_eq!(studies.active, [] as [RuntimeStudyState; 0]);
    assert!(!studies.automatic_removals.contains(&study_id));
    assert!(!studies.removing.contains(&study_id));
}

#[test]
fn automatic_study_removal_retries_after_runtime_failure() {
    let study_id = StudyInstanceId::try_from_u64(10).expect("study id");
    let mut studies = RuntimeStudiesState::default();
    studies.automatic_removals.insert(study_id);

    assert!(dispatch_automatic_study_removals(
        &mut studies,
        |received| {
            assert_eq!(received, study_id);
            Ok(())
        }
    ));
    assert!(studies.removing.contains(&study_id));
    assert!(study_removal_failed(&mut studies, study_id));
    assert!(!studies.removing.contains(&study_id));
    assert!(studies.automatic_removals.contains(&study_id));

    let mut retries = 0;
    assert!(dispatch_automatic_study_removals(
        &mut studies,
        |received| {
            retries += 1;
            assert_eq!(received, study_id);
            Ok(())
        }
    ));
    assert_eq!(retries, 1);
    assert!(studies.removing.contains(&study_id));
    assert!(studies.automatic_removals.contains(&study_id));
}

#[test]
fn clearing_unregistered_runtime_studies_cancels_pending_and_drops_deferred_work() {
    let mut studies = RuntimeStudiesState::default();
    studies.pending.insert(
        1,
        PendingRuntimeStudyState {
            persisted: legacy_runtime_study(1, ChartIndicator::Wma, true).expect("WMA study"),
            resolved_chart_series: None,
            remove_on_registration: false,
            persist_on_registration: true,
            blocked: false,
        },
    );
    studies.deferred.push(PendingRuntimeStudyState {
        persisted: legacy_runtime_study(2, ChartIndicator::Bollinger, true)
            .expect("Bollinger study"),
        resolved_chart_series: None,
        remove_on_registration: false,
        persist_on_registration: false,
        blocked: false,
    });

    assert!(discard_unregistered_runtime_studies(&mut studies));
    assert_eq!(studies.deferred, [] as [PendingRuntimeStudyState; 0]);
    assert!(
        studies
            .pending
            .values()
            .all(|state| state.remove_on_registration)
    );
    assert!(!discard_unregistered_runtime_studies(&mut studies));
}

#[test]
fn durable_study_output_dependency_resolves_to_the_restored_runtime_identity() {
    let series = BarSeriesKey {
        provider_id: "hyperliquid".to_string(),
        instrument_id: "hyperliquid:perp:BTC".to_string(),
        entitlement_id: "hyperliquid-public".to_string(),
        period: aeris_market_data::BarPeriod::time(60).expect("minute period"),
        definition_version: 1,
    };
    let upstream_id = StudyInstanceId::try_from_u64(7).expect("runtime study id");
    let upstream = RuntimeStudyState {
        study_id: upstream_id,
        persisted: legacy_runtime_study(1, ChartIndicator::Sma, true).expect("SMA study"),
        resolved_chart_series: Some(series.clone()),
    };
    let mut downstream = legacy_runtime_study(2, ChartIndicator::Sma, true).expect("SMA study");
    downstream.dependencies = vec![WorkspaceStudyDependencyState {
        kind: WorkspaceStudyDependencyKind::StudyOutput as i32,
        study_local_id: 1,
        output_identifier: aeris_study_sdk::BUILTIN_SMA_OUTPUT_IDENTIFIER.to_string(),
        ..WorkspaceStudyDependencyState::default()
    }];

    let registration = runtime_study_registration(&downstream, &series, &[upstream])
        .expect("durable output dependency resolves");

    assert_eq!(
        registration.definition.dependencies,
        vec![StudyDependency::Output(upstream_id.output(0))]
    );
}

fn retained_tape_trade(ordinal: u64) -> aeris_market_runtime::RetainedMarketTrade {
    let nanos = i64::try_from(ordinal).expect("small ordinal") * 1_000_000_000;
    aeris_market_runtime::RetainedMarketTrade {
        ingestion_ordinal: ordinal,
        observed_unix_nanos: nanos,
        trading_day: None,
        trade: Arc::new(aeris_market_data::MarketTrade {
            metadata: aeris_market_data::EventMetadata {
                provider_id: "provider".into(),
                instrument_id: "instrument".into(),
                entitlement_id: "entitlement".into(),
                session_generation: 1,
                source_sequence: ordinal,
                timestamps: aeris_market_data::QualifiedTimestamp {
                    exchange_unix_nanos: Some(nanos),
                    provider_unix_nanos: None,
                    received_unix_nanos: nanos,
                },
            },
            trade_id: format!("trade-{ordinal}"),
            price: 100,
            quantity: 1,
            aggressor: aeris_market_data::AggressorSide::Buy,
        }),
    }
}

#[test]
fn order_flow_projection_keeps_one_applied_trade_and_the_sweep_window() {
    let trades = (10..=20).map(retained_tape_trade).collect::<Vec<_>>();
    assert_eq!(order_flow_projection_start(&trades, None, 2), 0);
    // Ordinal 15 is index 5; keep it as continuity evidence for ordinals 16..=20.
    assert_eq!(order_flow_projection_start(&trades, Some(15), 2), 5);
    // The sweep window reaches further back than the unapplied suffix.
    assert_eq!(order_flow_projection_start(&trades, Some(20), 2), 9);
    // An applied ordinal older than the window still sends the covering window.
    assert_eq!(order_flow_projection_start(&trades, Some(3), 2), 0);
    assert_eq!(order_flow_projection_start(&[], Some(3), 2), 0);
}
