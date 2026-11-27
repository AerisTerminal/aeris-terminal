//! Pine-class composite study example using only `aeris_study_sdk::prelude::*` and `order_flow`.
//!
//! Demonstrates:
//! - Fluent builders (`StudyDefinitionBuilder`, `StudySettingBuilder`, `StudyOutputBuilder`)
//! - Semantic settings (`Color`, `Timeframe`, `Session`, `Source`)
//! - `StepLine` + `PerPoint` dynamic coloring, `set_bar_color`, `set_background_color`,
//!   `add_level`, and `add_fill`
//! - Transactional semantic scene primitives (ray lines, bottom-right dashboard table)
//! - Deduplicated `OncePerBarClose` alerts
//! - Canonical bounded order-flow footprint aggregation from `StudyTradeWindow`

use aeris_study_sdk::order_flow::build_footprint_from_window;
use aeris_study_sdk::prelude::*;

fn series(instrument: &str, seconds: u32) -> BarSeriesKey {
    BarSeriesKey {
        provider_id: "example".to_string(),
        instrument_id: instrument.to_string(),
        entitlement_id: "example".to_string(),
        period: BarPeriod::time(seconds).expect("valid bar period"),
        definition_version: 1,
    }
}

fn registration() -> NativeStudyRegistration {
    let primary = series("CME:ES", 60);
    let htf = series("CME:ES", 3600);

    let definition = StudyDefinitionBuilder::new("example.pine_class_composite")
        .market(primary, StreamRequirements::BARS.with(MarketStream::Trades))
        .market(htf, StreamRequirements::BARS)
        .setting(
            StudySettingBuilder::color("bull_color", StudyColor::rgb(0x22, 0xc5, 0x5e))
                .label("Bull Color")
                .group("Style")
                .build(),
        )
        .setting(
            StudySettingBuilder::timeframe(
                "htf",
                StudyTimeframe::parse_label("1h").expect("valid 1h timeframe"),
                true,
            )
            .label("Higher Timeframe")
            .group("Context")
            .build(),
        )
        .setting(
            StudySettingBuilder::session(
                "rth",
                StudySession::new(9 * 60 + 30, 16 * 60, "America/New_York"),
            )
            .label("RTH Session")
            .group("Context")
            .build(),
        )
        .setting(
            StudySettingBuilder::source("source", StudySource::Close, true)
                .label("Price Source")
                .group("Inputs")
                .build(),
        )
        .output(
            StudyOutputBuilder::new("htf_close", "HTF Close", StudyPlotKind::StepLine)
                .point_style(StudyPointStyle::PerPoint)
                .build(),
        )
        .output(StudyOutputBuilder::new("midline", "Session Mid", StudyPlotKind::Line).build())
        .invalidation(StudyInvalidationPolicy::FromFirstChanged)
        .build();

    NativeStudyRegistration {
        definition,
        settings: StudySettings::default(),
        program: NativeStudyProgram::stateless(calculate_composite),
    }
}

fn calculate_composite(ctx: &mut StudyExecutionContext<'_>) -> Result<(), String> {
    let bull = ctx
        .settings()
        .color("bull_color")
        .unwrap_or_else(|| StudyColor::rgb(0x22, 0xc5, 0x5e));
    let bear = StudyColor::rgb(0xef, 0x44, 0x44);

    let primary_close = {
        let Some(StudyInputSeries::Market(bars)) = ctx.input(0) else {
            return Ok(());
        };
        let close = bars.field(StudyBarField::Close);
        let scale = close.scale();
        (0..close.len())
            .map(|i| close.value(i).and_then(|v| fixed_point_to_f64(v, scale)))
            .collect::<Vec<_>>()
    };
    let primary_len = primary_close.len();
    if primary_len == 0 {
        return Ok(());
    }

    let timestamps = ctx.timestamps().to_vec();
    let last_idx = primary_len - 1;
    let last_ts = timestamps[last_idx];
    let last_price = primary_close[last_idx].unwrap_or(0.0);

    let net_delta = ctx
        .live_market(0)
        .and_then(StudyLiveMarketData::trades)
        .and_then(|trades| {
            build_footprint_from_window(trades, 25, last_ts - 60_000_000_000, last_ts + 1).ok()
        })
        .map_or(0, |fp| fp.net_delta());

    for (i, &price) in primary_close.iter().enumerate() {
        if let Some(out0) = ctx.output(0) {
            out0.set(i, price)?;
            out0.set_point_color(i, Some(if net_delta >= 0 { bull } else { bear }))?;
        }
        if let Some(out1) = ctx.output(1) {
            out1.set(i, price.map(|p| p * 0.995))?;
        }
    }

    ctx.set_bar_color(last_idx, Some(if net_delta >= 0 { bull } else { bear }))?;
    ctx.set_background_color(last_idx, Some(StudyColor::rgba(0x22, 0xc5, 0x5e, 0x18)))?;
    ctx.add_level(StudyLevelSpec {
        value: StudyDecimal {
            mantissa: 10000,
            scale: 2,
        },
        color: bull,
        style: StudyLineStyle::Dashed,
        title: Some("Key Level".to_string()),
    })?;
    ctx.add_fill(StudyFillSpec {
        first_output_index: 0,
        second_output_index: 1,
        color: StudyColor::rgba(0x38, 0xbd, 0xf8, 0x24),
    })?;

    update_dashboard_scene(
        ctx,
        timestamps[0],
        last_ts,
        last_price,
        net_delta,
        bull,
        bear,
    )?;

    if net_delta > 0 {
        ctx.emit_alert(StudyAlertEvent {
            condition_id: "positive_delta".to_string(),
            message: format!("Positive bar delta: {net_delta}"),
            bar_timestamp_unix_nanos: last_ts,
            frequency: StudyAlertFrequency::OncePerBarClose,
        })?;
    }
    Ok(())
}

fn update_dashboard_scene(
    ctx: &mut StudyExecutionContext<'_>,
    first_ts: i64,
    last_ts: i64,
    last_price: f64,
    net_delta: i64,
    bull: StudyColor,
    bear: StudyColor,
) -> Result<(), String> {
    ctx.scene_mut().clear();
    let _ = ctx.scene_mut().create_line(StudyLineObject {
        start: StudyPoint::new(first_ts, last_price),
        end: StudyPoint::new(last_ts, last_price),
        color: bull,
        width: 2,
        style: StudyLineStyle::Solid,
        extend: StudyLineExtend::Right,
    })?;
    let table_id = ctx.scene_mut().create_table(
        StudyTablePosition::BottomRight,
        2,
        1,
        Some(StudyColor::rgb(0x33, 0x41, 0x55)),
        1,
    )?;
    ctx.scene_mut().set_table_cell(
        table_id,
        0,
        0,
        StudyTableCell {
            text: "Delta".to_string(),
            text_color: Some(StudyColor::rgb(0xe2, 0xe8, 0xf0)),
            background_color: None,
            align: StudyTextAlign::Left,
        },
    )?;
    ctx.scene_mut().set_table_cell(
        table_id,
        1,
        0,
        StudyTableCell {
            text: net_delta.to_string(),
            text_color: Some(if net_delta >= 0 { bull } else { bear }),
            background_color: None,
            align: StudyTextAlign::Right,
        },
    )
}

fn main() {
    let reg = registration();
    assert_eq!(reg.definition.identifier, "example.pine_class_composite");
    assert_eq!(reg.definition.outputs.len(), 2);
}
