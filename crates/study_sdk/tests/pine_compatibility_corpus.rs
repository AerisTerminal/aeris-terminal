//! Phase K Pine Compatibility Corpus — authored strictly against `aeris_study_sdk::prelude::*`.
//!
//! Each test proves that a representative advanced TradingView-class indicator can be
//! expressed cleanly using only generic Study SDK primitives (semantic settings, MTF
//! alignment, per-bar candle coloring, transactional lines/boxes/labels/tables,
//! visible-range chart context, and typed alerts) with zero indicator-specific core code.

use aeris_study_sdk::prelude::*;

fn sample_series(instrument: &str, period: BarPeriod) -> BarSeriesKey {
    BarSeriesKey {
        provider_id: "corpus_provider".to_string(),
        instrument_id: instrument.to_string(),
        entitlement_id: "corpus".to_string(),
        period,
        definition_version: 1,
    }
}

fn calculate_ksr6(ctx: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let bull = ctx
        .settings()
        .color("bull_color")
        .unwrap_or(StudyColor::rgb(0x22, 0xc5, 0x5e));
    let timestamps = ctx.timestamps().to_vec();
    let last_ts = *timestamps.last().unwrap_or(&0);

    if let Some(StudyInputSeries::Market(htf)) = ctx.input(1) {
        let _daily_idx = align_higher_timeframe_bar(
            last_ts,
            None,
            htf,
            StudyRequestContext {
                confirmation: StudyMtfConfirmation::ConfirmedOnly,
                gaps: StudyMtfGapPolicy::CarryForward,
            },
        );
    }

    if !timestamps.is_empty() {
        ctx.set_bar_color(timestamps.len() - 1, Some(bull))?;
        let _sr_ray = ctx.scene_mut().create_line(StudyLineObject {
            start: StudyPoint::new(timestamps[0], 100.0),
            end: StudyPoint::new(last_ts, 100.0),
            color: bull,
            width: 2,
            style: StudyLineStyle::Dashed,
            extend: StudyLineExtend::Right,
        })?;
        let table =
            ctx.scene_mut()
                .create_table(StudyTablePosition::BottomRight, 2, 2, Some(bull), 1)?;
        ctx.scene_mut().set_table_cell(
            table,
            0,
            0,
            StudyTableCell {
                text: "ADR%".to_string(),
                text_color: Some(bull),
                background_color: None,
                align: StudyTextAlign::Left,
            },
        )?;
    }
    Ok(())
}

/// 1. KSR6-class composite overlay:
///    - 4 moving-average overlays
///    - Semantic Color, Symbol (RS benchmark), and Timeframe (daily ADR / market timer) settings
///    - Trend candle coloring (`set_bar_color`)
///    - Support/resistance ray lines (`create_line`)
///    - Bottom-right metrics dashboard table (`create_table` + `set_table_cell`)
#[test]
fn ksr6_composite_overlay_is_expressible_through_public_sdk_only() {
    let primary = sample_series("NVDA", BarPeriod::time(300).expect("5m"));
    let daily = resolve_market_input(
        &primary,
        None,
        Some(StudyTimeframe::parse_label("1D").expect("1D")),
        StreamRequirements::BARS,
    )
    .expect("daily MTF input");
    let benchmark = resolve_market_input(
        &primary,
        Some(&StudySymbolRef::new("corpus_provider", "SPY")),
        Some(StudyTimeframe::CurrentChart),
        StreamRequirements::BARS,
    )
    .expect("benchmark cross-symbol input");

    let definition = StudyDefinitionBuilder::new("corpus.ksr6")
        .market(primary, StreamRequirements::BARS)
        .market(daily.series, daily.streams)
        .market(benchmark.series, benchmark.streams)
        .setting(
            StudySettingBuilder::color("bull_color", StudyColor::rgb(0x22, 0xc5, 0x5e))
                .label("Bullish Trend")
                .group("Style")
                .build(),
        )
        .setting(
            StudySettingBuilder::symbol(
                "rs_symbol",
                StudySymbolRef::new("corpus_provider", "SPY"),
                false,
            )
            .label("RS Benchmark")
            .group("Context")
            .build(),
        )
        .setting(
            StudySettingBuilder::timeframe(
                "adr_timeframe",
                StudyTimeframe::parse_label("1D").expect("1D"),
                false,
            )
            .label("ADR Timeframe")
            .group("Context")
            .build(),
        )
        .output(
            StudyOutputBuilder::line("ma_10", "EMA 10")
                .legend_label("10")
                .build(),
        )
        .output(
            StudyOutputBuilder::line("ma_20", "EMA 20")
                .legend_label("20")
                .build(),
        )
        .output(
            StudyOutputBuilder::line("ma_50", "SMA 50")
                .legend_label("50")
                .build(),
        )
        .output(
            StudyOutputBuilder::line("ma_200", "SMA 200")
                .legend_label("200")
                .build(),
        )
        .invalidation(StudyInvalidationPolicy::FromFirstChanged)
        .build();

    let registration = NativeStudyRegistration {
        settings: StudySettings::defaults(&definition.settings).expect("valid KSR6 settings"),
        definition,
        program: NativeStudyProgram::stateless(calculate_ksr6),
    };

    assert_eq!(registration.definition.outputs.len(), 4);
    assert_eq!(registration.definition.dependencies.len(), 3);
}

/// 2. `ZigZag` / pivot indicator with long-lived mutable line & label lifecycles.
#[test]
fn zigzag_mutable_scene_lifecycle_is_expressible_through_public_sdk_only() {
    #[derive(Clone, Default)]
    struct ZigZagState {
        last_line: Option<StudySceneObjectId>,
    }

    let definition = StudyDefinitionBuilder::new("corpus.zigzag")
        .market(
            sample_series("ES", BarPeriod::time(60).expect("1m")),
            StreamRequirements::BARS,
        )
        .setting(
            StudySettingBuilder::integer("depth", 12)
                .label("Pivot Depth")
                .integer_bounds(Some(2), Some(200), Some(1))
                .build(),
        )
        .output(
            StudyOutputBuilder::new("pivots", "ZigZag Pivots", StudyPlotKind::Circles)
                .point_style(StudyPointStyle::PerPoint)
                .build(),
        )
        .invalidation(StudyInvalidationPolicy::FromFirstChanged)
        .build();

    let registration = NativeStudyRegistration {
        settings: StudySettings::defaults(&definition.settings).expect("valid zigzag settings"),
        definition,
        program: NativeStudyProgram::stateful(
            |ctx| {
                let ts = ctx.timestamps().to_vec();
                let Some(&first) = ts.first() else {
                    return Ok(());
                };
                let last = *ts.last().unwrap_or(&first);
                let line = StudyLineObject {
                    start: StudyPoint::new(first, 5000.0),
                    end: StudyPoint::new(last, 5025.0),
                    color: StudyColor::rgb(0x38, 0xbd, 0xf8),
                    width: 2,
                    style: StudyLineStyle::Solid,
                    extend: StudyLineExtend::None,
                };
                let existing = ctx.state_mut::<ZigZagState>().and_then(|s| s.last_line);
                if let Some(id) = existing {
                    ctx.scene_mut().update(id, StudySceneObject::Line(line))?;
                } else {
                    let id = ctx.scene_mut().create_line(line)?;
                    if let Some(state) = ctx.state_mut::<ZigZagState>() {
                        state.last_line = Some(id);
                    }
                }
                let _label = ctx.scene_mut().create_label(StudyLabelObject {
                    point: StudyPoint::new(last, 5025.0),
                    text: "HH".to_string(),
                    style: StudyLabelStyle::LabelDown,
                    color: StudyColor::rgb(0x22, 0xc5, 0x5e),
                    text_color: StudyColor::rgb(0xff, 0xff, 0xff),
                })?;
                Ok(())
            },
            |_| {
                Ok(NativeStudyState::new_transactional(
                    ZigZagState::default(),
                    |_| size_of::<ZigZagState>(),
                    Clone::clone,
                ))
            },
        ),
    };

    assert_eq!(
        registration.definition.outputs[0].plot,
        StudyPlotKind::Circles
    );
    assert_eq!(
        registration.definition.outputs[0].point_style,
        StudyPointStyle::PerPoint
    );
}

/// 3. Smart-money / FVG + Opening-Range session study with boxes, levels, and confirmed alerts.
#[test]
fn fvg_and_session_opening_range_with_alerts_are_expressible_through_public_sdk_only() {
    let session = StudySession::parse_label("09:30-10:00 America/New_York").expect("OR session");
    let definition = StudyDefinitionBuilder::new("corpus.fvg_opening_range")
        .market(
            sample_series("NQ", BarPeriod::time(300).expect("5m")),
            StreamRequirements::BARS,
        )
        .setting(
            StudySettingBuilder::session("opening_range", session)
                .label("Opening Range")
                .group("Session")
                .build(),
        )
        .output(StudyOutputBuilder::line("or_mid", "OR Mid").build())
        .invalidation(StudyInvalidationPolicy::FromFirstChanged)
        .build();

    let registration = NativeStudyRegistration {
        settings: StudySettings::defaults(&definition.settings).expect("valid OR settings"),
        definition,
        program: NativeStudyProgram::stateless(|ctx| {
            let ts = ctx.timestamps().to_vec();
            if ts.len() >= 2 {
                let _fvg_box = ctx.scene_mut().create_box(StudyBoxObject {
                    top_left: StudyPoint::new(ts[0], 18_100.0),
                    bottom_right: StudyPoint::new(ts[ts.len() - 1], 18_050.0),
                    border_color: StudyColor::rgb(0x60, 0xa5, 0xfa),
                    border_width: 1,
                    border_style: StudyLineStyle::Solid,
                    fill_color: Some(StudyColor::rgba(96, 165, 250, 36)),
                    text: Some("FVG".to_string()),
                    text_color: Some(StudyColor::rgb(0xbf, 0xdb, 0xfe)),
                })?;
                ctx.add_level(StudyLevelSpec {
                    value: StudyDecimal {
                        mantissa: 18_100,
                        scale: 0,
                    },
                    color: StudyColor::rgb(0xf5, 0x9e, 0x0b),
                    style: StudyLineStyle::Dashed,
                    title: Some("OR High".to_string()),
                })?;
                ctx.emit_alert(StudyAlertEvent {
                    condition_id: "or_breakout_high".to_string(),
                    message: "Opening range high breakout".to_string(),
                    bar_timestamp_unix_nanos: *ts.last().unwrap(),
                    frequency: StudyAlertFrequency::OncePerBarClose,
                })?;
            }
            Ok(())
        })
        .with_chart_context(),
    };

    assert!(registration.program.observes_chart_context);
}

/// 4. Multi-symbol / MTF heatmap dashboard using semantic Symbol/Timeframe inputs and tables.
#[test]
fn multi_symbol_mtf_heatmap_dashboard_is_expressible_through_public_sdk_only() {
    let base = sample_series("ES", BarPeriod::time(60).expect("1m"));
    let sym_a = resolve_market_input(
        &base,
        Some(&StudySymbolRef::new("corpus_provider", "NQ")),
        Some(StudyTimeframe::parse_label("5m").expect("5m")),
        StreamRequirements::BARS,
    )
    .expect("NQ 5m");
    let sym_b = resolve_market_input(
        &base,
        Some(&StudySymbolRef::new("corpus_provider", "YM")),
        Some(StudyTimeframe::parse_label("15m").expect("15m")),
        StreamRequirements::BARS,
    )
    .expect("YM 15m");

    let definition = StudyDefinitionBuilder::new("corpus.mtf_heatmap")
        .market(base, StreamRequirements::BARS)
        .market(sym_a.series, sym_a.streams)
        .market(sym_b.series, sym_b.streams)
        .output(StudyOutputBuilder::line("anchor", "Heatmap Anchor").build())
        .invalidation(StudyInvalidationPolicy::SameRange)
        .build();

    let registration = NativeStudyRegistration {
        settings: StudySettings::defaults(&definition.settings).expect("valid heatmap settings"),
        definition,
        program: NativeStudyProgram::stateless(|ctx| {
            let table = ctx.scene_mut().create_table(
                StudyTablePosition::TopRight,
                3,
                3,
                Some(StudyColor::rgb(0x47, 0x55, 0x69)),
                1,
            )?;
            for (row, label) in ["ES", "NQ", "YM"].into_iter().enumerate() {
                let row_u16 = u16::try_from(row).unwrap_or(0);
                ctx.scene_mut().set_table_cell(
                    table,
                    0,
                    row_u16,
                    StudyTableCell {
                        text: label.to_string(),
                        text_color: Some(StudyColor::rgb(0xf8, 0xfa, 0xfc)),
                        background_color: Some(StudyColor::rgb(0x1e, 0x29, 0x3b)),
                        align: StudyTextAlign::Left,
                    },
                )?;
            }
            Ok(())
        }),
    };
    assert_eq!(registration.definition.dependencies.len(), 3);
}

/// 5. Visible-range volume profile + custom candle + lower-timeframe intrabar study.
#[test]
fn visible_range_and_lower_timeframe_intrabar_studies_are_expressible_through_public_sdk_only() {
    let primary = sample_series("ES", BarPeriod::time(300).expect("5m"));
    let ltf = sample_series("ES", BarPeriod::time(60).expect("1m"));
    let definition = StudyDefinitionBuilder::new("corpus.vr_intrabar")
        .market(primary, StreamRequirements::BARS)
        .market(ltf, StreamRequirements::BARS)
        .output(
            StudyOutputBuilder::new("poc", "Visible POC", StudyPlotKind::StepLine)
                .point_style(StudyPointStyle::PerPoint)
                .build(),
        )
        .invalidation(StudyInvalidationPolicy::FromFirstChanged)
        .build();

    let registration = NativeStudyRegistration {
        settings: StudySettings::defaults(&definition.settings).expect("valid settings"),
        definition,
        program: NativeStudyProgram::stateless(|ctx| {
            let chart = ctx.chart_context();
            let timestamps = ctx.timestamps().to_vec();
            if let (Some(&first), Some(&last)) = (timestamps.first(), timestamps.last())
                && chart.contains_timestamp(last)
            {
                if let Some(StudyInputSeries::Market(ltf_series)) = ctx.input(1) {
                    let _sub_bars = intrabar_range(first, last.saturating_add(1), ltf_series);
                }
                ctx.set_bar_color(
                    timestamps.len() - 1,
                    Some(StudyColor::rgb(0x38, 0xbd, 0xf8)),
                )?;
            }
            Ok(())
        })
        .with_chart_context()
        .with_intrabar_state(|_| Ok(NativeStudyState::new(0u64, |_| size_of::<u64>()))),
    };

    assert!(registration.program.observes_chart_context);
    assert!(registration.program.intrabar_state_factory.is_some());
}

/// 6. Label/polyline churn study verifying compact `Create`/`Update`/`Delete` scene deltas.
#[test]
fn label_and_polyline_scene_deltas_remain_compact_under_churn() {
    let mut prev = StudySceneState::default();
    let poly_id = prev
        .create_polyline(StudyPolylineObject {
            points: vec![StudyPoint::new(100, 10.0), StudyPoint::new(200, 12.0)],
            color: StudyColor::rgb(0x38, 0xbd, 0xf8),
            width: 2,
            style: StudyLineStyle::Solid,
            fill_color: None,
            closed: false,
        })
        .expect("polyline creates");
    let label_id = prev
        .create_label(StudyLabelObject {
            point: StudyPoint::new(200, 12.0),
            text: "Swing".to_string(),
            style: StudyLabelStyle::Circle,
            color: StudyColor::rgb(0x22, 0xc5, 0x5e),
            text_color: StudyColor::rgb(0xff, 0xff, 0xff),
        })
        .expect("label creates");

    let mut next = prev.clone();
    next.update(
        poly_id,
        StudySceneObject::Polyline(StudyPolylineObject {
            points: vec![
                StudyPoint::new(100, 10.0),
                StudyPoint::new(200, 12.0),
                StudyPoint::new(300, 11.5),
            ],
            color: StudyColor::rgb(0x38, 0xbd, 0xf8),
            width: 2,
            style: StudyLineStyle::Solid,
            fill_color: None,
            closed: false,
        }),
    )
    .expect("polyline updates");
    assert!(next.delete(label_id));

    let deltas = prev.diff(&next);
    assert_eq!(deltas.len(), 2);
    assert!(
        deltas
            .iter()
            .any(|d| matches!(d, StudySceneDelta::Delete { id } if *id == label_id))
    );
    assert!(
        deltas
            .iter()
            .any(|d| matches!(d, StudySceneDelta::Update { id, .. } if *id == poly_id))
    );
}

#[test]
fn historical_equivalence_semantic_codecs_sessions_and_event_timelines_match_reference() {
    let ns = |s: i64| s * 1_000_000_000;

    // 1. Semantic color / timeframe / source reference round-trips.
    let color = StudyColor::parse_hex("#38bdf8cc").expect("valid RGBA hex");
    assert_eq!(color.to_rgba_u32(), 0x38bd_f8cc);
    assert_eq!(color.to_hex(), "#38bdf8cc");
    assert_eq!(
        StudyTimeframe::parse_label("1h").map(StudyTimeframe::to_label),
        Some("1h".to_string())
    );
    assert_eq!(
        StudySource::parse_label("hlc3").map(|src| src.to_label()),
        Some("hlc3".to_string())
    );

    // 2. Overnight session wrap reference fixture (CME Globex 18:00-17:00 America/Chicago).
    let globex = StudySession::parse_label("18:00-17:00 America/Chicago").expect("valid globex");
    assert_eq!(globex.to_label(), "18:00-17:00 America/Chicago");
    assert!(globex.contains_minute(18 * 60));
    assert!(globex.contains_minute(0));
    assert!(globex.contains_minute(16 * 60 + 59));
    assert!(!globex.contains_minute(17 * 60));
    assert!(!globex.contains_minute(17 * 60 + 30));

    // 3. Pure non-bar event timeline deterministic second-projection fixture.
    let mut timeline = StudyEventTimeline::new(64, StudyEventCoalescePolicy::ExactSequence);
    for (seq, (ts_ms, val)) in [(1_100, 100.0), (1_500, 101.5), (2_050, 102.0)]
        .into_iter()
        .enumerate()
    {
        timeline
            .push(StudyEventPoint {
                timestamp_unix_nanos: ts_ms * 1_000_000,
                source_sequence: seq as u64 + 1,
                kind: StudyEventKind::Trade,
                value: val,
            })
            .expect("pushes event");
    }
    let (ts, vals) = timeline.project_to_chart_seconds();
    assert_eq!(ts, vec![ns(1), ns(2)]);
    assert_eq!(vals, vec![Some(101.5), Some(102.0)]);
}

#[test]
fn large_history_and_high_object_count_scene_soak_remains_strictly_bounded() {
    let mut scene = StudySceneState::default();
    let mut live_ids = Vec::with_capacity(400);
    for i in 0..400_u32 {
        let id = scene
            .create_box(StudyBoxObject {
                top_left: StudyPoint::new(i64::from(i) * 60_000_000_000, 100.0 + f64::from(i)),
                bottom_right: StudyPoint::new(
                    (i64::from(i) + 1) * 60_000_000_000,
                    99.0 + f64::from(i),
                ),
                border_color: StudyColor::rgb(0x38, 0xbd, 0xf8),
                border_width: 1,
                border_style: StudyLineStyle::Solid,
                fill_color: Some(StudyColor::rgba(0x38, 0xbd, 0xf8, 0x20)),
                text: None,
                text_color: None,
            })
            .expect("initial 400 boxes fit under bound");
        live_ids.push(id);
    }
    assert_eq!(scene.len(), 400);

    // 100 live churn waves: delete 5 oldest, create 5 newest, update 5 active -> O(changed) deltas.
    for wave in 0..100_u32 {
        let mut next = scene.clone();
        for _ in 0..5 {
            let oldest = live_ids.remove(0);
            assert!(next.delete(oldest));
        }
        for k in 0..5_u32 {
            let ts = (400 + i64::from(wave) * 5 + i64::from(k)) * 60_000_000_000;
            let id = next
                .create_box(StudyBoxObject {
                    top_left: StudyPoint::new(ts, 500.0 + f64::from(k)),
                    bottom_right: StudyPoint::new(ts + 60_000_000_000, 499.0 + f64::from(k)),
                    border_color: StudyColor::rgb(0x22, 0xc5, 0x5e),
                    border_width: 1,
                    border_style: StudyLineStyle::Solid,
                    fill_color: None,
                    text: None,
                    text_color: None,
                })
                .expect("steady-state create stays bounded");
            live_ids.push(id);
        }
        for &target_id in live_ids.iter().take(5) {
            next.update(
                target_id,
                StudySceneObject::Label(StudyLabelObject {
                    point: StudyPoint::new(i64::from(wave) * 1_000_000_000, 250.0),
                    text: format!("W{wave}"),
                    style: StudyLabelStyle::TextOnly,
                    color: StudyColor::rgb(0xf5, 0x9e, 0x0b),
                    text_color: StudyColor::rgb(0xff, 0xff, 0xff),
                }),
            )
            .expect("steady-state update stays bounded");
        }
        let deltas = scene.diff(&next);
        assert_eq!(
            deltas.len(),
            15,
            "delta count must be O(changed), never O(scene)"
        );
        assert_eq!(next.len(), 400);
        scene = next;
    }

    // Fill to exact MAXIMUM_STUDY_SCENE_OBJECTS and confirm deterministic overflow rejection.
    while scene.len() < aeris_study_sdk::MAXIMUM_STUDY_SCENE_OBJECTS {
        scene
            .create_label(StudyLabelObject {
                point: StudyPoint::new(1_000_000_000, 1.0),
                text: "cap".to_string(),
                style: StudyLabelStyle::TextOnly,
                color: StudyColor::rgb(0, 0, 0),
                text_color: StudyColor::rgb(255, 255, 255),
            })
            .expect("fits up to exact cap");
    }
    assert!(
        scene
            .create_label(StudyLabelObject {
                point: StudyPoint::new(2_000_000_000, 1.0),
                text: "overflow".to_string(),
                style: StudyLabelStyle::TextOnly,
                color: StudyColor::rgb(0, 0, 0),
                text_color: StudyColor::rgb(255, 255, 255),
            })
            .is_err()
    );
}
