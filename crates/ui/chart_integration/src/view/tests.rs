#![cfg(test)]

use super::*;
use aeris_application::{Provenanced, ReplayTailOperation, ReplayTailUpdate};
use aeris_charts_engine::{
    AppearanceColor, AxisTextMidpoint, ChartCursor, ChartKey, DrawingKind, DrawingModifiers,
    InputModifiers, PointerInput, WheelSample,
};
use std::collections::HashSet;

// Input helpers drive the Aeris Charts input controller exactly as the GPUI listeners do: the
// shared adapter forwards one normalized event, then the view turns engine requests into product
// state.

fn pointer(x: f64, y: f64) -> PointerInput {
    PointerInput {
        x,
        y,
        ..PointerInput::default()
    }
}

fn with_modifiers(x: f64, y: f64, modifiers: InputModifiers) -> PointerInput {
    PointerInput {
        modifiers,
        ..pointer(x, y)
    }
}

const SHIFT: InputModifiers = InputModifiers {
    shift: true,
    control: false,
    alt: false,
    meta: false,
};

const CTRL: InputModifiers = InputModifiers {
    shift: false,
    control: true,
    alt: false,
    meta: false,
};

fn press_with(chart: &mut AerisChartView, input: PointerInput, click_count: u32) {
    chart.engine.input_pointer_down(input, click_count);
    chart.process_input_events();
}

fn press(chart: &mut AerisChartView, x: f64, y: f64) {
    press_with(chart, pointer(x, y), 1);
}

fn move_to(chart: &mut AerisChartView, input: PointerInput, pressed: bool) {
    chart.engine.input_pointer_move(input, pressed);
    chart.process_input_events();
}

fn hover(chart: &mut AerisChartView, x: f64, y: f64) {
    move_to(chart, pointer(x, y), false);
}

fn release_with(chart: &mut AerisChartView, input: PointerInput) {
    chart.engine.input_pointer_up(input);
    chart.process_input_events();
}

fn release(chart: &mut AerisChartView, x: f64, y: f64) {
    release_with(chart, pointer(x, y));
}

fn click(chart: &mut AerisChartView, x: f64, y: f64) {
    press(chart, x, y);
    release(chart, x, y);
}

fn double_click(chart: &mut AerisChartView, x: f64, y: f64) {
    click(chart, x, y);
    press_with(chart, pointer(x, y), 2);
    release(chart, x, y);
}

/// Press, move through intermediate samples past the drag threshold, and release.
fn drag_with(
    chart: &mut AerisChartView,
    from: (f64, f64),
    to: (f64, f64),
    modifiers: InputModifiers,
) {
    press_with(chart, with_modifiers(from.0, from.1, modifiers), 1);
    for step in 1..=4 {
        let t = f64::from(step) / 4.0;
        let x = from.0 + (to.0 - from.0) * t;
        let y = from.1 + (to.1 - from.1) * t;
        move_to(chart, with_modifiers(x, y, modifiers), true);
    }
    release_with(chart, with_modifiers(to.0, to.1, modifiers));
}

fn drag(chart: &mut AerisChartView, from: (f64, f64), to: (f64, f64)) {
    drag_with(chart, from, to, InputModifiers::default());
}

fn key_with(chart: &mut AerisChartView, key: ChartKey, modifiers: InputModifiers) -> bool {
    let handled = chart.engine.input_key_down(key, modifiers, false, 0.0);
    chart.process_input_events();
    handled
}

fn key(chart: &mut AerisChartView, key: ChartKey) -> bool {
    key_with(chart, key, InputModifiers::default())
}

fn cursor(chart: &AerisChartView) -> ChartCursor {
    chart.engine.input_cursor()
}

fn custom_color(color: &str) -> AppearanceColor {
    AppearanceColor::Custom(color.to_string())
}

fn study_output(
    chart: &AerisChartView,
    study_id: u64,
    output_index: usize,
) -> aeris_charts_engine::ExternalStudyOutputInfo {
    chart
        .engine
        .external_study_output_info(study_id, output_index)
        .expect("study output is tracked by Aeris Charts")
}

fn study_series_ids(chart: &AerisChartView, study_id: u64) -> Vec<u32> {
    chart
        .engine
        .external_study_outputs()
        .into_iter()
        .filter_map(|output| (output.study_id == study_id).then_some(output.series_id))
        .collect()
}

fn interactive_chart() -> AerisChartView {
    let mut chart = AerisChartView::new();
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    chart.engine.fit_content();
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    chart.fitted = true;
    chart
}

fn order_flow_trade(ordinal: u64, timestamp_micros: i64, volume: f64) -> OrderFlowTrade {
    OrderFlowTrade {
        ingestion_ordinal: ordinal,
        timestamp_micros,
        price: 100.0 + ordinal.to_f64().expect("small ordinal") * 0.25,
        volume,
        aggressor: if ordinal.is_multiple_of(2) {
            aeris_charts_engine::AggressorSide::Sell
        } else {
            aeris_charts_engine::AggressorSide::Buy
        },
        session_id: Some(7),
    }
}

fn footprint_total_volume(chart: &AerisChartView) -> f64 {
    chart
        .engine
        .footprint_bars(chart.footprint_series_id().expect("footprint"))
        .expect("footprint bars")
        .iter()
        .map(|bar| bar.total_volume)
        .sum()
}

#[test]
fn footprint_rewrites_its_window_and_keeps_history_across_a_reconnect() {
    let mut chart = AerisChartView::empty();
    chart.set_chart_type(ChartType::Footprint);
    let aggregation = OrderFlowAggregation::TimeMicros(60_000_000);
    let mut trades = vec![
        order_flow_trade(1, 1_000_000, 2.0),
        order_flow_trade(2, 2_000_000, 3.0),
    ];
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &trades)
        .unwrap();
    trades[0].volume = 5.0;
    chart.invalidate_order_flow_prefix();
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &trades)
        .unwrap();
    assert!((footprint_total_volume(&chart) - 8.0).abs() < f64::EPSILON);
    let stream = chart
        .order_flow_state
        .as_ref()
        .unwrap()
        .presentation
        .trade_stream();

    // The reconnected session restarts its ordinals; a print the chart already holds is not
    // counted twice, and the session's new prints continue the same bars.
    let reconnected = [
        order_flow_trade(1, 2_000_000, 3.0),
        order_flow_trade(2, 3_000_000, 4.0),
    ];
    chart.invalidate_order_flow_prefix();
    chart
        .apply_order_flow_trades("instrument:test", 8, aggregation, 0.25, &reconnected)
        .unwrap();
    assert_eq!(
        chart
            .order_flow_state
            .as_ref()
            .unwrap()
            .presentation
            .trade_stream(),
        stream
    );
    assert!((footprint_total_volume(&chart) - 12.0).abs() < f64::EPSILON);
    assert_eq!(
        chart.order_flow_resume_ordinal("instrument:test", 8, aggregation, 0.25),
        Some(2)
    );
    assert!(
        chart
            .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &trades)
            .is_err(),
        "a retired provider session never mutates the chart"
    );
    chart
        .apply_order_flow_trades("instrument:test", 8, aggregation, 0.25, &reconnected)
        .unwrap();
    assert!((footprint_total_volume(&chart) - 12.0).abs() < f64::EPSILON);
}

#[test]
fn a_restarted_tape_in_the_same_session_keeps_older_footprint_bars() {
    let mut chart = AerisChartView::empty();
    chart.set_chart_type(ChartType::Footprint);
    let aggregation = OrderFlowAggregation::TimeMicros(60_000_000);
    let history = (1..=3)
        .map(|ordinal| order_flow_trade(ordinal, i64::try_from(ordinal).unwrap() * 60_000_000, 2.0))
        .collect::<Vec<_>>();
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &history)
        .unwrap();

    // The runtime cleared its window without a new provider session: its ordinals restart and
    // it holds only the prints since the reset, so the host invalidates the applied prefix.
    let restarted = [
        order_flow_trade(1, 181_000_000, 4.0),
        order_flow_trade(2, 241_000_000, 1.0),
    ];
    chart.invalidate_order_flow_prefix();
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &restarted)
        .unwrap();
    let footprint = chart.footprint_series_id().expect("footprint");
    let starts = chart
        .engine
        .footprint_bars(footprint)
        .expect("footprint bars")
        .iter()
        .map(|bar| bar.start_timestamp_micros / 60_000_000)
        .collect::<Vec<_>>();
    assert_eq!(starts, [1, 2, 3, 4]);
    assert!((footprint_total_volume(&chart) - 11.0).abs() < f64::EPSILON);
    assert_eq!(
        chart.order_flow_resume_ordinal("instrument:test", 7, aggregation, 0.25),
        Some(2)
    );

    // The restarted window keeps streaming as a suffix onto the same bars.
    chart
        .apply_order_flow_trades(
            "instrument:test",
            7,
            aggregation,
            0.25,
            &[
                order_flow_trade(2, 241_000_000, 1.0),
                order_flow_trade(3, 242_000_000, 3.0),
            ],
        )
        .unwrap();
    assert!((footprint_total_volume(&chart) - 14.0).abs() < f64::EPSILON);
}

#[test]
fn order_flow_settings_and_chart_type_changes_keep_footprint_history() {
    let mut chart = AerisChartView::empty();
    chart.set_chart_type(ChartType::Footprint);
    let aggregation = OrderFlowAggregation::TimeMicros(60_000_000);
    let first = vec![
        order_flow_trade(1, 1_000_000, 2.0),
        order_flow_trade(2, 61_000_000, 3.0),
    ];
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &first)
        .unwrap();
    let stream = chart
        .order_flow_state
        .as_ref()
        .unwrap()
        .presentation
        .trade_stream();
    // The runtime window has since evicted both prints.
    let window = [order_flow_trade(3, 121_000_000, 5.0)];
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &window)
        .unwrap();
    assert!((footprint_total_volume(&chart) - 10.0).abs() < f64::EPSILON);

    let mut settings = chart.order_flow_settings();
    settings.ticks_per_row = 4;
    settings.display_mode = FootprintDisplayMode::Delta;
    settings.show_cumulative_delta = true;
    settings.big_trades = Some(BigTradesSettings::default());
    assert!(chart.set_order_flow_settings(settings).unwrap());
    assert_eq!(chart.footprint_ticks_per_row(), Some(4));
    assert!(chart.has_order_flow_study(OrderFlowStudy::CumulativeDelta));
    assert!(chart.drawn_big_trades().is_some());
    assert!((footprint_total_volume(&chart) - 10.0).abs() < f64::EPSILON);

    settings.show_cumulative_delta = false;
    settings.big_trades = None;
    assert!(chart.set_order_flow_settings(settings).unwrap());
    // Candles with no tape study still follow the stream, so the footprint returns whole.
    chart.set_chart_type(ChartType::Candles);
    assert!(chart.footprint_series_id().is_none());
    let window = [
        order_flow_trade(3, 121_000_000, 5.0),
        order_flow_trade(4, 122_000_000, 1.0),
    ];
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &window)
        .unwrap();
    chart.set_chart_type(ChartType::Footprint);
    assert!(chart.has_footprint_bars());
    assert!((footprint_total_volume(&chart) - 11.0).abs() < f64::EPSILON);
    assert_eq!(
        chart
            .order_flow_state
            .as_ref()
            .unwrap()
            .presentation
            .trade_stream(),
        stream
    );
}

#[test]
fn order_flow_resume_ordinal_scopes_suffix_projection_and_appends_skip_forced_layout() {
    let mut chart = AerisChartView::empty();
    chart.set_chart_type(ChartType::Footprint);
    let aggregation = OrderFlowAggregation::TimeMicros(60_000_000);
    let resume = |chart: &AerisChartView, generation, tick| {
        chart.order_flow_resume_ordinal("instrument:test", generation, aggregation, tick)
    };
    assert_eq!(resume(&chart, 7, 0.25), None);
    let tape = (1..=6)
        .map(|ordinal| order_flow_trade(ordinal, i64::try_from(ordinal).unwrap() * 1_000_000, 1.0))
        .collect::<Vec<_>>();
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &tape[..4])
        .expect("covering tape");
    assert!(chart.layout_dirty, "a covering install lays out");
    assert_eq!(resume(&chart, 7, 0.25), Some(4));
    assert_eq!(resume(&chart, 8, 0.25), None, "another provider generation");
    assert_eq!(resume(&chart, 7, 0.5), None, "another price increment");
    assert_eq!(
        chart.order_flow_resume_ordinal("instrument:other", 7, aggregation, 0.25),
        None
    );

    // A host sends one already-applied trade plus the new suffix.
    chart.layout_dirty = false;
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &tape[3..])
        .expect("suffix append");
    assert!(
        !chart.layout_dirty,
        "a tip append invalidates only the frame"
    );
    assert_eq!(resume(&chart, 7, 0.25), Some(6));

    let mut full = AerisChartView::empty();
    full.set_chart_type(ChartType::Footprint);
    full.apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &tape)
        .expect("full tape");
    let bars = |chart: &AerisChartView| {
        chart
            .engine
            .footprint_bars(chart.footprint_series_id().expect("footprint"))
            .expect("bars")
    };
    assert_eq!(bars(&chart), bars(&full));

    chart.invalidate_order_flow_prefix();
    assert_eq!(
        resume(&chart, 7, 0.25),
        None,
        "a rewrite needs the covering window"
    );
}

fn enable_cvd_and_delta(
    chart: &mut AerisChartView,
    aggregation: OrderFlowAggregation,
    trades: &[OrderFlowTrade],
) -> u32 {
    let mut settings = chart.order_flow_settings();
    settings.show_cumulative_delta = true;
    settings.show_delta_histogram = true;
    assert!(
        chart
            .set_order_flow_settings(settings)
            .expect("settings apply")
    );
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, trades)
        .expect("selected studies are added");
    chart
        .footprint_series_id()
        .expect("reconfigured footprint series")
}

#[test]
fn cvd_and_delta_run_on_every_primary_chart_presentation() {
    let mut chart = AerisChartView::empty();
    let aggregation = OrderFlowAggregation::TimeMicros(60_000_000);
    let mut trades = vec![
        order_flow_trade(1, 1_000_000, 2.0),
        order_flow_trade(2, 2_000_000, 3.0),
    ];
    let mut settings = chart.order_flow_settings();
    settings.show_cumulative_delta = true;
    settings.show_delta_histogram = true;
    assert!(
        chart
            .set_order_flow_settings(settings)
            .expect("settings apply")
    );

    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &trades)
        .expect("candlestick chart accepts order-flow studies");
    assert!(chart.footprint_series_id().is_none());
    assert!(chart.has_order_flow_study(OrderFlowStudy::CumulativeDelta));
    assert!(chart.has_order_flow_study(OrderFlowStudy::Delta));
    assert_eq!(series_entry(&chart, 0).render_before_time, None);
    assert!(chart.order_flow_state.is_some());

    chart.set_chart_type(ChartType::Line);
    assert!(
        chart.order_flow_state.is_some(),
        "switching between ordinary price presentations preserves the shared stream"
    );
    assert!(chart.has_order_flow_study(OrderFlowStudy::CumulativeDelta));
    assert!(chart.has_order_flow_study(OrderFlowStudy::Delta));
    trades.push(order_flow_trade(3, 3_000_000, 5.0));
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &trades)
        .expect("line chart advances order-flow studies");

    chart.set_chart_type(ChartType::Footprint);
    assert!(
        chart.footprint_series_id().is_some(),
        "the footprint joins the existing stream"
    );
    assert!(chart.has_footprint_bars());
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &trades)
        .expect("footprint advances on the shared presentation");
    assert!(chart.has_order_flow_study(OrderFlowStudy::CumulativeDelta));
    assert!(chart.has_order_flow_study(OrderFlowStudy::Delta));

    chart.set_chart_type(ChartType::Bars);
    assert!(chart.order_flow_state.is_some());
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &trades)
        .expect("bars keep only the study panes");
    assert!(chart.footprint_series_id().is_none());
    assert!(chart.has_order_flow_study(OrderFlowStudy::CumulativeDelta));
    assert!(chart.has_order_flow_study(OrderFlowStudy::Delta));
    assert_eq!(series_entry(&chart, 0).render_before_time, None);
}

#[test]
fn footprint_keeps_bars_after_runtime_prefix_eviction_and_across_a_gap() {
    let mut chart = AerisChartView::empty();
    chart.set_chart_type(ChartType::Footprint);
    let aggregation = OrderFlowAggregation::TimeMicros(60_000_000);
    let first = vec![
        order_flow_trade(1, 1_000_000, 2.0),
        order_flow_trade(2, 1_500_000, 3.0),
    ];
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &first)
        .expect("initial covering tape");
    let footprint_series = chart
        .engine
        .series
        .iter()
        .find(|series| series.kind == aeris_charts_engine::SeriesKind::Footprint && !series.removed)
        .map(|series| series.id)
        .expect("footprint series");
    let after_covering = chart
        .engine
        .footprint_work_stats(footprint_series)
        .expect("stats");

    let third = order_flow_trade(3, 2_000_000, 5.0);
    let mut appended = first.clone();
    appended.push(third.clone());
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &appended)
        .expect("tip suffix");
    let after_tip = chart
        .engine
        .footprint_work_stats(footprint_series)
        .expect("stats");
    assert!(after_tip.incremental_ticks > after_covering.incremental_ticks);
    let total_volume = |chart: &AerisChartView| {
        chart
            .engine
            .footprint_bars(footprint_series)
            .expect("footprint bars")
            .iter()
            .map(|bar| bar.total_volume)
            .sum::<f64>()
    };

    // The runtime's sliding window evicts trade 1, then every trade, while the
    // chart keeps the bars it already built from them.
    let fourth = order_flow_trade(4, 61_000_000, 7.0);
    chart
        .apply_order_flow_trades(
            "instrument:test",
            7,
            aggregation,
            0.25,
            &[first[1].clone(), third, fourth],
        )
        .expect("suffix after prefix eviction");
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &[])
        .expect("empty window after full eviction");
    assert!((total_volume(&chart) - 17.0).abs() < f64::EPSILON);
    assert_eq!(
        chart
            .engine
            .footprint_bars(footprint_series)
            .expect("footprint bars")
            .len(),
        2
    );

    // A gap means unseen trades were evicted; the built bars stay and the window continues them.
    chart
        .apply_order_flow_trades(
            "instrument:test",
            7,
            aggregation,
            0.25,
            &[
                order_flow_trade(8, 61_000_000, 7.0),
                order_flow_trade(9, 121_000_000, 11.0),
            ],
        )
        .expect("window continues the bars after a gap");
    assert!(
        (total_volume(&chart) - 35.0).abs() < f64::EPSILON,
        "a new print sharing the newest print's microsecond still counts"
    );
    assert_eq!(
        chart
            .engine
            .footprint_bars(footprint_series)
            .expect("footprint bars")
            .len(),
        3
    );
}

#[test]
fn footprint_rebuilds_rows_when_refreshed_metadata_corrects_the_tick_size() {
    let mut chart = AerisChartView::empty();
    chart.set_chart_type(ChartType::Footprint);
    let mut trades = vec![
        order_flow_trade(1, 1_000_000, 2.0),
        order_flow_trade(2, 1_500_000, 3.0),
    ];
    trades[0].price = 100.0;
    trades[1].price = 101.0;
    let aggregation = OrderFlowAggregation::TimeMicros(60_000_000);
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 1e-8, &trades)
        .expect("fallback tick");
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 1.0, &trades)
        .expect("corrected tick rebuilds instead of rejecting the tape");
    let footprints = chart
        .engine
        .series
        .iter()
        .filter(|series| {
            series.kind == aeris_charts_engine::SeriesKind::Footprint && !series.removed
        })
        .map(|series| series.id)
        .collect::<Vec<_>>();
    assert_eq!(footprints.len(), 1, "the stale footprint series is removed");
    let bars = chart
        .engine
        .footprint_bars(footprints[0])
        .expect("footprint bars");
    assert_eq!(bars.len(), 1);
    assert_eq!(bars[0].levels.len(), 2);
}

#[test]
fn sweep_classifier_requires_same_side_time_proximity_and_multiple_levels() {
    let mut trades = vec![
        order_flow_trade(1, 1_000_000, 6.0),
        order_flow_trade(3, 1_020_000, 7.0),
        order_flow_trade(5, 1_040_000, 8.0),
    ];
    for (index, trade) in trades.iter_mut().enumerate() {
        trade.ingestion_ordinal = u64::try_from(index + 1).expect("small index");
        trade.aggressor = aeris_charts_engine::AggressorSide::Buy;
        trade.price = 100.0 + index.to_f64().expect("small index") * 0.25;
    }
    let sweeps = classify_order_flow_sweeps(&trades);
    assert_eq!(sweeps.len(), 1);
    assert_eq!(sweeps[0].price_levels, 3);
    assert!((sweeps[0].total_volume - 21.0).abs() < f64::EPSILON);

    trades[1].aggressor = aeris_charts_engine::AggressorSide::Sell;
    assert_eq!(
        classify_order_flow_sweeps(&trades),
        [] as [OrderFlowSweep; 0]
    );
}

fn apply_two_print_tape(chart: &mut AerisChartView) {
    let trades = [
        order_flow_trade(1, 1_000_000, 2.0),
        order_flow_trade(2, 2_000_000, 3.0),
    ];
    let aggregation = OrderFlowAggregation::TimeMicros(60_000_000);
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &trades)
        .expect("tape applies");
}

fn candle_chart_with_big_trades(
    big_trades: BigTradesSettings,
) -> (AerisChartView, aeris_charts_engine::NativePrimitiveId) {
    let mut chart = AerisChartView::empty();
    let mut settings = chart.order_flow_settings();
    settings.big_trades = Some(big_trades);
    assert!(
        chart
            .set_order_flow_settings(settings)
            .expect("settings apply")
    );
    apply_two_print_tape(&mut chart);
    let id = chart.drawn_big_trades().expect("big trades over candles");
    (chart, id)
}

#[test]
fn big_trades_follow_durable_settings_on_every_price_presentation() {
    let mut chart = AerisChartView::empty();
    apply_two_print_tape(&mut chart);
    assert!(
        chart.order_flow_state.is_none(),
        "a candle chart requests no tape before big trades are added"
    );

    let (mut chart, id) = candle_chart_with_big_trades(BigTradesSettings {
        size: BigTradesSize::Large,
        ..BigTradesSettings::default()
    });
    assert!(chart.has_indicators());
    assert!(chart.footprint_series_id().is_none());
    let colors = AerisTheme::dark().colors;
    let options = chart.engine.big_trades_options(id).expect("options");
    assert_eq!(options.buy_color, colors.buy_bubble.css_rgba());
    assert_eq!(options.sell_color, colors.sell_bubble.css_rgba());
    assert_eq!(options.buy_border_color, colors.bullish.css_rgba());
    assert_eq!(options.sell_border_color, colors.bearish.css_rgba());
    let row = chart
        .legend_rows()
        .into_iter()
        .find(|row| row.item == LegendItem::BigTrades)
        .expect("big trades legend row");
    assert_eq!(row.title, "Big Trades");
    assert!(row.visible && row.settings_available);

    // The footprint rebuild keeps the bubbles over the shared product price series.
    chart.set_chart_type(ChartType::Footprint);
    apply_two_print_tape(&mut chart);
    assert!(chart.footprint_series_id().is_some());
    let id = chart
        .drawn_big_trades()
        .expect("big trades over the footprint");
    assert_eq!(
        chart.engine.big_trades_options(id).expect("options").size,
        BigTradesSize::Large
    );

    assert!(chart.remove_legend_indicator(LegendItem::BigTrades));
    assert_eq!(chart.order_flow_settings().big_trades, None);
    assert!(
        !chart
            .legend_rows()
            .iter()
            .any(|row| row.item == LegendItem::BigTrades)
    );
    apply_two_print_tape(&mut chart);
    assert_eq!(chart.drawn_big_trades(), None);
    assert!(!chart.has_indicators());
}

#[test]
fn big_trades_settings_restyle_the_drawn_indicator_in_place() {
    let (mut chart, id) = candle_chart_with_big_trades(BigTradesSettings::default());
    assert!(chart.set_legend_item_visible(LegendItem::BigTrades, false));
    assert!(
        !chart
            .order_flow_settings()
            .big_trades
            .expect("added")
            .visible
    );
    assert_eq!(chart.drawn_big_trades(), Some(id));
    assert!(
        !chart
            .engine
            .big_trades_options(id)
            .expect("options")
            .visible
    );
    let mut settings = chart.order_flow_settings();
    settings.big_trades = Some(BigTradesSettings {
        filter: BigTradesFilter::Fixed {
            minimum_volume: 2.0,
        },
        size: BigTradesSize::Large,
        show_volume: false,
        visible: true,
    });
    assert!(chart.set_order_flow_settings(settings).expect("restyle"));
    assert_eq!(chart.drawn_big_trades(), Some(id));
    let options = chart.engine.big_trades_options(id).expect("options");
    assert_eq!(options.size, BigTradesSize::Large);
    assert!(!options.show_volume && options.visible);
    assert_eq!(chart.big_trades_threshold(), Some(2.0));
    assert_eq!(
        chart
            .engine
            .big_trades_snapshot(id)
            .expect("snapshot")
            .bubbles
            .len(),
        2
    );
    let mut invalid = settings;
    invalid.big_trades = Some(BigTradesSettings {
        filter: BigTradesFilter::Fixed {
            minimum_volume: 0.0,
        },
        ..BigTradesSettings::default()
    });
    assert!(chart.set_order_flow_settings(invalid).is_err());
    assert_eq!(chart.order_flow_settings(), settings);

    chart.set_theme(ChartTheme::Light);
    assert_eq!(chart.drawn_big_trades(), Some(id));
    assert_eq!(
        chart
            .engine
            .big_trades_options(id)
            .expect("options")
            .buy_color,
        AerisTheme::light().colors.buy_bubble.css_rgba()
    );
}

#[test]
fn sweeps_must_outsize_nine_in_ten_recent_prints() {
    // Nine one-lot prints and a two-level buy run of 2 + 3 lots.
    let mut trades = (1..=9)
        .map(|ordinal| {
            order_flow_trade(
                ordinal,
                i64::try_from(ordinal).expect("small") * 1_000_000,
                1.0,
            )
        })
        .collect::<Vec<_>>();
    for (offset, volume) in [(0, 2.0), (1, 3.0)] {
        let mut trade = order_flow_trade(
            10 + offset,
            20_000_000 + i64::try_from(offset).expect("small"),
            volume,
        );
        trade.aggressor = aeris_charts_engine::AggressorSide::Buy;
        trade.price = 200.0 + offset.to_f64().expect("small") * 0.25;
        trades.push(trade);
    }
    let sweeps = classify_order_flow_sweeps(&trades);
    assert_eq!(
        sweeps.len(),
        1,
        "a run above the 90th percentile print is a sweep"
    );
    assert!((sweeps[0].total_volume - 5.0).abs() < f64::EPSILON);

    // The same run among larger prints no longer stands out.
    for trade in &mut trades[..9] {
        trade.volume = 8.0;
    }
    assert_eq!(
        classify_order_flow_sweeps(&trades),
        [] as [OrderFlowSweep; 0]
    );
    assert_eq!(classify_order_flow_sweeps(&[]), [] as [OrderFlowSweep; 0]);
}

#[test]
#[ignore = "manual release-mode order-flow burst frame measurement"]
fn measured_order_flow_burst_frame_work_stays_inside_one_frame() {
    const BURST_FRAMES: u64 = 512;
    const TRADES_PER_FRAME: u64 = 128;
    const FRAME_BUDGET_NANOS: u128 = 16_000_000;
    let mut chart = AerisChartView::empty();
    chart.set_chart_type(ChartType::Footprint);
    let mut trades = Vec::with_capacity(
        usize::try_from(BURST_FRAMES * TRADES_PER_FRAME).expect("bounded burst"),
    );
    let burst_started = std::time::Instant::now();
    let mut maximum_frame_nanos = 0_u128;
    for frame in 0..BURST_FRAMES {
        for offset in 0..TRADES_PER_FRAME {
            let ordinal = frame * TRADES_PER_FRAME + offset + 1;
            let mut trade = order_flow_trade(
                ordinal,
                i64::try_from(ordinal * 1_000).expect("bounded timestamp"),
                1.0 + (ordinal % 20).to_f64().expect("small volume"),
            );
            trade.price = 100.0 + (ordinal % 64).to_f64().expect("small price") * 0.25;
            trades.push(trade);
        }
        let frame_started = std::time::Instant::now();
        chart
            .apply_order_flow_trades(
                "instrument:burst",
                7,
                OrderFlowAggregation::TimeMicros(60_000_000),
                0.25,
                &trades,
            )
            .expect("bounded tape applies");
        std::hint::black_box(chart.engine.build_frame());
        maximum_frame_nanos = maximum_frame_nanos.max(frame_started.elapsed().as_nanos());
    }
    let elapsed = burst_started.elapsed();
    eprintln!(
        "Order-flow burst: {} trades across {BURST_FRAMES} frames in {elapsed:?}; max frame={}ns",
        trades.len(),
        maximum_frame_nanos
    );

    assert_eq!(
        trades.len(),
        usize::try_from(BURST_FRAMES * TRADES_PER_FRAME).expect("bounded burst")
    );
    assert!(maximum_frame_nanos <= FRAME_BUDGET_NANOS);
}

fn series_entry(chart: &AerisChartView, id: u32) -> &aeris_charts_engine::SeriesEntry {
    chart
        .engine
        .series_entries()
        .iter()
        .find(|series| series.id == id && !series.removed)
        .expect("live series identity resolves")
}

fn legend_text(row: &LegendRow) -> String {
    row.values
        .iter()
        .map(|value| value.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn assert_aeris_charts_theme(chart: &AerisChartView, theme: ChartTheme) {
    let mut reference = ChartEngine::new(1.0, 1.0, 1.0);
    reference.set_theme(theme);
    let expected = reference.options.get();
    let options = chart.engine.options.get();
    assert_eq!(
        options.layout.background.color,
        expected.layout.background.color
    );
    assert_eq!(options.layout.text_color, expected.layout.text_color);
    assert_eq!(
        options.layout.muted_text_color,
        expected.layout.muted_text_color
    );
    assert_eq!(
        options.left_price_scale.text_color.as_deref(),
        expected.left_price_scale.text_color.as_deref()
    );
    assert_eq!(
        options.right_price_scale.text_color.as_deref(),
        expected.right_price_scale.text_color.as_deref()
    );
    assert_eq!(
        options.layout.font_family,
        aeris_design_system::platform_font_stack()
    );
    assert_eq!(
        options.watermark.font_family,
        aeris_design_system::platform_font_stack()
    );
    assert_eq!(
        options.grid.vert_lines.color,
        expected.grid.vert_lines.color
    );
    assert_eq!(
        options.crosshair.vert_line.color,
        expected.crosshair.vert_line.color
    );
    assert_eq!(
        options.crosshair.vert_line.label_background_color,
        expected.crosshair.vert_line.label_background_color
    );
    assert_eq!(options.layout.bullish_color, expected.layout.bullish_color);
    assert_eq!(options.layout.bearish_color, expected.layout.bearish_color);
}

fn visible_series_point(chart: &AerisChartView, id: u32) -> (f64, f64) {
    chart
        .engine
        .series_data(id)
        .into_iter()
        .rev()
        .find_map(|point| {
            let x = chart.engine.time_to_coordinate(point.time.to_f64()?)?;
            let y = chart.engine.series_price_to_coordinate(id, point.close)?;
            (x >= 0.0 && x <= chart.engine.pane_w && y >= 0.0 && y <= chart.engine.pane_h)
                .then_some((x, y))
        })
        .expect("series has a visible point")
}

#[test]
fn crosshair_alert_action_reaches_the_host_with_aeris_charts_price_context() {
    let mut chart = interactive_chart();
    let price = chart
        .engine
        .series_data(0)
        .last()
        .expect("fixture price")
        .close;
    let y = chart
        .engine
        .series_price_to_coordinate(0, price)
        .expect("price coordinate");
    let center = chart.engine.pane_w / 2.0;
    hover(&mut chart, center, y);
    let action_x = (-100..=4_096)
        .map(f64::from)
        .find(|x| chart.engine.alert_create_hit_at(*x, y))
        .expect("alert action is hit-testable");
    hover(&mut chart, action_x, y);
    assert_eq!(cursor(&chart), ChartCursor::Pointer);
    click(&mut chart, action_x, y);
    let requests = chart.take_alert_create_requests();
    assert_eq!(requests.len(), 1);
    assert!((requests[0].price - price).abs() < f64::EPSILON);
    assert_eq!(
        requests[0].condition,
        aeris_charts_engine::AlertCondition::Crossing
    );
}

#[test]
fn empty_chart_surface_accepts_its_first_real_snapshot() {
    let mut chart = AerisChartView::empty();
    assert!(!chart.has_market_data());
    assert_eq!(chart.queued_replay_update_count(), 0);
    assert_eq!(chart.expected_replay_sequence(), None);
    assert_eq!(chart.replay_bridge_metrics(), ChartBridgeMetrics::default());
    assert!(chart.latest_market_provenance().is_none());

    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
        .expect("embedded replay validates");
    chart.load_replay(&replay).expect("first snapshot installs");
    assert!(chart.has_market_data());
    assert_eq!(series_entry(&chart, 0).title, "AXF");
    assert_eq!(chart.legend_rows()[0].title, "AXF · 1m · XNAS");
    assert_eq!(
        chart.expected_replay_sequence(),
        replay.stream().last_sequence().checked_add(1)
    );
    assert!(chart.latest_market_provenance().is_some());
}

#[test]
fn externally_applied_time_range_is_not_echoed_to_the_link_coordinator() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
        .expect("embedded replay validates");
    let mut source = AerisChartView::with_replay(&replay);
    let mut target = AerisChartView::with_replay(&replay);
    for chart in [&mut source, &mut target] {
        chart
            .engine
            .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
        chart.engine.fit_content();
        chart
            .engine
            .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    }
    let times = source
        .engine
        .data_layer()
        .series_data(0)
        .expect("primary series")
        .0;
    let from = times[3].to_f64().expect("time");
    let to = times[9].to_f64().expect("time");
    source.engine.set_visible_time_range(from, to);
    let event = source
        .take_sync_events()
        .into_iter()
        .find(|event| {
            matches!(
                event.kind,
                aeris_charts_engine::ChartSyncEventKind::VisibleTimeRange { .. }
            )
        })
        .expect("local range event");
    target.apply_external_sync_event(&event.kind);
    assert_eq!(
        target.take_sync_events(),
        [] as [aeris_charts_engine::ChartSyncEvent; 0]
    );
    assert_eq!(target.engine.visible_time_range(), Some((from, to)));
}

#[test]
fn covering_forming_snapshot_recovers_the_aeris_charts_view_without_a_new_candle() {
    let baseline = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .expect("replay");
    let mut chart = AerisChartView::with_replay(&baseline);
    chart.mark_replay_stream_invalid();
    let mut request_id = 0;
    assert!(
        chart
            .try_dispatch_replay_recovery(|request| {
                request_id = request.request_id;
                Ok::<(), ()>(())
            })
            .expect("dispatch")
    );
    let fresh = baseline
        .clone()
        .try_with_publication_generation(baseline.evidence().publication_generation + 1)
        .expect("forming revision");
    assert!(
        chart
            .install_replay_recovery(request_id, &fresh)
            .expect("install")
    );
    assert!(!chart.replay_bridge_metrics().recovery_pending);
    assert!(!chart.replay_bridge_metrics().snapshot_required);
    assert_eq!(
        chart.expected_replay_sequence(),
        fresh.evidence().last_sequence.checked_add(1)
    );
    assert_eq!(chart.engine.series_data(0).len(), 2);
}

#[test]
fn host_chart_type_survives_snapshot_install() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
        .expect("embedded replay validates");
    let mut chart = AerisChartView::empty();
    assert_eq!(chart.chart_type(), ChartType::Candles);
    chart.set_chart_type(ChartType::Line);
    assert_eq!(chart.chart_type(), ChartType::Line);
    assert_eq!(
        series_entry(&chart, 0).kind,
        aeris_charts_engine::SeriesKind::Line
    );

    chart.load_replay(&replay).expect("first snapshot installs");
    assert_eq!(chart.chart_type(), ChartType::Line);
    assert_eq!(
        series_entry(&chart, 0).kind,
        aeris_charts_engine::SeriesKind::Line
    );
    assert_eq!(
        series_entry(&chart, chart.volume_series).kind,
        aeris_charts_engine::SeriesKind::Histogram
    );
}

#[test]
fn line_with_markers_survives_replay_and_appearance_reset_without_leaking_to_other_types() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
        .expect("embedded replay validates");
    let mut chart = AerisChartView::empty();

    assert_eq!(
        ChartType::from_identifier("line_with_markers"),
        Some(ChartType::LineWithMarkers)
    );
    assert!(ChartType::ALL.contains(&ChartType::LineWithMarkers));
    chart.set_chart_type(ChartType::LineWithMarkers);
    assert_eq!(
        series_entry(&chart, 0).kind,
        aeris_charts_engine::SeriesKind::Line
    );
    assert!(series_entry(&chart, 0).point_markers);

    chart.load_replay(&replay).expect("snapshot installs");
    assert_eq!(chart.chart_type(), ChartType::LineWithMarkers);
    assert!(series_entry(&chart, 0).point_markers);
    let bars = chart.engine.series_data(0).len();

    chart.reset_appearance_settings();
    assert!(series_entry(&chart, 0).point_markers);
    chart.set_chart_type(ChartType::Line);
    assert!(!series_entry(&chart, 0).point_markers);
    chart.set_chart_type(ChartType::LineWithMarkers);
    assert!(series_entry(&chart, 0).point_markers);
    chart.set_chart_type(ChartType::Candles);
    assert!(!series_entry(&chart, 0).point_markers);
    assert_eq!(chart.engine.series_data(0).len(), bars);
}

#[test]
fn session_plan_levels_are_bounded_transient_lines_restored_with_the_price_series() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
        .expect("embedded replay validates");
    let mut chart = AerisChartView::with_replay(&replay);
    let levels = vec![
        (10_250.25, "opening range".to_string()),
        (10_200.0, "invalidation".to_string()),
    ];

    chart
        .replace_session_plan_levels(levels.clone())
        .expect("valid plan levels install");
    assert_eq!(chart.session_plan_levels(), levels);
    assert_eq!(series_entry(&chart, 0).price_lines.len(), 2);
    assert!(
        series_entry(&chart, 0)
            .price_lines
            .iter()
            .all(|line| line.title.starts_with("PLAN · "))
    );

    chart.set_chart_type(ChartType::Line);
    assert_eq!(chart.session_plan_levels(), levels);
    assert_eq!(series_entry(&chart, 0).price_lines.len(), 2);
    assert_eq!(chart.engine.drawings_json(), "[]");

    chart
        .replace_session_plan_levels(Vec::new())
        .expect("empty projection clears plan lines");
    assert_eq!(chart.session_plan_levels(), [] as [(f64, String); 0]);
    assert!(series_entry(&chart, 0).price_lines.is_empty());
}

#[test]
fn invalid_session_plan_level_projection_preserves_the_current_lines() {
    let mut chart = AerisChartView::new();
    let levels = vec![(100.0, "planned entry".to_string())];
    chart
        .replace_session_plan_levels(levels.clone())
        .expect("valid plan level installs");

    assert!(
        chart
            .replace_session_plan_levels(vec![(f64::NAN, "bad".to_string())])
            .is_err()
    );
    assert_eq!(chart.session_plan_levels(), levels);
    assert_eq!(series_entry(&chart, 0).price_lines.len(), 1);
}

#[test]
fn selected_price_precision_survives_snapshot_install_and_restores_a_replacement_chart() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
        .expect("embedded replay validates");
    let mut chart = AerisChartView::empty();
    assert!(chart.apply_price_axis_menu_action(
        0,
        false,
        PriceAxisMenuAction::SetPrecision(Some(0))
    ));

    chart
        .load_replay(&replay)
        .expect("the first snapshot keeps chart presentation state");
    assert_eq!(chart.selected_price_precision(), Some(0));
    assert_eq!(series_entry(&chart, 0).price_format.precision, 0);

    let retained_precision = chart.selected_price_precision();
    let mut replacement = AerisChartView::with_replay(&replay);
    assert!(replacement.apply_price_axis_menu_action(
        0,
        false,
        PriceAxisMenuAction::SetPrecision(retained_precision)
    ));
    assert_eq!(replacement.selected_price_precision(), Some(0));
    assert_eq!(series_entry(&replacement, 0).price_format.precision, 0);
}

#[test]
fn brushable_area_composes_over_area_series_and_restores_ohlc() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
        .expect("embedded replay validates");
    let mut chart = AerisChartView::with_replay(&replay);
    let original = chart.engine.series_data(0);
    assert_ne!(original, [] as [aeris_charts_engine::SeriesDataPoint; 0]);
    let original_high = original[0].high;

    chart.set_chart_type(ChartType::BrushableArea);
    assert_eq!(chart.chart_type(), ChartType::BrushableArea);
    assert_eq!(
        series_entry(&chart, 0).kind,
        aeris_charts_engine::SeriesKind::Area
    );
    assert_eq!(chart.engine.feature_series_kind(0), None);
    let aeris_line_width = serde_json::from_str::<serde_json::Value>(
        &chart
            .engine
            .series_options_json(0)
            .expect("brushable options"),
    )
    .expect("brushable options are JSON")["line_width"]
        .as_f64()
        .expect("Aeris Charts supplies a brushable line width");
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    assert!(chart.engine.is_brushable_area(0));
    let start = chart.engine.time_scale.index_to_coordinate(2);
    let end = chart.engine.time_scale.index_to_coordinate(8);
    // A plain drag still pans; Shift+drag compares a range instead.
    let scroll = chart.engine.scroll_position();
    drag(&mut chart, (start, 200.0), (end, 200.0));
    assert_ne!(chart.engine.scroll_position().to_bits(), scroll.to_bits());
    assert_eq!(chart.engine.brushable_area_range(0), None);
    let start = chart.engine.time_scale.index_to_coordinate(2);
    let end = chart.engine.time_scale.index_to_coordinate(8);
    let scroll = chart.engine.scroll_position();
    drag_with(&mut chart, (start, 200.0), (end, 200.0), SHIFT);
    assert_eq!(chart.engine.scroll_position().to_bits(), scroll.to_bits());
    let selected = chart
        .engine
        .brushable_area_range(0)
        .expect("shift-drag installs a comparison range");
    assert!(selected.from < selected.to);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &chart.engine.series_options_json(0).expect("area options")
        )
        .expect("area options are JSON")["line_width"]
            .as_f64(),
        Some(aeris_line_width)
    );

    chart.set_chart_type(ChartType::Candles);
    assert_eq!(
        series_entry(&chart, 0).kind,
        aeris_charts_engine::SeriesKind::Candlestick
    );
    assert_eq!(
        chart.engine.series_data(0)[0].high.to_bits(),
        original_high.to_bits()
    );
}

#[test]
fn calendar_month_snapshot_reaches_aeris_charts_with_variable_month_spacing() {
    let baseline = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 3 })
        .expect("fixture snapshot");
    let month_starts = [1_704_067_200_i64, 1_706_745_600, 1_709_251_200];
    let bars = baseline
        .bars()
        .iter()
        .zip(month_starts)
        .map(|(item, timestamp)| {
            let mut bar = *item.value();
            bar.exchange_timestamp_seconds = timestamp;
            bar.exchange_timestamp_unix_nanos = timestamp.saturating_mul(1_000_000_000);
            bar
        })
        .collect();
    let mut definition = baseline.bar_definition().clone();
    definition.definition_id = "rithmic:fixture:calendar-months:1".to_string();
    definition.interval_seconds = 0;
    definition.trades_per_bar = None;
    definition.calendar_months = Some(1);
    let replay = ReplaySnapshot::try_new(
        baseline.instrument().clone(),
        baseline.provenance(),
        definition,
        bars,
    )
    .expect("calendar snapshot validates");

    let chart = AerisChartView::with_replay(&replay);
    assert_eq!(series_entry(&chart, 0).title, "AXF");
    assert_eq!(chart.legend_rows()[0].title, "AXF · 1M · XNAS");
    let installed_times = chart
        .engine
        .series_data(0)
        .into_iter()
        .map(|point| point.time)
        .collect::<Vec<_>>();

    assert!(chart.has_market_data());
    assert_eq!(installed_times, month_starts);
    assert_ne!(
        month_starts[1] - month_starts[0],
        month_starts[2] - month_starts[1]
    );
}

#[test]
fn series_updates_dirty_layout_without_discarding_viewport_dimensions() {
    let mut chart = AerisChartView::new();
    chart.built_for = (1280.0, 720.0, 1.25);
    chart.layout_dirty = false;

    chart.invalidate_series_layout();

    assert_eq!(chart.built_for, (1280.0, 720.0, 1.25));
    assert!(chart.layout_dirty);
    assert_eq!(chart.frame.panes, [] as [aeris_charts_engine::FramePane; 0]);
    assert_eq!(chart.axis_prims, [] as [Prim; 0]);
}

#[test]
fn occluded_mouse_up_inside_chart_is_not_geometrically_outside() {
    let mut chart = interactive_chart();
    chart.viewport_bounds = Bounds::new(
        gpui::point(px(100.0), px(80.0)),
        gpui::size(px(640.0), px(360.0)),
    );

    assert!(!chart.release_outside_chart(gpui::point(px(420.0), px(240.0))));
    assert!(!chart.release_outside_chart(gpui::point(px(100.0), px(80.0))));
    assert!(!chart.release_outside_chart(gpui::point(px(739.0), px(439.0))));
    assert!(chart.release_outside_chart(gpui::point(px(99.0), px(240.0))));
    assert!(chart.release_outside_chart(gpui::point(px(420.0), px(441.0))));
}

#[test]
fn chart_applies_live_tail_replace_and_append_in_one_frame_boundary() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .expect("embedded replay validates");
    let mut chart = AerisChartView::with_replay(&replay);
    let initial_expected = chart
        .expected_replay_sequence()
        .expect("snapshot establishes sequence");
    let source = replay.bars().last().cloned().expect("tail exists");
    let mut replacement_bar = *source.value();
    replacement_bar.close = replacement_bar.close.saturating_add(1);
    let mut replacement_provenance = source.provenance().clone();
    replacement_provenance.event_id = "tail-replacement".to_string();
    let replacement = ReplayTailUpdate::try_new(
        Provenanced::new(replacement_bar, replacement_provenance),
        replay.evidence().publication_generation + 1,
        true,
        ReplayTailOperation::Revise,
    )
    .expect("replacement validates");
    chart
        .try_queue_replay_update(ReplayStreamUpdate::Tail(replacement))
        .expect("replacement queues");
    chart.layout_dirty = false;
    assert_eq!(chart.apply_pending_data(), SeriesMutation::TailReplace);
    assert!(!chart.layout_dirty);
    assert_eq!(chart.expected_replay_sequence(), Some(initial_expected));
    assert_eq!(
        chart
            .latest_market_provenance()
            .map(|provenance| provenance.event_id.as_str()),
        Some("tail-replacement")
    );

    let appended = EmbeddedReplaySource
        .load_delta(initial_expected.saturating_sub(1))
        .expect("fixture delta loads")
        .expect("fixture delta exists");
    let appended = ReplayTailUpdate::try_new(
        appended.item().clone(),
        replay.evidence().publication_generation + 2,
        true,
        ReplayTailOperation::Append,
    )
    .expect("append validates");
    chart
        .try_queue_replay_update(ReplayStreamUpdate::Tail(appended))
        .expect("append queues");
    assert_eq!(chart.apply_pending_data(), SeriesMutation::Append);
    assert!(chart.layout_dirty);
    assert_eq!(
        chart.expected_replay_sequence(),
        initial_expected.checked_add(1)
    );
}

#[test]
fn live_bars_stay_whitespace_under_a_footprint_and_return_on_candles() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .expect("embedded replay validates");
    let mut chart = AerisChartView::with_replay(&replay);
    chart.set_chart_type(ChartType::Footprint);
    let expected = chart
        .expected_replay_sequence()
        .expect("snapshot establishes sequence");
    let appended = EmbeddedReplaySource
        .load_delta(expected.saturating_sub(1))
        .expect("fixture delta loads")
        .expect("fixture delta exists");
    let appended = ReplayTailUpdate::try_new(
        appended.item().clone(),
        replay.evidence().publication_generation + 1,
        true,
        ReplayTailOperation::Append,
    )
    .expect("append validates");
    chart
        .try_queue_replay_update(ReplayStreamUpdate::Tail(appended))
        .expect("append queues");
    assert_eq!(chart.apply_pending_data(), SeriesMutation::Append);
    let (times, columns) = chart
        .engine
        .data_layer()
        .series_data(0)
        .expect("price data");
    assert_eq!(times.len(), 3);
    assert!(
        columns
            .iter()
            .all(|column| column.iter().all(|value| value.is_nan())),
        "a live bar must not draw a candle under the footprint"
    );

    chart.set_chart_type(ChartType::Candles);
    let (_, columns) = chart
        .engine
        .data_layer()
        .series_data(0)
        .expect("price data");
    assert!(
        columns
            .iter()
            .all(|column| column.iter().all(|value| value.is_finite()))
    );
}

#[cfg(feature = "diagnostics")]
#[test]
fn snapshot_installation_records_queued_and_direct_foreground_durations() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 64 })
        .expect("fixture validates");
    let replacement = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 65 })
        .expect("replacement fixture validates")
        .try_with_publication_generation(replay.evidence().publication_generation.saturating_add(1))
        .expect("replacement generation validates");
    let mut chart = AerisChartView::with_replay(&replay);

    assert!(
        chart
            .try_queue_replay_update(ReplayStreamUpdate::Snapshot(replacement))
            .is_ok()
    );
    chart.apply_pending_data();

    assert!(chart.take_snapshot_installation_nanos().is_some());
    assert_eq!(chart.take_snapshot_installation_nanos(), None);

    let direct_replacement = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 66 })
        .expect("direct replacement fixture validates")
        .try_with_publication_generation(replay.evidence().publication_generation.saturating_add(2))
        .expect("direct replacement generation validates");
    chart
        .load_replay(&direct_replacement)
        .expect("direct replacement installs");
    assert!(chart.take_snapshot_installation_nanos().is_some());
    assert_eq!(chart.take_snapshot_installation_nanos(), None);
}

#[test]
fn visible_time_range_roundtrips_through_persistent_unix_nanos() {
    let mut chart = interactive_chart();
    let (start, end) = chart
        .visible_time_range_unix_nanos()
        .expect("interactive chart has a viewport");
    let restored = (start + 60_000_000_000, end - 60_000_000_000);
    assert!(chart.set_visible_time_range_unix_nanos(restored.0, restored.1));
    assert_eq!(chart.visible_time_range_unix_nanos(), Some(restored));
}

#[test]
fn visible_time_range_extrapolates_past_loaded_history_for_backfill() {
    let mut chart = interactive_chart();
    let first = chart
        .engine
        .series_data(0)
        .first()
        .and_then(|point| point.time.to_i64())
        .expect("fixture first timestamp");
    chart.engine.set_visible_logical_range(-20.0, 20.0);
    let first_seconds = first.to_f64().expect("fixture timestamp converts");
    let clamped_start = chart
        .engine
        .visible_time_range()
        .expect("clamped Aeris Charts viewport")
        .0;
    assert!((clamped_start - first_seconds).abs() < f64::EPSILON);
    let (start, _) = chart
        .visible_time_range_unix_nanos()
        .expect("host viewport extrapolates");
    assert!(start < first.saturating_mul(1_000_000_000));
}

#[test]
fn chart_series_retention_is_owned_by_market_runtime() {
    let chart = interactive_chart();
    assert_eq!(chart.engine.series_max_points(0), None);
    assert_eq!(chart.engine.series_max_points(chart.volume_series), None);
}

#[test]
fn aeris_charts_theme_owns_chart_cosmetics_and_series_defaults() {
    let chart = AerisChartView::empty();
    let series = &chart.engine.series[0];
    assert_aeris_charts_theme(&chart, ChartTheme::Dark);
    assert!(series.line_color.is_none());
    assert!(series.up_color.is_none());
    assert!(series.down_color.is_none());
    assert!(series.wick_up_color.is_none());
    assert!(series.wick_down_color.is_none());
    assert!(series.border_up_color.is_none());
    assert!(series.border_down_color.is_none());
}

#[test]
fn chart_legend_text_colors_project_platform_chrome_and_aeris_charts_market_colors() {
    for (theme, platform) in [
        (ChartTheme::Light, AerisTheme::light()),
        (ChartTheme::Dark, AerisTheme::dark()),
    ] {
        let palette = legend_palette(theme, "#089981", "#f7525f");
        let colors = platform.colors;
        assert_eq!(palette.text, gpui_theme_color(colors.text_primary));
        assert_eq!(palette.muted, gpui_theme_color(colors.text_secondary));
        assert_eq!(palette.hover, gpui_theme_color(colors.hover_bg));
        assert_eq!(palette.danger, gpui_theme_color(colors.danger));
        assert_eq!(
            palette.bullish,
            rgba(Color::parse_css("#089981").expect("valid color").0)
        );
        assert_eq!(
            palette.bearish,
            rgba(Color::parse_css("#f7525f").expect("valid color").0)
        );
    }
}

#[test]
fn last_value_cluster_uses_instrument_title_and_host_clock() {
    let mut chart = interactive_chart();
    let series = series_entry(&chart, 0);
    assert_eq!(series.title, "AXF");
    assert!(series.title_visible);
    assert!(series.countdown_visible);
    assert!(series.last_value_visible);

    let last_time = chart
        .engine
        .series_data(0)
        .last()
        .expect("replay has bars")
        .time
        .to_f64()
        .expect("bar time fits f64");
    let measure =
        |text: &str, _bold: bool| f64::from(u32::try_from(text.len()).unwrap_or(u32::MAX)) * 7.0;
    let _ = chart.engine.build_axis_frame(80.0, measure, measure);
    assert!(!chart.engine.frame_requires_axis());
    chart.pin_host_clock();
    assert!(chart.engine.frame_requires_axis());
    chart.engine.set_now_seconds(last_time + 10.0);
    let texts: Vec<String> = chart
        .engine
        .build_axis_frame(80.0, measure, measure)
        .labels
        .into_iter()
        .map(|label| label.text)
        .collect();
    assert!(
        texts.iter().any(|text| text == "AXF"),
        "title chip missing from last-value cluster: {texts:?}"
    );
    assert!(
        texts.iter().any(|text| text == "00:50"),
        "countdown missing from last-value cluster: {texts:?}"
    );
}

#[test]
fn the_symbol_legend_reports_a_load_only_when_it_turns_over() {
    let mut chart = AerisChartView::empty();
    assert!(!chart.asset_loading.is_present());
    assert!(chart.set_asset_loading(true));
    assert!(chart.asset_loading.is_present());
    // The host mirrors its lifecycle on every poll, so a repeat must not ask
    // for a repaint the surface does not need.
    assert!(!chart.set_asset_loading(true));
    assert!(chart.set_asset_loading(false));
    assert!(!chart.asset_loading.is_present());
}

#[test]
fn indicator_clusters_never_carry_the_candle_close_countdown() {
    let mut chart = interactive_chart();
    for indicator in ChartIndicator::ALL {
        let ids = chart
            .add_indicator(indicator)
            .expect("catalog indicator is available");
        for id in ids {
            assert!(
                !series_entry(&chart, id).countdown_visible,
                "{indicator:?} output {id} counts down to a bar close it does not own"
            );
        }
    }
    assert!(series_entry(&chart, 0).countdown_visible);
}

#[test]
fn price_axis_menu_controls_aeris_charts_series_chrome_and_scale() {
    let mut chart = interactive_chart();
    let sma = chart
        .add_indicator(ChartIndicator::Sma)
        .expect("sma is available");
    let volume_precision = series_entry(&chart, chart.volume_series)
        .price_format
        .precision;
    let state = chart
        .price_axis_menu_state(0, false)
        .expect("right price scale");
    assert!(state.enabled(PriceAxisMenuState::PRICE_LINE));
    assert!(state.enabled(PriceAxisMenuState::LAST_VALUE));
    assert!(state.enabled(PriceAxisMenuState::TITLE));
    assert!(state.enabled(PriceAxisMenuState::COUNTDOWN));
    assert!(state.enabled(PriceAxisMenuState::AUTO_SCALE));
    assert!(!state.enabled(PriceAxisMenuState::INVERT_SCALE));
    assert!(!state.left);
    assert_eq!(state.mode, 0);
    assert_eq!(state.precision, None);
    assert!(state.enabled(PriceAxisMenuState::INDICATOR_NAMES));
    assert!(state.enabled(PriceAxisMenuState::INDICATOR_VALUES));
    assert!(state.enabled(PriceAxisMenuState::INDICATOR_PRICE_LINES));
    assert!(state.enabled(PriceAxisMenuState::ALIGN_LABELS));
    assert!(!state.enabled(PriceAxisMenuState::BID_ASK));

    assert!(chart.apply_price_axis_menu_action(0, false, PriceAxisMenuAction::ToggleTitle));
    assert!(
        !chart
            .price_axis_menu_state(0, false)
            .unwrap()
            .enabled(PriceAxisMenuState::TITLE)
    );
    assert!(chart.apply_price_axis_menu_action(
        0,
        false,
        PriceAxisMenuAction::ToggleIndicatorNameLabels
    ));
    assert!(!series_entry(&chart, sma[0]).title_visible);
    assert!(series_entry(&chart, sma[0]).last_value_visible);
    assert!(chart.apply_price_axis_menu_action(0, false, PriceAxisMenuAction::ToggleBidAsk));
    assert!(series_entry(&chart, 0).bid_ask_visible);
    assert!(chart.apply_price_axis_menu_action(0, false, PriceAxisMenuAction::ToggleAlignLabels));
    assert!(
        !chart
            .price_axis_menu_state(0, false)
            .unwrap()
            .enabled(PriceAxisMenuState::ALIGN_LABELS)
    );
    assert!(chart.apply_price_axis_menu_action(0, false, PriceAxisMenuAction::SetMode(1)));
    assert!(chart.apply_price_axis_menu_action(0, false, PriceAxisMenuAction::SetLeft(true)));
    let moved = chart
        .price_axis_menu_state(0, true)
        .expect("left price scale");
    assert!(moved.left);
    assert_eq!(moved.mode, 1);
    assert_eq!(
        series_entry(&chart, 0).price_scale_target,
        PriceScaleTarget::Left
    );
    assert!(chart.apply_price_axis_menu_action(
        0,
        true,
        PriceAxisMenuAction::SetPrecision(Some(0))
    ));
    assert_eq!(
        chart.price_axis_menu_state(0, true).unwrap().precision,
        Some(0)
    );
    assert_eq!(series_entry(&chart, 0).price_format.precision, 0);
    assert_eq!(series_entry(&chart, sma[0]).price_format.precision, 0);
    assert_eq!(
        series_entry(&chart, chart.volume_series)
            .price_format
            .precision,
        volume_precision,
        "the main price-axis choice must not overwrite the overlay volume format"
    );

    let ribbon = chart
        .add_indicator(ChartIndicator::EmaRibbon)
        .expect("EMA ribbon is available after selecting an explicit precision");
    assert!(
        ribbon
            .iter()
            .all(|id| series_entry(&chart, *id).price_format.precision == 0),
        "new price indicators must inherit the selected main-axis precision"
    );
}

#[test]
fn indicator_label_preference_applies_to_every_indicator_and_later_additions() {
    let mut chart = interactive_chart();
    assert!(chart.apply_price_axis_menu_action(
        0,
        false,
        PriceAxisMenuAction::ToggleIndicatorNameLabels
    ));
    assert!(chart.apply_price_axis_menu_action(
        0,
        false,
        PriceAxisMenuAction::ToggleIndicatorValueLabels
    ));
    assert!(!chart.indicator_name_labels_visible());
    assert!(!chart.indicator_value_labels_visible());
    let state = chart
        .price_axis_menu_state(0, false)
        .expect("right price scale");
    assert!(!state.enabled(PriceAxisMenuState::INDICATOR_NAMES));
    assert!(!state.enabled(PriceAxisMenuState::INDICATOR_VALUES));

    let sma = chart
        .add_indicator(ChartIndicator::Sma)
        .expect("sma is available");
    let rsi = chart
        .add_indicator(ChartIndicator::Rsi)
        .expect("rsi is available");
    assert_ne!(series_entry(&chart, rsi[0]).pane_index, 0);
    for id in sma.iter().chain(rsi.iter()).copied() {
        let series = series_entry(&chart, id);
        assert!(!series.last_value_visible, "labels stayed on {id}");
        assert!(!series.title_visible, "title stayed on {id}");
        assert!(
            series.price_line_visible,
            "price line followed labels on {id}"
        );
    }

    assert!(chart.apply_price_axis_menu_action(
        0,
        false,
        PriceAxisMenuAction::ToggleIndicatorNameLabels
    ));
    for id in sma.iter().chain(rsi.iter()).copied() {
        let series = series_entry(&chart, id);
        assert!(!series.last_value_visible);
        assert!(series.title_visible);
    }
    assert!(chart.apply_price_axis_menu_action(
        0,
        false,
        PriceAxisMenuAction::ToggleIndicatorValueLabels
    ));
    for id in sma.iter().chain(rsi.iter()).copied() {
        let series = series_entry(&chart, id);
        assert!(series.last_value_visible);
        assert!(series.title_visible);
    }
}

#[test]
fn volume_profile_indicator_round_trips_through_state_legend_and_removal() {
    let mut chart = interactive_chart();
    chart
        .add_indicator(ChartIndicator::VolumeProfile)
        .expect("volume profile binds to price and volume");
    assert!(
        chart.add_indicator(ChartIndicator::VolumeProfile).is_err(),
        "one visible-range profile per chart"
    );
    assert!(chart.indicator_states().contains(&ChartIndicatorState {
        indicator: ChartIndicator::VolumeProfile,
        visible: true,
    }));
    assert!(
        chart
            .legend_rows()
            .iter()
            .any(|row| row.item == LegendItem::VolumeProfile && row.pane == 0)
    );

    assert!(chart.set_legend_item_visible(LegendItem::VolumeProfile, false));
    assert!(!chart.set_legend_item_visible(LegendItem::VolumeProfile, false));
    assert!(chart.indicator_states().contains(&ChartIndicatorState {
        indicator: ChartIndicator::VolumeProfile,
        visible: false,
    }));

    let mut restored = interactive_chart();
    restored
        .restore_indicator_states(&chart.indicator_states())
        .expect("persisted profile restores");
    assert_eq!(restored.indicator_states(), chart.indicator_states());

    assert!(chart.remove_legend_indicator(LegendItem::VolumeProfile));
    assert!(
        !chart
            .indicator_states()
            .iter()
            .any(|state| state.indicator == ChartIndicator::VolumeProfile)
    );
    assert_eq!(
        ChartIndicator::from_identifier(ChartIndicator::VolumeProfile.identifier()),
        Some(ChartIndicator::VolumeProfile)
    );
}

#[test]
fn indicator_price_line_preference_applies_to_every_plot() {
    let mut chart = interactive_chart();
    let ema_fast = chart
        .add_indicator(ChartIndicator::Ema)
        .expect("ema is available");
    let ema_slow = chart
        .add_indicator(ChartIndicator::Ema)
        .expect("second ema is available");
    let macd = chart
        .add_indicator(ChartIndicator::Macd)
        .expect("macd is available");
    assert_eq!(ema_fast.len(), 1);
    assert_eq!(ema_slow.len(), 1);
    assert_eq!(macd.len(), 3);
    let macd_pane = series_entry(&chart, macd[0]).pane_index;
    assert_ne!(macd_pane, 0);
    let indicator_ids: Vec<u32> = ema_fast
        .iter()
        .chain(ema_slow.iter())
        .chain(macd.iter())
        .copied()
        .collect();
    for id in &indicator_ids {
        assert!(
            series_entry(&chart, *id).price_line_visible,
            "price line missing on {id}"
        );
    }

    assert!(chart.apply_price_axis_menu_action(0, false, PriceAxisMenuAction::TogglePriceLine));
    assert!(!series_entry(&chart, 0).price_line_visible);
    for id in &indicator_ids {
        assert!(
            series_entry(&chart, *id).price_line_visible,
            "overlay or oscillator plot {id} followed the symbol price line"
        );
    }

    assert!(chart.apply_price_axis_menu_action(
        macd_pane,
        false,
        PriceAxisMenuAction::TogglePriceLine
    ));
    assert!(series_entry(&chart, 0).price_line_visible);
    for id in &macd {
        assert!(
            series_entry(&chart, *id).price_line_visible,
            "macd plot {id} followed the symbol price line"
        );
    }

    assert!(chart.apply_price_axis_menu_action(
        0,
        false,
        PriceAxisMenuAction::ToggleIndicatorPriceLines
    ));
    assert!(!chart.indicator_price_lines_visible());
    assert!(
        !chart
            .price_axis_menu_state(0, false)
            .unwrap()
            .enabled(PriceAxisMenuState::INDICATOR_PRICE_LINES)
    );
    assert!(series_entry(&chart, 0).price_line_visible);
    for id in &indicator_ids {
        assert!(
            !series_entry(&chart, *id).price_line_visible,
            "indicator plot {id} kept its price line"
        );
    }

    let sma = chart
        .add_indicator(ChartIndicator::Sma)
        .expect("sma is available");
    assert!(!series_entry(&chart, sma[0]).price_line_visible);

    assert!(chart.apply_price_axis_menu_action(
        0,
        false,
        PriceAxisMenuAction::ToggleIndicatorPriceLines
    ));
    for id in indicator_ids.iter().chain(sma.iter()).copied() {
        assert!(
            series_entry(&chart, id).price_line_visible,
            "indicator plot {id} stayed hidden"
        );
    }
}

#[test]
fn aeris_charts_theme_switch_is_atomic_for_data_viewport_drawings_and_indicators() {
    let mut chart = interactive_chart();
    let volume = chart
        .add_indicator(ChartIndicator::Volume)
        .expect("volume is available");
    let vwap = chart
        .add_indicator(ChartIndicator::Vwap)
        .expect("vwap is available");
    assert!(
        chart
            .engine
            .drawing_create_begin(DrawingKind::HorizontalLine, None)
    );
    assert_eq!(
        chart
            .engine
            .drawing_create_click(300.0, 200.0, DrawingModifiers::default()),
        1
    );
    chart.engine.time_scale.zoom(300.0, 0.5);
    chart.engine.scroll_to_position(-12.0);

    let price_data = chart
        .engine
        .data_layer()
        .series_data(0)
        .map(|(times, columns)| (times.to_vec(), columns.map(<[f64]>::to_vec)))
        .expect("price data");
    let volume_data = chart
        .engine
        .data_layer()
        .series_data(chart.volume_series)
        .map(|(times, columns)| (times.to_vec(), columns.map(<[f64]>::to_vec)))
        .expect("volume data");
    let spacing = chart.engine.bar_spacing();
    let offset = chart.engine.right_offset();
    let drawings = chart.engine.drawings_json();
    let pane_count = chart.engine.panes.len();
    let series_count = chart.engine.series.len();

    chart.set_theme(ChartTheme::Light);
    assert_aeris_charts_theme(&chart, ChartTheme::Light);
    chart.set_theme(ChartTheme::Dark);
    assert_aeris_charts_theme(&chart, ChartTheme::Dark);
    assert_eq!(
        chart
            .engine
            .data_layer()
            .series_data(0)
            .map(|(times, columns)| { (times.to_vec(), columns.map(<[f64]>::to_vec)) }),
        Some(price_data)
    );
    assert_eq!(
        chart
            .engine
            .data_layer()
            .series_data(chart.volume_series)
            .map(|(times, columns)| { (times.to_vec(), columns.map(<[f64]>::to_vec)) }),
        Some(volume_data)
    );
    assert_eq!(chart.engine.bar_spacing().to_bits(), spacing.to_bits());
    assert_eq!(chart.engine.right_offset().to_bits(), offset.to_bits());
    assert_eq!(chart.engine.drawings_json(), drawings);
    assert_eq!(chart.engine.panes.len(), pane_count);
    assert_eq!(chart.engine.series.len(), series_count);
    assert_eq!(volume, vec![chart.volume_series]);
    assert!(series_entry(&chart, chart.volume_series).visible);
    assert_eq!(
        chart
            .engine
            .indicator_info(vwap[0])
            .map(|info| info.kind.into_owned()),
        Some("vwap".to_owned())
    );
}

#[test]
fn wheel_zoom_and_horizontal_scroll_mutate_aeris_charts_without_refitting() {
    let mut chart = interactive_chart();
    let wheel = |delta_x, delta_y| WheelSample {
        x: 400.0,
        y: 200.0,
        delta_x,
        delta_y,
        ..WheelSample::default()
    };
    let spacing = chart.engine.bar_spacing();
    assert!(chart.engine.input_wheel(wheel(0.0, 1.0)));
    assert!((chart.engine.bar_spacing() - spacing).abs() > f64::EPSILON);
    let offset = chart.engine.right_offset();
    assert!(chart.engine.input_wheel(wheel(1.0, 0.0)));
    assert!((chart.engine.right_offset() - offset).abs() > f64::EPSILON);
    assert!(chart.fitted);

    // The Terminal keeps professional price-axis wheel zoom.
    let axis_x = chart.engine.pane_w + 10.0;
    let range = chart
        .engine
        .price_scale_visible_range_for(0, PriceScaleTarget::Right);
    assert!(chart.engine.input_wheel(WheelSample {
        x: axis_x,
        ..wheel(0.0, 1.0)
    }));
    assert_ne!(
        chart
            .engine
            .price_scale_visible_range_for(0, PriceScaleTarget::Right),
        range
    );
}

#[test]
fn mouse_pan_and_crosshair_have_bounded_lifecycle() {
    let mut chart = interactive_chart();
    press(&mut chart, 300.0, 200.0);
    assert_eq!(chart.engine.crosshair, Some((300.0, 200.0)));
    let offset = chart.engine.right_offset();
    move_to(&mut chart, pointer(320.0, 200.0), true);
    move_to(&mut chart, pointer(340.0, 200.0), true);
    assert!((chart.engine.right_offset() - offset).abs() > f64::EPSILON);
    assert_eq!(cursor(&chart), ChartCursor::Grabbing);
    release(&mut chart, 340.0, 200.0);
    assert_ne!(cursor(&chart), ChartCursor::Grabbing);
    hover(&mut chart, -1.0, 200.0);
    assert!(chart.engine.crosshair.is_none());
}

#[test]
fn pane_copy_price_uses_aeris_chart_context() {
    let mut chart = interactive_chart();
    let (x, y) = visible_series_point(&chart, 0);
    let context = chart
        .engine
        .chart_context_at(x, y)
        .expect("pane click has Aeris Charts context");
    let expected = chart
        .engine
        .series_format_price(0, context.price)
        .expect("asset price format");
    chart.engine.input_context_menu(x, y);
    chart.process_input_events();
    let request = chart
        .take_context_menu_request()
        .expect("a pane right-click requests the menu");
    assert_eq!(request.kind, ChartContextKind::Pane);
    assert_eq!(request.copy_price.as_deref(), Some(expected.as_str()));
}

#[test]
fn price_axis_context_menu_does_not_copy_price() {
    let mut chart = interactive_chart();
    let axis_x = chart.engine.pane_w + 1.0;
    chart.engine.input_context_menu(axis_x, 200.0);
    chart.process_input_events();
    let request = chart
        .take_context_menu_request()
        .expect("an axis right-click requests the menu");
    assert!(matches!(
        request.kind,
        ChartContextKind::PriceAxis {
            pane: 0,
            left: false
        }
    ));
    assert!(request.copy_price.is_none());
}

#[test]
fn empty_chart_has_no_copy_price() {
    let mut chart = AerisChartView::empty();
    chart.engine.input_context_menu(100.0, 200.0);
    chart.process_input_events();
    assert!(
        chart
            .take_context_menu_request()
            .is_none_or(|request| request.copy_price.is_none())
    );
}

#[test]
fn axes_drag_and_double_click_reset_through_aeris_charts() {
    let mut chart = interactive_chart();
    let time_y = chart.engine.pane_h + 10.0;
    hover(&mut chart, 300.0, time_y);
    assert_eq!(cursor(&chart), ChartCursor::ResizeHorizontal);
    let spacing = chart.engine.bar_spacing();
    drag(&mut chart, (300.0, time_y), (340.0, time_y));
    assert_ne!(chart.engine.bar_spacing().to_bits(), spacing.to_bits());

    let right_axis_x = chart.engine.pane_w + 1.0;
    assert_eq!(
        chart
            .engine
            .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
        Some(true)
    );
    drag(&mut chart, (right_axis_x, 200.0), (right_axis_x, 240.0));
    assert_eq!(
        chart
            .engine
            .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
        Some(false)
    );

    let locked_range = chart
        .engine
        .price_scale_visible_range_for(0, PriceScaleTarget::Right);
    drag(&mut chart, (300.0, 200.0), (300.0, 260.0));
    assert_ne!(
        chart
            .engine
            .price_scale_visible_range_for(0, PriceScaleTarget::Right),
        locked_range
    );
    assert_eq!(
        chart
            .engine
            .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
        Some(false)
    );

    chart.engine.time_scale.start_scroll(0.0);
    chart.engine.time_scale.scroll_to(80.0);
    chart.engine.time_scale.end_scroll();
    let offset = chart.engine.right_offset();
    assert!(offset.abs() > f64::EPSILON);
    double_click(&mut chart, 300.0, time_y);
    assert!(chart.engine.right_offset().abs() < offset.abs());

    // A price-axis double-click restores that scale's automatic range.
    double_click(&mut chart, right_axis_x, 200.0);
    assert_eq!(
        chart
            .engine
            .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
        Some(true)
    );
}

#[test]
fn reset_view_restores_native_time_defaults_and_automatic_price_scaling() {
    let mut chart = interactive_chart();
    chart.engine.time_scale.start_scroll(0.0);
    chart.engine.time_scale.scroll_to(80.0);
    chart.engine.time_scale.end_scroll();
    chart
        .engine
        .set_price_scale_auto_scale_for(0, PriceScaleTarget::Right, false);
    assert!(chart.engine.right_offset().abs() > f64::EPSILON);
    assert_eq!(
        chart
            .engine
            .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
        Some(false)
    );

    chart.reset_view();

    let reset_margin = chart.engine.pane_w * 0.10 / chart.engine.bar_spacing();
    assert!((chart.engine.right_offset() - reset_margin).abs() < f64::EPSILON);
    assert_eq!(
        chart
            .engine
            .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
        Some(true)
    );
    assert!(chart.fitted);
}

#[test]
fn scroll_to_latest_preserves_zoom_and_returns_to_real_time_edge() {
    let mut chart = interactive_chart();
    chart.engine.time_scale.zoom(300.0, 0.5);
    chart.engine.scroll_to_position(-12.0);
    let spacing = chart.engine.bar_spacing();

    chart.scroll_to_latest();

    assert!(chart.is_at_latest());
    assert!((chart.engine.scroll_position() - REAL_TIME_RIGHT_OFFSET_BARS).abs() < f64::EPSILON);
    assert!((chart.engine.bar_spacing() - spacing).abs() < f64::EPSILON);
}

#[test]
fn fixed_time_chart_projects_future_axis_times_without_synthetic_bars() {
    let chart = interactive_chart();
    let (times, _) = chart.engine.data_layer().series_data(0).unwrap();
    assert!(times.len() >= 2);
    let canonical_len = times.len();
    let last = times.last().unwrap().to_f64().unwrap();
    let previous = times[times.len() - 2].to_f64().unwrap();
    let cadence = last - previous;
    assert!(cadence > 0.0);
    let last_index = chart.engine.time_to_index(last, false).unwrap();
    let future_x = chart
        .engine
        .logical_to_coordinate(last_index.to_f64().unwrap() + 1.0)
        .unwrap();

    assert_eq!(
        chart.engine.coordinate_to_time(future_x),
        Some(last + cadence)
    );
    assert_eq!(
        chart.engine.data_layer().series_data(0).unwrap().0.len(),
        canonical_len
    );
}

#[test]
fn indicator_catalog_maps_to_aeris_charts_with_legacy_defaults() {
    let cases = [
        (ChartIndicator::Sma, 1, "sma", "SMA 20"),
        (ChartIndicator::Ema, 1, "ema", "EMA 20"),
        (ChartIndicator::EmaRibbon, 5, "ema_ribbon", "EMA 5"),
        (ChartIndicator::Wma, 1, "wma", "WMA 20"),
        (ChartIndicator::Bollinger, 3, "bollinger", "Bollinger 20 2"),
        (ChartIndicator::Rsi, 1, "rsi", "RSI 14"),
        (ChartIndicator::Macd, 3, "macd", "MACD 12 26 9"),
        (
            ChartIndicator::Stochastic,
            2,
            "stochastic",
            "Stochastic 14 3",
        ),
        (ChartIndicator::Atr, 1, "atr", "ATR 14"),
    ];

    for (indicator, output_count, kind, title) in cases {
        let mut chart = interactive_chart();
        let ids = chart
            .add_indicator(indicator)
            .expect("supported indicator is created");

        assert_eq!(ids.len(), output_count);
        assert_eq!(series_entry(&chart, ids[0]).title, title);
        for id in ids {
            assert_eq!(
                chart
                    .engine
                    .indicator_info(id)
                    .expect("indicator lineage")
                    .kind,
                kind
            );
        }
    }
}

#[test]
fn legend_pane_layout_tracks_the_plot_width_so_legends_stay_off_the_price_axis() {
    let mut chart = interactive_chart();
    chart
        .add_indicator(ChartIndicator::Macd)
        .expect("MACD binds to Aeris Charts");
    for width in [1280.0_f32, 420.0] {
        chart.engine.css_width = f64::from(width);
        chart.engine.css_height = 720.0;
        chart
            .engine
            .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
        assert!(chart.sync_legend_pane_layout() || !chart.legend_panes.is_empty());
        let plot_width = chart.engine.pane_w.to_f32().expect("plot width");
        assert!(
            chart.legend_panes.len() > 1,
            "MACD adds its own pane at {width}"
        );
        for pane in &chart.legend_panes {
            assert!(
                (pane.width - plot_width).abs() < f32::EPSILON,
                "legend width follows the plot"
            );
            assert!(
                pane.left + pane.width <= width,
                "legend stays left of the price axis at {width}"
            );
        }
    }
}

#[test]
fn chart_legends_group_outputs_and_follow_native_indicator_panes() {
    let mut chart = interactive_chart();
    let bollinger = chart
        .add_indicator(ChartIndicator::Bollinger)
        .expect("Bollinger is created");
    let macd = chart
        .add_indicator(ChartIndicator::Macd)
        .expect("MACD is created");
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);

    let rows = chart.legend_rows();
    let asset = rows.first().expect("asset legend is always first");
    assert_eq!(asset.item, LegendItem::Asset);
    assert_eq!(asset.pane, 0);
    assert!(
        asset
            .values
            .iter()
            .any(|value| value.text.starts_with("O "))
    );
    assert!(
        asset
            .values
            .iter()
            .any(|value| value.text.starts_with("C "))
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.item == LegendItem::Indicator(bollinger[0]))
            .count(),
        1
    );
    let macd_row = rows
        .iter()
        .find(|row| row.item == LegendItem::Indicator(macd[0]))
        .expect("grouped MACD legend");
    assert!(macd_row.pane > 0);
    let macd_values = legend_text(macd_row);
    assert!(macd_values.contains("MACD"));
    assert!(macd_values.contains("Signal"));
    assert!(macd_values.contains("Histogram"));
}

#[test]
fn asset_legend_shows_ohlc_only_on_candles_and_bars() {
    let mut chart = interactive_chart();
    let asset_values = |chart: &AerisChartView| {
        let row = chart
            .legend_rows()
            .into_iter()
            .find(|row| row.item == LegendItem::Asset)
            .expect("asset legend");
        legend_text(&row)
    };
    assert!(asset_values(&chart).contains("O "));
    assert!(asset_values(&chart).contains("C "));

    chart.set_chart_type(ChartType::Bars);
    assert!(asset_values(&chart).contains("O "));
    assert!(asset_values(&chart).contains("C "));

    for chart_type in [
        ChartType::Line,
        ChartType::Area,
        ChartType::Baseline,
        ChartType::BrushableArea,
    ] {
        chart.set_chart_type(chart_type);
        assert!(
            asset_values(&chart).is_empty(),
            "OHLC stayed on {chart_type:?}"
        );
    }

    chart.set_chart_type(ChartType::Candles);
    assert!(asset_values(&chart).contains("O "));
    assert!(asset_values(&chart).contains("C "));
}

#[test]
fn chart_appearance_round_trips_series_grid_and_crosshair_styles() {
    let mut chart = interactive_chart();
    let revision = chart.user_state_revision();
    let appearance = ChartAppearanceSettings {
        grid_visible: false,
        grid_color: custom_color("#334155"),
        grid_style: 1,
        crosshair_color: custom_color("#94A3B8"),
        crosshair_width: 3,
        crosshair_style: 0,
        up_color: custom_color("#10B981"),
        down_color: custom_color("#EF4444"),
        wick_up_color: custom_color("#34D399"),
        wick_down_color: custom_color("#F87171"),
        border_up_color: custom_color("#059669"),
        border_down_color: custom_color("#DC2626"),
        wick_visible: false,
        border_visible: false,
        open_visible: false,
        thin_bars: false,
        line_color: "#3B82F6".to_string(),
        line_width: 4,
        line_style: 2,
        area_top_color: "#2563EB".to_string(),
        baseline_top_color: "#22C55E".to_string(),
        baseline_bottom_color: "#F43F5E".to_string(),
    };

    assert!(chart.set_appearance_settings(&appearance));
    assert_eq!(chart.appearance_settings(), appearance);
    let series_options = serde_json::from_str::<serde_json::Value>(
        &chart
            .engine
            .series_options_json(0)
            .expect("primary series options"),
    )
    .expect("primary series options are JSON");
    assert_eq!(series_options["area_bottom_color"], "");
    assert_eq!(chart.user_state_revision(), revision + 1);
    assert!(!chart.set_appearance_settings(&appearance));
    assert_eq!(chart.user_state_revision(), revision + 1);
}

#[test]
fn chart_time_zone_is_durable_user_state_and_uses_the_shared_parity_list() {
    let mut chart = interactive_chart();
    let revision = chart.user_state_revision();
    assert_eq!(chart.time_zone_id(), aeris_charts_engine::DEFAULT_TIME_ZONE);
    assert!(AerisChartView::supported_time_zones().contains(&"America/New_York"));
    assert!(AerisChartView::supported_time_zones().contains(&"Asia/Astana"));

    assert!(
        chart
            .set_time_zone("America/New_York")
            .expect("supported zone")
    );
    assert_eq!(chart.time_zone_id(), "America/New_York");
    assert_eq!(chart.user_state_revision(), revision + 1);
    assert!(!chart.set_time_zone("America/New_York").expect("same zone"));
    assert_eq!(chart.user_state_revision(), revision + 1);
    assert!(chart.set_time_zone("Mars/Olympus_Mons").is_err());
}

#[test]
fn time_zone_badges_match_tradingview_utc_offset_notation_and_follow_dst() {
    assert_eq!(
        AerisChartView::time_zone_badge_label("America/New_York", 1_768_435_200),
        Some("UTC-5".to_string())
    );
    assert_eq!(
        AerisChartView::time_zone_badge_label("America/New_York", 1_784_073_600),
        Some("UTC-4".to_string())
    );
    assert_eq!(
        AerisChartView::time_zone_badge_label("Asia/Kolkata", 1_784_073_600),
        Some("UTC+5:30".to_string())
    );
    assert_eq!(
        AerisChartView::time_zone_badge_label("Etc/UTC", 1_784_073_600),
        Some("UTC".to_string())
    );
}

#[test]
fn selected_time_zone_clock_uses_the_same_utc_offset_notation_as_the_menu() {
    let mut chart = AerisChartView::new();
    chart
        .set_time_zone("America/New_York")
        .expect("New York zone accepted");

    let winter = chart.time_zone_clock_label_at(1_768_435_200);
    assert!(winter.ends_with("UTC-5"), "{winter}");
    assert!(!winter.contains("EST"), "{winter}");

    let summer = chart.time_zone_clock_label_at(1_784_073_600);
    assert!(summer.ends_with("UTC-4"), "{summer}");
    assert!(!summer.contains("EDT"), "{summer}");
}

#[test]
fn selected_time_zone_date_time_crosses_the_local_calendar_day() {
    let mut chart = AerisChartView::new();
    // 2026-01-15 00:00:00 UTC is still the previous evening in New York.
    assert_eq!(
        chart.time_zone_date_time_label_at(1_768_435_200),
        "2026-01-15 00:00:00"
    );
    chart
        .set_time_zone("America/New_York")
        .expect("New York zone accepted");
    assert_eq!(
        chart.time_zone_date_time_label_at(1_768_435_200),
        "2026-01-14 19:00:00"
    );
}

#[test]
fn canvas_appearance_updates_do_not_rewrite_primary_series_options() {
    let mut chart = interactive_chart();
    let mut series = chart.appearance_settings();
    series.up_color = custom_color("#10B981");
    series.down_color = custom_color("#EF4444");
    series.line_color = "#3B82F6".to_string();
    series.area_top_color = "#2563EB80".to_string();
    assert!(chart.set_series_appearance_settings(&series));

    let before = chart
        .engine
        .series_options_json(0)
        .expect("primary series options");
    let mut canvas = chart.appearance_settings();
    canvas.grid_visible = !canvas.grid_visible;
    canvas.grid_style = 1;
    canvas.crosshair_width = 3;
    assert!(chart.set_canvas_appearance_settings(&canvas));

    assert_eq!(
        chart
            .engine
            .series_options_json(0)
            .expect("primary series options"),
        before
    );
    assert_eq!(chart.appearance_settings().area_top_color, "#2563EB80");
}

#[test]
fn series_appearance_updates_do_not_rewrite_canvas_options() {
    let mut chart = interactive_chart();
    let mut canvas = chart.appearance_settings();
    canvas.grid_visible = false;
    canvas.grid_color = custom_color("#334155");
    canvas.grid_style = 1;
    canvas.crosshair_color = custom_color("#94A3B8");
    canvas.crosshair_width = 3;
    canvas.crosshair_style = 0;
    assert!(chart.set_canvas_appearance_settings(&canvas));

    let before = {
        let options = chart.engine.options.get();
        (
            options.grid.vert_lines.visible,
            options.grid.vert_lines.color.clone(),
            options.grid.vert_lines.style,
            options.crosshair.vert_line.color.clone(),
            options.crosshair.vert_line.width,
            options.crosshair.vert_line.style,
        )
    };
    let mut series = chart.appearance_settings();
    series.area_top_color = "#2563EB80".to_string();
    series.line_width = 4;
    assert!(chart.set_series_appearance_settings(&series));
    let options = chart.engine.options.get();
    assert_eq!(
        (
            options.grid.vert_lines.visible,
            options.grid.vert_lines.color.clone(),
            options.grid.vert_lines.style,
            options.crosshair.vert_line.color.clone(),
            options.crosshair.vert_line.width,
            options.crosshair.vert_line.style,
        ),
        before
    );
}

#[test]
fn aeris_charts_default_grid_color_tracks_theme_but_custom_grid_color_does_not() {
    let mut chart = interactive_chart();
    assert_eq!(
        chart.engine.options.get().grid.vert_lines.color,
        aeris_charts_grid_color(ChartTheme::Dark)
    );

    chart.set_theme(ChartTheme::Light);
    assert_eq!(
        chart.engine.options.get().grid.vert_lines.color,
        aeris_charts_grid_color(ChartTheme::Light)
    );

    let mut custom = chart.appearance_settings();
    custom.grid_color = custom_color("#334155");
    assert!(chart.set_appearance_settings(&custom));
    chart.set_theme(ChartTheme::Dark);
    assert_eq!(chart.engine.options.get().grid.vert_lines.color, "#334155");

    let mut persisted_light_default = chart.appearance_settings();
    persisted_light_default.grid_color = AppearanceColor::Theme;
    assert!(chart.set_appearance_settings(&persisted_light_default));
    assert_eq!(
        chart.engine.options.get().grid.vert_lines.color,
        aeris_charts_grid_color(ChartTheme::Dark)
    );
}

#[test]
fn persisted_light_aeris_charts_market_defaults_stay_unpinned_on_a_dark_chart() {
    let light = AerisChartView::empty_with_theme(ChartTheme::Light);
    let persisted = light.appearance_settings();
    let light_defaults = FinancialThemeColors::for_theme(ChartTheme::Light);
    assert_eq!(persisted.up_color, AppearanceColor::Theme);
    assert_eq!(persisted.down_color, AppearanceColor::Theme);
    assert_eq!(
        persisted.effective_up_color(ChartTheme::Light),
        light_defaults.bullish
    );

    let mut dark = AerisChartView::empty_with_theme(ChartTheme::Dark);
    let _ = dark.set_appearance_settings(&persisted);

    let series = series_entry(&dark, 0);
    assert!(series.up_color.is_none());
    assert!(series.down_color.is_none());
    assert!(series.wick_up_color.is_none());
    assert!(series.wick_down_color.is_none());
    assert!(series.border_up_color.is_none());
    assert!(series.border_down_color.is_none());

    let dark_defaults = FinancialThemeColors::for_theme(ChartTheme::Dark);
    assert_eq!(dark_defaults.bullish, "#089981");
    assert_eq!(dark_defaults.bearish, "#f7525f");
    let effective = dark.appearance_settings();
    assert_eq!(effective.up_color, AppearanceColor::Theme);
    assert_eq!(effective.down_color, AppearanceColor::Theme);
    assert_eq!(effective.wick_up_color, AppearanceColor::Theme);
    assert_eq!(effective.wick_down_color, AppearanceColor::Theme);
    assert_eq!(effective.border_up_color, AppearanceColor::Theme);
    assert_eq!(effective.border_down_color, AppearanceColor::Theme);

    let bullish = effective.effective_up_color(dark.theme);
    let bearish = effective.effective_down_color(dark.theme);
    let palette = legend_palette(dark.theme, &bullish, &bearish);
    assert_eq!(
        palette.bullish,
        rgba(
            Color::parse_css(dark_defaults.bullish)
                .expect("Aeris Charts bullish color is valid CSS")
                .0
        )
    );
    assert_eq!(
        palette.bearish,
        rgba(
            Color::parse_css(dark_defaults.bearish)
                .expect("Aeris Charts bearish color is valid CSS")
                .0
        )
    );
}

#[test]
fn custom_market_and_crosshair_colors_stay_pinned_across_theme_switches() {
    let mut chart = AerisChartView::empty_with_theme(ChartTheme::Dark);
    let aeris_defaults = chart.appearance_settings();
    let mut custom = aeris_defaults.clone();
    custom.up_color = custom_color("#112233");
    custom.down_color = custom_color("#445566");
    custom.wick_up_color = custom_color("#778899");
    custom.wick_down_color = custom_color("#AABBCC");
    custom.border_up_color = custom_color("#123456");
    custom.border_down_color = custom_color("#654321");
    custom.crosshair_color = custom_color("#ABCDEF");
    custom.grid_visible = false;
    custom.grid_style = 2;
    custom.line_width = 4;
    custom.line_style = 2;

    assert!(chart.set_appearance_settings(&custom));
    chart.set_theme(ChartTheme::Light);

    let series = series_entry(&chart, 0);
    assert_eq!(series.up_color.as_deref(), Some("#112233"));
    assert_eq!(series.down_color.as_deref(), Some("#445566"));
    assert_eq!(series.wick_up_color.as_deref(), Some("#778899"));
    assert_eq!(series.wick_down_color.as_deref(), Some("#AABBCC"));
    assert_eq!(series.border_up_color.as_deref(), Some("#123456"));
    assert_eq!(series.border_down_color.as_deref(), Some("#654321"));
    assert_eq!(
        chart.engine.options.get().crosshair.vert_line.color,
        "#ABCDEF"
    );

    chart.set_theme(ChartTheme::Dark);
    assert_eq!(chart.appearance_settings(), custom);

    let revision = chart.user_state_revision();
    chart.reset_appearance_settings();
    assert_eq!(chart.user_state_revision(), revision + 1);
    let series = series_entry(&chart, 0);
    assert!(series.up_color.is_none());
    assert!(series.down_color.is_none());
    assert!(series.wick_up_color.is_none());
    assert!(series.wick_down_color.is_none());
    assert!(series.border_up_color.is_none());
    assert!(series.border_down_color.is_none());
    assert_eq!(chart.appearance_settings(), aeris_defaults);
    let defaults = FinancialThemeColors::for_theme(ChartTheme::Dark);
    let effective = chart.appearance_settings();
    assert_eq!(
        effective.effective_up_color(ChartTheme::Dark),
        defaults.bullish
    );
    assert_eq!(
        effective.effective_down_color(ChartTheme::Dark),
        defaults.bearish
    );
    assert_eq!(
        effective.effective_crosshair_color(ChartTheme::Dark),
        defaults.crosshair
    );
}

#[test]
fn canonical_crosshair_color_tracks_aeris_theme() {
    let mut chart = AerisChartView::empty_with_theme(ChartTheme::Light);
    let light = FinancialThemeColors::for_theme(ChartTheme::Light);
    assert_eq!(
        chart.engine.options.get().crosshair.vert_line.color,
        light.crosshair
    );

    chart.set_theme(ChartTheme::Dark);
    let dark = FinancialThemeColors::for_theme(ChartTheme::Dark);
    assert_eq!(
        chart.engine.options.get().crosshair.vert_line.color,
        dark.crosshair
    );
}

#[test]
fn legend_values_follow_volume_direction_and_indicator_series_colors() {
    let mut chart = interactive_chart();
    chart
        .add_indicator(ChartIndicator::Volume)
        .expect("volume is created");
    let ribbon = chart
        .add_indicator(ChartIndicator::EmaRibbon)
        .expect("EMA ribbon is created");
    let rows = chart.legend_rows();
    let asset = rows
        .iter()
        .find(|row| row.item == LegendItem::Asset)
        .expect("asset legend");
    let volume = rows
        .iter()
        .find(|row| row.item == LegendItem::Volume)
        .expect("volume legend");
    assert_ne!(asset.values_tone, LegendValueTone::Neutral);
    assert_eq!(volume.values_tone, asset.values_tone);

    let ribbon_row = rows
        .iter()
        .find(|row| row.item == LegendItem::Indicator(ribbon[0]))
        .expect("EMA ribbon legend");
    assert_eq!(ribbon_row.title, "EMA Ribbon");
    assert_eq!(ribbon_row.values.len(), ribbon.len());
    for (value, id) in ribbon_row.values.iter().zip(ribbon) {
        assert_eq!(value.color, series_entry(&chart, id).line_color);
        assert!(!value.text.contains("EMA"));
        assert!(!value.text.contains("Ribbon"));
    }
}

#[test]
fn legend_visibility_preserves_rows_and_indicator_removal_clears_bindings() {
    let mut chart = interactive_chart();
    let sma = chart
        .add_indicator(ChartIndicator::Sma)
        .expect("SMA is created")[0];

    assert_ne!(chart.legend_rows()[0].values, [] as [LegendValue; 0]);
    assert!(chart.set_legend_item_visible(LegendItem::Asset, false));
    assert!(!series_entry(&chart, 0).visible);
    assert_eq!(chart.legend_rows()[0].item, LegendItem::Asset);
    assert!(!chart.legend_rows()[0].visible);
    assert_eq!(chart.legend_rows()[0].values, [] as [LegendValue; 0]);
    assert!(chart.set_legend_item_visible(LegendItem::Indicator(sma), false));
    assert!(!series_entry(&chart, sma).visible);
    let sma_row = chart
        .legend_rows()
        .into_iter()
        .find(|row| row.item == LegendItem::Indicator(sma))
        .expect("SMA legend remains while hidden");
    assert!(!sma_row.visible);
    assert_eq!(sma_row.values, [] as [LegendValue; 0]);
    assert!(chart.set_legend_item_visible(LegendItem::Indicator(sma), true));
    assert!(series_entry(&chart, sma).visible);
    assert!(
        chart
            .legend_rows()
            .iter()
            .any(|row| row.item == LegendItem::Indicator(sma)
                && row.visible
                && !row.values.is_empty())
    );
    assert!(chart.remove_legend_indicator(LegendItem::Indicator(sma)));
    assert!(
        chart
            .legend_rows()
            .iter()
            .all(|row| row.item != LegendItem::Indicator(sma))
    );
    assert!(!chart.remove_legend_indicator(LegendItem::Asset));
}

#[test]
fn hidden_volume_keeps_its_legend_until_removed() {
    let mut chart = interactive_chart();
    let volume = chart
        .add_indicator(ChartIndicator::Volume)
        .expect("volume is created")[0];

    assert!(chart.set_legend_item_visible(LegendItem::Volume, false));
    assert!(!series_entry(&chart, volume).visible);
    assert!(chart.has_indicators());
    let volume_row = chart
        .legend_rows()
        .into_iter()
        .find(|row| row.item == LegendItem::Volume)
        .expect("volume legend remains while hidden");
    assert!(!volume_row.visible);
    assert_eq!(volume_row.values, [] as [LegendValue; 0]);
    assert!(chart.remove_legend_indicator(LegendItem::Volume));
    assert!(!chart.has_indicators());
    assert!(
        chart
            .legend_rows()
            .iter()
            .all(|row| row.item != LegendItem::Volume)
    );
}

#[test]
fn native_indicator_hover_selection_and_delete_reach_aeris_charts() {
    let mut chart = interactive_chart();
    let indicator = chart
        .add_indicator(ChartIndicator::Sma)
        .expect("SMA is created")[0];
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    let (x, y) = visible_series_point(&chart, indicator);
    assert_eq!(chart.engine.hit_test_series(x, y), Some(indicator));

    hover(&mut chart, x, y);
    assert_eq!(chart.engine.hovered_series(), Some(indicator));
    assert_eq!(cursor(&chart), ChartCursor::Pointer);
    click(&mut chart, x, y);
    assert_eq!(chart.engine.selected_series(), Some(indicator));
    assert!(chart.has_deletable_selection());

    assert!(chart.remove_selected_chart_object());
    assert!(chart.engine.indicator_info(indicator).is_none());
    assert!(
        chart
            .engine
            .series_entries()
            .iter()
            .all(|series| series.id != indicator || series.removed)
    );
}

#[test]
fn grouped_indicator_delete_removes_every_native_output() {
    let mut chart = interactive_chart();
    let outputs = chart
        .add_indicator(ChartIndicator::Macd)
        .expect("MACD is created");
    chart.engine.set_selected_series(Some(outputs[1]));

    assert!(chart.remove_selected_chart_object());
    for output in outputs {
        assert!(
            chart
                .engine
                .series_entries()
                .iter()
                .all(|series| series.id != output || series.removed)
        );
    }
}

#[test]
fn clear_indicators_removes_every_native_output_and_hides_volume() {
    let mut chart = interactive_chart();
    assert!(!chart.has_indicators());
    let volume = chart
        .add_indicator(ChartIndicator::Volume)
        .expect("volume is shown")[0];
    let macd = chart
        .add_indicator(ChartIndicator::Macd)
        .expect("MACD is created");
    assert!(chart.has_indicators());

    assert!(chart.clear_indicators());
    assert!(!chart.has_indicators());
    assert!(!series_entry(&chart, volume).visible);
    for output in macd {
        assert!(
            chart
                .engine
                .series_entries()
                .iter()
                .all(|series| series.id != output || series.removed)
        );
    }
    assert!(!chart.clear_indicators());
    assert_eq!(
        chart
            .add_indicator(ChartIndicator::Volume)
            .expect("volume can be shown again"),
        vec![volume]
    );
    assert!(chart.has_indicators());
}

#[test]
fn product_series_are_protected_and_volume_remains_reusable() {
    let mut chart = interactive_chart();
    chart.engine.set_selected_series(Some(0));
    assert!(!chart.has_deletable_selection());
    assert!(!chart.remove_selected_chart_object());
    assert!(series_entry(&chart, 0).visible);

    let volume = chart
        .add_indicator(ChartIndicator::Volume)
        .expect("volume is shown")[0];
    chart.engine.set_selected_series(Some(volume));
    assert!(chart.has_deletable_selection());
    assert!(chart.remove_selected_chart_object());
    assert!(!series_entry(&chart, volume).visible);
    assert_eq!(
        chart
            .add_indicator(ChartIndicator::Volume)
            .expect("volume can be shown again"),
        vec![volume]
    );
    assert!(series_entry(&chart, volume).visible);
}

#[test]
fn replay_volume_drives_histogram_and_vwap_with_real_weights() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
        .expect("embedded replay validates");
    let mut chart = AerisChartView::with_replay(&replay);
    let (_, volume_columns) = chart
        .engine
        .data_layer()
        .series_data(chart.volume_series)
        .expect("parallel volume series");
    let expected_volume = replay
        .bars()
        .iter()
        .map(|item| {
            item.value()
                .volume
                .to_f64()
                .expect("fixture volume fits f64")
        })
        .collect::<Vec<_>>();
    assert_eq!(volume_columns[3], expected_volume);
    assert!(!series_entry(&chart, chart.volume_series).visible);
    assert!(series_entry(&chart, chart.volume_series).histogram_updown);
    assert_eq!(
        series_entry(&chart, chart.volume_series).price_scale_target,
        PriceScaleTarget::Overlay
    );

    assert_eq!(
        chart
            .add_indicator(ChartIndicator::Volume)
            .expect("volume histogram"),
        vec![chart.volume_series]
    );
    assert!(series_entry(&chart, chart.volume_series).visible);

    let vwap = chart
        .add_indicator(ChartIndicator::Vwap)
        .expect("volume-weighted average");
    assert_eq!(vwap.len(), 1);
    assert_eq!(
        chart
            .engine
            .indicator_info(vwap[0])
            .map(|info| info.kind.into_owned()),
        Some("vwap".to_owned())
    );
    let (_, vwap_columns) = chart
        .engine
        .data_layer()
        .series_data(vwap[0])
        .expect("vwap output data");
    assert_eq!(vwap_columns[3].len(), replay.bars().len());
    assert!(vwap_columns[3].iter().all(|value| value.is_finite()));
}

#[test]
fn indicator_api_rejects_an_empty_chart_without_inventing_series() {
    let mut chart = AerisChartView::empty();
    let initial_series = chart.engine.series.len();

    for indicator in ChartIndicator::ALL {
        assert_eq!(
            chart.add_indicator(indicator),
            Err(ChartIndicatorError::MarketDataUnavailable)
        );
    }

    assert_eq!(chart.engine.series.len(), initial_series);
}

#[test]
fn study_output_projection_preserves_gaps_fences_generations_and_removes_cleanly() {
    let mut chart = AerisChartView::empty();
    let timestamps = [
        60_i64 * 1_000_000_000,
        120_i64 * 1_000_000_000,
        180_i64 * 1_000_000_000,
    ];
    let first = [None, Some(20.0), Some(30.0)];
    let descriptor = ChartStudyOutputDescriptor {
        title: "Test Study",
        legend_label: None,
        plot: ChartStudyPlotKind::Line,
        pane: ChartStudyPaneTarget::Price,
        scale: ChartStudyScaleTarget::Primary,
        settings_available: true,
        threshold_region: None,
        point_style: ChartStudyPointStyle::Uniform,
        input_requirements: ChartStudyInputRequirements::BARS
            .with(ChartStudyInputStream::Trades)
            .with(ChartStudyInputStream::Depth),
    };

    assert_eq!(
        chart.install_study_output(7, 0, descriptor, 1, &timestamps, &first),
        Ok(true)
    );
    let state = study_output(&chart, 7, 0);
    let points = chart.engine.series_data(state.series_id);
    assert_eq!(points.len(), 3);
    assert!(points[0].close.is_nan());
    assert_eq!(points[1].close.to_bits(), 20.0_f64.to_bits());
    assert_eq!(points[2].close.to_bits(), 30.0_f64.to_bits());
    assert_eq!(series_entry(&chart, state.series_id).title, "Test Study");
    assert!(!series_entry(&chart, state.series_id).countdown_visible);
    assert!(chart.has_indicators());
    assert!(!chart.clear_indicators());
    assert_eq!(chart.study_visible(7), Some(true));
    assert!(chart.set_study_visible(7, false));
    assert_eq!(chart.study_visible(7), Some(false));

    let duplicate_generation_values = [Some(1.0), Some(2.0), Some(3.0)];
    assert_eq!(
        chart.install_study_output(
            7,
            0,
            descriptor,
            1,
            &timestamps,
            &duplicate_generation_values,
        ),
        Ok(false)
    );
    assert_eq!(
        chart.engine.series_data(state.series_id)[2].close.to_bits(),
        30.0_f64.to_bits()
    );

    let newer = [Some(10.0), Some(20.0), Some(40.0)];
    assert_eq!(
        chart.install_study_output(7, 0, descriptor, 2, &timestamps, &newer),
        Ok(true)
    );
    assert_eq!(study_output(&chart, 7, 0).series_id, state.series_id);
    assert_eq!(
        chart.engine.series_data(state.series_id)[2].close.to_bits(),
        40.0_f64.to_bits()
    );
    assert_eq!(chart.study_visible(7), Some(false));
    assert!(chart.legend_rows().iter().any(|row| {
        row.item
            == LegendItem::Study {
                study_id: 7,
                series_id: state.series_id,
            }
            && row.title == "Test Study"
            && row.settings_available
    }));

    assert!(chart.remove_study_outputs(&[7]));
    assert_eq!(
        chart.engine.external_study_outputs(),
        [] as [aeris_charts_engine::ExternalStudyOutputInfo; 0]
    );
    assert_eq!(chart.study_visible(7), None);
    assert!(
        chart
            .engine
            .series_entries()
            .iter()
            .all(|series| series.id != state.series_id || series.removed)
    );
    assert!(!chart.remove_study_outputs(&[7]));
}

#[test]
fn study_output_projection_retains_typed_input_requirements() {
    let mut chart = AerisChartView::empty();
    let descriptor = ChartStudyOutputDescriptor {
        title: "Trade depth study",
        legend_label: None,
        plot: ChartStudyPlotKind::Line,
        pane: ChartStudyPaneTarget::Price,
        scale: ChartStudyScaleTarget::Primary,
        settings_available: false,
        threshold_region: None,
        point_style: ChartStudyPointStyle::Uniform,
        input_requirements: ChartStudyInputRequirements::BARS
            .with(ChartStudyInputStream::Trades)
            .with(ChartStudyInputStream::Depth),
    };
    chart
        .install_study_output(9, 0, descriptor, 1, &[60_i64 * 1_000_000_000], &[Some(1.0)])
        .expect("study output installs");
    let requirements = chart
        .study_output_input_requirements(9, 0)
        .expect("study input metadata is retained");
    assert!(requirements.contains(ChartStudyInputStream::Bars));
    assert!(requirements.contains(ChartStudyInputStream::Trades));
    assert!(requirements.contains(ChartStudyInputStream::Depth));
    assert!(!requirements.contains(ChartStudyInputStream::Quotes));
}

#[test]
fn study_output_projection_inherits_native_series_defaults() {
    let mut chart = AerisChartView::empty();
    assert!(
        chart.engine.series_apply_price_format_json(
            0,
            r#"{"type":"price","precision":4,"min_move":0.0001}"#,
        )
    );
    let descriptor = ChartStudyOutputDescriptor {
        title: "Defaults",
        legend_label: None,
        plot: ChartStudyPlotKind::Line,
        pane: ChartStudyPaneTarget::Price,
        scale: ChartStudyScaleTarget::Primary,
        settings_available: false,
        threshold_region: None,
        point_style: ChartStudyPointStyle::Uniform,
        input_requirements: ChartStudyInputRequirements::NONE,
    };
    chart
        .install_study_output(
            88,
            0,
            descriptor,
            1,
            &[60_i64 * 1_000_000_000],
            &[Some(1.0)],
        )
        .expect("study installs");
    let series_id = study_output(&chart, 88, 0).series_id;
    let entry = series_entry(&chart, series_id);
    assert_eq!(entry.line_width, Some(2.0));
    assert_eq!(entry.price_format.precision, 4);
}

#[test]
fn study_output_projection_rejects_invalid_presentation_before_creating_series() {
    let mut chart = AerisChartView::empty();
    let initial_series = chart.engine.series.len();
    let descriptor = ChartStudyOutputDescriptor {
        title: "Invalid Threshold Histogram",
        legend_label: Some("Histogram"),
        plot: ChartStudyPlotKind::Histogram,
        pane: ChartStudyPaneTarget::Dedicated { group: 0 },
        scale: ChartStudyScaleTarget::Primary,
        settings_available: false,
        threshold_region: Some(ChartStudyThresholdRegion {
            lower: 20.0,
            upper: 80.0,
        }),
        point_style: ChartStudyPointStyle::Uniform,
        input_requirements: ChartStudyInputRequirements::NONE,
    };

    assert_eq!(
        chart.install_study_output(
            99,
            0,
            descriptor,
            1,
            &[60_i64 * 1_000_000_000],
            &[Some(50.0)],
        ),
        Err(ChartStudyOutputError::InvalidPresentation)
    );
    assert_eq!(
        chart.engine.external_study_outputs(),
        [] as [aeris_charts_engine::ExternalStudyOutputInfo; 0]
    );
    assert_eq!(chart.engine.series.len(), initial_series);
}

#[test]
fn study_legend_control_ids_do_not_overflow_or_alias_control_kinds() {
    let study = LegendItem::Study {
        study_id: u64::MAX,
        series_id: u32::MAX,
    };
    let visibility = legend_control_element_id(study, LegendControl::Visibility(true));
    let settings = legend_control_element_id(study, LegendControl::Settings);
    let remove = legend_control_element_id(study, LegendControl::Remove);

    assert_eq!(visibility.1, (1_u64 << 63) | u64::from(u32::MAX));
    assert_ne!(visibility, settings);
    assert_ne!(settings, remove);
    assert_ne!(visibility, remove);
    assert_ne!(
        visibility,
        legend_control_element_id(LegendItem::Asset, LegendControl::Visibility(true))
    );
}

#[test]
fn settings_requests_are_bounded_to_one_latest_legend_item() {
    let mut chart = AerisChartView::empty();
    assert_eq!(chart.take_settings_request(), None);
    chart.pending_settings_request = Some(ChartSettingsRequest::Study(7));
    chart.pending_settings_request = Some(ChartSettingsRequest::Study(9));
    assert_eq!(
        chart.take_settings_request(),
        Some(ChartSettingsRequest::Study(9))
    );
    assert_eq!(chart.take_settings_request(), None);
}

#[test]
fn study_remove_request_targets_one_runtime_study() {
    let mut chart = AerisChartView::empty();
    assert_eq!(chart.take_study_remove_request(), None);
    chart.pending_study_remove = Some(7);
    assert_eq!(chart.take_study_remove_request(), Some(7));
    assert_eq!(chart.take_study_remove_request(), None);
}

#[test]
fn selected_study_output_requests_one_owner_level_removal_without_deleting_a_line() {
    let mut chart = AerisChartView::empty();
    let timestamps = [60_i64 * 1_000_000_000];
    let values = [Some(20.0)];
    let descriptor = ChartStudyOutputDescriptor {
        title: "EMA Ribbon",
        legend_label: None,
        plot: ChartStudyPlotKind::Line,
        pane: ChartStudyPaneTarget::Price,
        scale: ChartStudyScaleTarget::Primary,
        settings_available: true,
        threshold_region: None,
        point_style: ChartStudyPointStyle::Uniform,
        input_requirements: ChartStudyInputRequirements::NONE,
    };
    assert_eq!(
        chart.install_study_output(11, 0, descriptor, 1, &timestamps, &values),
        Ok(true)
    );
    assert_eq!(
        chart.install_study_output(11, 1, descriptor, 1, &timestamps, &values),
        Ok(true)
    );
    let series_ids = study_series_ids(&chart, 11);
    chart.engine.set_selected_series(Some(series_ids[1]));

    assert!(chart.remove_selected_chart_object());
    assert_eq!(chart.take_study_remove_request(), Some(11));
    assert_eq!(chart.engine.selected_series(), None);
    assert!(
        series_ids
            .iter()
            .all(|id| !series_entry(&chart, *id).removed)
    );
}

#[test]
fn selecting_one_runtime_study_output_selects_the_complete_indicator() {
    let mut chart = interactive_chart();
    let source = chart.engine.series_data(0);
    let timestamps = source
        .iter()
        .map(|point| point.time.saturating_mul(1_000_000_000))
        .collect::<Vec<_>>();
    let upper = source
        .iter()
        .map(|point| Some(point.close + 10.0))
        .collect::<Vec<_>>();
    let lower = source
        .iter()
        .map(|point| Some(point.close - 10.0))
        .collect::<Vec<_>>();
    let descriptor = ChartStudyOutputDescriptor {
        title: "EMA Ribbon",
        legend_label: None,
        plot: ChartStudyPlotKind::Line,
        pane: ChartStudyPaneTarget::Price,
        scale: ChartStudyScaleTarget::Primary,
        settings_available: true,
        threshold_region: None,
        point_style: ChartStudyPointStyle::Uniform,
        input_requirements: ChartStudyInputRequirements::NONE,
    };
    assert_eq!(
        chart.install_study_output(11, 0, descriptor, 1, &timestamps, &upper),
        Ok(true)
    );
    assert_eq!(
        chart.install_study_output(11, 1, descriptor, 1, &timestamps, &lower),
        Ok(true)
    );
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    let series_ids = study_series_ids(&chart, 11);
    let selected = series_ids[1];
    let (x, y) = visible_series_point(&chart, selected);

    click(&mut chart, x, y);
    assert_eq!(chart.engine.selected_series(), Some(selected));
    assert_eq!(
        chart.engine.selected_series_members().collect::<Vec<_>>(),
        series_ids
    );
}

#[test]
fn multi_output_study_legend_visibility_toggles_the_whole_study() {
    let mut chart = AerisChartView::empty();
    let timestamps = [60_i64 * 1_000_000_000];
    let values = [Some(20.0)];
    let upper = ChartStudyOutputDescriptor {
        title: "Bollinger 20 2",
        legend_label: Some("Upper"),
        plot: ChartStudyPlotKind::Line,
        pane: ChartStudyPaneTarget::Price,
        scale: ChartStudyScaleTarget::Primary,
        settings_available: true,
        threshold_region: None,
        point_style: ChartStudyPointStyle::Uniform,
        input_requirements: ChartStudyInputRequirements::NONE,
    };
    let lower = ChartStudyOutputDescriptor {
        legend_label: Some("Lower"),
        ..upper
    };

    assert_eq!(
        chart.install_study_output(11, 0, upper, 1, &timestamps, &values),
        Ok(true)
    );
    assert_eq!(
        chart.install_study_output(11, 1, lower, 1, &timestamps, &values),
        Ok(true)
    );
    let colors = chart
        .engine
        .external_study_outputs()
        .into_iter()
        .filter(|output| output.study_id == 11)
        .map(|output| {
            series_entry(&chart, output.series_id)
                .line_color
                .clone()
                .expect("study output has a stable color")
        })
        .collect::<HashSet<_>>();
    assert_eq!(colors.len(), 2);
    let legend_rows = chart
        .legend_rows()
        .into_iter()
        .filter(|row| matches!(row.item, LegendItem::Study { study_id: 11, .. }))
        .collect::<Vec<_>>();
    assert_eq!(legend_rows.len(), 1);
    assert_eq!(legend_rows[0].title, "Bollinger 20 2");
    assert!(
        legend_rows[0]
            .values
            .iter()
            .any(|value| value.text.starts_with("Upper "))
    );
    assert!(
        legend_rows[0]
            .values
            .iter()
            .any(|value| value.text.starts_with("Lower "))
    );
    let legend_item = legend_rows[0].item;

    assert!(chart.set_legend_item_visible(legend_item, false));
    assert_eq!(chart.study_visible(11), Some(false));
    assert!(
        chart
            .engine
            .external_study_outputs()
            .iter()
            .filter(|output| output.study_id == 11)
            .all(|output| !series_entry(&chart, output.series_id).visible)
    );
    assert!(chart.set_legend_item_visible(legend_item, true));
    assert_eq!(chart.study_visible(11), Some(true));
}

#[test]
fn study_outputs_inherit_indicator_chrome_and_live_updates_do_not_dirty_layout() {
    let mut chart = AerisChartView::empty();
    chart.apply_indicator_chrome_preferences(false, false, false);
    let timestamps = [60_i64 * 1_000_000_000];
    let descriptor = ChartStudyOutputDescriptor {
        title: "EMA 20",
        legend_label: None,
        plot: ChartStudyPlotKind::Line,
        pane: ChartStudyPaneTarget::Price,
        scale: ChartStudyScaleTarget::Primary,
        settings_available: true,
        threshold_region: None,
        point_style: ChartStudyPointStyle::Uniform,
        input_requirements: ChartStudyInputRequirements::NONE,
    };

    assert_eq!(
        chart.install_study_output(7, 0, descriptor, 1, &timestamps, &[Some(20.0)]),
        Ok(true)
    );
    let series_id = study_output(&chart, 7, 0).series_id;
    let series = series_entry(&chart, series_id);
    assert!(!series.title_visible);
    assert!(!series.last_value_visible);
    assert!(!series.price_line_visible);
    assert!(!series.countdown_visible);

    chart.reset_appearance_settings();
    assert!(
        !series_entry(&chart, series_id).countdown_visible,
        "appearance reset must not give a derived study candle-close ownership"
    );

    chart.layout_dirty = false;
    assert_eq!(
        chart.install_study_output(7, 0, descriptor, 2, &timestamps, &[Some(21.0)]),
        Ok(true)
    );
    assert!(!chart.layout_dirty);
}

#[test]
fn study_output_projection_rejects_subsecond_time_without_mutating_chart_state() {
    let mut chart = AerisChartView::empty();
    let initial_series = chart.engine.series.len();

    assert_eq!(
        chart.install_study_output(
            9,
            0,
            ChartStudyOutputDescriptor {
                title: "Tick Study",
                legend_label: None,
                plot: ChartStudyPlotKind::Line,
                pane: ChartStudyPaneTarget::Price,
                scale: ChartStudyScaleTarget::Primary,
                settings_available: false,
                threshold_region: None,
                point_style: ChartStudyPointStyle::Uniform,
                input_requirements: ChartStudyInputRequirements::NONE,
            },
            1,
            &[1_000_000_001],
            &[Some(1.0)],
        ),
        Err(ChartStudyOutputError::UnsupportedTimestampPrecision)
    );
    assert_eq!(
        chart.engine.external_study_outputs(),
        [] as [aeris_charts_engine::ExternalStudyOutputInfo; 0]
    );
    assert_eq!(chart.engine.series.len(), initial_series);
}

#[test]
fn study_outputs_share_declared_dedicated_pane_with_independent_plot_and_scale_kinds() {
    let mut chart = AerisChartView::empty();
    let timestamps = [60_i64 * 1_000_000_000, 120_i64 * 1_000_000_000];
    let values = [Some(1.0), Some(2.0)];

    assert_eq!(
        chart.install_study_output(
            11,
            0,
            ChartStudyOutputDescriptor {
                title: "Signal",
                legend_label: Some("Signal"),
                plot: ChartStudyPlotKind::Line,
                pane: ChartStudyPaneTarget::Dedicated { group: 3 },
                scale: ChartStudyScaleTarget::Primary,
                settings_available: true,
                threshold_region: None,
                point_style: ChartStudyPointStyle::Uniform,
                input_requirements: ChartStudyInputRequirements::NONE,
            },
            1,
            &timestamps,
            &values,
        ),
        Ok(true)
    );
    assert_eq!(
        chart.install_study_output(
            11,
            1,
            ChartStudyOutputDescriptor {
                title: "Histogram",
                legend_label: Some("Histogram"),
                plot: ChartStudyPlotKind::Histogram,
                pane: ChartStudyPaneTarget::Dedicated { group: 3 },
                scale: ChartStudyScaleTarget::Left,
                settings_available: true,
                threshold_region: None,
                point_style: ChartStudyPointStyle::Uniform,
                input_requirements: ChartStudyInputRequirements::NONE,
            },
            2,
            &timestamps,
            &values,
        ),
        Ok(true)
    );

    let line = study_output(&chart, 11, 0);
    let histogram = study_output(&chart, 11, 1);
    let line_entry = series_entry(&chart, line.series_id);
    let histogram_entry = series_entry(&chart, histogram.series_id);
    assert_eq!(line_entry.kind, aeris_charts_engine::SeriesKind::Line);
    assert_eq!(
        histogram_entry.kind,
        aeris_charts_engine::SeriesKind::Histogram
    );
    assert_ne!(line_entry.pane_index, 0);
    assert_eq!(line_entry.pane_index, histogram_entry.pane_index);
    assert_eq!(line_entry.price_scale_target, PriceScaleTarget::Right);
    assert_eq!(histogram_entry.price_scale_target, PriceScaleTarget::Left);
    let pane_id = chart
        .engine
        .pane_stable_id(line_entry.pane_index)
        .expect("dedicated study pane has a stable identity");
    assert_eq!(
        chart.engine.pane_index_for_id(pane_id),
        Some(line_entry.pane_index)
    );

    assert!(chart.remove_study_outputs(&[11]));
    assert!(chart.engine.pane_index_for_id(pane_id).is_none());
}

#[test]
fn indicator_metadata_matches_the_legacy_picker_copy() {
    assert_eq!(ChartIndicator::ALL.len(), 12);
    assert_eq!(ChartIndicator::Volume.label(), "Volume");
    assert_eq!(
        ChartIndicator::VolumeProfile.label(),
        "Volume Profile (Visible Range)"
    );
    assert_eq!(ChartIndicator::Vwap.parameters(), "Session anchored");
    assert_eq!(ChartIndicator::Sma.label(), "Moving Average");
    assert_eq!(ChartIndicator::Sma.parameters(), "Period 20");
    assert_eq!(
        ChartIndicator::EmaRibbon.parameters(),
        "Periods 5 · 10 · 20 · 50 · 200"
    );
    assert_eq!(
        ChartIndicator::Bollinger.parameters(),
        "Period 20 · Deviation 2"
    );
    assert_eq!(
        ChartIndicator::Macd.parameters(),
        "Fast 12 · Slow 26 · Signal 9"
    );
    assert_eq!(ChartIndicator::Stochastic.parameters(), "%K 14 · %D 3");
    assert_eq!(ChartIndicator::Atr.parameters(), "Period 14");
}

#[test]
fn anchored_drawing_tools_commit_real_aeris_charts_drawings_and_return_to_cursor() {
    let mut chart = interactive_chart();
    let tools = [
        (DrawingKind::TrendLine, 2, 160.0),
        (DrawingKind::HorizontalLine, 1, 180.0),
        (DrawingKind::VerticalLine, 1, 200.0),
        (DrawingKind::HorizontalRay, 1, 220.0),
        (DrawingKind::Rectangle, 2, 240.0),
        (DrawingKind::Text, 1, 260.0),
    ];
    let anchor_x = [260.0, 340.0];

    for (index, (tool, anchors, y)) in tools.into_iter().enumerate() {
        chart.set_drawing_tool(Some(tool));
        assert_eq!(chart.drawing_tool(), Some(tool));
        assert!(
            !chart.engine.drawing_create_active(),
            "arming must not start a pre-click handle"
        );
        for &x in anchor_x.iter().take(anchors) {
            click(&mut chart, x, y);
        }
        assert_eq!(chart.drawing_count(), index + 1);
        assert_eq!(chart.drawing_tool(), None);
        assert!(!chart.engine.drawing_create_active());
    }
    assert!(
        chart.is_editing_text(),
        "a placed text drawing opens its editor"
    );
}

#[test]
fn every_aeris_charts_drawing_tool_arms_from_the_host_toolbar() {
    let mut chart = interactive_chart();
    let kinds: Vec<_> = (0..=u8::MAX).filter_map(DrawingKind::from_u8).collect();
    assert!(
        kinds.len() >= 85,
        "the engine catalog shrank to {}",
        kinds.len()
    );
    for kind in kinds {
        chart.set_drawing_tool(Some(kind));
        assert_eq!(chart.drawing_tool(), Some(kind), "{kind:?} must arm");
    }
    chart.set_drawing_tool(None);
    assert_eq!(chart.drawing_tool(), None);
}

#[test]
fn data_bound_profile_tools_read_the_product_price_and_volume_series() {
    let mut chart = interactive_chart();
    for (kind, anchors) in [
        (DrawingKind::FixedRangeVolumeProfile, 2),
        (DrawingKind::AnchoredVolumeProfile, 1),
        (DrawingKind::AnchoredVwap, 1),
    ] {
        chart.set_drawing_tool(Some(kind));
        for &x in [220.0, 360.0].iter().take(anchors) {
            click(&mut chart, x, 200.0);
        }
        let id = chart
            .selected_drawing_id()
            .expect("profile drawing commits");
        let profile = chart
            .engine
            .drawing(id)
            .and_then(|drawing| drawing.profile.clone())
            .expect("profile drawings bind a data source");
        assert_eq!(
            profile.source,
            aeris_charts_engine::ProfileSource::Candles {
                price_series: 0,
                volume_series: chart.volume_series,
            }
        );
        assert!(profile.tick_size > 0.0 && profile.tick_size.is_finite());
    }
}

#[test]
fn icon_stamps_place_the_chosen_registered_stamp() {
    let mut chart = interactive_chart();
    let mut x = 200.0;
    for stamp in ChartDrawingStamp::ALL {
        chart.set_drawing_stamp(stamp);
        assert_eq!(chart.drawing_tool(), Some(DrawingKind::IconStamp));
        click(&mut chart, x, 220.0);
        x += 32.0;
        let id = chart.selected_drawing_id().expect("stamp commits");
        assert_eq!(
            chart
                .engine
                .drawing(id)
                .and_then(|drawing| drawing.icon_name.as_deref()),
            Some(stamp.icon_name())
        );
    }
    assert_eq!(chart.drawing_count(), ChartDrawingStamp::ALL.len());
}

#[test]
fn semantic_drawing_state_round_trips_after_indicator_panes_are_recreated() {
    let mut source = interactive_chart();
    source
        .add_indicator(ChartIndicator::Rsi)
        .expect("RSI creates its oscillator pane");
    source.set_drawing_tool(Some(DrawingKind::HorizontalLine));
    click(&mut source, 300.0, 180.0);
    let main_id = source
        .selected_drawing_id()
        .expect("main drawing is selected");
    assert!(source.set_selected_drawing_locked(true));
    let oscillator_id = source
        .engine
        .add_drawing(
            DrawingKind::HorizontalLine,
            1,
            vec![aeris_charts_engine::DrawingPoint {
                logical: 10.0,
                price: 50.0,
            }],
            None,
        )
        .expect("oscillator drawing creates");
    source.engine.set_selected_drawing(Some(oscillator_id));
    assert!(source.set_selected_drawing_locked(true));
    let json = source
        .export_semantic_state_json()
        .expect("drawings export");
    let locked = source.locked_drawing_ids();
    assert!(locked.contains(&main_id));
    assert!(locked.contains(&oscillator_id));

    let mut restored = interactive_chart();
    restored
        .add_indicator(ChartIndicator::Rsi)
        .expect("RSI recreates its oscillator pane");
    restored
        .import_semantic_state_json(&json, &locked)
        .expect("drawings restore");

    assert_eq!(restored.drawing_count(), 2);
    assert_eq!(restored.engine.drawings()[0].pane_index, 0);
    assert_eq!(restored.engine.drawings()[1].pane_index, 1);
    assert_eq!(restored.drawings_lock_summary().locked_count, 2);
}

#[test]
fn semantic_drawing_restore_uses_saved_time_instead_of_old_bar_index() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
        .expect("embedded replay validates");
    let mut source = AerisChartView::with_replay(&replay);
    source
        .engine
        .add_drawing(
            DrawingKind::TrendLine,
            0,
            vec![
                aeris_charts_engine::DrawingPoint {
                    logical: 5.5,
                    price: 100.0,
                },
                aeris_charts_engine::DrawingPoint {
                    logical: 12.0,
                    price: 110.0,
                },
            ],
            None,
        )
        .expect("drawing creates");
    let mut state: serde_json::Value = serde_json::from_str(
        &source
            .export_semantic_state_json()
            .expect("drawings export"),
    )
    .expect("export is JSON");
    let saved_time = state[0]["points"][0]["aeris_anchor_time"]
        .as_f64()
        .expect("first anchor has exchange time");
    assert_eq!(source.product_bars.logical_at_time(saved_time), Some(5.5));

    let later_time = source
        .product_bars
        .time_at_logical(7.5)
        .expect("later anchor time");
    state[0]["points"][0]["aeris_anchor_time"] = serde_json::Value::from(later_time);
    let mut restored = AerisChartView::with_replay(&replay);
    restored
        .import_semantic_state_json(&state.to_string(), &[])
        .expect("drawings restore");
    assert!((restored.engine.drawings()[0].points[0].logical - 7.5).abs() < f64::EPSILON);
    assert!((restored.engine.drawings()[0].points[1].logical - 12.0).abs() < f64::EPSILON);
}

#[test]
fn ctrl_leaves_the_cursor_crosshair_raw_without_drawing_work() {
    let mut chart = interactive_chart();
    assert_eq!(chart.drawing_tool(), None);

    let x = chart.engine.time_scale.logical_to_coordinate(32.0);
    let y = 200.0;
    hover(&mut chart, x, y);
    let free = chart.engine.build_frame();
    chart.engine.input_modifiers_changed(CTRL);
    let held = chart.engine.build_frame();
    let crosshair_color = Color::parse_css(&chart.engine.options.get().crosshair.horz_line.color)
        .expect("the package crosshair color is valid");
    let crosshair_y = |frame: &ChartFrame| {
        frame.panes[0].main.iter().find_map(|prim| match prim {
            Prim::HLine { y, color, .. } if *color == crosshair_color => Some(*y),
            _ => None,
        })
    };

    assert_eq!(crosshair_y(&free), crosshair_y(&held));
    assert_eq!(chart.drawing_tool(), None);
}

#[test]
fn armed_ctrl_magnet_snaps_the_crosshair_without_a_preview_dot() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(Some(DrawingKind::TrendLine));
    assert!(!chart.engine.drawing_create_active());

    let x = chart.engine.time_scale.logical_to_coordinate(32.0);
    let y = 200.0;
    hover(&mut chart, x, y);
    let free = chart.engine.build_frame();
    chart.engine.input_modifiers_changed(CTRL);
    let held = chart.engine.build_frame();
    let crosshair_color = Color::parse_css(&chart.engine.options.get().crosshair.horz_line.color)
        .expect("the package crosshair color is valid");
    let crosshair_y = |frame: &ChartFrame| {
        frame.panes[0].main.iter().find_map(|prim| match prim {
            Prim::HLine { y, color, .. } if *color == crosshair_color => Some(*y),
            _ => None,
        })
    };

    // Ctrl/Cmd is the drawing magnet: with a tool armed it snaps to the bar's rendered prices.
    assert_ne!(crosshair_y(&free), crosshair_y(&held));
    assert_eq!(
        held.panes[0]
            .main
            .iter()
            .filter(|prim| matches!(prim, Prim::Circle { .. }))
            .count(),
        0,
        "arming a tool must not create a pre-click anchor handle"
    );
}

#[test]
fn aeris_charts_upgrade_hides_crosshair_during_creation_and_restores_it_on_cancel() {
    let mut chart = interactive_chart();
    let color = Color::parse_css(&chart.engine.options.get().crosshair.horz_line.color)
        .expect("crosshair color");
    let visible = |frame: &ChartFrame| {
        frame.panes[0].main.iter().any(
            |primitive| matches!(primitive, Prim::HLine { color: actual, .. } if *actual == color),
        )
    };
    hover(&mut chart, 300.0, 200.0);
    assert!(visible(&chart.engine.build_frame()));
    chart.set_drawing_tool(Some(DrawingKind::TrendLine));
    click(&mut chart, 300.0, 200.0);
    hover(&mut chart, 320.0, 220.0);
    assert!(
        chart.engine.crosshair.is_some(),
        "host pointer coordinates remain available"
    );
    assert!(!visible(&chart.engine.build_frame()));
    chart.cancel_drawing();
    hover(&mut chart, 300.0, 200.0);
    assert!(visible(&chart.engine.build_frame()));
}

#[test]
fn text_tool_place_enters_edit_mode_and_keeps_typed_label() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(Some(DrawingKind::Text));
    click(&mut chart, 300.0, 200.0);
    assert!(chart.is_editing_text());
    assert_eq!(chart.drawing_tool(), None);

    assert!(chart.engine.drawing_text_edit_insert("NQ"));
    assert_eq!(
        chart.engine.drawing_text_edit().map(|(_, text, _)| text),
        Some("NQ")
    );
    assert!(chart.finish_text_edit());
    assert!(!chart.is_editing_text());
    assert_eq!(chart.drawing_count(), 1);
    assert_eq!(chart.engine.drawings()[0].text, "NQ");
}

#[test]
fn trend_line_hover_and_first_label_click_edit_without_replacing_the_line() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(Some(DrawingKind::TrendLine));
    click(&mut chart, 260.0, 160.0);
    click(&mut chart, 340.0, 160.0);
    let id = chart.engine.drawings()[0].id;
    let (label_x, label_y, _) = chart.engine.drawing_text_transform(id).unwrap();
    let label_x = label_x - 20.0;
    assert!(chart.engine.hit_test_drawing(label_x, label_y).is_none());

    hover(&mut chart, label_x, label_y);
    assert_eq!(chart.engine.hovered_text(), Some(id));
    assert_eq!(cursor(&chart), ChartCursor::Text);
    assert!(chart.engine.build_frame().panes[0].main.iter().any(
        |primitive| matches!(primitive, Prim::RotatedText { text, .. } if text == "+ Add text")
    ));
    click(&mut chart, label_x, label_y);
    assert_eq!(
        chart
            .engine
            .drawing_text_edit()
            .map(|(editing, _, _)| editing),
        Some(id)
    );
    assert!(chart.engine.drawing_text_edit_insert("Breakout"));
    assert!(chart.finish_text_edit());
    assert_eq!(
        chart
            .engine
            .drawing(id)
            .map(|drawing| drawing.text.as_str()),
        Some("Breakout")
    );

    assert!(chart.engine.begin_drawing_text_edit(id, true));
    assert!(chart.engine.set_drawing_text_edit("Breakout!", 9));
    click(&mut chart, 260.0, 160.0);
    assert!(!chart.is_editing_text());
    assert_eq!(
        chart
            .engine
            .drawing(id)
            .map(|drawing| drawing.text.as_str()),
        Some("Breakout!")
    );

    chart.engine.input_pointer_leave();
    assert_eq!(chart.engine.hovered_text(), None);

    assert!(chart.engine.begin_drawing_text_edit(id, true));
    assert!(chart.engine.set_drawing_text_edit("", 0));
    assert!(chart.finish_text_edit());
    assert_eq!(chart.drawing_count(), 1);
    assert_eq!(
        chart
            .engine
            .drawing(id)
            .map(|drawing| drawing.text.as_str()),
        Some("")
    );
}

#[test]
fn pointer_exit_keeps_the_active_text_edit_session() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(Some(DrawingKind::Text));
    click(&mut chart, 300.0, 200.0);

    chart.engine.input_pointer_leave();
    chart.engine.input_cancel();

    assert!(chart.is_editing_text());
    assert!(chart.engine.drawing_text_edit_insert("ES"));
    assert_eq!(
        chart.engine.drawing_text_edit().map(|(_, text, _)| text),
        Some("ES")
    );
}

#[test]
fn text_edit_accepts_committed_altgr_and_multicharacter_input() {
    use aeris_charts_render_gpui::input::text_edit_key;
    use gpui::Modifiers;

    let mut chart = interactive_chart();
    chart.set_drawing_tool(Some(DrawingKind::Text));
    click(&mut chart, 300.0, 200.0);
    let event =
        |key: &str, text: &str, modifiers: Modifiers, prefer_character_input| KeyDownEvent {
            keystroke: gpui::Keystroke {
                modifiers,
                key: key.into(),
                key_char: Some(text.into()),
            },
            is_held: false,
            prefer_character_input,
        };
    text_edit_key(
        &mut chart.engine,
        &event(
            "q",
            "@",
            Modifiers {
                control: true,
                alt: true,
                ..Modifiers::default()
            },
            true,
        ),
    );
    text_edit_key(
        &mut chart.engine,
        &event("emoji", "👩‍💻", Modifiers::default(), false),
    );
    assert_eq!(
        chart.engine.drawing_text_edit().map(|(_, text, _)| text),
        Some("@👩‍💻")
    );
}

#[test]
fn activate_request_is_latched_until_taken() {
    let mut chart = interactive_chart();
    assert!(!chart.take_activate_request());
    chart.pending_activate = ActivationRequest::Pending;
    assert!(chart.take_activate_request());
    assert!(!chart.take_activate_request());
}

#[test]
fn unfinished_empty_text_edit_is_removed_on_finish() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(Some(DrawingKind::Text));
    click(&mut chart, 300.0, 200.0);
    assert!(chart.is_editing_text());
    assert!(chart.finish_text_edit());
    assert!(!chart.is_editing_text());
    assert_eq!(chart.drawing_count(), 0);
}

#[test]
fn brush_capture_commits_on_release_and_returns_to_cursor() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(Some(DrawingKind::Brush));

    drag(&mut chart, (240.0, 180.0), (320.0, 240.0));

    assert_eq!(chart.drawing_count(), 1);
    assert_eq!(chart.drawing_tool(), None);
    assert!(!chart.engine.brush_create_active());
}

#[test]
fn brush_capture_coalesces_pointer_samples_to_one_knot_per_flush() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(Some(DrawingKind::Brush));

    press(&mut chart, 100.0, 100.0);
    for offset in 1..=8 {
        let x = 100.0 + f64::from(offset);
        move_to(&mut chart, pointer(x, 100.0 + x), true);
    }
    assert!(
        chart.engine.flush_coalesced_input(),
        "the newest pending sample is captured once"
    );
    assert!(
        !chart.engine.flush_coalesced_input(),
        "an idle flush without a pending sample captures nothing"
    );
    release(&mut chart, 180.0, 180.0);

    let drawing = &chart.engine.drawings()[0];
    assert_eq!(drawing.kind, DrawingKind::Brush);
    assert_eq!(
        drawing.points.len(),
        3,
        "start + one coalesced move + release; intermediate staircase samples must not become knots"
    );
}

#[test]
fn path_stays_armed_until_enter_or_double_click_finishes() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(Some(DrawingKind::Path));
    assert!(
        !chart.engine.drawing_create_active(),
        "arming must not start a pre-click handle"
    );

    click(&mut chart, 260.0, 180.0);
    click(&mut chart, 340.0, 220.0);
    click(&mut chart, 400.0, 200.0);
    assert_eq!(chart.drawing_count(), 0);
    assert_eq!(chart.drawing_tool(), Some(DrawingKind::Path));
    assert!(key(&mut chart, ChartKey::Backspace));
    assert!(key(&mut chart, ChartKey::Enter));
    assert_eq!(chart.drawing_count(), 1);
    assert_eq!(chart.engine.drawings()[0].kind, DrawingKind::Path);
    assert_eq!(chart.engine.drawings()[0].points.len(), 2);
    assert_eq!(chart.drawing_tool(), None);

    chart.set_drawing_tool(Some(DrawingKind::Path));
    click(&mut chart, 260.0, 200.0);
    double_click(&mut chart, 340.0, 240.0);
    assert_eq!(chart.drawing_count(), 2);
    assert_eq!(chart.drawing_tool(), None);
    assert!(!chart.engine.drawing_create_active());
}

#[test]
fn cursor_selects_and_moves_unlocked_drawings_but_locked_drawings_do_not_drag() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(Some(DrawingKind::HorizontalLine));
    click(&mut chart, 300.0, 200.0);
    let id = chart.selected_drawing_id().expect("drawing selected");
    let (_, drawing_y) = chart
        .engine
        .drawing_point_to_coordinate(id, 0)
        .expect("drawing coordinate");
    let before = chart.engine.drawing(id).expect("drawing").points.clone();
    chart.set_drawing_tool(None);

    assert!(chart.set_selected_drawing_locked(true));
    drag(&mut chart, (500.0, drawing_y), (500.0, drawing_y + 30.0));
    assert_eq!(chart.engine.drawing(id).expect("drawing").points, before);
    assert_eq!(chart.selected_drawing_id(), Some(id));

    assert!(chart.set_selected_drawing_locked(false));
    press(&mut chart, 500.0, drawing_y);
    assert!(chart.engine.drawing_drag_active());
    assert_eq!(cursor(&chart), ChartCursor::Grabbing);
    move_to(&mut chart, pointer(500.0, drawing_y + 30.0), true);
    release(&mut chart, 500.0, drawing_y + 30.0);
    assert!(!chart.engine.drawing_drag_active());
    assert_ne!(chart.engine.drawing(id).expect("drawing").points, before);
}

#[test]
fn lock_summary_delete_clear_and_escape_follow_toolbar_contract() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(Some(DrawingKind::HorizontalLine));
    click(&mut chart, 300.0, 180.0);
    chart.set_drawing_tool(Some(DrawingKind::HorizontalLine));
    click(&mut chart, 300.0, 240.0);
    assert_eq!(chart.drawing_count(), 2);

    assert!(chart.set_all_drawings_locked(true));
    assert_eq!(
        chart.drawings_lock_summary(),
        DrawingsLockSummary {
            total: 2,
            locked_count: 2,
            all_locked: true,
        }
    );
    assert!(key(&mut chart, ChartKey::Delete));
    assert_eq!(chart.drawing_count(), 1);
    assert_eq!(chart.drawings_lock_summary().locked_count, 1);

    assert!(key(&mut chart, ChartKey::Escape));
    assert_eq!(chart.drawing_tool(), None);
    assert!(!chart.engine.drawing_create_active());
    chart.clear_drawings();
    assert_eq!(
        chart.drawings_lock_summary(),
        DrawingsLockSummary::default()
    );
    assert!(!key(&mut chart, ChartKey::Backspace));
}

#[test]
fn drawing_history_steps_back_and_forward_and_keeps_the_armed_tool() {
    let mut chart = interactive_chart();
    assert!(!chart.can_undo_drawing());
    assert!(!chart.can_redo_drawing());
    assert!(!chart.undo_drawing());

    let revision = chart.user_state_revision();
    chart.set_drawing_tool(Some(DrawingKind::HorizontalLine));
    click(&mut chart, 300.0, 180.0);
    chart.set_drawing_tool(Some(DrawingKind::HorizontalLine));
    click(&mut chart, 300.0, 240.0);
    assert_eq!(chart.drawing_count(), 2);
    assert!(chart.can_undo_drawing());
    assert!(
        chart.user_state_revision() > revision,
        "committed drawings dirty durable state"
    );

    chart.set_drawing_tool(Some(DrawingKind::HorizontalLine));
    let revision = chart.user_state_revision();
    assert!(chart.undo_drawing());
    assert!(chart.user_state_revision() > revision);
    assert_eq!(chart.drawing_count(), 1);
    assert!(chart.can_redo_drawing());
    assert_eq!(chart.drawing_tool(), Some(DrawingKind::HorizontalLine));
    assert!(
        !chart.engine.drawing_create_active(),
        "stepping history must keep the tool selected without a pre-click handle"
    );

    assert!(chart.redo_drawing());
    assert_eq!(chart.drawing_count(), 2);
    assert!(!chart.can_redo_drawing());
    assert!(!chart.redo_drawing());
}

#[test]
fn cursor_mode_still_falls_through_to_chart_pan_on_a_drawing_miss() {
    let mut chart = interactive_chart();
    let offset = chart.engine.right_offset();
    drag(&mut chart, (300.0, 200.0), (360.0, 200.0));
    assert!((chart.engine.right_offset() - offset).abs() > f64::EPSILON);
    assert_eq!(chart.drawing_count(), 0);
}

#[test]
fn keyboard_navigation_scrolls_zooms_resets_and_ignores_unknown_keys() {
    let mut chart = interactive_chart();
    let offset = chart.engine.scroll_position();
    // Arrow pans are velocity-owned: the key starts an engine animation that key-up ends.
    assert!(key(&mut chart, ChartKey::ArrowRight));
    assert!(chart.engine.input_animating());
    chart.engine.input_tick(200.0);
    assert!(chart.engine.scroll_position() > offset);
    assert!(chart.engine.input_key_up(ChartKey::ArrowRight));
    assert!(!chart.engine.input_animating());

    let before_page = chart.engine.scroll_position();
    assert!(key(&mut chart, ChartKey::PageUp));
    let after_page_up = chart.engine.scroll_position();
    assert!(after_page_up < before_page);
    assert!(key(&mut chart, ChartKey::PageDown));
    assert!(chart.engine.scroll_position() > after_page_up);

    let spacing = chart.engine.bar_spacing();
    assert!(key(&mut chart, ChartKey::ZoomIn));
    assert!((chart.engine.bar_spacing() - spacing).abs() > f64::EPSILON);

    hover(&mut chart, 100.0, 100.0);
    assert!(chart.engine.crosshair.is_some());
    assert!(key(&mut chart, ChartKey::Escape));
    assert!(chart.engine.crosshair.is_none());
    assert!(key(&mut chart, ChartKey::Home));
    let reset_margin = chart.engine.pane_w * 0.10 / chart.engine.bar_spacing();
    assert!((chart.engine.scroll_position() - reset_margin).abs() < f64::EPSILON);
    chart.engine.scroll_to_position(-4.0);
    assert!(!chart.is_at_latest());
    assert!(key(&mut chart, ChartKey::End));
    assert!(chart.is_at_latest());
    assert!(aeris_charts_render_gpui::input::chart_key("a").is_none());
}

#[test]
fn native_pointer_state_ends_a_drag_when_mouse_up_was_lost() {
    let mut chart = interactive_chart();
    press(&mut chart, 300.0, 200.0);
    move_to(&mut chart, pointer(330.0, 200.0), true);
    assert_eq!(cursor(&chart), ChartCursor::Grabbing);

    move_to(&mut chart, pointer(340.0, 200.0), false);

    assert_eq!(cursor(&chart), ChartCursor::Crosshair);
    assert_eq!(chart.engine.crosshair, Some((340.0, 200.0)));
}

#[test]
fn host_modal_suspension_clears_crosshair_and_active_pointer_gestures() {
    let mut chart = interactive_chart();
    press(&mut chart, 300.0, 200.0);
    move_to(&mut chart, pointer(330.0, 200.0), true);
    assert!(chart.engine.crosshair.is_some());
    assert_eq!(cursor(&chart), ChartCursor::Grabbing);

    chart.suspend_pointer_interaction();

    assert!(chart.engine.crosshair.is_none());
    assert!(chart.engine.separator_hover.is_none());
    assert_eq!(
        chart.pointer_interaction,
        PointerInteractionState::Suspended
    );
    assert_eq!(chart.cursor_style(), CursorStyle::Arrow);
    let offset = chart.engine.right_offset();
    move_to(&mut chart, pointer(380.0, 200.0), true);
    assert_eq!(
        chart.engine.right_offset().to_bits(),
        offset.to_bits(),
        "a suspended gesture never resumes"
    );

    chart.resume_pointer_interaction();
    assert_eq!(chart.pointer_interaction, PointerInteractionState::Active);
    assert_eq!(chart.cursor_style(), CursorStyle::Crosshair);
}

#[test]
fn platform_crosshair_time_label_keeps_time_of_day_visible() {
    let mut chart = interactive_chart();
    let time = chart
        .engine
        .series_data(0)
        .last()
        .and_then(|bar| bar.time.to_f64())
        .expect("replay has a representable timestamp");
    let x = chart
        .engine
        .time_to_coordinate(time)
        .expect("replay timestamp is visible");
    chart.engine.crosshair = Some((x, 100.0));
    let measure =
        |text: &str, _bold: bool| f64::from(u32::try_from(text.len()).unwrap_or(u32::MAX)) * 7.0;

    let label = chart
        .engine
        .build_axis_frame(80.0, measure, measure)
        .labels
        .into_iter()
        .find(|label| label.midpoint == AxisTextMidpoint::StableTime)
        .expect("crosshair time label is present");

    assert!(chart.engine.time_visible);
    assert!(!chart.engine.seconds_visible);
    assert!(label.text.contains(':'), "time missing from {}", label.text);
    assert_ne!(
        label.text.split_whitespace().last(),
        Some("00:00"),
        "intraday crosshair collapsed to midnight in {}",
        label.text
    );
}

#[test]
fn platform_crosshair_time_label_omits_midnight_for_weekly_bars() {
    let baseline = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 3 })
        .expect("fixture snapshot");
    let week_starts = [1_735_689_600_i64, 1_736_294_400, 1_736_899_200];
    let bars = baseline
        .bars()
        .iter()
        .zip(week_starts)
        .map(|(item, timestamp)| {
            let mut bar = *item.value();
            bar.exchange_timestamp_seconds = timestamp;
            bar.exchange_timestamp_unix_nanos = timestamp.saturating_mul(1_000_000_000);
            bar
        })
        .collect();
    let mut definition = baseline.bar_definition().clone();
    definition.definition_id = "fixture:calendar-weeks:1".to_string();
    definition.interval_seconds = 7 * 86_400;
    definition.trades_per_bar = None;
    definition.calendar_months = None;
    let replay = ReplaySnapshot::try_new(
        baseline.instrument().clone(),
        baseline.provenance(),
        definition,
        bars,
    )
    .expect("weekly snapshot validates");
    let mut chart = AerisChartView::with_replay(&replay);
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    chart.engine.fit_content();
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    let time = week_starts[2]
        .to_f64()
        .expect("weekly timestamp is representable");
    let x = chart
        .engine
        .time_to_coordinate(time)
        .expect("weekly timestamp is visible");
    chart.engine.crosshair = Some((x, 100.0));
    let measure =
        |text: &str, _bold: bool| f64::from(u32::try_from(text.len()).unwrap_or(u32::MAX)) * 7.0;

    let label = chart
        .engine
        .build_axis_frame(80.0, measure, measure)
        .labels
        .into_iter()
        .find(|label| label.midpoint == AxisTextMidpoint::StableTime)
        .expect("weekly crosshair date label is present");

    assert!(!chart.engine.time_visible);
    assert!(!chart.engine.seconds_visible);
    assert!(
        !label.text.contains(':'),
        "weekly label shows time: {}",
        label.text
    );
    chart.set_theme(ChartTheme::Light);
    assert!(!chart.engine.time_visible);
}

#[test]
fn indicator_separator_resize_has_bounded_native_pointer_state() {
    let mut chart = interactive_chart();
    chart
        .add_indicator(ChartIndicator::Rsi)
        .expect("RSI creates its indicator pane");
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    assert_eq!(chart.engine.panes.len(), 2);
    let separator_y = chart.engine.panes[1].top;
    let first_stretch = chart.engine.panes[0].stretch_factor;
    let first_height = chart.engine.panes[0].height;

    hover(&mut chart, 300.0, separator_y);
    assert_eq!(cursor(&chart), ChartCursor::ResizeRow);
    assert_eq!(chart.engine.separator_hover, Some(0));

    press(&mut chart, 300.0, separator_y);
    assert!(chart.engine.crosshair.is_none());
    move_to(&mut chart, pointer(300.0, separator_y + 20.0), true);
    assert!(chart.engine.panes[0].stretch_factor > first_stretch);
    // Mouse events may arrive faster than chart frames. The latest pointer
    // position must win even before a layout pass updates pane heights.
    move_to(&mut chart, pointer(300.0, separator_y + 40.0), true);
    assert!((chart.engine.panes[0].stretch_factor - (first_height + 40.0)).abs() < 1e-6);
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    assert!((chart.engine.panes[1].top - (separator_y + 40.0)).abs() < 1.0);

    move_to(&mut chart, pointer(300.0, separator_y + 40.0), false);
    assert_eq!(cursor(&chart), ChartCursor::ResizeRow);
}

#[test]
fn escape_cancels_every_active_gesture_and_clears_pointer_state() {
    let mut chart = interactive_chart();
    let time_y = chart.engine.pane_h + 10.0;
    press(&mut chart, 300.0, time_y);
    move_to(&mut chart, pointer(320.0, time_y), true);
    assert_eq!(cursor(&chart), ChartCursor::ResizeHorizontal);

    assert!(key(&mut chart, ChartKey::Escape));

    let spacing = chart.engine.bar_spacing();
    move_to(&mut chart, pointer(360.0, time_y), true);
    assert_eq!(
        chart.engine.bar_spacing().to_bits(),
        spacing.to_bits(),
        "the scale session ended"
    );
    assert!(chart.engine.crosshair.is_none());
}

#[test]
fn pointer_cursor_truthfully_tracks_chart_and_axis_gestures() {
    let mut chart = interactive_chart();
    hover(&mut chart, 300.0, 200.0);
    assert_eq!(cursor(&chart), ChartCursor::Crosshair);
    let time_y = chart.engine.pane_h + 1.0;
    hover(&mut chart, 300.0, time_y);
    assert_eq!(cursor(&chart), ChartCursor::ResizeHorizontal);
    let axis_x = chart.engine.pane_w + 1.0;
    hover(&mut chart, axis_x, 200.0);
    assert_eq!(cursor(&chart), ChartCursor::ResizeVertical);

    press(&mut chart, 300.0, 200.0);
    move_to(&mut chart, pointer(330.0, 200.0), true);
    assert_eq!(cursor(&chart), ChartCursor::Grabbing);
    release(&mut chart, 330.0, 200.0);
    assert_eq!(cursor(&chart), ChartCursor::Crosshair);
}

#[test]
fn chart_cursors_map_to_visible_native_gpui_cursors() {
    use aeris_charts_render_gpui::input::cursor_style;
    #[cfg(target_os = "windows")]
    {
        assert_eq!(
            cursor_style(ChartCursor::VerticalGrab),
            CursorStyle::ResizeUpDown
        );
        assert_eq!(
            cursor_style(ChartCursor::VerticalGrabbing),
            CursorStyle::ResizeUpDown
        );
    }
    #[cfg(not(target_os = "windows"))]
    {
        assert_eq!(
            cursor_style(ChartCursor::VerticalGrab),
            CursorStyle::OpenHand
        );
        assert_eq!(
            cursor_style(ChartCursor::VerticalGrabbing),
            CursorStyle::ClosedHand
        );
    }
    // GPUI names diagonal cursors geometrically; `nwse` runs up-left to down-right.
    assert_eq!(
        cursor_style(ChartCursor::ResizeNwse),
        CursorStyle::ResizeUpLeftDownRight
    );
    assert_eq!(
        cursor_style(ChartCursor::ResizeNesw),
        CursorStyle::ResizeUpRightDownLeft
    );
}

fn price_series_values(chart: &AerisChartView) -> (Vec<i64>, Vec<f64>) {
    let (times, columns) = chart
        .engine
        .data_layer()
        .series_data(0)
        .expect("price data");
    (
        times.to_vec(),
        columns
            .iter()
            .flat_map(|column| column.iter().copied())
            .collect(),
    )
}

fn asset_legend_values(chart: &AerisChartView) -> Vec<String> {
    chart
        .legend_rows()
        .into_iter()
        .find(|row| row.item == LegendItem::Asset)
        .expect("asset row")
        .values
        .into_iter()
        .map(|value| value.text)
        .collect()
}

fn footprint_chart() -> AerisChartView {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
        .expect("embedded replay validates");
    let mut chart = AerisChartView::empty();
    chart.load_replay(&replay).expect("snapshot installs");
    chart.set_chart_type(ChartType::Footprint);
    chart
}

/// Applies a tape that covers only the chart's last bar.
fn apply_last_bar_tape(chart: &mut AerisChartView) -> (OrderFlowAggregation, Vec<OrderFlowTrade>) {
    let (times, _) = price_series_values(chart);
    let spacing = times[1] - times[0];
    let last_micros = *times.last().expect("bars") * 1_000_000;
    let trades = vec![
        order_flow_trade(1, last_micros, 2.0),
        order_flow_trade(2, last_micros + 1, 3.0),
    ];
    let aggregation =
        OrderFlowAggregation::TimeMicros(u64::try_from(spacing * 1_000_000).expect("spacing"));
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &trades)
        .expect("tape applies");
    (aggregation, trades)
}

#[test]
fn footprint_draws_only_bars_the_trade_tape_covers() {
    let mut chart = footprint_chart();
    let price = series_entry(&chart, 0);
    assert_eq!(price.kind, aeris_charts_engine::SeriesKind::Candlestick);
    assert!(
        price.visible,
        "the price series keeps the bar grid and time axis"
    );
    let (times, values) = price_series_values(&chart);
    assert!(
        values.iter().all(|value| value.is_nan()),
        "a footprint price series is whitespace, so no candle exists to draw"
    );
    assert_eq!(asset_legend_values(&chart), ["Waiting for order flow"]);

    apply_last_bar_tape(&mut chart);
    assert_eq!(series_entry(&chart, 0).render_before_time, None);
    assert_eq!(
        chart
            .engine
            .footprint_bars(chart.footprint_series_id().expect("footprint series"))
            .expect("footprint bars")
            .len(),
        1
    );
    let footprint = chart.footprint_series_id().expect("footprint series");
    let footprint_entry = series_entry(&chart, footprint);
    assert!(
        footprint_entry.last_value_visible
            && footprint_entry.price_line_visible
            && footprint_entry.countdown_visible,
        "the footprint owns the price chrome over its whitespace price series"
    );
    let read_out = asset_legend_values(&chart);
    assert_eq!(
        read_out.len(),
        4,
        "the legend reads the footprint bar's OHLC"
    );
    assert!(
        read_out.iter().all(|value| !value.ends_with("--")),
        "{read_out:?}"
    );

    chart.set_chart_type(ChartType::Candles);
    assert!(chart.footprint_series_id().is_none());
    let (candle_times, candle_values) = price_series_values(&chart);
    assert_eq!(candle_times, times);
    assert!(
        candle_values.iter().all(|value| value.is_finite()),
        "leaving the footprint restores the product OHLC"
    );
}

#[test]
fn footprint_legend_keeps_one_price_row_and_order_flow_rows_follow_settings() {
    let mut chart = footprint_chart();
    let (aggregation, trades) = apply_last_bar_tape(&mut chart);
    let rows = chart.legend_rows();
    let price_rows = rows
        .iter()
        .filter(|row| row.item == LegendItem::Asset)
        .collect::<Vec<_>>();
    assert_eq!(price_rows.len(), 1);
    assert!(price_rows[0].visible);
    assert!(
        !rows
            .iter()
            .any(|row| matches!(row.item, LegendItem::OrderFlow(_)))
    );
    assert!(!chart.has_indicators());

    let footprint = enable_cvd_and_delta(&mut chart, aggregation, &trades);
    let rows = chart.legend_rows();
    assert!(rows.iter().any(|row| {
        row.item == LegendItem::OrderFlow(OrderFlowStudy::CumulativeDelta) && row.title == "CVD"
    }));
    assert!(
        rows.iter()
            .any(|row| row.item == LegendItem::OrderFlow(OrderFlowStudy::Delta))
    );

    assert!(chart.set_legend_item_visible(LegendItem::Asset, false));
    assert!(!series_entry(&chart, 0).visible);
    assert!(!series_entry(&chart, footprint).visible);
    assert!(chart.set_legend_item_visible(LegendItem::Asset, true));

    assert!(chart.set_legend_item_visible(LegendItem::OrderFlow(OrderFlowStudy::Delta), false));
    assert!(chart.remove_legend_indicator(LegendItem::OrderFlow(OrderFlowStudy::Delta)));
    assert!(!chart.order_flow_settings().show_delta_histogram);
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &trades)
        .expect("tape rebuilds after the settings change");
    let rows = chart.legend_rows();
    assert!(
        !rows
            .iter()
            .any(|row| row.item == LegendItem::OrderFlow(OrderFlowStudy::Delta))
    );

    // Clearing indicators turns order-flow studies off through settings and never
    // deletes the footprint itself.
    assert!(chart.clear_indicators());
    assert!(!chart.order_flow_settings().show_cumulative_delta);
    chart
        .apply_order_flow_trades("instrument:test", 7, aggregation, 0.25, &trades)
        .expect("tape rebuilds after clearing indicators");
    assert!(chart.footprint_series_id().is_some());
    assert!(!chart.has_indicators());
}

#[test]
fn fresh_market_opens_on_latest_bars_keeping_warm_up_history_off_screen() {
    let fitted_chart = |bar_count: usize| {
        let replay = EmbeddedReplaySource
            .load_snapshot(LoadEmbeddedReplay { bar_count })
            .expect("embedded replay");
        let mut chart = AerisChartView::with_replay(&replay);
        chart
            .engine
            .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
        chart.engine.fit_content();
        chart
    };

    let mut chart = fitted_chart(1_600);
    let (_, fitted_right) = chart.engine.visible_logical_range().expect("fitted range");
    assert!(chart.narrow_to_initial_window());
    let (left, right) = chart
        .engine
        .visible_logical_range()
        .expect("narrowed range");
    assert!(
        (right - fitted_right).abs() <= 1.0,
        "the latest bar stays in view"
    );
    assert!(right - left <= INITIAL_VISIBLE_BARS + 1.0);
    assert!(left >= 900.0, "warm-up history stays left of the viewport");
    assert!(!chart.narrow_to_initial_window());

    let mut short = fitted_chart(400);
    assert!(!short.narrow_to_initial_window());
}

#[test]
fn study_line_width_applies_to_current_and_later_outputs_and_clears_on_removal() {
    let mut chart = AerisChartView::empty();
    let timestamps = [60_i64 * 1_000_000_000, 120_i64 * 1_000_000_000];
    let values = [Some(10.0), Some(11.0)];
    let descriptor = ChartStudyOutputDescriptor {
        title: "Ribbon",
        legend_label: None,
        plot: ChartStudyPlotKind::Line,
        pane: ChartStudyPaneTarget::Price,
        scale: ChartStudyScaleTarget::Primary,
        settings_available: true,
        threshold_region: None,
        point_style: ChartStudyPointStyle::Uniform,
        input_requirements: ChartStudyInputRequirements::BARS,
    };
    let width_of = |chart: &AerisChartView, output_index: usize| {
        series_entry(chart, study_output(chart, 9, output_index).series_id).line_width
    };

    assert_eq!(
        chart.install_study_output(9, 0, descriptor, 1, &timestamps, &values),
        Ok(true)
    );
    assert_eq!(chart.study_line_width(9), DEFAULT_STUDY_LINE_WIDTH);
    assert!(chart.set_study_line_width(9, 4));
    assert!(!chart.set_study_line_width(9, 4));
    assert_eq!(width_of(&chart, 0), Some(4.0));

    assert_eq!(
        chart.install_study_output(9, 1, descriptor, 1, &timestamps, &values),
        Ok(true)
    );
    assert_eq!(width_of(&chart, 1), Some(4.0));

    assert!(chart.set_study_line_width(9, 1));
    assert!(
        chart.set_study_line_width(9, 9),
        "out-of-range widths clamp to the maximum"
    );
    assert_eq!(chart.study_line_width(9), MAXIMUM_STUDY_LINE_WIDTH);

    assert!(chart.remove_study_outputs(&[9]));
    assert_eq!(chart.study_line_width(9), DEFAULT_STUDY_LINE_WIDTH);
}

#[test]
fn appearance_reset_keeps_host_study_line_widths() {
    let mut chart = AerisChartView::empty();
    let timestamps = [60_i64 * 1_000_000_000, 120_i64 * 1_000_000_000];
    let values = [Some(10.0), Some(11.0)];
    let descriptor = ChartStudyOutputDescriptor {
        title: "Ribbon",
        legend_label: None,
        plot: ChartStudyPlotKind::Line,
        pane: ChartStudyPaneTarget::Price,
        scale: ChartStudyScaleTarget::Primary,
        settings_available: true,
        threshold_region: None,
        point_style: ChartStudyPointStyle::Uniform,
        input_requirements: ChartStudyInputRequirements::BARS,
    };
    chart.set_study_line_width(9, 1);
    assert_eq!(
        chart.install_study_output(9, 0, descriptor, 1, &timestamps, &values),
        Ok(true)
    );
    let series_id = study_output(&chart, 9, 0).series_id;
    assert_eq!(series_entry(&chart, series_id).line_width, Some(1.0));

    chart.reset_appearance_settings();

    assert_eq!(
        series_entry(&chart, series_id).line_width,
        Some(1.0),
        "resetting chart styling must not revert a study to the engine's default stroke"
    );
}
