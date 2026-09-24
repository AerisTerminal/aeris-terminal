#![cfg(test)]

use super::*;
use aeris_application::{Provenanced, ReplayTailOperation, ReplayTailUpdate};
use nucleuscharts_engine::AxisTextMidpoint;

fn interactive_chart() -> NucleusChartView {
    let mut chart = NucleusChartView::new();
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

fn series_entry(chart: &NucleusChartView, id: u32) -> &nucleuscharts_engine::SeriesEntry {
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

fn assert_nucleus_theme(chart: &NucleusChartView, theme: ChartTheme) {
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

fn visible_series_point(chart: &NucleusChartView, id: u32) -> (f64, f64) {
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
fn crosshair_alert_action_reaches_the_host_with_nucleus_price_context() {
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
    chart.engine.crosshair = Some((chart.engine.pane_w / 2.0, y));
    let action_x = (-100..=4_096)
        .map(f64::from)
        .find(|x| chart.engine.alert_create_hit_at(*x, y))
        .expect("alert action is hit-testable");
    chart.update_cursor(action_x, y);
    assert_eq!(chart.cursor_style, CursorStyle::PointingHand);
    assert!(chart.engine.activate_alert_create_at(action_x, y));
    let requests = chart.take_alert_create_requests();
    assert_eq!(requests.len(), 1);
    assert!((requests[0].price - price).abs() < f64::EPSILON);
    assert_eq!(
        requests[0].condition,
        nucleuscharts_engine::AlertCondition::Crossing
    );
}

#[test]
fn empty_chart_surface_accepts_its_first_real_snapshot() {
    let mut chart = NucleusChartView::empty();
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
fn covering_forming_snapshot_recovers_the_nucleus_view_without_a_new_candle() {
    let baseline = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .expect("replay");
    let mut chart = NucleusChartView::with_replay(&baseline);
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
    let mut chart = NucleusChartView::empty();
    assert_eq!(chart.chart_type(), ChartType::Candles);
    chart.set_chart_type(ChartType::Line);
    assert_eq!(chart.chart_type(), ChartType::Line);
    assert_eq!(
        series_entry(&chart, 0).kind,
        nucleuscharts_engine::SeriesKind::Line
    );

    chart.load_replay(&replay).expect("first snapshot installs");
    assert_eq!(chart.chart_type(), ChartType::Line);
    assert_eq!(
        series_entry(&chart, 0).kind,
        nucleuscharts_engine::SeriesKind::Line
    );
    assert_eq!(
        series_entry(&chart, chart.volume_series).kind,
        nucleuscharts_engine::SeriesKind::Histogram
    );
}

#[test]
fn selected_price_precision_survives_snapshot_install_and_restores_a_replacement_chart() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 16 })
        .expect("embedded replay validates");
    let mut chart = NucleusChartView::empty();
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
    let mut replacement = NucleusChartView::with_replay(&replay);
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
    let mut chart = NucleusChartView::with_replay(&replay);
    let original = chart.engine.series_data(0);
    assert!(!original.is_empty());
    let original_high = original[0].high;

    chart.set_chart_type(ChartType::BrushableArea);
    assert_eq!(chart.chart_type(), ChartType::BrushableArea);
    assert_eq!(
        series_entry(&chart, 0).kind,
        nucleuscharts_engine::SeriesKind::Area
    );
    assert_eq!(chart.engine.feature_series_kind(0), None);
    let nucleus_line_width = serde_json::from_str::<serde_json::Value>(
        &chart
            .engine
            .series_options_json(0)
            .expect("brushable options"),
    )
    .expect("brushable options are JSON")["line_width"]
        .as_f64()
        .expect("Nucleus supplies a brushable line width");
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    let start = chart.engine.time_scale.index_to_coordinate(2);
    let end = chart.engine.time_scale.index_to_coordinate(8);
    chart.begin_drag(start, 200.0, 1, false);
    assert!(matches!(chart.drag, Some(ChartDrag::Pane { .. })));
    chart.end_drag(start, 200.0);
    chart.begin_drag(start, 200.0, 1, true);
    assert_eq!(chart.drag, Some(ChartDrag::BrushableRange));
    chart.drag_to(end, 200.0);
    chart.end_drag(end, 200.0);
    assert!(chart.drag.is_none());
    let selected = chart
        .brushable_tooltip
        .and_then(|id| chart.engine.delta_tooltip_active_range(id))
        .expect("shift-drag installs a comparison range");
    assert!(selected.from < selected.to);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &chart.engine.series_options_json(0).expect("area options")
        )
        .expect("area options are JSON")["line_width"]
            .as_f64(),
        Some(nucleus_line_width)
    );

    chart.set_chart_type(ChartType::Candles);
    assert_eq!(
        series_entry(&chart, 0).kind,
        nucleuscharts_engine::SeriesKind::Candlestick
    );
    assert_eq!(
        chart.engine.series_data(0)[0].high.to_bits(),
        original_high.to_bits()
    );
}

#[test]
fn calendar_month_snapshot_reaches_nucleus_with_variable_month_spacing() {
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

    let chart = NucleusChartView::with_replay(&replay);
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
    let mut chart = NucleusChartView::new();
    chart.built_for = (1280.0, 720.0, 1.25);
    chart.layout_dirty = false;

    chart.invalidate_series_layout();

    assert_eq!(chart.built_for, (1280.0, 720.0, 1.25));
    assert!(chart.layout_dirty);
    assert!(chart.frame.panes.is_empty());
    assert!(chart.axis_prims.is_empty());
}

#[test]
fn mouse_up_out_finishes_chart_gesture_without_stopping_window_propagation() {
    assert!(should_stop_mouse_up_propagation(false));
    assert!(!should_stop_mouse_up_propagation(true));
}

#[test]
fn occluded_mouse_up_inside_chart_is_not_geometrically_outside() {
    let mut chart = interactive_chart();
    chart.viewport_origin = (100.0, 80.0);
    chart.built_for = (640.0, 360.0, 1.0);

    assert!(chart.position_is_inside_viewport(gpui::point(px(420.0), px(240.0))));
    assert!(chart.position_is_inside_viewport(gpui::point(px(100.0), px(80.0))));
    assert!(chart.position_is_inside_viewport(gpui::point(px(740.0), px(440.0))));
    assert!(!chart.position_is_inside_viewport(gpui::point(px(99.0), px(240.0))));
    assert!(!chart.position_is_inside_viewport(gpui::point(px(420.0), px(441.0))));
}

#[test]
fn chart_applies_live_tail_replace_and_append_in_one_frame_boundary() {
    let replay = EmbeddedReplaySource
        .load_snapshot(LoadEmbeddedReplay { bar_count: 2 })
        .expect("embedded replay validates");
    let mut chart = NucleusChartView::with_replay(&replay);
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
    let mut chart = NucleusChartView::with_replay(&replay);

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
        .expect("clamped Nucleus viewport")
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
fn nucleus_theme_owns_chart_cosmetics_and_series_defaults() {
    let chart = NucleusChartView::empty();
    let series = &chart.engine.series[0];
    assert_nucleus_theme(&chart, ChartTheme::Dark);
    assert!(series.line_color.is_none());
    assert!(series.up_color.is_none());
    assert!(series.down_color.is_none());
    assert!(series.wick_up_color.is_none());
    assert!(series.wick_down_color.is_none());
    assert!(series.border_up_color.is_none());
    assert!(series.border_down_color.is_none());
}

#[test]
fn chart_legend_text_colors_project_platform_chrome_and_nucleus_market_colors() {
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
    let mut chart = NucleusChartView::empty();
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
fn price_axis_menu_controls_nucleus_series_chrome_and_scale() {
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
fn nucleus_theme_switch_is_atomic_for_data_viewport_drawings_and_indicators() {
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
    assert_nucleus_theme(&chart, ChartTheme::Light);
    chart.set_theme(ChartTheme::Dark);
    assert_nucleus_theme(&chart, ChartTheme::Dark);
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
        chart.engine.indicator_info(vwap[0]).map(|info| info.kind),
        Some("vwap")
    );
}

#[test]
fn wheel_zoom_and_horizontal_scroll_mutate_nucleus_without_refitting() {
    let mut chart = interactive_chart();
    let spacing = chart.engine.bar_spacing();
    chart.apply_wheel(400.0, 200.0, 0.0, 1.0);
    assert!((chart.engine.bar_spacing() - spacing).abs() > f64::EPSILON);
    let offset = chart.engine.right_offset();
    chart.apply_wheel(400.0, 200.0, 1.0, 0.0);
    assert!((chart.engine.right_offset() - offset).abs() > f64::EPSILON);
    assert!(chart.fitted);
}

#[test]
fn mouse_pan_and_crosshair_have_bounded_lifecycle() {
    let mut chart = interactive_chart();
    chart.begin_drag(300.0, 200.0, 1, false);
    assert_eq!(chart.drag, Some(ChartDrag::Pane { price_pan: None }));
    assert_eq!(chart.engine.crosshair, Some((300.0, 200.0)));
    let offset = chart.engine.right_offset();
    chart.drag_to(340.0, 200.0);
    assert!((chart.engine.right_offset() - offset).abs() > f64::EPSILON);
    chart.end_drag(340.0, 200.0);
    assert!(chart.drag.is_none());
    chart.update_crosshair(-1.0, 200.0);
    assert!(chart.engine.crosshair.is_none());
}

#[test]
fn pane_copy_price_uses_nucleus_chart_context() {
    let chart = interactive_chart();
    let (x, y) = visible_series_point(&chart, 0);
    let context = chart
        .engine
        .chart_context_at(x, y)
        .expect("pane click has Nucleus context");
    let expected = chart
        .engine
        .series_format_price(0, context.price)
        .expect("asset price format");
    let request = chart.context_menu_request(gpui::point(gpui::px(0.0), gpui::px(0.0)), x, y);
    assert_eq!(request.kind, ChartContextKind::Pane);
    assert_eq!(request.copy_price.as_deref(), Some(expected.as_str()));
    assert_eq!(
        chart.formatted_copy_price(x, y).as_deref(),
        Some(expected.as_str())
    );
}

#[test]
fn price_axis_context_menu_does_not_copy_price() {
    let chart = interactive_chart();
    let axis_x = chart.engine.pane_w + 1.0;
    let request =
        chart.context_menu_request(gpui::point(gpui::px(0.0), gpui::px(0.0)), axis_x, 200.0);
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
    let chart = NucleusChartView::empty();
    assert!(chart.formatted_copy_price(100.0, 200.0).is_none());
}

#[test]
fn axes_drag_and_double_click_reset_through_nucleus() {
    let mut chart = interactive_chart();
    chart.begin_drag(300.0, chart.engine.pane_h + 10.0, 1, false);
    assert_eq!(chart.drag, Some(ChartDrag::TimeAxis));
    chart.drag_to(340.0, chart.engine.pane_h + 10.0);
    chart.end_drag(340.0, chart.engine.pane_h + 10.0);
    assert!(chart.drag.is_none());

    let right_axis_x = chart.engine.pane_w + 1.0;
    assert_eq!(
        chart
            .engine
            .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
        Some(true)
    );
    chart.begin_drag(right_axis_x, 200.0, 1, false);
    assert!(matches!(
        chart.drag,
        Some(ChartDrag::PriceAxis {
            target: PriceScaleTarget::Right,
            ..
        })
    ));
    assert_eq!(
        chart
            .engine
            .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
        Some(false)
    );
    chart.drag_to(right_axis_x, 240.0);
    assert_eq!(
        chart
            .engine
            .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
        Some(false)
    );
    chart.end_drag(right_axis_x, 240.0);
    assert!(chart.drag.is_none());
    assert_eq!(
        chart
            .engine
            .price_scale_auto_scale_for(0, PriceScaleTarget::Right),
        Some(false)
    );

    let locked_range = chart
        .engine
        .price_scale_visible_range_for(0, PriceScaleTarget::Right);
    chart.begin_drag(300.0, 200.0, 1, false);
    assert!(matches!(
        chart.drag,
        Some(ChartDrag::Pane {
            price_pan: Some((_, PriceScaleTarget::Right))
        })
    ));
    chart.drag_to(300.0, 260.0);
    chart.end_drag(300.0, 260.0);
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
    chart.begin_drag(300.0, chart.engine.pane_h + 10.0, 2, false);
    assert!(chart.engine.right_offset().abs() < offset.abs());
    assert!(chart.drag.is_none());
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
    assert!((chart.engine.bar_spacing() - spacing).abs() < f64::EPSILON);
}

#[test]
fn indicator_catalog_maps_to_nucleus_with_legacy_defaults() {
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
        .expect("MACD binds to nucleus");
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
    let asset_values = |chart: &NucleusChartView| {
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
        grid_color: "#334155".to_string(),
        grid_style: 1,
        crosshair_color: "#94A3B8".to_string(),
        crosshair_width: 3,
        crosshair_style: 0,
        up_color: "#10B981".to_string(),
        down_color: "#EF4444".to_string(),
        wick_up_color: "#34D399".to_string(),
        wick_down_color: "#F87171".to_string(),
        border_up_color: "#059669".to_string(),
        border_down_color: "#DC2626".to_string(),
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
fn canvas_appearance_updates_do_not_rewrite_primary_series_options() {
    let mut chart = interactive_chart();
    let mut series = chart.appearance_settings();
    series.up_color = "#10B981".to_string();
    series.down_color = "#EF4444".to_string();
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
    canvas.grid_color = "#334155".to_string();
    canvas.grid_style = 1;
    canvas.crosshair_color = "#94A3B8".to_string();
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
fn nucleus_default_grid_color_tracks_theme_but_custom_grid_color_does_not() {
    let mut chart = interactive_chart();
    assert_eq!(
        chart.engine.options.get().grid.vert_lines.color,
        nucleus_grid_color(ChartTheme::Dark)
    );

    chart.set_theme(ChartTheme::Light);
    assert_eq!(
        chart.engine.options.get().grid.vert_lines.color,
        nucleus_grid_color(ChartTheme::Light)
    );

    let mut custom = chart.appearance_settings();
    custom.grid_color = "#334155".to_string();
    assert!(chart.set_appearance_settings(&custom));
    chart.set_theme(ChartTheme::Dark);
    assert_eq!(chart.engine.options.get().grid.vert_lines.color, "#334155");

    let mut persisted_light_default = chart.appearance_settings();
    persisted_light_default.grid_color = nucleus_grid_color(ChartTheme::Light);
    assert!(chart.set_appearance_settings(&persisted_light_default));
    assert_eq!(
        chart.engine.options.get().grid.vert_lines.color,
        nucleus_grid_color(ChartTheme::Dark)
    );
}

#[test]
fn persisted_light_nucleus_market_defaults_stay_unpinned_on_a_dark_chart() {
    let light = NucleusChartView::empty_with_theme(ChartTheme::Light);
    let persisted = light.appearance_settings();
    let light_defaults = nucleus_theme_appearance_defaults(ChartTheme::Light);
    assert_eq!(persisted.up_color, light_defaults.bullish);
    assert_eq!(persisted.down_color, light_defaults.bearish);

    let mut dark = NucleusChartView::empty_with_theme(ChartTheme::Dark);
    let _ = dark.set_appearance_settings(&persisted);

    let series = series_entry(&dark, 0);
    assert!(series.up_color.is_none());
    assert!(series.down_color.is_none());
    assert!(series.wick_up_color.is_none());
    assert!(series.wick_down_color.is_none());
    assert!(series.border_up_color.is_none());
    assert!(series.border_down_color.is_none());

    let dark_defaults = nucleus_theme_appearance_defaults(ChartTheme::Dark);
    assert_eq!(dark_defaults.bullish, "#7c8db0");
    let effective = dark.appearance_settings();
    assert_eq!(effective.up_color, dark_defaults.bullish);
    assert_eq!(effective.down_color, dark_defaults.bearish);
    assert_eq!(effective.wick_up_color, effective.up_color);
    assert_eq!(effective.wick_down_color, effective.down_color);
    assert_eq!(effective.border_up_color, effective.up_color);
    assert_eq!(effective.border_down_color, effective.down_color);

    let palette = legend_palette(dark.theme, &effective.up_color, &effective.down_color);
    assert_eq!(
        palette.bullish,
        rgba(
            Color::parse_css(&dark_defaults.bullish)
                .expect("Nucleus bullish color is valid CSS")
                .0
        )
    );
    assert_eq!(
        palette.bearish,
        rgba(
            Color::parse_css(&dark_defaults.bearish)
                .expect("Nucleus bearish color is valid CSS")
                .0
        )
    );
}

#[test]
fn custom_market_and_crosshair_colors_stay_pinned_across_theme_switches() {
    let mut chart = NucleusChartView::empty_with_theme(ChartTheme::Dark);
    let nucleus_defaults = chart.appearance_settings();
    let mut custom = nucleus_defaults.clone();
    custom.up_color = "#112233".to_string();
    custom.down_color = "#445566".to_string();
    custom.wick_up_color = "#778899".to_string();
    custom.wick_down_color = "#AABBCC".to_string();
    custom.border_up_color = "#123456".to_string();
    custom.border_down_color = "#654321".to_string();
    custom.crosshair_color = "#ABCDEF".to_string();
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
    assert_eq!(chart.appearance_settings(), nucleus_defaults);
    let defaults = nucleus_theme_appearance_defaults(ChartTheme::Dark);
    let effective = chart.appearance_settings();
    assert_eq!(effective.up_color, defaults.bullish);
    assert_eq!(effective.down_color, defaults.bearish);
    assert_eq!(effective.crosshair_color, defaults.crosshair);
}

#[test]
fn canonical_crosshair_color_tracks_nucleus_theme() {
    let mut chart = NucleusChartView::empty_with_theme(ChartTheme::Light);
    let light = nucleus_theme_appearance_defaults(ChartTheme::Light);
    assert_eq!(
        chart.engine.options.get().crosshair.vert_line.color,
        light.crosshair
    );

    chart.set_theme(ChartTheme::Dark);
    let dark = nucleus_theme_appearance_defaults(ChartTheme::Dark);
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

    assert!(!chart.legend_rows()[0].values.is_empty());
    assert!(chart.set_legend_item_visible(LegendItem::Asset, false));
    assert!(!series_entry(&chart, 0).visible);
    assert_eq!(chart.legend_rows()[0].item, LegendItem::Asset);
    assert!(!chart.legend_rows()[0].visible);
    assert!(chart.legend_rows()[0].values.is_empty());
    assert!(chart.set_legend_item_visible(LegendItem::Indicator(sma), false));
    assert!(!series_entry(&chart, sma).visible);
    let sma_row = chart
        .legend_rows()
        .into_iter()
        .find(|row| row.item == LegendItem::Indicator(sma))
        .expect("SMA legend remains while hidden");
    assert!(!sma_row.visible);
    assert!(sma_row.values.is_empty());
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
    assert!(volume_row.values.is_empty());
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
fn native_indicator_hover_selection_and_delete_reach_nucleus() {
    let mut chart = interactive_chart();
    let indicator = chart
        .add_indicator(ChartIndicator::Sma)
        .expect("SMA is created")[0];
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    let (x, y) = visible_series_point(&chart, indicator);
    assert_eq!(chart.engine.hit_test_series(x, y), Some(indicator));

    chart.update_cursor(x, y);
    assert_eq!(chart.engine.hovered_series(), Some(indicator));
    assert_eq!(chart.cursor_style, CursorStyle::PointingHand);
    assert!(chart.select_series_at(x, y));
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
    let mut chart = NucleusChartView::with_replay(&replay);
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
        chart.engine.indicator_info(vwap[0]).map(|info| info.kind),
        Some("vwap")
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
    let mut chart = NucleusChartView::empty();
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
    let mut chart = NucleusChartView::empty();
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
    };

    assert_eq!(
        chart.install_study_output(7, 0, descriptor, 1, &timestamps, &first),
        Ok(true)
    );
    let state = chart
        .study_series
        .get(&(7, 0))
        .cloned()
        .expect("study series is tracked");
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
    assert_eq!(
        chart
            .study_series
            .get(&(7, 0))
            .map(|current| current.series_id),
        Some(state.series_id)
    );
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
    assert!(chart.study_series.is_empty());
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
fn study_output_projection_inherits_native_series_defaults() {
    let mut chart = NucleusChartView::empty();
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
    let series_id = chart.study_series[&(88, 0)].series_id;
    let entry = series_entry(&chart, series_id);
    assert_eq!(entry.line_width, Some(2.0));
    assert_eq!(entry.price_format.precision, 4);
}

#[test]
fn study_output_projection_rejects_invalid_presentation_before_creating_series() {
    let mut chart = NucleusChartView::empty();
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
    assert!(chart.study_series.is_empty());
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
fn study_settings_requests_are_bounded_to_one_latest_study_identity() {
    let mut chart = NucleusChartView::empty();
    assert_eq!(chart.take_study_settings_request(), None);
    chart.pending_study_settings = Some(7);
    chart.pending_study_settings = Some(9);
    assert_eq!(chart.take_study_settings_request(), Some(9));
    assert_eq!(chart.take_study_settings_request(), None);
}

#[test]
fn study_remove_request_targets_one_runtime_study() {
    let mut chart = NucleusChartView::empty();
    assert_eq!(chart.take_study_remove_request(), None);
    chart.pending_study_remove = Some(7);
    assert_eq!(chart.take_study_remove_request(), Some(7));
    assert_eq!(chart.take_study_remove_request(), None);
}

#[test]
fn selected_study_output_requests_one_owner_level_removal_without_deleting_a_line() {
    let mut chart = NucleusChartView::empty();
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
    };
    assert_eq!(
        chart.install_study_output(11, 0, descriptor, 1, &timestamps, &values),
        Ok(true)
    );
    assert_eq!(
        chart.install_study_output(11, 1, descriptor, 1, &timestamps, &values),
        Ok(true)
    );
    let series_ids = chart
        .study_series
        .values()
        .map(|state| state.series_id)
        .collect::<Vec<_>>();
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
    let series_ids = chart
        .study_series
        .values()
        .map(|state| state.series_id)
        .collect::<Vec<_>>();
    let selected = series_ids[1];
    let (x, y) = visible_series_point(&chart, selected);

    assert!(chart.select_series_at(x, y));
    assert_eq!(chart.engine.selected_series(), Some(selected));
    assert_eq!(
        chart.engine.selected_series_members().collect::<Vec<_>>(),
        series_ids
    );
}

#[test]
fn multi_output_study_legend_visibility_toggles_the_whole_study() {
    let mut chart = NucleusChartView::empty();
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
        .study_series
        .iter()
        .filter(|((study_id, _), _)| *study_id == 11)
        .map(|(_, state)| {
            series_entry(&chart, state.series_id)
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
            .study_series
            .iter()
            .filter(|((study_id, _), _)| *study_id == 11)
            .all(|(_, state)| !series_entry(&chart, state.series_id).visible)
    );
    assert!(chart.set_legend_item_visible(legend_item, true));
    assert_eq!(chart.study_visible(11), Some(true));
}

#[test]
fn study_outputs_inherit_indicator_chrome_and_live_updates_do_not_dirty_layout() {
    let mut chart = NucleusChartView::empty();
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
    };

    assert_eq!(
        chart.install_study_output(7, 0, descriptor, 1, &timestamps, &[Some(20.0)]),
        Ok(true)
    );
    let series_id = chart.study_series[&(7, 0)].series_id;
    let series = series_entry(&chart, series_id);
    assert!(!series.title_visible);
    assert!(!series.last_value_visible);
    assert!(!series.price_line_visible);

    chart.layout_dirty = false;
    assert_eq!(
        chart.install_study_output(7, 0, descriptor, 2, &timestamps, &[Some(21.0)]),
        Ok(true)
    );
    assert!(!chart.layout_dirty);
}

#[test]
fn study_output_projection_rejects_subsecond_time_without_mutating_chart_state() {
    let mut chart = NucleusChartView::empty();
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
            },
            1,
            &[1_000_000_001],
            &[Some(1.0)],
        ),
        Err(ChartStudyOutputError::UnsupportedTimestampPrecision)
    );
    assert!(chart.study_series.is_empty());
    assert_eq!(chart.engine.series.len(), initial_series);
}

#[test]
fn study_outputs_share_declared_dedicated_pane_with_independent_plot_and_scale_kinds() {
    let mut chart = NucleusChartView::empty();
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
            },
            2,
            &timestamps,
            &values,
        ),
        Ok(true)
    );

    let line = &chart.study_series[&(11, 0)];
    let histogram = &chart.study_series[&(11, 1)];
    let line_entry = series_entry(&chart, line.series_id);
    let histogram_entry = series_entry(&chart, histogram.series_id);
    assert_eq!(line_entry.kind, nucleuscharts_engine::SeriesKind::Line);
    assert_eq!(
        histogram_entry.kind,
        nucleuscharts_engine::SeriesKind::Histogram
    );
    assert_ne!(line_entry.pane_index, 0);
    assert_eq!(line_entry.pane_index, histogram_entry.pane_index);
    assert_eq!(line_entry.price_scale_target, PriceScaleTarget::Right);
    assert_eq!(histogram_entry.price_scale_target, PriceScaleTarget::Left);
    let pane_id = chart.study_panes[&(11, 3)];
    assert_eq!(
        chart.engine.pane_index_for_id(pane_id),
        Some(line_entry.pane_index)
    );

    assert!(chart.remove_study_outputs(&[11]));
    assert!(!chart.study_panes.contains_key(&(11, 3)));
    assert!(chart.engine.pane_index_for_id(pane_id).is_none());
}

#[test]
fn indicator_metadata_matches_the_legacy_picker_copy() {
    assert_eq!(ChartIndicator::ALL.len(), 11);
    assert_eq!(ChartIndicator::Volume.label(), "Volume");
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
fn anchored_drawing_tools_commit_real_nucleus_drawings_and_return_to_cursor() {
    let mut chart = interactive_chart();
    let tools = [
        (ChartDrawingTool::TrendLine, 2, 160.0),
        (ChartDrawingTool::HorizontalLine, 1, 180.0),
        (ChartDrawingTool::VerticalLine, 1, 200.0),
        (ChartDrawingTool::Ray, 1, 220.0),
        (ChartDrawingTool::Rectangle, 2, 240.0),
        (ChartDrawingTool::Text, 1, 260.0),
    ];
    let anchor_x = [260.0, 340.0];

    for (index, (tool, anchors, y)) in tools.into_iter().enumerate() {
        chart.set_drawing_tool(tool);
        assert_eq!(chart.drawing_tool(), tool);
        assert!(
            !chart.engine.drawing_create_active(),
            "arming must not start a pre-click handle"
        );
        for &x in anchor_x.iter().take(anchors) {
            let handled = chart.drawing_pointer_down(x, y, DrawingModifiers::default(), 1);
            assert!(handled);
        }
        assert_eq!(chart.drawing_count(), index + 1);
        assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);
        assert!(!chart.engine.drawing_create_active());
    }
}

#[test]
fn semantic_drawing_state_round_trips_after_indicator_panes_are_recreated() {
    let mut source = interactive_chart();
    source
        .add_indicator(ChartIndicator::Rsi)
        .expect("RSI creates its oscillator pane");
    source.set_drawing_tool(ChartDrawingTool::HorizontalLine);
    assert!(source.drawing_pointer_down(300.0, 180.0, DrawingModifiers::default(), 1,));
    let main_id = source
        .selected_drawing_id()
        .expect("main drawing is selected");
    assert!(source.set_selected_drawing_locked(true));
    let oscillator_id = source
        .engine
        .add_drawing(
            DrawingKind::HorizontalLine,
            1,
            vec![nucleuscharts_engine::DrawingPoint {
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
    let mut source = NucleusChartView::with_replay(&replay);
    source
        .engine
        .add_drawing(
            DrawingKind::TrendLine,
            0,
            vec![
                nucleuscharts_engine::DrawingPoint {
                    logical: 5.5,
                    price: 100.0,
                },
                nucleuscharts_engine::DrawingPoint {
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
    let mut restored = NucleusChartView::with_replay(&replay);
    restored
        .import_semantic_state_json(&state.to_string(), &[])
        .expect("drawings restore");
    assert!((restored.engine.drawings()[0].points[0].logical - 7.5).abs() < f64::EPSILON);
    assert!((restored.engine.drawings()[0].points[1].logical - 12.0).abs() < f64::EPSILON);
}

#[test]
fn armed_ctrl_magnet_snaps_the_crosshair_without_a_preview_dot() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(ChartDrawingTool::TrendLine);
    assert!(!chart.engine.drawing_create_active());

    let x = chart.engine.time_scale.logical_to_coordinate(32.0);
    let y = 200.0;
    chart.update_crosshair(x, y);
    let free = chart.engine.build_frame();
    chart.update_crosshair_magnet(true);
    let snapped = chart.engine.build_frame();
    let crosshair_color = Color::parse_css(&chart.engine.options.get().crosshair.horz_line.color)
        .expect("the package crosshair color is valid");
    let crosshair_y = |frame: &ChartFrame| {
        frame.panes[0].main.iter().find_map(|prim| match prim {
            Prim::HLine { y, color, .. } if *color == crosshair_color => Some(*y),
            _ => None,
        })
    };

    assert_ne!(crosshair_y(&free), crosshair_y(&snapped));
    assert_eq!(
        snapped.panes[0]
            .main
            .iter()
            .filter(|prim| matches!(prim, Prim::Circle { .. }))
            .count(),
        0,
        "arming a tool must not create a pre-click anchor handle"
    );
}

#[test]
fn nucleus_upgrade_hides_crosshair_during_creation_and_restores_it_on_cancel() {
    let mut chart = interactive_chart();
    let color = Color::parse_css(&chart.engine.options.get().crosshair.horz_line.color)
        .expect("crosshair color");
    let visible = |frame: &ChartFrame| {
        frame.panes[0].main.iter().any(
            |primitive| matches!(primitive, Prim::HLine { color: actual, .. } if *actual == color),
        )
    };
    chart.update_crosshair(300.0, 200.0);
    assert!(visible(&chart.engine.build_frame()));
    chart.set_drawing_tool(ChartDrawingTool::TrendLine);
    assert!(chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default(), 1));
    chart.move_pointer(320.0, 220.0, false, DrawingModifiers::default());
    assert!(
        chart.engine.crosshair.is_some(),
        "host pointer coordinates remain available"
    );
    assert!(!visible(&chart.engine.build_frame()));
    chart.cancel_drawing();
    chart.update_crosshair(300.0, 200.0);
    assert!(visible(&chart.engine.build_frame()));
}

#[test]
fn text_tool_place_enters_edit_mode_and_keeps_typed_label() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(ChartDrawingTool::Text);
    assert!(chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default(), 1));
    assert!(chart.is_editing_text());
    assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);

    assert!(chart.set_editing_text_value("NQ"));
    assert_eq!(chart.editing_text_value().as_deref(), Some("NQ"));
    assert!(chart.finish_text_edit());
    assert!(!chart.is_editing_text());
    assert_eq!(chart.drawing_count(), 1);
    assert_eq!(chart.engine.drawings()[0].text, "NQ");
}

#[test]
fn pointer_exit_keeps_the_active_text_edit_session() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(ChartDrawingTool::Text);
    assert!(chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default(), 1));

    chart.cancel_pointer_gesture();

    assert!(chart.is_editing_text());
    assert!(chart.set_editing_text_value("ES"));
    assert_eq!(chart.editing_text_value().as_deref(), Some("ES"));
}

#[test]
fn text_caret_tracks_the_end_of_centered_and_empty_labels() {
    assert_eq!(
        text_caret_geometry(100.0, 100.0, 40.0, 20.0, "center", "middle", false,),
        (120.0, 88.0, 24.0)
    );
    assert_eq!(
        text_caret_geometry(100.0, 100.0, 0.0, 20.0, "center", "middle", true,),
        (90.0, 88.0, 24.0)
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
    chart.set_drawing_tool(ChartDrawingTool::Text);
    assert!(chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default(), 1));
    assert!(chart.is_editing_text());
    assert!(chart.finish_text_edit());
    assert!(!chart.is_editing_text());
    assert_eq!(chart.drawing_count(), 0);
}

#[test]
fn brush_capture_commits_on_release_and_returns_to_cursor() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(ChartDrawingTool::Brush);

    assert!(chart.drawing_pointer_down(240.0, 180.0, DrawingModifiers::default(), 1));
    assert!(chart.drawing_pointer_move(280.0, 210.0, true, DrawingModifiers::default()));
    assert!(chart.drawing_pointer_up(320.0, 240.0, DrawingModifiers::default()));

    assert_eq!(chart.drawing_count(), 1);
    assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);
    assert!(!chart.engine.brush_create_active());
}

#[test]
fn brush_capture_coalesces_pointer_samples_to_one_knot_per_flush() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(ChartDrawingTool::Brush);
    let modifiers = DrawingModifiers::default();

    assert!(chart.drawing_pointer_down(100.0, 100.0, modifiers, 1));
    for offset in 1..=8 {
        let x = 100.0 + f64::from(offset);
        assert!(chart.drawing_pointer_move(x, 100.0 + x, true, modifiers));
    }
    assert!(
        chart.flush_pending_brush(),
        "the newest pending sample is captured once"
    );
    assert!(
        !chart.flush_pending_brush(),
        "an idle flush without a pending sample captures nothing"
    );
    assert!(chart.drawing_pointer_up(180.0, 180.0, modifiers));

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
    chart.set_drawing_tool(ChartDrawingTool::Path);
    assert!(
        !chart.engine.drawing_create_active(),
        "arming must not start a pre-click handle"
    );

    assert!(chart.drawing_pointer_down(260.0, 180.0, DrawingModifiers::default(), 1));
    assert!(chart.drawing_pointer_down(340.0, 220.0, DrawingModifiers::default(), 1));
    assert!(chart.drawing_pointer_down(400.0, 200.0, DrawingModifiers::default(), 1));
    assert_eq!(chart.drawing_count(), 0);
    assert_eq!(chart.drawing_tool(), ChartDrawingTool::Path);
    assert!(chart.apply_key("backspace", false));
    assert!(chart.apply_key("enter", false));
    assert_eq!(chart.drawing_count(), 1);
    assert_eq!(chart.engine.drawings()[0].kind, DrawingKind::Path);
    assert_eq!(chart.engine.drawings()[0].points.len(), 2);
    assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);

    chart.set_drawing_tool(ChartDrawingTool::Path);
    assert!(chart.drawing_pointer_down(260.0, 200.0, DrawingModifiers::default(), 1));
    assert!(chart.drawing_pointer_down(340.0, 240.0, DrawingModifiers::default(), 1));
    assert!(chart.drawing_pointer_down(340.0, 240.0, DrawingModifiers::default(), 2));
    assert_eq!(chart.drawing_count(), 2);
    assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);
    assert!(!chart.engine.drawing_create_active());
}

#[test]
fn cursor_selects_and_moves_unlocked_drawings_but_locked_drawings_do_not_drag() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
    assert!(chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default(), 1));
    let id = chart.selected_drawing_id().expect("drawing selected");
    let (_, drawing_y) = chart
        .engine
        .drawing_point_to_coordinate(id, 0)
        .expect("drawing coordinate");
    chart.set_drawing_tool(ChartDrawingTool::Cursor);

    assert!(chart.set_selected_drawing_locked(true));
    assert!(chart.drawing_pointer_down(500.0, drawing_y, DrawingModifiers::default(), 1));
    assert!(!chart.engine.drawing_drag_active());
    assert_eq!(chart.selected_drawing_id(), Some(id));

    assert!(chart.set_selected_drawing_locked(false));
    assert!(chart.drawing_pointer_down(500.0, drawing_y, DrawingModifiers::default(), 1));
    assert!(chart.engine.drawing_drag_active());
    assert!(chart.drawing_pointer_up(500.0, drawing_y + 30.0, DrawingModifiers::default()));
    assert!(!chart.engine.drawing_drag_active());
}

#[test]
fn lock_summary_delete_clear_and_escape_follow_toolbar_contract() {
    let mut chart = interactive_chart();
    chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
    assert!(chart.drawing_pointer_down(300.0, 180.0, DrawingModifiers::default(), 1));
    chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
    assert!(chart.drawing_pointer_down(300.0, 240.0, DrawingModifiers::default(), 1));
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
    assert!(chart.apply_key("delete", false));
    assert_eq!(chart.drawing_count(), 1);
    assert_eq!(chart.drawings_lock_summary().locked_count, 1);

    assert!(chart.apply_key("escape", false));
    assert_eq!(chart.drawing_tool(), ChartDrawingTool::Cursor);
    assert!(!chart.engine.drawing_create_active());
    chart.clear_drawings();
    assert_eq!(
        chart.drawings_lock_summary(),
        DrawingsLockSummary::default()
    );
    assert!(!chart.apply_key("backspace", false));
}

#[test]
fn drawing_history_steps_back_and_forward_and_keeps_the_armed_tool() {
    let mut chart = interactive_chart();
    assert!(!chart.can_undo_drawing());
    assert!(!chart.can_redo_drawing());
    assert!(!chart.undo_drawing());

    chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
    assert!(chart.drawing_pointer_down(300.0, 180.0, DrawingModifiers::default(), 1));
    chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
    assert!(chart.drawing_pointer_down(300.0, 240.0, DrawingModifiers::default(), 1));
    assert_eq!(chart.drawing_count(), 2);
    assert!(chart.can_undo_drawing());

    chart.set_drawing_tool(ChartDrawingTool::HorizontalLine);
    assert!(chart.undo_drawing());
    assert_eq!(chart.drawing_count(), 1);
    assert!(chart.can_redo_drawing());
    assert_eq!(chart.drawing_tool(), ChartDrawingTool::HorizontalLine);
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
    assert!(!chart.drawing_pointer_down(300.0, 200.0, DrawingModifiers::default(), 1));

    chart.begin_drag(300.0, 200.0, 1, false);

    assert_eq!(chart.drag, Some(ChartDrag::Pane { price_pan: None }));
}

#[test]
fn keyboard_navigation_scrolls_zooms_resets_and_ignores_unknown_keys() {
    let mut chart = interactive_chart();
    let offset = chart.engine.scroll_position();
    assert!(chart.apply_key("right", false));
    assert!((chart.engine.scroll_position() - offset - 1.0).abs() < f64::EPSILON);
    assert!(chart.apply_key("left", true));
    assert!((chart.engine.scroll_position() - offset + 9.0).abs() < f64::EPSILON);

    let page = chart.engine.pane_w / chart.engine.bar_spacing() * KEYBOARD_PAGE_FRACTION;
    let before_page = chart.engine.scroll_position();
    assert!(chart.apply_key("pageup", false));
    assert!((chart.engine.scroll_position() - before_page + page).abs() < f64::EPSILON);
    assert!(chart.apply_key("pagedown", false));
    assert!((chart.engine.scroll_position() - before_page).abs() < f64::EPSILON);

    let spacing = chart.engine.bar_spacing();
    assert!(chart.apply_key("+", false));
    assert!((chart.engine.bar_spacing() - spacing).abs() > f64::EPSILON);

    chart.engine.crosshair = Some((100.0, 100.0));
    assert!(chart.apply_key("escape", false));
    assert!(chart.engine.crosshair.is_none());
    assert!(chart.apply_key("home", false));
    let reset_margin = chart.engine.pane_w * 0.10 / chart.engine.bar_spacing();
    assert!((chart.engine.scroll_position() - reset_margin).abs() < f64::EPSILON);
    chart.engine.scroll_to_position(-4.0);
    assert!(!chart.is_at_latest());
    assert!(chart.apply_key("end", false));
    assert!(chart.is_at_latest());
    assert!(!chart.apply_key("a", false));
}

#[test]
fn native_pointer_state_ends_a_drag_when_mouse_up_was_lost() {
    let mut chart = interactive_chart();
    chart.begin_drag(300.0, 200.0, 1, false);
    assert_eq!(chart.drag, Some(ChartDrag::Pane { price_pan: None }));

    chart.move_pointer(340.0, 200.0, false, DrawingModifiers::default());

    assert!(chart.drag.is_none());
    assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
    assert_eq!(chart.engine.crosshair, Some((340.0, 200.0)));
}

#[test]
fn host_modal_suspension_clears_crosshair_and_active_pointer_gestures() {
    let mut chart = interactive_chart();
    chart.begin_drag(300.0, 200.0, 1, false);
    assert!(chart.engine.crosshair.is_some());
    assert!(chart.drag.is_some());

    chart.suspend_pointer_interaction();

    assert!(chart.engine.crosshair.is_none());
    assert!(chart.drag.is_none());
    assert!(chart.engine.separator_hover.is_none());
    assert_eq!(
        chart.pointer_interaction,
        PointerInteractionState::Suspended
    );
    assert_eq!(chart.cursor_style, CursorStyle::Arrow);

    chart.resume_pointer_interaction();
    assert_eq!(chart.pointer_interaction, PointerInteractionState::Active);
    assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
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
    let mut chart = NucleusChartView::with_replay(&replay);
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

    chart.update_cursor(300.0, separator_y);
    assert_eq!(chart.cursor_style, CursorStyle::ResizeRow);
    assert_eq!(chart.engine.separator_hover, Some(0));

    chart.begin_drag(300.0, separator_y, 1, false);
    assert!(matches!(
        chart.drag,
        Some(ChartDrag::PaneSeparator { index: 0, .. })
    ));
    assert!(chart.engine.crosshair.is_none());
    chart.layout_dirty = false;
    chart.drag_to(300.0, separator_y + 20.0);
    assert!(chart.layout_dirty, "dragging must rebuild pane geometry");
    assert!(chart.engine.panes[0].stretch_factor > first_stretch);
    // Mouse events may arrive faster than chart frames. The latest pointer
    // position must win even before a layout pass updates pane heights.
    chart.drag_to(300.0, separator_y + 40.0);
    assert!((chart.engine.panes[0].stretch_factor - (first_height + 40.0)).abs() < 1e-6);
    chart
        .engine
        .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
    assert!((chart.engine.panes[1].top - (separator_y + 40.0)).abs() < 1.0);

    chart.move_pointer(
        300.0,
        separator_y + 40.0,
        false,
        DrawingModifiers::default(),
    );
    assert!(chart.drag.is_none());
    assert_eq!(chart.cursor_style, CursorStyle::ResizeRow);
}

#[test]
fn escape_cancels_every_active_gesture_and_clears_pointer_state() {
    let mut chart = interactive_chart();
    chart.begin_drag(300.0, chart.engine.pane_h + 10.0, 1, false);
    assert_eq!(chart.drag, Some(ChartDrag::TimeAxis));

    assert!(chart.apply_key("escape", false));

    assert!(chart.drag.is_none());
    assert!(chart.engine.crosshair.is_none());
    assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
}

#[test]
fn pointer_cursor_truthfully_tracks_chart_and_axis_gestures() {
    let mut chart = interactive_chart();
    chart.update_cursor(300.0, 200.0);
    assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
    chart.update_cursor(300.0, chart.engine.pane_h + 1.0);
    assert_eq!(chart.cursor_style, CursorStyle::ResizeLeftRight);
    chart.update_cursor(chart.engine.pane_w + 1.0, 200.0);
    assert_eq!(chart.cursor_style, CursorStyle::ResizeUpDown);

    chart.begin_drag(300.0, 200.0, 1, false);
    assert_eq!(chart.cursor_style, CursorStyle::ClosedHand);
    chart.end_drag(300.0, 200.0);
    assert_eq!(chart.cursor_style, CursorStyle::Crosshair);
}
