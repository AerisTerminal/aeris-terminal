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
