#![cfg(test)]

use super::*;

#[cfg(feature = "diagnostics")]
#[test]
fn live_snapshot_gaps_keep_the_metric_and_bound_the_evidence() {
    let (count, gaps, truncated) = snapshot_gaps([0, 60, 240, 300], 60);
    assert_eq!(count, 1);
    assert_eq!(gaps[0].start_utc_ns, 120);
    assert_eq!(gaps[0].missing_intervals, 2);
    assert!(!truncated);

    let timestamps = (0..=33).map(|index| index * 120);
    let (count, gaps, truncated) = snapshot_gaps(timestamps, 60);
    assert_eq!(count, 33);
    assert_eq!(gaps.len(), 32);
    assert_eq!(gaps[31].start_utc_ns, 31 * 120 + 60);
    assert!(truncated);
}

#[test]
fn chart_countdown_stops_only_for_the_products_closed_session() {
    use aeris_contracts::{MarketSessionPhase, MarketSessionSource, MarketSessionStatus};
    let product = InstallProviderInstrument {
        instrument_id: "rithmic:Future:GCZ6:COMEX".into(),
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
    let closed = status("rithmic:Future:GCZ6:COMEX", MarketSessionPhase::Closed);
    assert!(!chart_market_trading(Some(&closed), Some(&product)));
    for phase in [
        MarketSessionPhase::Regular,
        MarketSessionPhase::Overnight,
        MarketSessionPhase::AlwaysOpen,
        MarketSessionPhase::Unknown,
    ] {
        assert!(
            chart_market_trading(
                Some(&status("rithmic:Future:GCZ6:COMEX", phase)),
                Some(&product)
            ),
            "{phase:?} keeps the countdown"
        );
    }
    assert!(chart_market_trading(None, Some(&product)));
    assert!(
        chart_market_trading(
            Some(&status(
                "rithmic:Future:ESZ6:CME",
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
fn legacy_chart_legend_shows_everything_and_explicit_choices_round_trip() {
    assert_eq!(restored_chart_legend(None), ChartLegendOptions::default());
    assert_eq!(
        restored_chart_legend(Some(WorkspaceChartLegendState::default())),
        ChartLegendOptions::default()
    );
    let names_only = ChartLegendOptions {
        symbol_values: false,
        indicators: false,
        ..ChartLegendOptions::default()
    };
    assert_eq!(
        restored_chart_legend(Some(persisted_chart_legend(names_only))),
        names_only
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
        order_book_depth: None,
        cumulative_delta_style: 0,
        cumulative_delta: None,
        delta_histogram: None,
        order_book_depth_options: None,
        depth_heatmap: None,
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
    assert_eq!(restored.depth_heatmap, None, "the depth heatmap is opt-in");

    for visible in [true, false] {
        let heatmap = Some(DepthHeatmapSettings { visible });
        let persisted = persisted_order_flow_settings(OrderFlowSettings {
            depth_heatmap: heatmap,
            ..OrderFlowSettings::default()
        });
        assert_eq!(persisted.depth_heatmap, Some(visible));
        let restored = restored_order_flow_settings(&persisted).expect("heatmap restores");
        assert_eq!(restored.depth_heatmap, heatmap);
    }
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
        order_book_depth: None,
        cumulative_delta_style: 0,
        cumulative_delta: None,
        delta_histogram: None,
        order_book_depth_options: None,
        depth_heatmap: None,
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
fn order_flow_indicator_settings_round_trip() {
    let color = ChartColor::from_hex;
    let books = [
        None,
        Some(OrderBookDepthSettings::default()),
        Some(OrderBookDepthSettings {
            visible: false,
            levels_per_side: 200,
            max_width: 20,
            opacity_percent: 100,
            show_sizes: false,
            bid_color: color("#112233"),
            ask_color: color("#44556680"),
        }),
    ];
    let cumulative = [
        CumulativeDeltaSettings::default(),
        CumulativeDeltaSettings {
            style: CumulativeDeltaStyle::Candles,
            reset: CumulativeDeltaReset::Continuous,
            line_color: color("#ABCDEF"),
            up_color: color("#00FF0080"),
            down_color: None,
        },
    ];
    for order_book_depth in books {
        for cumulative_delta in cumulative {
            let settings = OrderFlowSettings {
                show_cumulative_delta: true,
                cumulative_delta,
                show_delta_histogram: true,
                delta_histogram: DeltaHistogramSettings {
                    up_color: color("#26A69A"),
                    down_color: None,
                },
                order_book_depth,
                ..OrderFlowSettings::default()
            };
            let persisted = persisted_order_flow_settings(settings);
            assert_eq!(restored_order_flow_settings(&persisted), Some(settings));
        }
    }
}

#[test]
fn older_order_flow_settings_restore_indicator_defaults() {
    let older = WorkspaceOrderFlowSettingsState {
        study_visibility_revision: 1,
        order_book_depth: Some(false),
        cumulative_delta_style: 1,
        ..WorkspaceOrderFlowSettingsState::default()
    };
    let restored = restored_order_flow_settings(&older).expect("older settings restore");
    assert_eq!(
        restored.cumulative_delta,
        CumulativeDeltaSettings {
            style: CumulativeDeltaStyle::Candles,
            ..CumulativeDeltaSettings::default()
        }
    );
    assert_eq!(restored.delta_histogram, DeltaHistogramSettings::default());
    assert_eq!(
        restored.order_book_depth,
        Some(OrderBookDepthSettings {
            visible: false,
            ..OrderBookDepthSettings::default()
        })
    );
    let without_book = WorkspaceOrderFlowSettingsState {
        order_book_depth: None,
        ..older
    };
    let restored = restored_order_flow_settings(&without_book).expect("older settings restore");
    assert_eq!(restored.order_book_depth, None);
}

#[test]
fn damaged_order_flow_indicator_settings_are_rejected() {
    let valid = persisted_order_flow_settings(OrderFlowSettings {
        order_book_depth: Some(OrderBookDepthSettings::default()),
        ..OrderFlowSettings::default()
    });
    assert!(restored_order_flow_settings(&valid).is_some());
    let cumulative = valid.cumulative_delta.clone().expect("persisted CVD");
    let delta = valid.delta_histogram.clone().expect("persisted delta");
    let book = valid
        .order_book_depth_options
        .clone()
        .expect("persisted book");
    let damaged_books = [
        WorkspaceOrderBookDepthState {
            levels_per_side: 0,
            ..book.clone()
        },
        WorkspaceOrderBookDepthState {
            levels_per_side: 201,
            ..book.clone()
        },
        WorkspaceOrderBookDepthState {
            max_width: 19,
            ..book.clone()
        },
        WorkspaceOrderBookDepthState {
            opacity_percent: 101,
            ..book.clone()
        },
        WorkspaceOrderBookDepthState {
            opacity_percent: 70_000,
            ..book.clone()
        },
        WorkspaceOrderBookDepthState {
            bid_color: "not a color".to_string(),
            ..book
        },
    ];
    let damaged = [
        WorkspaceOrderFlowSettingsState {
            cumulative_delta_style: 2,
            ..valid.clone()
        },
        WorkspaceOrderFlowSettingsState {
            cumulative_delta: Some(WorkspaceCumulativeDeltaState {
                reset: 2,
                ..cumulative.clone()
            }),
            ..valid.clone()
        },
        WorkspaceOrderFlowSettingsState {
            cumulative_delta: Some(WorkspaceCumulativeDeltaState {
                line_color: "not a color".to_string(),
                ..cumulative
            }),
            ..valid.clone()
        },
        WorkspaceOrderFlowSettingsState {
            delta_histogram: Some(WorkspaceDeltaHistogramState {
                down_color: "not a color".to_string(),
                ..delta
            }),
            ..valid.clone()
        },
    ]
    .into_iter()
    .chain(
        damaged_books
            .into_iter()
            .map(|book| WorkspaceOrderFlowSettingsState {
                order_book_depth_options: Some(book),
                ..valid.clone()
            }),
    );
    for state in damaged {
        assert_eq!(restored_order_flow_settings(&state), None, "{state:?}");
    }
}

#[test]
fn order_book_frame_projects_resting_levels_onto_the_price_grid() {
    use aeris_market_data::{OrderBookColumnLevel, OrderBookFrame, OrderBookRow, OrderBookState};
    let level = |price, quantity: Option<i64>, order_count| OrderBookColumnLevel {
        price,
        quantity,
        order_count,
        price_text: String::new(),
        quantity_text: String::new(),
        traded_volume: 0,
        traded_volume_text: String::new(),
        relative_size_bps: 0,
    };
    let mut frame = OrderBookFrame {
        provider_id: "rithmic".to_string(),
        instrument_id: "ESZ6".to_string(),
        entitlement_id: "test".to_string(),
        session_generation: 1,
        selection_generation: 1,
        revision: 1,
        source_watermark: 1,
        bbo_source_watermark: 1,
        state: OrderBookState::Ready,
        price_scale: 2,
        quantity_scale: 0,
        price_increment: Some(25),
        best_bid: Some(level(500_000, Some(4), None)),
        best_ask: Some(level(500_025, None, None)),
        traded_volumes: std::collections::BTreeMap::new(),
        trade_source_watermark: 0,
        top_of_book_only: false,
        rows: vec![
            OrderBookRow {
                bid: Some(level(500_000, Some(12), Some(3))),
                ask: Some(level(500_025, Some(7), Some(0))),
            },
            OrderBookRow {
                bid: Some(level(499_975, Some(0), None)),
                ask: None,
            },
        ],
        order_queues: None,
    };
    let (tick_size, bids, asks) = chart_order_book_depth(&frame).expect("ready book");
    assert!((tick_size - 0.25).abs() < f64::EPSILON);
    assert_eq!(
        bids,
        vec![ChartDepthLevel {
            price: 5_000.0,
            size: 12.0,
            order_count: Some(3),
        }],
        "an empty level is not drawn"
    );
    assert_eq!(
        asks,
        vec![ChartDepthLevel {
            price: 5_000.25,
            size: 7.0,
            order_count: None,
        }],
        "a zero order count means the provider published none"
    );

    frame.top_of_book_only = true;
    let (_, bids, asks) = chart_order_book_depth(&frame).expect("top of book");
    assert_eq!(bids.len(), 1);
    assert!(asks.is_empty(), "a best offer without a size has no bar");

    frame.state =
        OrderBookState::Recovering(aeris_market_data::OrderBookRecoveryReason::AwaitingSnapshot);
    assert_eq!(chart_order_book_depth(&frame), None);
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
        output_colors: Vec::new(),
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
        output_colors: Vec::new(),
        local_id: 1,
        identifier: "example.missing".to_string(),
        implementation_revision: 1,
        settings: Vec::new(),
        dependencies: Vec::new(),
        visible: true,
        output_identifiers: vec!["value".to_string()],
    };
    let builtin = legacy_runtime_study(2, ChartIndicator::Sma, true).expect("SMA study");
    let studies = StudiesController::restore(vec![
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
    ]);
    assert_eq!(studies.next_deferred_index(), Some(1));
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
    let mut studies = StudiesController::default();
    studies.activate(RuntimeStudyState {
        study_id,
        persisted: original.clone(),
        resolved_chart_series: Some(series.clone()),
    });
    studies.begin_reinitialization(
        study_id,
        PendingStudyReinitialization {
            series,
            replacement_settings: Some(replacement.settings),
        },
    );

    // Runtime failure retires only the pending candidate. The active
    // durable configuration is not replaced until success is acknowledged.
    let now = std::time::Instant::now();
    studies.reject_reinitialization(study_id, now);
    assert_eq!(studies.active()[0].persisted, original);
    assert!(!studies.suppresses_output(study_id));
    let series = studies.active()[0]
        .resolved_chart_series
        .clone()
        .expect("resolved series");
    assert!(
        studies.reinitialization_due(study_id, &series, now),
        "a rejected settings change never delays automatic synchronization"
    );
}

#[test]
fn rejected_automatic_study_reinitialization_waits_before_retrying_the_same_series() {
    let study_id = StudyInstanceId::try_from_u64(1).expect("study id");
    let rejected = custom_package_series("instrument:rithmic:CME:MNQ");
    let other = custom_package_series("instrument:rithmic:CME:ES");
    let mut studies = StudiesController::default();
    studies.begin_reinitialization(
        study_id,
        PendingStudyReinitialization {
            series: rejected.clone(),
            replacement_settings: None,
        },
    );
    let now = std::time::Instant::now();
    studies.reject_reinitialization(study_id, now);

    assert!(!studies.is_reinitializing(study_id));
    assert!(!studies.reinitialization_due(study_id, &rejected, now));
    assert!(studies.reinitialization_due(study_id, &other, now));
    assert!(studies.reinitialization_due(study_id, &rejected, now + STUDY_RETRY_DELAY));

    studies.begin_reinitialization(
        study_id,
        PendingStudyReinitialization {
            series: rejected.clone(),
            replacement_settings: None,
        },
    );
    assert_eq!(studies.complete_reinitialization(study_id), Some(false));
    assert!(studies.reinitialization_due(study_id, &rejected, now));
}

#[test]
fn unqueued_study_work_reports_once_and_waits_before_retrying_the_same_series() {
    let study_id = StudyInstanceId::try_from_u64(1).expect("study id");
    let failed = custom_package_series("instrument:rithmic:CME:MNQ");
    let other = custom_package_series("instrument:rithmic:CME:ES");
    let now = std::time::Instant::now();
    let later = now + STUDY_RETRY_DELAY;
    let mut studies = StudiesController::default();

    assert!(studies.registration_due(Some(&failed), now));
    assert!(studies.defer_registration(Some(failed.clone()), now));
    assert!(!studies.registration_due(Some(&failed), now));
    assert!(studies.registration_due(Some(&other), now));
    assert!(studies.registration_due(None, now));
    assert!(studies.registration_due(Some(&failed), later));
    assert!(!studies.defer_registration(Some(failed.clone()), later));
    assert!(studies.defer_registration(None, later));
    assert!(!studies.registration_due(None, later));
    assert!(!studies.discard_unregistered());
    assert!(studies.registration_due(None, later));

    assert!(studies.defer_reinitialization(study_id, failed.clone(), now));
    assert!(!studies.reinitialization_due(study_id, &failed, now));
    assert!(studies.reinitialization_due(study_id, &other, now));
    assert!(studies.reinitialization_due(study_id, &failed, later));
    assert!(!studies.defer_reinitialization(study_id, failed, later));
    assert!(studies.defer_reinitialization(study_id, other, later));
}

fn reinitializing_dependency_chain() -> (
    StudiesController,
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
    let mut studies = StudiesController::default();
    studies.activate(RuntimeStudyState {
        study_id: root_id,
        persisted: root,
        resolved_chart_series: Some(old_series.clone()),
    });
    studies.activate(RuntimeStudyState {
        study_id: downstream_id,
        persisted: downstream,
        resolved_chart_series: Some(old_series),
    });
    studies.begin_reinitialization(
        root_id,
        PendingStudyReinitialization {
            series: new_series.clone(),
            replacement_settings: None,
        },
    );
    studies.begin_reinitialization(
        downstream_id,
        PendingStudyReinitialization {
            series: new_series.clone(),
            replacement_settings: None,
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
    assert!(!studies.is_reinitializing(root_id));
    assert!(studies.is_reinitializing(downstream_id));
    assert!(studies.suppresses_output(root_id));
    assert!(studies.suppresses_output(downstream_id));

    studies.invalidate_study_outputs(&[root_id, downstream_id]);
    chart.remove_study_outputs(&[root_id.get(), downstream_id.get()]);
    assert!(!studies.suppresses_output(root_id));
    assert!(studies.suppresses_output(downstream_id));
    assert!(studies.is_reinitializing(downstream_id));
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
    assert!(!studies.has_reinitializations());
    assert!(studies.suppresses_output(downstream_id));
    studies.invalidate_study_outputs(&[downstream_id]);
    assert!(!studies.suppresses_output(downstream_id));
    install_dependent_output(&mut chart, downstream_id, 3, 3.0);
    assert_eq!(chart.study_visible(downstream_id.get()), Some(true));
    assert!(
        studies
            .active()
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

fn runtime_subtree_removal_fixture() -> (StudiesController, StudyInstanceId, StudyInstanceId) {
    let root_id = StudyInstanceId::try_from_u64(1).expect("root study id");
    let downstream_id = StudyInstanceId::try_from_u64(2).expect("downstream study id");
    let unrelated_id = StudyInstanceId::try_from_u64(3).expect("unrelated study id");
    let mut studies = StudiesController::restore(vec![
        pending_runtime_study(dependent_legacy_study(5, 4), true),
        pending_runtime_study(
            legacy_runtime_study(6, ChartIndicator::Wma, true).expect("unrelated deferred study"),
            false,
        ),
    ]);
    studies.activate(RuntimeStudyState {
        study_id: root_id,
        persisted: legacy_runtime_study(1, ChartIndicator::Sma, true).expect("root study"),
        resolved_chart_series: None,
    });
    studies.activate(RuntimeStudyState {
        study_id: downstream_id,
        persisted: dependent_legacy_study(2, 1),
        resolved_chart_series: None,
    });
    studies.activate(RuntimeStudyState {
        study_id: unrelated_id,
        persisted: legacy_runtime_study(3, ChartIndicator::Wma, true).expect("unrelated study"),
        resolved_chart_series: None,
    });
    studies.insert_pending(
        7,
        pending_runtime_study(dependent_legacy_study(4, 2), false),
    );
    studies.mark_removing(root_id);
    studies.begin_reinitialization(
        downstream_id,
        PendingStudyReinitialization {
            series: custom_package_series("instrument:rithmic:CME:MES"),
            replacement_settings: None,
        },
    );
    (studies, root_id, downstream_id)
}

#[test]
fn manual_runtime_subtree_removal_waits_for_authoritative_ack_before_durable_cleanup() {
    let (mut studies, root_id, downstream_id) = runtime_subtree_removal_fixture();

    let persisted_before_ack = studies
        .persisted_states(|_| None)
        .into_iter()
        .map(|state| state.local_id)
        .collect::<Vec<_>>();
    assert_eq!(
        persisted_before_ack,
        vec![1, 2, 3, 4, 5, 6],
        "queuing manual runtime removal must retain the durable root and every descendant until runtime ACK"
    );

    assert!(!studies.removal_failed(root_id));
    assert!(!studies.is_removing(root_id));
    assert_eq!(
        studies
            .persisted_states(|_| None)
            .into_iter()
            .map(|state| state.local_id)
            .collect::<Vec<_>>(),
        persisted_before_ack,
        "manual removal failure leaves the ordinary durable graph unchanged"
    );

    studies.mark_removing(root_id);
    assert_eq!(
        studies
            .persisted_states(|_| None)
            .into_iter()
            .map(|state| state.local_id)
            .collect::<Vec<_>>(),
        persisted_before_ack,
        "retrying manual removal still cannot advance durable cleanup ahead of runtime authority"
    );

    assert!(studies.remove_runtime_subtree(&[root_id, downstream_id]));
    assert_eq!(
        studies
            .active()
            .iter()
            .map(|state| state.persisted.local_id)
            .collect::<Vec<_>>(),
        vec![3]
    );
    assert!(
        studies
            .pending()
            .get(&7)
            .is_some_and(|state| state.remove_on_registration)
    );
    assert_eq!(
        studies
            .deferred()
            .iter()
            .map(|state| state.persisted.local_id)
            .collect::<Vec<_>>(),
        vec![6]
    );
    assert_eq!(studies.count(), 2);
    assert_eq!(
        studies
            .persisted_states(|_| None)
            .into_iter()
            .map(|state| state.local_id)
            .collect::<Vec<_>>(),
        vec![3, 6],
        "persisted graph excludes the removed runtime subtree and every pending/deferred durable descendant"
    );
    assert!(!studies.is_removing(root_id));
    assert!(!studies.is_reinitializing(downstream_id));
    assert!(!studies.suppresses_output(downstream_id));
}

#[test]
fn persisted_runtime_managed_indicators_are_restored_by_the_runtime_owner_only() {
    let state = WorkspaceChartState {
        indicators: vec![
            WorkspaceChartIndicatorState {
                kind: "sma".to_string(),
                visible: false,
                volume: None,
            },
            WorkspaceChartIndicatorState {
                kind: "ema".to_string(),
                visible: true,
                volume: None,
            },
            WorkspaceChartIndicatorState {
                kind: "wma".to_string(),
                visible: true,
                volume: None,
            },
            WorkspaceChartIndicatorState {
                kind: "bollinger".to_string(),
                visible: false,
                volume: None,
            },
            WorkspaceChartIndicatorState {
                kind: "vwap".to_string(),
                visible: true,
                volume: None,
            },
            WorkspaceChartIndicatorState {
                kind: "atr".to_string(),
                visible: false,
                volume: None,
            },
            WorkspaceChartIndicatorState {
                kind: "rsi".to_string(),
                visible: true,
                volume: None,
            },
            WorkspaceChartIndicatorState {
                kind: "macd".to_string(),
                visible: false,
                volume: None,
            },
            WorkspaceChartIndicatorState {
                kind: "stochastic".to_string(),
                visible: true,
                volume: None,
            },
        ],
        ..WorkspaceChartState::default()
    };
    let restored = persisted_runtime_studies(Some(&state));
    assert_eq!(restored.len(), 9);
    let expected = [
        (aeris_study_sdk::BUILTIN_SMA_IDENTIFIER, false),
        (aeris_study_sdk::BUILTIN_EMA_IDENTIFIER, true),
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
        restored[3].persisted.output_identifiers,
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
fn retired_legacy_indicator_kinds_are_dropped_without_affecting_other_indicators() {
    let state = WorkspaceChartState {
        indicators: [
            "ema_ribbon",
            "sma",
            "market_structure",
            "fair_value_gaps",
            "order_blocks",
        ]
        .into_iter()
        .map(|kind| WorkspaceChartIndicatorState {
            kind: kind.to_string(),
            visible: true,
            volume: None,
        })
        .collect(),
        ..WorkspaceChartState::default()
    };

    let restored = persisted_runtime_studies(Some(&state));

    assert_eq!(restored.len(), 1);
    assert_eq!(
        restored[0].persisted.identifier,
        aeris_study_sdk::BUILTIN_SMA_IDENTIFIER
    );
    assert!(!restored[0].blocked);
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
                volume: None,
            },
            WorkspaceChartIndicatorState {
                kind: "rsi".to_string(),
                visible: true,
                volume: None,
            },
            WorkspaceChartIndicatorState {
                kind: "rsi".to_string(),
                visible: false,
                volume: None,
            },
            WorkspaceChartIndicatorState {
                kind: "bollinger".to_string(),
                visible: false,
                volume: None,
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
            volume: None,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Ema,
            visible: true,
            volume: None,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Wma,
            visible: false,
            volume: None,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Bollinger,
            visible: true,
            volume: None,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Vwap,
            visible: true,
            volume: None,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Atr,
            visible: true,
            volume: None,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Rsi,
            visible: false,
            volume: None,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Macd,
            visible: true,
            volume: None,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Stochastic,
            visible: false,
            volume: None,
        },
        ChartIndicatorState {
            indicator: ChartIndicator::Volume,
            visible: true,
            volume: None,
        },
    ]);

    assert_eq!(
        persisted,
        vec![WorkspaceChartIndicatorState {
            kind: "volume".to_string(),
            visible: true,
            volume: None,
        }]
    );
}

#[test]
fn volume_settings_round_trip_through_the_layout_and_reject_damaged_values() {
    let settings = VolumeSettings {
        up_color: Some("#26A69A80".to_string()),
        down_color: None,
        average: VolumeAverageSettings {
            enabled: true,
            kind: VolumeAverageKind::Exponential,
            length: 50,
            color: Some("#FF9800".to_string()),
        },
        placement: VolumePlacement::OwnPane,
    };
    let persisted = persisted_volume_settings(&settings);
    assert_eq!(
        persisted.down_color, "",
        "a default color stays theme-neutral"
    );
    assert_eq!(restored_volume_settings(&persisted), Some(settings));
    assert_eq!(
        restored_volume_settings(&persisted_volume_settings(&VolumeSettings::default())),
        Some(VolumeSettings::default())
    );

    for damaged in [
        WorkspaceVolumeSettingsState {
            average_length: 0,
            ..persisted.clone()
        },
        WorkspaceVolumeSettingsState {
            average_length: 501,
            ..persisted.clone()
        },
        WorkspaceVolumeSettingsState {
            average_kind: 2,
            ..persisted.clone()
        },
        WorkspaceVolumeSettingsState {
            placement: 2,
            ..persisted
        },
    ] {
        assert_eq!(restored_volume_settings(&damaged), None);
    }
}

#[test]
fn runtime_study_plan_count_excludes_removing_and_cancelled_pending_instances() {
    let first = StudyInstanceId::try_from_u64(1).expect("first study id");
    let removing = StudyInstanceId::try_from_u64(2).expect("removing study id");
    let mut studies = StudiesController::default();
    studies.activate(RuntimeStudyState {
        study_id: first,
        persisted: legacy_runtime_study(1, ChartIndicator::Wma, true).expect("WMA study"),
        resolved_chart_series: None,
    });
    studies.activate(RuntimeStudyState {
        study_id: removing,
        persisted: legacy_runtime_study(2, ChartIndicator::Bollinger, true)
            .expect("Bollinger study"),
        resolved_chart_series: None,
    });
    studies.mark_removing(removing);
    studies.insert_pending(
        3,
        PendingRuntimeStudyState {
            persisted: legacy_runtime_study(3, ChartIndicator::Wma, true).expect("WMA study"),
            resolved_chart_series: None,
            remove_on_registration: false,
            persist_on_registration: true,
            blocked: false,
        },
    );
    studies.insert_pending(
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
    studies.push_deferred(PendingRuntimeStudyState {
        persisted: legacy_runtime_study(5, ChartIndicator::Bollinger, true)
            .expect("Bollinger study"),
        resolved_chart_series: None,
        remove_on_registration: false,
        persist_on_registration: false,
        blocked: false,
    });

    assert_eq!(studies.count(), 3);
}

#[test]
fn automatic_study_removal_retries_after_full_without_resurrecting_presentation_or_persistence() {
    let study_id = StudyInstanceId::try_from_u64(9).expect("study id");
    let mut studies = StudiesController::default();
    studies.activate_for_removal(RuntimeStudyState {
        study_id,
        persisted: legacy_runtime_study(9, ChartIndicator::Sma, true).expect("SMA study"),
        resolved_chart_series: None,
    });

    let now = Instant::now();
    let mut attempts = 0;
    assert!(!studies.dispatch_automatic_removals(now, |received| {
        attempts += 1;
        Err(TrySendError::Full(received))
    }));
    assert_eq!(attempts, 1);
    assert!(studies.is_automatic_removal(study_id));
    assert!(!studies.is_removing(study_id));
    assert!(studies.suppresses_output(study_id));
    assert_eq!(studies.count(), 0);
    assert_eq!(
        studies.persisted_states(|_| None),
        [] as [aeris_contracts::WorkspaceChartStudyState; 0]
    );

    assert!(studies.dispatch_automatic_removals(now, |received| {
        attempts += 1;
        assert_eq!(received, study_id);
        Ok(())
    }));
    assert_eq!(attempts, 2);
    assert!(studies.is_removing(study_id));
    assert!(studies.dispatch_automatic_removals(now, |_| panic!(
        "an in-flight automatic removal must not be queued twice"
    )));

    assert!(studies.remove_runtime_subtree(&[study_id]));
    assert_eq!(studies.active(), [] as [RuntimeStudyState; 0]);
    assert!(!studies.is_automatic_removal(study_id));
    assert!(!studies.is_removing(study_id));
}

#[test]
fn automatic_study_removal_retries_after_runtime_failure() {
    let study_id = StudyInstanceId::try_from_u64(10).expect("study id");
    let mut studies = StudiesController::default();
    studies.activate_for_removal(RuntimeStudyState {
        study_id,
        persisted: legacy_runtime_study(10, ChartIndicator::Sma, true).expect("SMA study"),
        resolved_chart_series: None,
    });

    let now = Instant::now();
    assert!(studies.dispatch_automatic_removals(now, |received| {
        assert_eq!(received, study_id);
        Ok(())
    }));
    assert!(studies.is_removing(study_id));
    assert!(studies.removal_failed(study_id));
    assert!(!studies.is_removing(study_id));
    assert!(studies.is_automatic_removal(study_id));

    let mut retries = 0;
    assert!(studies.dispatch_automatic_removals(now, |received| {
        retries += 1;
        assert_eq!(received, study_id);
        Ok(())
    }));
    assert_eq!(retries, 1);
    assert!(studies.is_removing(study_id));
    assert!(studies.is_automatic_removal(study_id));
}

#[test]
fn clearing_unregistered_runtime_studies_cancels_pending_and_drops_deferred_work() {
    let mut studies = StudiesController::default();
    studies.insert_pending(
        1,
        PendingRuntimeStudyState {
            persisted: legacy_runtime_study(1, ChartIndicator::Wma, true).expect("WMA study"),
            resolved_chart_series: None,
            remove_on_registration: false,
            persist_on_registration: true,
            blocked: false,
        },
    );
    studies.push_deferred(PendingRuntimeStudyState {
        persisted: legacy_runtime_study(2, ChartIndicator::Bollinger, true)
            .expect("Bollinger study"),
        resolved_chart_series: None,
        remove_on_registration: false,
        persist_on_registration: false,
        blocked: false,
    });

    assert!(studies.discard_unregistered());
    assert_eq!(studies.deferred(), [] as [PendingRuntimeStudyState; 0]);
    assert!(
        studies
            .pending()
            .values()
            .all(|state| state.remove_on_registration)
    );
    assert!(!studies.discard_unregistered());
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

mod instrument_drawings {
    use super::*;
    use aeris_application::{EmbeddedReplaySource, LoadEmbeddedReplay};
    use aeris_desktop::market_worker::{
        EngineWorkerStartup, MarketWorkerCommand, market_projection_channel,
    };
    use gpui::{TestAppContext, VisualTestContext};
    use std::num::NonZeroUsize;
    use std::sync::{atomic::AtomicU64, mpsc};

    const LINE: &str = r#"{"schema":"aeris_charts-drawings","schema_version":1,"drawings":[{"id":1,"kind":"horizontal_line","pane_index":0,"points":[{"logical":10.0,"price":100.0}]}]}"#;
    const LOCKED_LINE: &str = r#"{"schema":"aeris_charts-drawings","schema_version":1,"drawings":[{"id":1,"kind":"horizontal_line","pane_index":0,"locked":true,"points":[{"logical":10.0,"price":100.0}]}]}"#;
    /// The bare drawing array workspaces saved before Aeris Charts owned the drawings document.
    const LEGACY_LINE: &str = r#"[{"id":1,"kind":"horizontal_line","pane_index":0,"points":[{"logical":10.0,"price":100.0}]}]"#;

    struct SurfaceHarness(Entity<WorkspaceSurface>);

    impl Render for SurfaceHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn instrument(instrument_id: &str) -> InstallProviderInstrument {
        let mut instrument = local_state::fixture_workspace().watchlist_entries[0]
            .instrument
            .clone()
            .expect("default instrument");
        instrument.instrument_id = instrument_id.to_string();
        instrument
    }

    fn open_surface(
        cx: &mut TestAppContext,
        product: InstallProviderInstrument,
        restored: Option<WorkspaceChartState>,
    ) -> (
        Entity<WorkspaceSurface>,
        mpsc::Receiver<MarketWorkerCommand>,
        &mut VisualTestContext,
    ) {
        let (commands, requests) = mpsc::sync_channel(8);
        let (surface, cx) = open_surface_with_commands(cx, product, restored, commands);
        (surface, requests, cx)
    }

    fn open_surface_with_commands(
        cx: &mut TestAppContext,
        product: InstallProviderInstrument,
        restored: Option<WorkspaceChartState>,
        commands: mpsc::SyncSender<MarketWorkerCommand>,
    ) -> (Entity<WorkspaceSurface>, &mut VisualTestContext) {
        let (_publications, messages) = market_projection_channel(NonZeroUsize::MIN);
        let (_, shutdown) = mpsc::sync_channel(1);
        let worker = MarketDataWorker::from_channels(
            commands,
            messages,
            shutdown,
            None,
            Some(Arc::new(AtomicU64::new(1))),
        );
        let (view, cx) = cx.add_window_view(move |window, cx| {
            gpui_base::init(cx);
            SurfaceHarness(workspace_surface_entity(
                MarketWorkerStartup::Loading(Box::new(EngineWorkerStartup {
                    product,
                    interval: ChartInterval::Minute1,
                    restored_viewport: None,
                    subscription_id: "drawings_test".into(),
                    worker_label: "drawings_test".into(),
                })),
                worker,
                &DesktopLifecycle::new_for_test(),
                chart_chrome::ChartChromePreferences::default(),
                WorkspaceSurfaceRestore {
                    chart: restored,
                    side_panel: None,
                },
                window,
                cx,
            ))
        });
        let surface = cx.read(|cx| view.read(cx).0.clone());
        (surface, cx)
    }

    /// Installs the covering snapshot of `product`, as a completed symbol switch does.
    fn show(surface: &Entity<WorkspaceSurface>, product: &str, cx: &mut VisualTestContext) {
        let snapshot = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count: 64 })
            .expect("embedded replay validates");
        surface.update(cx, |surface, cx| {
            surface.product = Some(instrument(product));
            surface.install_snapshot_chart(&snapshot, cx);
        });
    }

    fn drawing_count(surface: &Entity<WorkspaceSurface>, cx: &VisualTestContext) -> usize {
        cx.read(|cx| {
            surface
                .read(cx)
                .chart
                .as_ref()
                .map_or(0, |chart| chart.read(cx).drawing_count())
        })
    }

    fn saved(surface: &Entity<WorkspaceSurface>, cx: &VisualTestContext) -> WorkspaceChartState {
        cx.read(|cx| surface.read(cx).workspace_chart_state(cx))
            .expect("loaded chart state")
    }

    #[gpui::test]
    fn pane_settings_saved_during_chart_loading_match_the_loaded_chart(cx: &mut TestAppContext) {
        use prost::Message as _;
        let original = WorkspaceChartState {
            chart_type: "bars".into(),
            time_zone: "Etc/UTC".into(),
            context_panel_tab: 1,
            chart_link_group: 1,
            show_order_management_lines: Some(true),
            show_execution_marks: Some(true),
            extend_order_lines_left: Some(true),
            ..WorkspaceChartState::default()
        };
        let (surface, _requests, cx) = open_surface(cx, instrument("A"), Some(original));
        surface.update(cx, |surface, _| {
            surface.chart_time_zone = "America/New_York".into();
            surface.chart_trading_visibility = ChartTradingVisibilitySettings {
                show_order_management_lines: false,
                show_execution_marks: false,
                extend_order_lines_left: false,
            };
            surface.context_panel.set_visible(true);
            surface.context_panel.set_tab(ContextPanelTab::Energy);
            surface.context_panel.set_height(320.0);
            surface.chart_link_group = 2;
            surface.chart_link_flags = 1;
        });
        let pending = cx
            .read(|cx| surface.read(cx).workspace_chart_state(cx))
            .expect("presentation saves before the snapshot");
        assert_eq!(pending.chart_type, "bars");
        assert_eq!(pending.time_zone, "America/New_York");
        assert_eq!(pending.show_order_management_lines, Some(false));
        assert_eq!(pending.show_execution_marks, Some(false));
        assert_eq!(pending.extend_order_lines_left, Some(false));
        assert!(pending.context_panel_visible);
        assert_eq!(
            pending.context_panel_tab,
            ContextPanelTab::Energy.persisted()
        );
        assert_eq!(pending.context_panel_height, 320);
        assert_eq!(pending.chart_link_group, 2);
        assert_eq!(pending.chart_link_flags, 1);

        let decoded = WorkspaceChartState::decode(pending.encode_to_vec().as_slice())
            .expect("pending presentation survives protobuf");
        let (reopened, _requests, cx) = open_surface(cx, instrument("A"), Some(decoded));
        show(&reopened, "A", cx);
        let reopened_state = saved(&reopened, cx);
        assert_eq!(reopened_state.time_zone, pending.time_zone);
        assert_eq!(
            reopened_state.show_order_management_lines,
            pending.show_order_management_lines
        );
        assert_eq!(
            reopened_state.show_execution_marks,
            pending.show_execution_marks
        );
        assert_eq!(
            reopened_state.extend_order_lines_left,
            pending.extend_order_lines_left
        );
        assert_eq!(
            reopened_state.context_panel_visible,
            pending.context_panel_visible
        );
        assert_eq!(reopened_state.context_panel_tab, pending.context_panel_tab);
        assert_eq!(
            reopened_state.context_panel_height,
            pending.context_panel_height
        );
        assert_eq!(reopened_state.chart_link_group, pending.chart_link_group);
        assert_eq!(reopened_state.chart_link_flags, pending.chart_link_flags);

        show(&surface, "A", cx);
        let loaded = saved(&surface, cx);
        assert_eq!(loaded.time_zone, pending.time_zone);
        assert_eq!(
            loaded.show_order_management_lines,
            pending.show_order_management_lines
        );
        assert_eq!(loaded.show_execution_marks, pending.show_execution_marks);
        assert_eq!(
            loaded.extend_order_lines_left,
            pending.extend_order_lines_left
        );
        assert_eq!(loaded.context_panel_visible, pending.context_panel_visible);
        assert_eq!(loaded.context_panel_tab, pending.context_panel_tab);
        assert_eq!(loaded.context_panel_height, pending.context_panel_height);
        assert_eq!(loaded.chart_link_group, pending.chart_link_group);
        assert_eq!(loaded.chart_link_flags, pending.chart_link_flags);
    }

    #[gpui::test]
    fn pane_presentation_round_trips_all_chart_preferences(cx: &mut TestAppContext) {
        use prost::Message as _;

        let (source, _requests, cx) = open_surface(cx, instrument("A"), None);
        show(&source, "A", cx);
        let mut original = saved(&source, cx);
        original.chart_type = ChartType::HollowCandles.identifier().into();
        original.indicators = vec![WorkspaceChartIndicatorState {
            kind: ChartIndicator::Volume.identifier().into(),
            visible: false,
            volume: Some(persisted_volume_settings(&VolumeSettings::default())),
        }];
        original.price_axis = Some(WorkspacePriceAxisState {
            mode: 1,
            ..WorkspacePriceAxisState::default()
        });
        original.crosshair_mode = 2;
        let appearance = original.appearance.as_mut().expect("saved appearance");
        appearance.grid_visible = !appearance.grid_visible;
        std::mem::swap(&mut appearance.up_color, &mut appearance.down_color);
        original.order_flow = Some(persisted_order_flow_settings(OrderFlowSettings {
            show_cumulative_delta: true,
            ..OrderFlowSettings::default()
        }));
        original.studies =
            vec![legacy_runtime_study(1, ChartIndicator::Sma, false).expect("hidden saved study")];
        original.legend = Some(persisted_chart_legend(ChartLegendOptions {
            symbol_values: false,
            indicators: false,
            ..ChartLegendOptions::default()
        }));
        original.time_zone = "America/New_York".into();
        original.show_order_management_lines = Some(false);
        original.show_execution_marks = Some(false);
        original.extend_order_lines_left = Some(false);
        original.context_panel_visible = true;
        original.context_panel_tab = ContextPanelTab::Energy.persisted();
        original.context_panel_height = 320;
        original.chart_link_group = 2;
        original.chart_link_flags = 1;

        let (surface, _requests, cx) = open_surface(cx, instrument("A"), Some(original.clone()));
        let pending = cx
            .read(|cx| surface.read(cx).workspace_chart_state(cx))
            .expect("presentation before bars");
        assert_eq!(
            pending, original,
            "the pending save retains every preference"
        );
        let decoded = WorkspaceChartState::decode(pending.encode_to_vec().as_slice())
            .expect("pane presentation decodes");
        assert_eq!(decoded, original);
        show(&surface, "A", cx);
        let loaded = saved(&surface, cx);
        assert_eq!(loaded.chart_type, original.chart_type);
        assert_eq!(loaded.indicators, original.indicators);
        assert_eq!(loaded.price_axis.as_ref().map(|axis| axis.mode), Some(1));
        assert_eq!(loaded.crosshair_mode, original.crosshair_mode);
        assert_eq!(loaded.appearance, original.appearance);
        assert_eq!(loaded.order_flow, original.order_flow);
        assert_eq!(loaded.studies, original.studies);
        assert_eq!(loaded.legend, original.legend);
        assert_eq!(loaded.time_zone, original.time_zone);
        assert_eq!(loaded.show_order_management_lines, Some(false));
        assert_eq!(loaded.show_execution_marks, Some(false));
        assert_eq!(loaded.extend_order_lines_left, Some(false));
        assert!(loaded.context_panel_visible);
        assert_eq!(loaded.context_panel_tab, original.context_panel_tab);
        assert_eq!(loaded.context_panel_height, original.context_panel_height);
        assert_eq!(loaded.chart_link_group, original.chart_link_group);
        assert_eq!(loaded.chart_link_flags, original.chart_link_flags);
    }

    #[gpui::test]
    fn drawings_return_when_switching_back_and_after_a_restart(cx: &mut TestAppContext) {
        let mut layout = LayoutDrawings::default();
        let (surface, _requests, cx) = open_surface(cx, instrument("A"), None);
        let panes = [surface.clone()];
        show(&surface, "A", cx);
        share(&panes, &mut layout, cx);
        draw(&surface, LOCKED_LINE, cx);
        assert_eq!(drawing_count(&surface, cx), 1);

        // The switch replaces the chart before a sync publishes the edit.
        show(&surface, "B", cx);
        assert!(
            share(&panes, &mut layout, cx),
            "the replaced chart's edit is kept"
        );
        assert_eq!(
            drawing_count(&surface, cx),
            0,
            "drawings stay on their market"
        );
        assert_eq!(layout_instruments(&layout), ["A"]);
        assert!(layout.has_unsaved_changes());
        let on_b = saved(&surface, cx);
        assert!(
            on_b.instrument_drawings.is_empty(),
            "the layout owns drawings"
        );
        assert_eq!(on_b.chart_state_json, "");

        show(&surface, "A", cx);
        share(&panes, &mut layout, cx);
        assert_eq!(drawing_count(&surface, cx), 1);
        assert_eq!(layout.entries()[0].locked_drawing_ids, Vec::<u32>::new());
        assert_eq!(
            locked_count(&surface, cx),
            1,
            "the lock travels in the document"
        );
        layout.mark_saved();
        assert!(
            !share(&panes, &mut layout, cx),
            "showing drawings changes nothing"
        );
        assert!(!layout.has_unsaved_changes());

        let mut restarted = LayoutDrawings::from_saved(&layout.entries());
        let (reopened, _reopened_requests, cx) = open_surface(cx, instrument("B"), Some(on_b));
        let panes = [reopened.clone()];
        show(&reopened, "B", cx);
        share(&panes, &mut restarted, cx);
        assert_eq!(drawing_count(&reopened, cx), 0);
        show(&reopened, "A", cx);
        share(&panes, &mut restarted, cx);
        assert_eq!(drawing_count(&reopened, cx), 1);
        assert_eq!(locked_count(&reopened, cx), 1);
        assert!(!restarted.has_unsaved_changes());
    }

    fn locked_count(surface: &Entity<WorkspaceSurface>, cx: &VisualTestContext) -> usize {
        cx.read(|cx| {
            surface.read(cx).chart.as_ref().map_or(0, |chart| {
                chart.read(cx).drawings_lock_summary().locked_count
            })
        })
    }

    #[gpui::test]
    fn legacy_drawings_restore_onto_their_instrument_only(cx: &mut TestAppContext) {
        let legacy = crate::desktop::chart_drawings::legacy_pane_drawings(
            LEGACY_LINE,
            &[1],
            Some(&instrument("A")),
        )
        .expect("legacy drawings upgrade");
        let mut layout = LayoutDrawings::from_saved(&[legacy]);
        let (surface, _requests, cx) = open_surface(cx, instrument("A"), None);
        let panes = [surface.clone()];
        show(&surface, "A", cx);
        share(&panes, &mut layout, cx);
        assert_eq!(drawing_count(&surface, cx), 1);
        assert_eq!(locked_count(&surface, cx), 1);
        assert_eq!(layout_instruments(&layout), ["A"]);

        show(&surface, "B", cx);
        share(&panes, &mut layout, cx);
        assert_eq!(drawing_count(&surface, cx), 0);
    }

    const TWO_LINES: &str = r#"{"schema":"aeris_charts-drawings","schema_version":1,"drawings":[{"id":1,"kind":"horizontal_line","pane_index":0,"points":[{"logical":10.0,"price":100.0}]},{"id":2,"kind":"horizontal_line","pane_index":0,"points":[{"logical":20.0,"price":110.0}]}]}"#;

    /// A committed drawing edit on the surface's chart.
    fn draw(surface: &Entity<WorkspaceSurface>, json: &str, cx: &mut VisualTestContext) {
        surface.update(cx, |surface, cx| {
            let chart = surface.chart.clone().expect("chart");
            chart.update(cx, |chart, _| {
                chart.import_drawings_json(json).expect("drawing imports");
            });
        });
    }

    /// One sync of the layout's panes, as every frame runs.
    fn share(
        panes: &[Entity<WorkspaceSurface>],
        layout: &mut LayoutDrawings,
        cx: &mut VisualTestContext,
    ) -> bool {
        cx.update(|_, cx| sync_layout_drawings(&panes.iter(), layout, cx))
    }

    fn layout_instruments(layout: &LayoutDrawings) -> Vec<String> {
        let mut instruments = layout
            .entries()
            .into_iter()
            .map(|drawings| drawings.instrument_id)
            .collect::<Vec<_>>();
        instruments.sort();
        instruments
    }

    fn drawing_revision(surface: &Entity<WorkspaceSurface>, cx: &VisualTestContext) -> u64 {
        cx.read(|cx| {
            surface
                .read(cx)
                .chart
                .as_ref()
                .map_or(0, |chart| chart.read(cx).drawing_revision())
        })
    }

    #[gpui::test]
    fn panes_showing_one_instrument_share_its_drawings(cx: &mut TestAppContext) {
        let mut layout = LayoutDrawings::default();
        let (first, _first_requests, cx) = open_surface(cx, instrument("A"), None);
        let (second, _second_requests, cx) = open_surface(cx, instrument("A"), None);
        let panes = [first.clone(), second.clone()];
        show(&first, "A", cx);
        share(&panes, &mut layout, cx);
        draw(&first, LINE, cx);
        share(&panes, &mut layout, cx);

        show(&second, "A", cx);
        assert_eq!(drawing_count(&second, cx), 0);
        share(&panes, &mut layout, cx);
        assert_eq!(drawing_count(&second, cx), 1, "a new pane adopts them");

        draw(&second, TWO_LINES, cx);
        share(&panes, &mut layout, cx);
        assert_eq!(drawing_count(&first, cx), 2, "edits reach the other pane");
        let settled = drawing_revision(&first, cx);
        share(&panes, &mut layout, cx);
        assert_eq!(drawing_revision(&first, cx), settled, "nothing echoes back");

        first.update(cx, |surface, cx| {
            surface
                .chart
                .clone()
                .expect("chart")
                .update(cx, |chart, _| chart.clear_drawings());
        });
        share(&panes, &mut layout, cx);
        assert_eq!(
            drawing_count(&second, cx),
            0,
            "a clear reaches the other pane"
        );
        assert!(layout.entries().is_empty(), "a cleared set is not saved");

        show(&second, "B", cx);
        draw(&first, LINE, cx);
        share(&panes, &mut layout, cx);
        assert_eq!(
            drawing_count(&second, cx),
            0,
            "drawings stay on their market"
        );
        show(&second, "A", cx);
        share(&panes, &mut layout, cx);
        assert_eq!(drawing_count(&second, cx), 1, "switching back adopts them");

        show(&first, "B", cx);
        draw(&second, TWO_LINES, cx);
        share(&panes, &mut layout, cx);
        show(&first, "A", cx);
        share(&panes, &mut layout, cx);
        assert_eq!(
            drawing_count(&first, cx),
            2,
            "a pane showing another market remembers the current set"
        );
    }

    /// Drawings belong to the layout, not to the pane that drew them: a pane split off a
    /// chart and closed again never takes another instrument's drawings with it.
    #[gpui::test]
    fn a_split_pane_switching_instrument_keeps_every_instruments_drawings(cx: &mut TestAppContext) {
        let mut layout = LayoutDrawings::default();
        let (original, _original_requests, cx) = open_surface(cx, instrument("BTC"), None);
        show(&original, "BTC", cx);
        share(std::slice::from_ref(&original), &mut layout, cx);
        draw(&original, LINE, cx);
        share(std::slice::from_ref(&original), &mut layout, cx);
        show(&original, "HYPE", cx);
        share(std::slice::from_ref(&original), &mut layout, cx);
        draw(&original, TWO_LINES, cx);
        share(std::slice::from_ref(&original), &mut layout, cx);
        show(&original, "BTC", cx);
        share(std::slice::from_ref(&original), &mut layout, cx);
        assert_eq!(drawing_count(&original, cx), 1);

        let (split, _split_requests, cx) = open_surface(cx, instrument("BTC"), None);
        let panes = [original.clone(), split.clone()];
        show(&split, "BTC", cx);
        share(&panes, &mut layout, cx);
        assert_eq!(
            drawing_count(&split, cx),
            1,
            "the split shows BTC's drawings"
        );
        show(&split, "HYPE", cx);
        share(&panes, &mut layout, cx);
        assert_eq!(
            drawing_count(&split, cx),
            2,
            "the split shows HYPE's drawings"
        );
        assert_eq!(layout_instruments(&layout), ["BTC", "HYPE"]);

        // The split pane closes; the remaining pane switches to HYPE.
        show(&original, "HYPE", cx);
        share(std::slice::from_ref(&original), &mut layout, cx);
        assert_eq!(drawing_count(&original, cx), 2, "HYPE's drawings remain");
        show(&original, "BTC", cx);
        share(std::slice::from_ref(&original), &mut layout, cx);
        assert_eq!(drawing_count(&original, cx), 1, "BTC's drawings remain");
        assert_eq!(layout_instruments(&layout), ["BTC", "HYPE"]);
    }

    fn queued_registrations(requests: &mpsc::Receiver<MarketWorkerCommand>) -> usize {
        requests
            .try_iter()
            .filter(|command| matches!(command, MarketWorkerCommand::RegisterStudy(_)))
            .count()
    }

    fn study_retry_armed(surface: &Entity<WorkspaceSurface>, cx: &VisualTestContext) -> bool {
        cx.read(|cx| {
            let retry = &surface.read(cx).study_retry;
            retry.deadline.is_some() || retry.task.is_some()
        })
    }

    #[gpui::test]
    fn a_deferred_study_registration_retries_without_another_market_message(
        cx: &mut TestAppContext,
    ) {
        let restored = WorkspaceChartState {
            studies: vec![legacy_runtime_study(1, ChartIndicator::Sma, true).expect("SMA study")],
            ..WorkspaceChartState::default()
        };
        let (commands, requests) = mpsc::sync_channel(8);
        let filler = commands.clone();
        let (surface, cx) =
            open_surface_with_commands(cx, instrument("A"), Some(restored), commands);
        requests.try_iter().for_each(drop);
        while filler.try_send(MarketWorkerCommand::Shutdown).is_ok() {}

        show(&surface, "A", cx);
        assert_eq!(
            cx.read(|cx| surface.read(cx).studies.deferred().len()),
            1,
            "a full command lane defers the restored study"
        );
        assert!(study_retry_armed(&surface, cx));

        requests.try_iter().for_each(drop);
        let before_deadline = STUDY_RETRY_DELAY
            .checked_sub(Duration::from_millis(1))
            .expect("the retry delay exceeds one millisecond");
        cx.executor().advance_clock(before_deadline);
        cx.run_until_parked();
        assert_eq!(
            queued_registrations(&requests),
            0,
            "the retry waits for its deadline"
        );

        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert_eq!(
            queued_registrations(&requests),
            1,
            "the armed timer retries the registration"
        );
        cx.read(|cx| {
            let studies = &surface.read(cx).studies;
            assert!(studies.has_pending());
            assert_eq!(studies.deferred(), [] as [PendingRuntimeStudyState; 0]);
        });
        assert!(
            !study_retry_armed(&surface, cx),
            "nothing is armed once the retry was queued"
        );
    }

    #[gpui::test]
    fn a_refused_automatic_study_removal_retries_without_another_market_message(
        cx: &mut TestAppContext,
    ) {
        let study_id = StudyInstanceId::try_from_u64(9).expect("study id");
        let (commands, requests) = mpsc::sync_channel(8);
        let filler = commands.clone();
        let (surface, cx) = open_surface_with_commands(cx, instrument("A"), None, commands);
        requests.try_iter().for_each(drop);
        while filler.try_send(MarketWorkerCommand::Shutdown).is_ok() {}

        surface.update(cx, |surface, cx| {
            surface.studies.activate_for_removal(RuntimeStudyState {
                study_id,
                persisted: legacy_runtime_study(9, ChartIndicator::Sma, true).expect("SMA study"),
                resolved_chart_series: None,
            });
            surface.synchronize_runtime_studies(cx);
        });
        assert!(
            cx.read(|cx| !surface.read(cx).studies.is_removing(study_id)),
            "the full command lane refuses the removal"
        );
        assert!(study_retry_armed(&surface, cx));

        let removals = |requests: &mpsc::Receiver<MarketWorkerCommand>| {
            requests
                .try_iter()
                .filter(|command| {
                    matches!(command, MarketWorkerCommand::RemoveStudy(removed) if *removed == study_id)
                })
                .count()
        };
        requests.try_iter().for_each(drop);
        let before_deadline = STUDY_RETRY_DELAY
            .checked_sub(Duration::from_millis(1))
            .expect("the retry delay exceeds one millisecond");
        cx.executor().advance_clock(before_deadline);
        cx.run_until_parked();
        assert_eq!(removals(&requests), 0, "the retry waits for its deadline");

        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert_eq!(removals(&requests), 1, "the armed timer sends the removal");
        assert!(cx.read(|cx| surface.read(cx).studies.is_removing(study_id)));
        assert!(
            !study_retry_armed(&surface, cx),
            "nothing is armed once the removal was sent"
        );
    }

    #[gpui::test]
    fn a_pane_without_deferred_studies_arms_no_study_retry(cx: &mut TestAppContext) {
        let (surface, _requests, cx) = open_surface(cx, instrument("A"), None);
        show(&surface, "A", cx);
        assert!(!study_retry_armed(&surface, cx));
    }
}
