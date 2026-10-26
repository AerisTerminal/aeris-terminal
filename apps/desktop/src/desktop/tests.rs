#![cfg(test)]

use super::{
    CHART_CONTEXT_MENU_ROW_HEIGHT, CHART_CONTEXT_MENU_WIDTH, CHROME_MENU_LIST_HEIGHT,
    CHROME_MENU_MAX_HEIGHT, CHROME_MENU_SEARCH_HEIGHT, CHROME_MENU_WIDTH, CaptionPlatform,
    CaptionPointerOwner, ChartFailure, ChartNoticePlacement, ChartNoticeTone, ChartState,
    ChromeOverlayPhase, ConsumerResourceClass, HeaderControls, InputEvent, InstrumentMenuAction,
    InstrumentMenuEntry, InstrumentMenuSelection, MarketSummaryEntry, OVERLAY_EDGE_MARGIN,
    PRICE_AXIS_MENU_GAP, PriceAxisMenuFlyout, PriceAxisMenuRow, ProviderCatalogCommand,
    ProviderCatalogEvent, ProviderConnectionPresentation, SidePanel, SidePanelVisibility,
    SymbolBrowser, SymbolInputAction, SymbolSearchFailure, SymbolSearchScope,
    SymbolSelectionTarget, SymbolSubmitDecision, TIMEFRAME_FLYOUT_GAP, TIMEFRAME_FLYOUT_WIDTH,
    TIMEFRAME_MENU_WIDTH, TerminalProvider, TimeSalesFilter, TimeSalesSideFilter,
    TimeframeMenuGroup, WORKSPACE_TAB_GAP, WORKSPACE_TAB_STRIP_PADDING_LEFT, WatchlistDragState,
    WindowCommand, WindowMoveGestureEvent, WindowMoveGestureTransition, WorkspaceDragState,
    WorkspaceMaximizeTransition, active_workspace_after_close, bounded_status_detail,
    caption_keyboard_activates, caption_pointer_owner, catalog_rejection_message,
    chart_position_id, chart_price_fixed_point, chart_quantity_fixed_point, chart_status_detail,
    chart_surface_notice, chart_time_seconds_from_unix_nanos, chrome_menu_extent,
    chrome_overlay_progress, chrome_typeahead_char_from, claim_once, clamp_anchored_menu_left,
    clamp_chart_context_menu_origin, clamp_price_axis_menu_origin, clamped_side_panel_width,
    connection_presentation, connectivity_chart_state, counted, current_instrument_menu_index,
    desktop_key_bindings, durable_workspace_viewport, first_selectable_instrument_index,
    fullscreen_escape_command, gpui_color, instrument_listing_refresh_needed,
    instrument_row_highlighted, instrument_selector_label, instrument_target_after_close,
    overlay_height, price_axis_flyout_rows, price_axis_root_rows, project_provider_catalog_event,
    publication_chart_state, reconciled_bridge_state, reorder_workspace_ids, series_selector_label,
    should_finish_chrome_overlay_close, stabilized_connection_state, symbol_input_action,
    symbol_submit_decision, terminal_provider_id, timeframe_flyout_height, timeframe_flyout_offset,
    timeframe_flyout_row_is_active, timeframe_group_intervals, timeframe_interval_group,
    timeframe_menu_groups, timeframe_menu_row_label, timeframe_overlay_extent,
    timeframe_overlay_left, watchlist_drag_destination, watchlist_drag_translation, watchlist_step,
    window_move_gesture_transition, workspace_drag_destination, workspace_drag_translation,
    workspace_maximize_transition, workspace_split_ratio, workspace_switch,
    wrapped_workspace_index,
};
#[cfg(feature = "diagnostics")]
use super::{FOREGROUND_INTERACTION_SAMPLE_CAPACITY, ForegroundInteractionDiagnostics};
use aeris_chart_integration::{ChartSplitDirection, PriceAxisMenuState};
use aeris_contracts::{
    InstallProviderInstrument, ProviderCatalogRejectionReason, ProviderInstrumentKind,
    ProviderInstrumentSummary, WorkspaceLayoutState, WorkspacePaneState, WorkspaceSplitAxis,
    WorkspaceState,
};

pub(super) fn test_trading_service() -> aeris_trading_runtime::TradingService {
    aeris_trading_runtime::TradingService::start(aeris_trading_runtime::TradingServiceConfig {
        database_path: std::path::PathBuf::from(":memory:"),
        retention: aeris_trading_runtime::TradingRetention::default(),
        clock: std::sync::Arc::new(aeris_trading_runtime::clock::SystemClock::default()),
        execution: aeris_trading_runtime::ExecutionPolicy::default(),
    })
    .expect("test trading service starts")
}
use aeris_design_system::ThemeColor;
use aeris_market_data::{ChartInterval, MarketBar};
use aeris_observability::FeedConnectionState;
use gpui::{Bounds, point, px, size};

#[gpui::test]
fn every_desktop_window_opens_maximized_and_restores_to_its_default_size(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| {
        for window_index in [0, 1] {
            let options = super::desktop_window_options(window_index, cx);
            let Some(gpui::WindowBounds::Maximized(restore)) = options.window_bounds else {
                panic!("window {window_index} must open maximized");
            };
            assert_eq!(restore.size, size(px(1280.0), px(820.0)));
        }
    });
}

#[test]
fn chart_position_identity_is_shared_by_projection_and_intent_validation() {
    let account = aeris_trading::TradingAccountId::try_new("aeris-sim-1").expect("account");
    assert_eq!(
        chart_position_id(&account, "instrument:test:ES")
            .expect("chart position id")
            .as_str(),
        "position:aeris-sim-1:instrument:test:ES"
    );
}

#[test]
fn chart_prices_convert_at_eight_places_despite_float_representation() {
    // 1224.37 has no exact f64; scaled to eight places it lands a few ulps off a whole unit,
    // which a fixed 1e-6 tolerance rejected and so blocked TP/SL drags on the chart.
    for (price, units) in [
        (1224.37, 122_437_000_000),
        (43.123_4, 4_312_340_000),
        (98_765.43, 9_876_543_000_000),
    ] {
        assert_eq!(
            chart_price_fixed_point(price, 8).map(aeris_trading::FixedPoint::units),
            Some(units),
            "{price}"
        );
    }
    assert_eq!(
        chart_price_fixed_point(1224.37, 2).map(aeris_trading::FixedPoint::units),
        Some(122_437)
    );
    assert_eq!(chart_price_fixed_point(1224.375, 2), None);
    assert_eq!(chart_price_fixed_point(f64::NAN, 8), None);
    assert_eq!(chart_quantity_fixed_point(0.0, 8), None);
}

#[test]
fn selected_account_lock_is_derived_from_the_runtime_meter() {
    let account = aeris_trading::TradingAccountId::try_new("aeris-sim-2").expect("account");
    let mut state = super::TradingPnlState::default();
    state.order_entry.selected_account_id = Some(account.clone());
    state.risk_locks.push(aeris_trading_runtime::RiskLock {
        account_id: account,
        kind: aeris_trading_runtime::RiskLockKind::Manual,
        reason: "manual test lock".to_string(),
        locked_at_unix_nanos: 1,
        profile_id: None,
        profile_version: None,
    });

    assert_eq!(
        super::selected_account_lock_reason(&state),
        Some("manual test lock")
    );
    state.order_entry.selected_account_id =
        Some(aeris_trading::TradingAccountId::try_new("aeris-sim-1").expect("other account"));
    assert_eq!(super::selected_account_lock_reason(&state), None);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CatalogCommandOrderBookain {
    Search,
    Selection,
}

const fn catalog_rejection_order_bookain(
    command: ProviderCatalogCommand,
) -> CatalogCommandOrderBookain {
    match command {
        ProviderCatalogCommand::Search => CatalogCommandOrderBookain::Search,
        ProviderCatalogCommand::Selection => CatalogCommandOrderBookain::Selection,
    }
}

#[test]
fn instrument_menu_requests_a_listing_only_when_idle_and_not_already_shown() {
    let provider = TerminalProvider::Rithmic;
    let mut browser =
        SymbolBrowser::new(SymbolSearchScope::Provider(terminal_provider_id(provider)));
    let startup = browser
        .begin_search("")
        .expect("listing validates")
        .remove(0);
    assert!(
        !instrument_listing_refresh_needed(&browser, false, ""),
        "startup search is already pending"
    );

    let result = ProviderInstrumentSummary {
        kind: ProviderInstrumentKind::Instrument,
        symbol: "BTC-USD".to_string(),
        display_symbol: "BTC-USD".to_string(),
        exchange: "rithmic".to_string(),
        name: Some("BTC/USD".to_string()),
        product_code: Some("BTC-USD".to_string()),
        instrument_type: Some("spot".to_string()),
        expiration_date: None,
        asset_class: None,
    };
    assert!(browser.apply_results(provider, startup.request_id, vec![result]));
    assert!(
        !instrument_listing_refresh_needed(&browser, false, ""),
        "populated listing needs no refresh"
    );
    assert!(
        instrument_listing_refresh_needed(&browser, false, "ETH"),
        "a listing for another query is searched again"
    );

    let selection = browser
        .select(InstrumentMenuSelection::SearchResult { provider, index: 0 })
        .expect("catalog result is selectable");
    assert!(
        !instrument_listing_refresh_needed(&browser, true, ""),
        "in-flight selection defers the refresh"
    );
    assert!(browser.confirm_selection(selection.generation));
    assert!(browser.consume_completed_search(provider, selection.search_generation));
    assert!(
        instrument_listing_refresh_needed(&browser, false, ""),
        "consumed selection authorization reopens as a fresh listing"
    );
    let refreshed = browser
        .begin_search("")
        .expect("runtime provider catalog permits a fresh empty listing query")
        .remove(0);
    assert_eq!(refreshed.query, "");
    assert!(browser.search_pending());
    assert!(browser.reject_search(provider, refreshed.request_id, SymbolSearchFailure::Failed));

    browser.begin_search("ETH").expect("typed query validates");
    assert!(
        !instrument_listing_refresh_needed(&browser, false, ""),
        "a pending typed query outranks the listing"
    );
}

fn should_apply_rithmic_worker_stop(
    disconnected: bool,
    connection_state: Option<FeedConnectionState>,
) -> bool {
    disconnected
        && matches!(
            connection_state,
            Some(state) if !matches!(state, FeedConnectionState::Stopped)
        )
}

#[test]
fn chrome_overlay_motion_opens_and_closes_in_opposite_directions() {
    let assert_progress = |actual: f32, expected: f32| {
        assert!((actual - expected).abs() < f32::EPSILON);
    };
    assert_progress(
        chrome_overlay_progress(ChromeOverlayPhase::Opening, 0.0),
        0.0,
    );
    assert_progress(
        chrome_overlay_progress(ChromeOverlayPhase::Opening, 1.0),
        1.0,
    );
    assert_progress(
        chrome_overlay_progress(ChromeOverlayPhase::Closing, 0.0),
        1.0,
    );
    assert_progress(
        chrome_overlay_progress(ChromeOverlayPhase::Closing, 1.0),
        0.0,
    );
    assert_progress(
        chrome_overlay_progress(ChromeOverlayPhase::Opening, -1.0),
        0.0,
    );
    assert_progress(
        chrome_overlay_progress(ChromeOverlayPhase::Closing, 2.0),
        0.0,
    );
}

#[test]
fn stale_close_completion_cannot_remove_a_reopened_overlay() {
    assert!(should_finish_chrome_overlay_close(
        ChromeOverlayPhase::Closing,
        7,
        7
    ));
    assert!(!should_finish_chrome_overlay_close(
        ChromeOverlayPhase::Opening,
        8,
        7
    ));
    assert!(!should_finish_chrome_overlay_close(
        ChromeOverlayPhase::Closing,
        8,
        7
    ));
}

#[test]
fn timeframe_overlay_uses_the_trigger_left_edge() {
    let trigger = Bounds::new(point(px(214.0), px(52.0)), size(px(64.0), px(32.0)));
    assert_eq!(timeframe_overlay_left(Some(trigger)), px(214.0));
    assert_eq!(timeframe_overlay_left(None), px(0.0));
}

#[test]
fn timeframe_menu_groups_every_catalog_interval() {
    let groups: Vec<TimeframeMenuGroup> = ChartInterval::ALL
        .into_iter()
        .map(timeframe_interval_group)
        .collect();
    assert_eq!(
        groups,
        [
            TimeframeMenuGroup::Ticks,
            TimeframeMenuGroup::Seconds,
            TimeframeMenuGroup::Seconds,
            TimeframeMenuGroup::Seconds,
            TimeframeMenuGroup::Seconds,
            TimeframeMenuGroup::Seconds,
            TimeframeMenuGroup::Minutes,
            TimeframeMenuGroup::Minutes,
            TimeframeMenuGroup::Minutes,
            TimeframeMenuGroup::Minutes,
            TimeframeMenuGroup::Minutes,
            TimeframeMenuGroup::Hours,
            TimeframeMenuGroup::Hours,
            TimeframeMenuGroup::Hours,
            TimeframeMenuGroup::Hours,
            TimeframeMenuGroup::Hours,
            TimeframeMenuGroup::Days,
            TimeframeMenuGroup::Days,
            TimeframeMenuGroup::Weeks,
            TimeframeMenuGroup::Months,
        ]
    );
    let rithmic_groups: Vec<TimeframeMenuGroup> =
        super::provider_intervals(TerminalProvider::Rithmic)
            .iter()
            .copied()
            .map(timeframe_interval_group)
            .collect();
    assert_eq!(
        rithmic_groups,
        [
            TimeframeMenuGroup::Seconds,
            TimeframeMenuGroup::Seconds,
            TimeframeMenuGroup::Seconds,
            TimeframeMenuGroup::Seconds,
            TimeframeMenuGroup::Seconds,
            TimeframeMenuGroup::Minutes,
            TimeframeMenuGroup::Minutes,
            TimeframeMenuGroup::Minutes,
            TimeframeMenuGroup::Minutes,
            TimeframeMenuGroup::Minutes,
            TimeframeMenuGroup::Hours,
            TimeframeMenuGroup::Hours,
            TimeframeMenuGroup::Hours,
            TimeframeMenuGroup::Hours,
            TimeframeMenuGroup::Hours,
            TimeframeMenuGroup::Days,
            TimeframeMenuGroup::Weeks,
            TimeframeMenuGroup::Months,
        ]
    );
}

#[test]
fn seconds_timeframes_are_offered_only_by_second_bar_providers() {
    assert!(
        timeframe_menu_groups(super::provider_intervals(TerminalProvider::Rithmic))
            .contains(&TimeframeMenuGroup::Seconds)
    );
    assert!(
        !timeframe_menu_groups(super::provider_intervals(TerminalProvider::Hyperliquid))
            .contains(&TimeframeMenuGroup::Seconds)
    );
    assert_eq!(
        timeframe_group_intervals(TimeframeMenuGroup::Seconds, &ChartInterval::ALL),
        [
            ChartInterval::Second1,
            ChartInterval::Second5,
            ChartInterval::Second10,
            ChartInterval::Second15,
            ChartInterval::Second30,
        ]
    );
    assert_eq!(timeframe_menu_row_label(ChartInterval::Second1), "1 Second");
    assert_eq!(
        timeframe_menu_row_label(ChartInterval::Second30),
        "30 Seconds"
    );
    assert_eq!(TimeframeMenuGroup::Seconds.label(), "Seconds");
}

#[test]
fn timeframe_menu_uses_dual_group_and_interval_containers() {
    assert_eq!(
        timeframe_menu_groups(&ChartInterval::ALL),
        [
            TimeframeMenuGroup::Ticks,
            TimeframeMenuGroup::Seconds,
            TimeframeMenuGroup::Minutes,
            TimeframeMenuGroup::Hours,
            TimeframeMenuGroup::Days,
            TimeframeMenuGroup::Weeks,
            TimeframeMenuGroup::Months,
        ]
    );
    assert_eq!(
        timeframe_menu_groups(super::provider_intervals(TerminalProvider::Rithmic)),
        [
            TimeframeMenuGroup::Seconds,
            TimeframeMenuGroup::Minutes,
            TimeframeMenuGroup::Hours,
            TimeframeMenuGroup::Days,
            TimeframeMenuGroup::Weeks,
            TimeframeMenuGroup::Months,
        ]
    );
    assert_eq!(
        timeframe_group_intervals(TimeframeMenuGroup::Minutes, &ChartInterval::ALL),
        [
            ChartInterval::Minute1,
            ChartInterval::Minute3,
            ChartInterval::Minute5,
            ChartInterval::Minute15,
            ChartInterval::Minute30,
        ]
    );
    assert_eq!(timeframe_menu_row_label(ChartInterval::Minute1), "1 Minute");
    assert_eq!(timeframe_menu_row_label(ChartInterval::Hour4), "4 Hours");
    assert_eq!(timeframe_menu_row_label(ChartInterval::Day1), "1 Day");
    assert_eq!(TimeframeMenuGroup::Minutes.label(), "Minutes");
    assert!(
        timeframe_menu_groups(super::provider_intervals(TerminalProvider::Rithmic))
            .into_iter()
            .all(|group| {
                !timeframe_group_intervals(
                    group,
                    super::provider_intervals(TerminalProvider::Rithmic),
                )
                .is_empty()
            }),
        "a hovered group owns its submenu; the root list does not keep a flyout open"
    );
    assert!((timeframe_flyout_offset(1) - CHART_CONTEXT_MENU_ROW_HEIGHT).abs() < f32::EPSILON);
    assert!(
        (timeframe_flyout_height(1) - overlay_height(1.0, 0.0)).abs() < f32::EPSILON,
        "submenu height is its rows inside the shared menu panel chrome"
    );
    assert!(
        (timeframe_overlay_extent(5, None).0 - TIMEFRAME_MENU_WIDTH).abs() < f32::EPSILON,
        "closed menu must not reserve a dead gap beside the root list"
    );
    let hours = timeframe_overlay_extent(5, Some((1, 5)));
    assert!(
        (hours.0 - (TIMEFRAME_MENU_WIDTH + TIMEFRAME_FLYOUT_GAP + TIMEFRAME_FLYOUT_WIDTH)).abs()
            < f32::EPSILON
    );
    assert!(hours.1 >= timeframe_flyout_offset(1) + timeframe_flyout_height(5));
    assert!(
        !timeframe_flyout_row_is_active(ChartInterval::Hour1, ChartInterval::Minute1, None, 0),
        "hovering a group must not mark its first row selected"
    );
    assert!(timeframe_flyout_row_is_active(
        ChartInterval::Minute1,
        ChartInterval::Minute1,
        None,
        0
    ));
    assert!(timeframe_flyout_row_is_active(
        ChartInterval::Hour1,
        ChartInterval::Minute1,
        Some(0),
        0
    ));
}

#[test]
fn chrome_typeahead_opens_digits_as_intervals_and_letters_as_symbols() {
    assert_eq!(chrome_typeahead_char_from("1", Some("1"), false), Some('1'));
    assert_eq!(chrome_typeahead_char_from("m", Some("m"), false), Some('m'));
    assert_eq!(chrome_typeahead_char_from("m", Some("M"), true), Some('M'));
    assert_eq!(chrome_typeahead_char_from("m", None, true), Some('M'));
    assert_eq!(chrome_typeahead_char_from("a", Some("a"), false), Some('a'));
    assert_eq!(chrome_typeahead_char_from("enter", None, false), None);
    assert_eq!(
        super::provider_intervals(TerminalProvider::Rithmic)
            .iter()
            .copied()
            .filter(|interval| interval.matches_typeahead("1m"))
            .collect::<Vec<_>>(),
        [ChartInterval::Minute1]
    );
    assert_eq!(
        super::provider_intervals(TerminalProvider::Rithmic)
            .iter()
            .copied()
            .filter(|interval| interval.matches_typeahead("1M"))
            .collect::<Vec<_>>(),
        [ChartInterval::Month1]
    );
    assert!(ChartInterval::Minute1.matches_typeahead("1"));
    assert!(ChartInterval::Hour1.matches_typeahead("1"));
    assert!(!ChartInterval::Minute1.matches_typeahead("1M"));
}

mod timeframe_input {
    use super::super::layouts_view::{ClosePrompt, LayoutNamePurpose};
    use super::super::order_flow_dialog::{OrderFlowColorTarget, OrderFlowDialogDraft};
    use super::super::*;
    use aeris_desktop::market_worker::{
        EngineWorkerStartup, MarketProjectionSender, MarketWorkerCommand, market_projection_channel,
    };
    use gpui::{TestAppContext, VisualTestContext};
    use std::num::NonZeroUsize;
    use std::sync::{atomic::AtomicU64, mpsc};

    struct TimeframeHarness(Entity<WorkspaceSurface>);

    impl Render for TimeframeHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let surface = self.0.read(cx);
            let input = surface.timeframe_input.clone();
            let ticket = surface.order_quantity.ticket_input().clone();
            let quick = surface.chrome_overlay == Some(ChromeOverlay::QuickTimeframe);
            let symbol = (surface.chrome_overlay == Some(ChromeOverlay::Instrument)
                && surface.chrome_overlay_phase != ChromeOverlayPhase::Closing)
                .then(|| surface.symbol_input.clone())
                .flatten();
            let practice_name = surface
                .trading_pnl
                .account_creator
                .as_ref()
                .map(|creator| creator.name.clone());
            let big_trades_dialog = surface.big_trades_dialog().map(|dialog| {
                div()
                    .size_full()
                    .debug_selector(|| "big_trades_dialog".into())
                    .child(big_trades_dialog_layer(
                        &self.0,
                        dialog,
                        &AerisTheme::dark(),
                    ))
            });
            let volume_dialog = surface.volume_dialog().map(|dialog| {
                div()
                    .size_full()
                    .debug_selector(|| "volume_dialog".into())
                    .child(volume_dialog_layer(&self.0, dialog, &AerisTheme::dark()))
            });
            let order_flow_dialog = surface.order_flow_dialog().map(|dialog| {
                div()
                    .size_full()
                    .debug_selector(|| "order_flow_dialog".into())
                    .child(order_flow_dialog_layer(
                        &self.0,
                        dialog,
                        &AerisTheme::dark(),
                    ))
            });
            div()
                .size_full()
                .track_focus(&surface.chrome_focus)
                .on_key_down(cx.listener(|this, event, window, cx| {
                    if this.0.update(cx, |surface, cx| {
                        let workspace_focused = surface.holds_keyboard_focus(window, cx);
                        surface.on_terminal_key_down(event, workspace_focused, window, cx)
                    }) {
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }))
                .child(Input::new(&ticket))
                .when(quick, |root| root.child(Input::new(&input)))
                .when_some(symbol, |root, input| root.child(Input::new(&input)))
                .when_some(practice_name, |root, input| root.child(Input::new(&input)))
                .children(big_trades_dialog)
                .children(volume_dialog)
                .children(order_flow_dialog)
        }
    }

    fn harness(
        cx: &mut TestAppContext,
    ) -> (
        Entity<WorkspaceSurface>,
        mpsc::Receiver<MarketWorkerCommand>,
        MarketProjectionSender,
        &mut VisualTestContext,
    ) {
        let product = local_state::default_workspace().watchlist_entries[0]
            .instrument
            .clone()
            .expect("default instrument");
        harness_with(cx, product)
    }

    /// A surface restoring `product`, as a workspace reopened at startup does.
    fn harness_with(
        cx: &mut TestAppContext,
        product: InstallProviderInstrument,
    ) -> (
        Entity<WorkspaceSurface>,
        mpsc::Receiver<MarketWorkerCommand>,
        MarketProjectionSender,
        &mut VisualTestContext,
    ) {
        let (commands, requests) = mpsc::sync_channel(8);
        let (publications, messages) = market_projection_channel(NonZeroUsize::MIN);
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
            let surface = workspace_surface_entity(
                MarketWorkerStartup::Loading(Box::new(EngineWorkerStartup {
                    product,
                    interval: ChartInterval::Minute1,
                    restored_viewport: None,
                    subscription_id: "timeframe_test".into(),
                    worker_label: "timeframe_test".into(),
                })),
                worker,
                &DesktopLifecycle::new_for_test(),
                chart_chrome::ChartChromePreferences::default(),
                WorkspaceSurfaceRestore::default(),
                window,
                cx,
            );
            surface.read(cx).chrome_focus.clone().focus(window, cx);
            cx.observe(&surface, |_, _, cx| cx.notify()).detach();
            TimeframeHarness(surface)
        });
        let surface = cx.read(|cx| view.read(cx).0.clone());
        (surface, requests, publications, cx)
    }

    #[gpui::test]
    fn ticket_field_and_chart_menu_edit_one_order_quantity(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness(cx);
        let (ticket, menu) = cx.read(|cx| {
            let controls = &surface.read(cx).order_quantity;
            (
                controls.ticket_input().clone(),
                controls.menu_input().clone(),
            )
        });
        cx.read(|cx| assert_eq!(ticket.read(cx).value(), "1"));

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| surface.set_order_quantity(7, window, cx));
        });
        cx.read(|cx| {
            assert_eq!(surface.read(cx).trading_pnl.order_entry.quantity, 7);
            assert_eq!(
                ticket.read(cx).value(),
                "7",
                "the ticket shows a quantity set elsewhere"
            );
        });

        cx.update(|window, cx| {
            ticket.update(cx, |input, cx| input.set_value(" 12 ", window, cx));
            surface.update(cx, WorkspaceSurface::apply_ticket_quantity_text);
        });
        cx.read(|cx| {
            let surface = surface.read(cx);
            assert_eq!(surface.trading_pnl.order_entry.quantity, 12);
            assert!(!surface.order_quantity.ticket_invalid());
        });

        cx.update(|window, cx| {
            ticket.update(cx, |input, cx| input.set_value("1000", window, cx));
            surface.update(cx, WorkspaceSurface::apply_ticket_quantity_text);
        });
        cx.read(|cx| {
            let surface = surface.read(cx);
            assert_eq!(
                surface.trading_pnl.order_entry.quantity, 12,
                "rejected text never becomes the order quantity"
            );
            assert!(surface.order_quantity.ticket_invalid());
        });

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.toggle_order_quantity_menu(window, cx);
            });
            menu.update(cx, |input, cx| input.set_value("abc", window, cx));
            surface.update(cx, |surface, cx| {
                surface.submit_order_quantity_menu(window, cx);
            });
        });
        cx.read(|cx| {
            let surface = surface.read(cx);
            assert!(surface.order_quantity.menu_open());
            assert!(surface.order_quantity.menu_invalid());
            assert_eq!(surface.trading_pnl.order_entry.quantity, 12);
        });

        cx.update(|window, cx| {
            menu.update(cx, |input, cx| input.set_value("40", window, cx));
            surface.update(cx, |surface, cx| {
                surface.submit_order_quantity_menu(window, cx);
            });
        });
        cx.read(|cx| {
            let surface = surface.read(cx);
            assert!(!surface.order_quantity.menu_open());
            assert_eq!(surface.trading_pnl.order_entry.quantity, 40);
            assert!(!surface.order_quantity.ticket_invalid());
            assert_eq!(ticket.read(cx).value(), "40");
        });

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| surface.set_order_quantity(0, window, cx));
        });
        cx.read(|cx| assert_eq!(surface.read(cx).trading_pnl.order_entry.quantity, 1));
    }

    #[gpui::test]
    fn restored_chart_from_an_unavailable_provider_fails_explicitly(cx: &mut TestAppContext) {
        let product = InstallProviderInstrument {
            provider: "retired".to_string(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: "retired:demo:1001:1".to_string(),
            provider_symbol: "EURUSD".to_string(),
            display_symbol: "EURUSD".to_string(),
            venue_id: "retired".to_string(),
            price_scale: 5,
            quantity_scale: 2,
            entitlement_id: "retired-authorized".to_string(),
            ..Default::default()
        };
        let (surface, _requests, _publications, cx) = harness_with(cx, product);
        cx.read(|cx| {
            let surface = surface.read(cx);
            // Never silently presented as another provider's market.
            assert_eq!(surface.chart_state, ChartState::Error);
            assert_eq!(surface.provider, super::super::FALLBACK_TERMINAL_PROVIDER);
            assert_eq!(
                surface.chart_failure,
                Some(ChartFailure::UnsupportedProvider)
            );
            assert!(surface.product.is_none());
        });
    }

    #[gpui::test]
    fn quick_daily_enter_submits_exactly_one_selection(cx: &mut TestAppContext) {
        let (surface, requests, _publications, cx) = harness(cx);
        cx.simulate_keystrokes("1 shift-d");
        cx.read(|cx| assert_eq!(surface.read(cx).timeframe_input.read(cx).value(), "1D"));
        cx.simulate_keystrokes("enter");
        let selections = requests
            .try_iter()
            .filter_map(|command| match command {
                MarketWorkerCommand::EngineSelect(request) => Some(request.interval),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(selections, [ChartInterval::Day1]);
        cx.read(|cx| {
            assert_eq!(
                surface.read(cx).chart_pane_controller.pending_interval(),
                Some(ChartInterval::Day1)
            );
        });
    }

    #[gpui::test]
    fn repeated_quick_switches_keep_the_entered_units(cx: &mut TestAppContext) {
        let (surface, requests, publications, cx) = harness(cx);
        for (keys, interval) in [
            ("1 d", ChartInterval::Day1),
            ("4 h", ChartInterval::Hour4),
            ("1 shift-m", ChartInterval::Month1),
            ("1 m", ChartInterval::Minute1),
            ("5", ChartInterval::Minute5),
        ] {
            cx.simulate_keystrokes(keys);
            cx.simulate_keystrokes("enter enter");
            let selections = requests
                .try_iter()
                .filter_map(|command| match command {
                    MarketWorkerCommand::EngineSelect(request) => Some(request),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(selections.len(), 1, "one request for {keys}");
            assert_eq!(selections[0].interval, interval);
            publications
                .send(PaneProjectionEvent::LocalCommandOutcome(
                    aeris_desktop::market_worker::LocalCommandOutcome::EngineSelection {
                        sequence: selections[0].sequence,
                        succeeded: true,
                    },
                ))
                .expect("switch marker");
            cx.update(|_, cx| {
                surface.update(cx, |surface, cx| {
                    surface.poll_market_worker(cx);
                });
            });
            cx.executor().advance_clock(CHROME_OVERLAY_EXIT_DURATION);
            cx.run_until_parked();
        }
    }

    fn order_flow_tape(
        product: &InstallProviderInstrument,
        generation: u64,
        revision: u64,
        last_ordinal: u64,
    ) -> aeris_market_runtime::MarketTradeTapeSnapshot {
        let trades = (1..=last_ordinal)
            .map(|ordinal| {
                let seconds = 1_700_000_000 + i64::try_from(ordinal).expect("small ordinal");
                aeris_market_runtime::RetainedMarketTrade {
                    ingestion_ordinal: ordinal,
                    observed_unix_nanos: seconds * 1_000_000_000,
                    trading_day: None,
                    trade: Arc::new(aeris_market_data::MarketTrade {
                        metadata: aeris_market_data::EventMetadata {
                            provider_id: product.provider.clone(),
                            instrument_id: product.instrument_id.clone(),
                            entitlement_id: product.entitlement_id.clone(),
                            session_generation: 1,
                            source_sequence: ordinal,
                            timestamps: aeris_market_data::QualifiedTimestamp {
                                exchange_unix_nanos: Some(seconds * 1_000_000_000),
                                provider_unix_nanos: None,
                                received_unix_nanos: seconds * 1_000_000_000,
                            },
                        },
                        trade_id: format!("trade-{ordinal}"),
                        price: 100,
                        quantity: 2,
                        aggressor: aeris_market_data::AggressorSide::Buy,
                    }),
                }
            })
            .collect::<Vec<_>>();
        aeris_market_runtime::MarketTradeTapeSnapshot {
            consumer_id: aeris_market_runtime::MarketConsumerId(std::num::NonZeroU64::MIN),
            generation: aeris_market_runtime::MarketGenerationId(
                std::num::NonZeroU64::new(generation).expect("selection generation is nonzero"),
            ),
            provider_id: product.provider.clone(),
            instrument_id: product.instrument_id.clone(),
            entitlement_id: product.entitlement_id.clone(),
            provider_generation: 1,
            revision,
            source_watermark: last_ordinal,
            rewrite_generation: 0,
            price_scale: 0,
            quantity_scale: 0,
            trades: trades.into(),
        }
    }

    fn tape_message(
        snapshot: aeris_market_runtime::MarketTradeTapeSnapshot,
    ) -> PaneProjectionEvent {
        PaneProjectionEvent::RuntimeProjection(
            aeris_desktop::market_worker::RuntimeProjection::event(
                aeris_desktop::test_support::FixtureMarketEvents::default().trade_tape(snapshot),
            ),
        )
    }

    fn deliver_market_message(
        surface: &Entity<WorkspaceSurface>,
        publications: &MarketProjectionSender,
        message: PaneProjectionEvent,
        cx: &mut VisualTestContext,
    ) {
        publications.send(message).expect("market message");
        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                surface.poll_market_worker(cx);
            });
        });
    }

    fn requested_interval_sequence(
        surface: &Entity<WorkspaceSurface>,
        requests: &mpsc::Receiver<MarketWorkerCommand>,
        interval: ChartInterval,
        cx: &mut VisualTestContext,
    ) -> u64 {
        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                assert!(surface.select_interval(interval, cx));
            });
        });
        requests
            .try_iter()
            .find_map(|command| match command {
                MarketWorkerCommand::EngineSelect(request) => Some(request.sequence),
                _ => None,
            })
            .expect("engine selection")
    }

    /// The tape ordinal a chart applied for `instrument_id` drawn in `minutes` bars.
    fn applied_order_flow_ordinal(
        chart: &Entity<AerisChartView>,
        instrument_id: &str,
        minutes: u64,
        cx: &VisualTestContext,
    ) -> Option<u64> {
        cx.read(|cx| {
            chart.read(cx).order_flow_resume_ordinal(
                instrument_id,
                1,
                aeris_chart_integration::OrderFlowAggregation::TimeMicros(minutes * 60_000_000),
                1.0,
            )
        })
    }

    #[gpui::test]
    fn interval_switch_keeps_order_flow_off_the_superseded_chart(cx: &mut TestAppContext) {
        let product = InstallProviderInstrument {
            price_increment: Some(1),
            ..local_state::default_workspace().watchlist_entries[0]
                .instrument
                .clone()
                .expect("default instrument")
        };
        let (surface, requests, publications, cx) = harness_with(cx, product.clone());
        publish_snapshot(&surface, cx);
        let deliver = |message: PaneProjectionEvent, cx: &mut VisualTestContext| {
            deliver_market_message(&surface, &publications, message, cx);
        };
        let applied_ordinal =
            |chart: &Entity<AerisChartView>, minutes: u64, cx: &VisualTestContext| {
                applied_order_flow_ordinal(chart, &product.instrument_id, minutes, cx)
            };

        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                surface.set_chart_order_flow_settings(
                    OrderFlowSettings {
                        show_cumulative_delta: true,
                        show_delta_histogram: true,
                        big_trades: Some(BigTradesSettings::default()),
                        ..OrderFlowSettings::default()
                    },
                    cx,
                );
            });
        });
        deliver(tape_message(order_flow_tape(&product, 1, 1, 3)), cx);
        let one_minute = cx.read(|cx| surface.read(cx).chart.clone().expect("installed chart"));
        assert_eq!(applied_ordinal(&one_minute, 1, cx), Some(3));

        // The 5m selection is committed while the 1m chart stays on screen until the 5m
        // covering snapshot arrives; the instrument's tape keeps streaming meanwhile.
        let five_minutes =
            requested_interval_sequence(&surface, &requests, ChartInterval::Minute5, cx);
        deliver(
            PaneProjectionEvent::LocalCommandOutcome(
                aeris_desktop::market_worker::LocalCommandOutcome::EngineSelection {
                    sequence: five_minutes,
                    succeeded: true,
                },
            ),
            cx,
        );
        // A rewritten prefix must not invalidate the old chart's resume cursor.
        // The rewrite belongs to the replacement, which has not been drawn yet.
        let mut rewritten = order_flow_tape(&product, five_minutes, 2, 5);
        rewritten.rewrite_generation = 1;
        rewritten.provider_generation = 9;
        deliver(tape_message(rewritten), cx);
        assert_eq!(
            applied_ordinal(&one_minute, 1, cx),
            Some(3),
            "the superseded chart is not fed the committed selection's tape"
        );

        // A second selection before the 5m chart is drawn leaves the 1m chart on screen too.
        let fifteen_minutes =
            requested_interval_sequence(&surface, &requests, ChartInterval::Minute15, cx);
        let mut old_tape = order_flow_tape(&product, five_minutes, 3, 6);
        old_tape.provider_generation = 9;
        deliver(tape_message(old_tape), cx);
        assert_eq!(applied_ordinal(&one_minute, 1, cx), Some(3));

        deliver(
            PaneProjectionEvent::LocalCommandOutcome(
                aeris_desktop::market_worker::LocalCommandOutcome::EngineSelection {
                    sequence: fifteen_minutes,
                    succeeded: true,
                },
            ),
            cx,
        );
        // Even a higher revision from the retired selection cannot replace the
        // new binding's tape while the old chart is still on screen.
        deliver(
            tape_message(order_flow_tape(&product, five_minutes, 99, 99)),
            cx,
        );
        // A new selection may move to another provider session whose generation
        // starts lower than the previous one's. The selection fence, not the
        // provider's unrelated epoch, decides whether its tape is current.
        deliver(
            tape_message(order_flow_tape(&product, fifteen_minutes, 1, 6)),
            cx,
        );
        publish_snapshot_at_selection(&surface, fifteen_minutes, cx);
        let replacement = cx.read(|cx| surface.read(cx).chart.clone().expect("replacement chart"));
        assert_ne!(one_minute.entity_id(), replacement.entity_id());
        assert_eq!(
            applied_ordinal(&replacement, 15, cx),
            Some(6),
            "the replacement chart rebuilds its order flow from the retained tape"
        );
        deliver(
            tape_message(order_flow_tape(&product, fifteen_minutes, 2, 7)),
            cx,
        );
        assert_eq!(applied_ordinal(&replacement, 15, cx), Some(7));
    }

    #[gpui::test]
    fn invalid_quick_timeframe_stays_open_with_error_until_edited(cx: &mut TestAppContext) {
        let (surface, requests, _publications, cx) = harness(cx);
        cx.simulate_keystrokes("9 x");
        cx.simulate_keystrokes("enter");
        cx.read(|cx| {
            let surface = surface.read(cx);
            assert_eq!(surface.chrome_overlay, Some(ChromeOverlay::QuickTimeframe));
            assert_ne!(surface.chrome_overlay_phase, ChromeOverlayPhase::Closing);
            assert!(
                surface
                    .menu_state
                    .quick_timeframe_error
                    .as_ref()
                    .is_some_and(|error| error.message.contains("9x")),
                "an unmatched timeframe is reported, not silently ignored"
            );
        });
        assert!(
            requests
                .try_iter()
                .all(|command| !matches!(command, MarketWorkerCommand::EngineSelect(_))),
            "an invalid timeframe never reaches the market worker"
        );
        cx.simulate_keystrokes("backspace");
        cx.read(|cx| {
            let surface = surface.read(cx);
            assert_eq!(surface.chrome_overlay, Some(ChromeOverlay::QuickTimeframe));
            assert!(
                surface.menu_state.quick_timeframe_error.is_none(),
                "editing the text clears the error"
            );
        });
    }

    #[gpui::test]
    fn clicking_the_same_chrome_menu_trigger_twice_closes_it(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness(cx);
        cx.update(|window, cx| {
            surface.update(cx, |surface, surface_cx| {
                surface.open_chrome_overlay(ChromeOverlay::ChartType, window, surface_cx);
                assert_eq!(surface.chrome_overlay, Some(ChromeOverlay::ChartType));
                surface.open_chrome_overlay(ChromeOverlay::ChartType, window, surface_cx);
                assert_eq!(surface.chrome_overlay_phase, ChromeOverlayPhase::Closing);
            });
        });
    }

    #[gpui::test]
    fn symbol_search_replaces_selected_text_and_reopens_empty(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness(cx);
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.open_chrome_overlay(ChromeOverlay::Instrument, window, cx);
            });
        });
        cx.simulate_keystrokes("e t h");
        let input = cx.read(|cx| surface.read(cx).symbol_input.clone().expect("symbol input"));
        cx.read(|cx| assert_eq!(input.read(cx).value(), "eth"));

        // Select-all is cmd-a on macOS, where ctrl-a moves to the line start.
        let select_all = if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        };
        cx.simulate_keystrokes(&format!("{select_all} b t c"));
        cx.read(|cx| assert_eq!(input.read(cx).value(), "btc"));

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.close_chrome_overlay(window, cx);
            });
        });
        cx.read(|cx| assert!(input.read(cx).value().is_empty()));
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.open_chrome_overlay(ChromeOverlay::Instrument, window, cx);
            });
        });
        cx.read(|cx| assert!(input.read(cx).value().is_empty()));
        cx.simulate_keystrokes("s o l");
        cx.read(|cx| assert_eq!(input.read(cx).value(), "sol"));
    }

    #[gpui::test]
    fn closing_quick_menu_ignores_delayed_submit_and_change(cx: &mut TestAppContext) {
        let (surface, requests, _publications, cx) = harness(cx);
        cx.simulate_keystrokes("1 d escape");
        cx.update(|_, cx| {
            let input = surface.read(cx).timeframe_input.clone();
            input.update(cx, |_, cx| {
                cx.emit(InputEvent::Change);
                cx.emit(InputEvent::PressEnter {
                    secondary: false,
                    shift: false,
                });
            });
        });
        cx.run_until_parked();
        assert!(
            requests.try_recv().is_err(),
            "dismissal cannot select an interval"
        );
    }

    #[gpui::test]
    fn accounts_panel_opens_on_a_tab_and_drops_forms_when_leaving_it(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness(cx);
        cx.update(|window, cx| {
            surface.update(cx, |surface, surface_cx| {
                surface.open_accounts_panel(
                    accounts_panel::AccountsTab::TradingAccounts,
                    window,
                    surface_cx,
                );
            });
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let name = cx.new(|input_cx| InputState::new(window, input_cx));
            let equity = cx.new(|input_cx| InputState::new(window, input_cx));
            surface.update(cx, |surface, surface_cx| {
                assert_eq!(surface.chrome_overlay, Some(ChromeOverlay::Accounts));
                assert_eq!(
                    surface.menu_state.accounts_tab,
                    accounts_panel::AccountsTab::TradingAccounts
                );
                surface.trading_pnl.account_creator =
                    Some(PracticeAccountFormState { name, equity });
                surface.open_accounts_panel(
                    accounts_panel::AccountsTab::Connections,
                    window,
                    surface_cx,
                );
            });
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let surface = surface.read(cx);
            assert_eq!(
                surface.chrome_overlay,
                Some(ChromeOverlay::Accounts),
                "switching tabs keeps the panel open"
            );
            assert_eq!(
                surface.menu_state.accounts_tab,
                accounts_panel::AccountsTab::Connections
            );
            assert!(
                surface.trading_pnl.account_creator.is_none(),
                "an unfinished form never follows the panel to another tab"
            );
        });
    }

    #[gpui::test]
    fn accounts_panel_opens_centered_follows_a_drag_and_recenters_on_reopen(
        cx: &mut TestAppContext,
    ) {
        let (surface, _requests, _publications, cx) = harness(cx);
        let viewport = size(px(1600.0), px(1000.0));
        let extent = chrome_menu_extent(viewport, 80.0);
        let open = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                surface.update(cx, |surface, surface_cx| {
                    surface.open_accounts_panel(
                        accounts_panel::AccountsTab::Connections,
                        window,
                        surface_cx,
                    );
                });
            });
            cx.run_until_parked();
        };
        open(cx);
        let centered =
            cx.read(|cx| accounts_panel::accounts_panel_bounds(surface.read(cx), extent, viewport));
        assert_eq!(
            centered.origin,
            point(
                (viewport.width - centered.size.width) / 2.0,
                (viewport.height - centered.size.height) / 2.0
            ),
            "a fresh panel opens centered in the window"
        );
        cx.update(|_, cx| {
            surface.update(cx, |surface, surface_cx| {
                surface
                    .menu_state
                    .accounts_placement
                    .grab(point(px(40.0), px(10.0)));
                surface.move_accounts_panel(point(px(240.0), px(110.0)), surface_cx);
            });
        });
        let moved =
            cx.read(|cx| accounts_panel::accounts_panel_bounds(surface.read(cx), extent, viewport));
        assert_eq!(
            moved.origin,
            point(px(200.0), px(100.0)),
            "the panel follows the grab"
        );
        cx.update(|_, cx| {
            surface.update(cx, |surface, surface_cx| {
                surface.move_accounts_panel(point(px(-900.0), px(-900.0)), surface_cx);
            });
        });
        let clamped =
            cx.read(|cx| accounts_panel::accounts_panel_bounds(surface.read(cx), extent, viewport));
        assert_eq!(
            clamped.origin,
            point(px(OVERLAY_EDGE_MARGIN), px(OVERLAY_EDGE_MARGIN)),
            "a drag past the edge keeps the panel inside the window"
        );
        cx.update(|window, cx| {
            surface.update(cx, |surface, surface_cx| {
                surface.close_chrome_overlay(window, surface_cx);
            });
        });
        cx.run_until_parked();
        open(cx);
        let reopened =
            cx.read(|cx| accounts_panel::accounts_panel_bounds(surface.read(cx), extent, viewport));
        assert_eq!(
            reopened.origin, centered.origin,
            "every fresh open starts centered"
        );
    }

    #[test]
    fn accounts_panel_shrinks_to_the_room_under_the_header() {
        let roomy = accounts_panel::accounts_panel_size(chrome_menu_extent(
            size(px(1600.0), px(1000.0)),
            80.0,
        ));
        assert_eq!(
            roomy,
            size(px(ACCOUNTS_PANEL_WIDTH), px(ACCOUNTS_PANEL_HEIGHT))
        );
        let small = accounts_panel::accounts_panel_size(chrome_menu_extent(
            size(px(500.0), px(400.0)),
            80.0,
        ));
        assert!(small.width < px(500.0) && small.height < px(400.0 - 80.0));
    }

    #[gpui::test]
    fn digits_typed_in_the_order_quantity_field_stay_in_the_field(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness(cx);
        let ticket = cx.read(|cx| surface.read(cx).order_quantity.ticket_input().clone());
        cx.update(|window, cx| {
            ticket.update(cx, |input, input_cx| input.focus(window, input_cx));
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("5");

        cx.read(|cx| {
            assert_eq!(surface.read(cx).chrome_overlay, None);
            assert!(
                ticket.read(cx).value().contains('5'),
                "the digit lands in the quantity field"
            );
        });
    }

    #[gpui::test]
    fn practice_account_name_input_does_not_open_symbol_typeahead(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness(cx);
        cx.update(|window, cx| {
            let name = cx.new(|input_cx| InputState::new(window, input_cx));
            let equity = cx.new(|input_cx| InputState::new(window, input_cx));
            name.update(cx, |input, input_cx| input.focus(window, input_cx));
            surface.update(cx, |surface, surface_cx| {
                surface.trading_pnl.account_creator =
                    Some(PracticeAccountFormState { name, equity });
                surface_cx.notify();
            });
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("a");

        cx.read(|cx| {
            let surface = surface.read(cx);
            assert_eq!(surface.chrome_overlay, None);
            let creator = surface
                .trading_pnl
                .account_creator
                .as_ref()
                .expect("practice account form remains open");
            assert_eq!(creator.name.read(cx).value().as_ref(), "a");
        });
    }

    #[gpui::test]
    fn normal_timeframe_menu_keeps_keyboard_submission(cx: &mut TestAppContext) {
        let (surface, requests, _publications, cx) = harness(cx);
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.open_chrome_overlay(ChromeOverlay::Timeframe, window, cx);
            });
        });
        // Minutes -> Hours -> Days, then open the flyout and choose one day.
        cx.simulate_keystrokes("down down right enter");
        let selections = requests
            .try_iter()
            .filter_map(|command| match command {
                MarketWorkerCommand::EngineSelect(request) => Some(request.interval),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(selections, [ChartInterval::Day1]);
    }

    fn install_chart(
        surface: &Entity<WorkspaceSurface>,
        big_trades: Option<BigTradesSettings>,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                surface.chart = Some(cx.new(|_| AerisChartView::empty()));
                surface.set_chart_order_flow_settings(
                    OrderFlowSettings {
                        big_trades,
                        ..OrderFlowSettings::default()
                    },
                    cx,
                );
            });
        });
    }

    fn chart_big_trades(
        surface: &Entity<WorkspaceSurface>,
        cx: &VisualTestContext,
    ) -> Option<BigTradesSettings> {
        cx.read(|cx| {
            surface
                .read(cx)
                .chart_order_flow_settings(cx)
                .and_then(|settings| settings.big_trades)
        })
    }

    fn big_trades_minimum_input(
        surface: &Entity<WorkspaceSurface>,
        cx: &VisualTestContext,
    ) -> Entity<InputState> {
        cx.read(|cx| {
            surface
                .read(cx)
                .big_trades_dialog()
                .expect("big trades dialog is open")
                .minimum_volume
                .clone()
        })
    }

    fn publish_snapshot(surface: &Entity<WorkspaceSurface>, cx: &mut VisualTestContext) {
        publish_snapshot_at_selection(surface, 1, cx);
    }

    fn publish_snapshot_at_selection(
        surface: &Entity<WorkspaceSurface>,
        selection_generation: u64,
        cx: &mut VisualTestContext,
    ) {
        let snapshot = aeris_application::EmbeddedReplaySource
            .load_snapshot(aeris_application::LoadEmbeddedReplay { bar_count: 8 })
            .expect("embedded replay");
        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                surface.apply_publication(
                    aeris_desktop::market_worker::MarketWorkerPublication {
                        update: aeris_application::ReplayStreamUpdate::Snapshot(snapshot),
                        generation:
                            aeris_desktop::market_worker::MarketPublicationGeneration::from_tail(
                                1, 8, 1, 8,
                            )
                            .with_selection_generation(selection_generation),
                        subscription_id: "timeframe_test".into(),
                        worker_label: "timeframe_test".into(),
                        ui_diagnostics: None,
                    },
                    cx,
                );
            });
        });
        cx.run_until_parked();
    }

    struct TerminalHarness(Entity<TerminalApp>);

    impl Render for TerminalHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    fn terminal_harness(cx: &mut TestAppContext) -> (Entity<TerminalApp>, &mut VisualTestContext) {
        terminal_harness_with(cx, DesktopLifecycle::new_for_test(), None)
    }

    /// A terminal that opens new tabs; their market endpoints wait unstarted in the returned
    /// queue.
    fn tabbed_terminal_harness(
        cx: &mut TestAppContext,
    ) -> (
        Entity<TerminalApp>,
        engine_market_worker::UnstartedWorkspaceAdditions,
        &mut VisualTestContext,
    ) {
        let lifecycle = DesktopLifecycle::new_for_test();
        let (factory, additions) =
            engine_market_worker::WorkspaceMarketFactory::unstarted(lifecycle.market().clone());
        let (terminal, cx) = terminal_harness_with(cx, lifecycle, Some(factory));
        (terminal, additions, cx)
    }

    fn terminal_harness_with(
        cx: &mut TestAppContext,
        lifecycle: DesktopLifecycle,
        factory: Option<engine_market_worker::WorkspaceMarketFactory>,
    ) -> (Entity<TerminalApp>, &mut VisualTestContext) {
        let product = local_state::default_workspace().watchlist_entries[0]
            .instrument
            .clone()
            .expect("default instrument");
        let (commands, _requests) = mpsc::sync_channel(64);
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
            bind_desktop_keys(cx).expect("the registry shortcuts bind");
            let terminal = terminal_root(
                MarketWorkerStartup::Loading(Box::new(EngineWorkerStartup {
                    product,
                    interval: ChartInterval::Minute1,
                    restored_viewport: None,
                    subscription_id: "timeframe_test".into(),
                    worker_label: "timeframe_test".into(),
                })),
                worker,
                factory,
                &lifecycle,
                chart_chrome::ChartChromePreferences::default(),
                window,
                cx,
            );
            cx.observe(&terminal, |_, _, cx| cx.notify()).detach();
            TerminalHarness(terminal)
        });
        let terminal = cx.read(|cx| view.read(cx).0.clone());
        cx.run_until_parked();
        (terminal, cx)
    }

    #[gpui::test]
    fn keyboard_focus_returns_to_the_workspace_when_its_target_leaves_the_screen(
        cx: &mut TestAppContext,
    ) {
        let (terminal, cx) = terminal_harness(cx);
        let surface = cx.read(|cx| terminal.read(cx).active_surface());
        publish_snapshot(&surface, cx);
        let workspace_focused = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| terminal.read(cx).workspace_keyboard_focused(window, cx))
        };
        let chart_focused = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                surface
                    .read(cx)
                    .chart
                    .as_ref()
                    .is_some_and(|chart| chart.read(cx).has_keyboard_focus(window))
            })
        };
        cx.update(|window, cx| terminal.read(cx).chrome_focus.clone().focus(window, cx));
        cx.run_until_parked();
        assert!(workspace_focused(cx), "the shell counts as workspace focus");

        // Any focus target that is not drawn (a tab strip a frameless window hides, a closed
        // pane or dialog) would leave keys with no workspace handler to reach.
        let _off_screen = cx.update(|window, cx| {
            let handle = cx.focus_handle();
            handle.focus(window, cx);
            handle
        });
        cx.run_until_parked();
        assert!(chart_focused(cx), "lost focus returns to the active chart");
        assert!(workspace_focused(cx));

        // A symbol or interval switch swaps in a chart built from the replacement's snapshot.
        let first = cx.read(|cx| surface.read(cx).chart.clone().expect("installed chart"));
        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                surface.chart_pane_controller.request(2);
                surface.commit_pending_selection(2, cx);
            });
        });
        publish_snapshot_at_selection(&surface, 2, cx);
        let second = cx.read(|cx| surface.read(cx).chart.clone().expect("replacement chart"));
        assert_ne!(first.entity_id(), second.entity_id());
        drop(first);
        cx.run_until_parked();
        assert!(
            chart_focused(cx),
            "the replacement chart takes keyboard focus without another click"
        );
        assert!(workspace_focused(cx));

        // Ctrl+Tab lands in the selected workspace, as a browser lands in the selected page.
        cx.update(|window, cx| terminal.read(cx).chrome_focus.clone().focus(window, cx));
        cx.run_until_parked();
        assert!(!chart_focused(cx));
        cx.simulate_keystrokes("ctrl-tab");
        assert!(chart_focused(cx), "Ctrl+Tab focuses the workspace's chart");
    }

    #[gpui::test]
    fn keyboard_shortcuts_dialog_blocks_trading_keys_and_closes_on_escape(cx: &mut TestAppContext) {
        let (terminal, cx) = terminal_harness(cx);
        let surface = cx.read(|cx| terminal.read(cx).active_surface());
        publish_snapshot(&surface, cx);
        let shortcuts_open =
            |cx: &mut VisualTestContext| cx.read(|cx| terminal.read(cx).keyboard_shortcuts_open);
        let confirming = |cx: &mut VisualTestContext| {
            cx.read(|cx| terminal.read(cx).keyboard_trading.confirming())
        };

        cx.update(|window, cx| {
            terminal.update(cx, |terminal, cx| {
                terminal.open_keyboard_shortcuts(window, cx);
            });
        });
        cx.run_until_parked();
        assert!(shortcuts_open(cx));
        cx.simulate_keystrokes("ctrl-shift-x");
        assert!(
            !confirming(cx),
            "trading keys do nothing behind the shortcuts list"
        );
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("x").expect("key"),
        });

        cx.simulate_keystrokes("escape");
        assert!(!shortcuts_open(cx), "Escape closes the shortcuts list");
        cx.simulate_keystrokes("ctrl-shift-x");
        assert!(confirming(cx), "trading keys work again once it closes");
    }

    #[gpui::test]
    fn trading_shortcuts_ask_first_and_send_once_per_press(cx: &mut TestAppContext) {
        let (terminal, cx) = terminal_harness(cx);
        let surface = cx.read(|cx| terminal.read(cx).active_surface());
        publish_snapshot(&surface, cx);
        let confirming = |cx: &mut VisualTestContext| {
            cx.read(|cx| terminal.read(cx).keyboard_trading.confirming())
        };
        let release = |cx: &mut VisualTestContext, key: &str| {
            cx.simulate_event(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse(key).expect("key"),
            });
        };
        cx.update(|window, cx| terminal.read(cx).chrome_focus.clone().focus(window, cx));
        cx.run_until_parked();

        cx.simulate_keystrokes("ctrl-shift-x");
        assert!(confirming(cx), "a shortcut works at once and asks first");
        cx.simulate_keystrokes("escape");
        assert!(!confirming(cx), "Escape cancels the confirmation");
        cx.simulate_keystrokes("ctrl-shift-x");
        assert!(
            !confirming(cx),
            "a key repeat of the held chord does not ask again"
        );
        release(cx, "x");
        cx.simulate_keystrokes("ctrl-shift-x");
        assert!(confirming(cx), "a new press asks again");
        cx.simulate_keystrokes("enter");
        assert!(!confirming(cx), "Enter confirms");
        release(cx, "enter");

        cx.update(|_, cx| {
            terminal.update(cx, |terminal, _| {
                terminal.chart_chrome.trading_shortcuts =
                    crate::desktop::chart_chrome::TradingShortcutMode::OneClick;
            });
        });
        cx.simulate_keystrokes("ctrl-shift-x");
        assert!(!confirming(cx), "one-click trading sends without asking");
        release(cx, "x");
    }

    #[gpui::test]
    fn big_trades_settings_open_only_while_the_indicator_is_on_the_chart(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness(cx);
        install_chart(&surface, None, cx);
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.open_big_trades_dialog(window, cx);
                assert!(surface.big_trades_dialog().is_none());
                assert!(
                    surface
                        .add_order_flow_study(workspace_surface::OrderFlowMenuStudy::BigTrades, cx)
                );
                surface.open_big_trades_dialog(window, cx);
                assert!(surface.big_trades_dialog().is_some());
            });
        });
        assert_eq!(
            chart_big_trades(&surface, cx),
            Some(BigTradesSettings::default()),
            "the indicator menu adds the default indicator"
        );
    }

    #[gpui::test]
    fn order_book_on_chart_holds_the_book_demand_until_removed(cx: &mut TestAppContext) {
        let (surface, requests, _publications, cx) = harness(cx);
        install_chart(&surface, None, cx);
        let depth_demands = || order_book_demands(&requests);
        depth_demands();
        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                assert!(surface.add_order_flow_study(
                    workspace_surface::OrderFlowMenuStudy::OrderBookDepth,
                    cx
                ));
            });
        });
        cx.run_until_parked();
        assert_eq!(
            depth_demands(),
            vec![OrderBookDetail::Depth],
            "the chart's book opens the demand"
        );

        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                surface.set_side_panel_visible(SidePanel::OrderBook, true, cx);
                surface.set_side_panel_visible(SidePanel::OrderBook, false, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(
            depth_demands(),
            Vec::<OrderBookDetail>::new(),
            "the order book panel shares the open demand"
        );

        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                let settings = surface.chart_order_flow_settings(cx).expect("chart");
                surface.set_chart_order_flow_settings(
                    OrderFlowSettings {
                        order_book_depth: None,
                        ..settings
                    },
                    cx,
                );
            });
        });
        cx.run_until_parked();
        assert_eq!(
            depth_demands(),
            vec![OrderBookDetail::Hidden],
            "removing it releases the demand"
        );
    }

    fn order_book_demands(requests: &mpsc::Receiver<MarketWorkerCommand>) -> Vec<OrderBookDetail> {
        requests
            .try_iter()
            .filter_map(|command| match command {
                MarketWorkerCommand::OrderBook(detail) => Some(detail),
                _ => None,
            })
            .collect()
    }

    #[gpui::test]
    fn queue_column_holds_order_levels_only_while_the_docked_book_shows_it(
        cx: &mut TestAppContext,
    ) {
        let (surface, requests, _publications, cx) = harness_with(
            cx,
            aeris_desktop::test_support::fixture_futures_contract("MNQ"),
        );
        order_book_demands(&requests);
        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                surface.set_side_panel_visible(SidePanel::OrderBook, true, cx);
                surface.toggle_order_book_column(OrderBookColumn::Queue, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(
            order_book_demands(&requests),
            vec![OrderBookDetail::Depth, OrderBookDetail::OrderQueues],
            "the docked book opens depth and its queue column adds order levels"
        );

        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                surface.toggle_order_book_column(OrderBookColumn::Queue, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(
            order_book_demands(&requests),
            vec![OrderBookDetail::Depth],
            "hiding the queue keeps the book's depth and releases its order levels"
        );

        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                surface.toggle_order_book_column(OrderBookColumn::Queue, cx);
                surface.set_side_panel_visible(SidePanel::OrderBook, false, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(
            order_book_demands(&requests),
            vec![OrderBookDetail::OrderQueues, OrderBookDetail::Hidden],
            "closing the docked book releases everything it held"
        );
    }

    #[gpui::test]
    fn queue_column_is_not_offered_by_a_price_level_provider(cx: &mut TestAppContext) {
        let (surface, requests, _publications, cx) = harness(cx);
        order_book_demands(&requests);
        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                surface.set_side_panel_visible(SidePanel::OrderBook, true, cx);
                surface.toggle_order_book_column(OrderBookColumn::Queue, cx);
            });
        });
        cx.run_until_parked();
        assert_eq!(
            order_book_demands(&requests),
            vec![OrderBookDetail::Depth],
            "a provider without order levels is never asked for them"
        );
        cx.read(|cx| {
            let columns = surface.read(cx).order_book.read(cx).columns();
            assert!(!columns.is_offered(OrderBookColumn::Queue));
            assert!(!columns.is_visible(OrderBookColumn::Queue));
        });
    }

    #[gpui::test]
    fn big_trades_dialog_applies_only_a_valid_draft_and_keeps_visibility(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness(cx);
        let hidden = BigTradesSettings {
            visible: false,
            ..BigTradesSettings::default()
        };
        install_chart(&surface, Some(hidden), cx);
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| surface.open_big_trades_dialog(window, cx));
        });
        assert!(cx.debug_bounds("big_trades_dialog").is_some());
        let input = big_trades_minimum_input(&surface, cx);
        cx.read(|cx| {
            let dialog = surface.read(cx).big_trades_dialog().expect("dialog");
            assert_eq!(
                dialog
                    .intensity
                    .map(|intensity| BigTradesFilter::Auto { intensity }),
                Some(hidden.filter)
            );
            assert_eq!(dialog.size, hidden.size);
            assert_eq!(dialog.show_volume, hidden.show_volume);
            assert_eq!(
                input.read(cx).value(),
                "",
                "nothing is drawn yet, so no automatic threshold seeds the fixed minimum"
            );
        });

        cx.update(|window, cx| {
            input.update(cx, |input, cx| input.set_value("0", window, cx));
            surface.update(cx, |surface, cx| {
                surface.set_big_trades_dialog_filter(None, cx);
                surface.set_big_trades_dialog_size(BigTradesSize::Large, cx);
                surface.set_big_trades_dialog_show_volume(false, cx);
                surface.chart_persistence_dirty = false;
                surface.apply_big_trades_dialog(cx);
            });
        });
        cx.read(|cx| {
            let surface = surface.read(cx);
            let message = surface
                .big_trades_dialog()
                .and_then(|dialog| dialog.message.as_deref());
            assert!(
                message.is_some_and(|message| message.contains("greater than zero")),
                "an invalid minimum keeps the dialog open: {message:?}"
            );
            assert!(!surface.chart_persistence_dirty);
        });
        assert!(
            cx.debug_bounds("big_trades_dialog").is_some(),
            "the fixed draft renders its minimum volume input"
        );
        assert_eq!(chart_big_trades(&surface, cx), Some(hidden));

        cx.update(|window, cx| {
            input.update(cx, |input, cx| input.set_value(" 25.5 ", window, cx));
            surface.update(cx, WorkspaceSurface::apply_big_trades_dialog);
        });
        cx.read(|cx| {
            let surface = surface.read(cx);
            assert!(surface.big_trades_dialog().is_none());
            assert!(surface.chart_persistence_dirty);
        });
        assert!(cx.debug_bounds("big_trades_dialog").is_none());
        assert_eq!(
            chart_big_trades(&surface, cx),
            Some(BigTradesSettings {
                filter: BigTradesFilter::Fixed {
                    minimum_volume: 25.5
                },
                size: BigTradesSize::Large,
                show_volume: false,
                visible: false,
            }),
            "the draft applies while the legend keeps owning visibility"
        );
    }

    #[gpui::test]
    fn big_trades_dialog_reopens_on_the_fixed_minimum_and_resets_only_the_draft(
        cx: &mut TestAppContext,
    ) {
        let (surface, _requests, _publications, cx) = harness(cx);
        let fixed = BigTradesSettings {
            filter: BigTradesFilter::Fixed {
                minimum_volume: 0.1 + 0.2,
            },
            size: BigTradesSize::Small,
            show_volume: false,
            visible: true,
        };
        install_chart(&surface, Some(fixed), cx);
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| surface.open_big_trades_dialog(window, cx));
        });
        let input = big_trades_minimum_input(&surface, cx);
        cx.read(|cx| {
            let dialog = surface.read(cx).big_trades_dialog().expect("dialog");
            assert_eq!(dialog.intensity, None);
            assert_eq!(dialog.size, BigTradesSize::Small);
            assert!(!dialog.show_volume);
            assert_eq!(
                input.read(cx).value(),
                "0.3",
                "a fractional minimum reads without binary rounding noise"
            );
        });

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.reset_big_trades_dialog(window, cx);
            });
        });
        cx.read(|cx| {
            let defaults = BigTradesSettings::default();
            let dialog = surface.read(cx).big_trades_dialog().expect("dialog");
            assert_eq!(
                dialog
                    .intensity
                    .map(|intensity| BigTradesFilter::Auto { intensity }),
                Some(defaults.filter)
            );
            assert_eq!(dialog.size, defaults.size);
            assert_eq!(dialog.show_volume, defaults.show_volume);
        });

        cx.update(|_, cx| surface.update(cx, WorkspaceSurface::close_big_trades_dialog));
        assert!(cx.debug_bounds("big_trades_dialog").is_none());
        assert_eq!(
            chart_big_trades(&surface, cx),
            Some(fixed),
            "only Apply changes the chart"
        );
    }

    fn chart_volume(
        surface: &Entity<WorkspaceSurface>,
        cx: &VisualTestContext,
    ) -> Option<VolumeSettings> {
        cx.read(|cx| {
            surface
                .read(cx)
                .chart
                .as_ref()
                .and_then(|chart| chart.read(cx).volume_settings())
        })
    }

    fn volume_length_input(
        surface: &Entity<WorkspaceSurface>,
        cx: &VisualTestContext,
    ) -> Entity<InputState> {
        cx.read(|cx| {
            surface
                .read(cx)
                .volume_dialog()
                .expect("volume dialog is open")
                .average_length
                .clone()
        })
    }

    fn open_volume_dialog_on_a_volume_chart(
        surface: &Entity<WorkspaceSurface>,
        cx: &mut VisualTestContext,
    ) {
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                let chart = cx.new(|_| AerisChartView::new());
                chart.update(cx, |chart, _| {
                    chart
                        .add_indicator(ChartIndicator::Volume)
                        .expect("volume is available");
                });
                surface.chart = Some(chart);
                surface.open_volume_dialog(window, cx);
            });
        });
    }

    #[gpui::test]
    fn volume_dialog_keeps_an_invalid_draft_open_without_changing_the_chart(
        cx: &mut TestAppContext,
    ) {
        let (surface, _requests, _publications, cx) = harness(cx);
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.chart = Some(cx.new(|_| AerisChartView::new()));
                surface.open_volume_dialog(window, cx);
                assert!(surface.volume_dialog().is_none());
            });
        });
        open_volume_dialog_on_a_volume_chart(&surface, cx);
        assert!(cx.debug_bounds("volume_dialog").is_some());
        let input = volume_length_input(&surface, cx);
        cx.read(|cx| {
            let dialog = surface.read(cx).volume_dialog().expect("dialog");
            assert_eq!(dialog.draft, VolumeSettings::default());
            assert!(!dialog.draft.average.enabled, "the average starts off");
            assert_eq!(input.read(cx).value(), "20");
        });

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.set_volume_dialog_average_enabled(true, cx);
                surface.toggle_volume_dialog_color_picker(VolumeColorTarget::Up, window, cx);
                surface.set_volume_dialog_color(VolumeColorTarget::Up, "not a color", cx);
                let dialog = surface.volume_dialog().expect("dialog");
                assert!(
                    dialog
                        .color_picker
                        .as_ref()
                        .is_some_and(|picker| picker.error.is_some()),
                    "an invalid color is reported in its picker"
                );
                assert_eq!(dialog.draft.up_color, None);
                surface.set_volume_dialog_color(VolumeColorTarget::Up, "#123456", cx);
            });
            input.update(cx, |input, cx| input.set_value("0", window, cx));
            surface.update(cx, |surface, cx| {
                surface.chart_persistence_dirty = false;
                surface.apply_volume_dialog(cx);
            });
        });
        cx.read(|cx| {
            let surface = surface.read(cx);
            let dialog = surface.volume_dialog().expect("dialog stays open");
            assert!(
                dialog
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("1 to 500")),
                "an invalid length keeps the dialog open: {:?}",
                dialog.message
            );
            assert!(
                dialog
                    .color_picker
                    .as_ref()
                    .is_some_and(|picker| picker.error.is_none())
            );
            assert!(!surface.chart_persistence_dirty);
        });
        assert!(
            cx.debug_bounds("volume_dialog").is_some(),
            "the enabled average renders its settings and the open picker"
        );
        assert_eq!(chart_volume(&surface, cx), Some(VolumeSettings::default()));
    }

    #[gpui::test]
    fn volume_dialog_applies_a_valid_draft_and_reset_waits_for_apply(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness(cx);
        open_volume_dialog_on_a_volume_chart(&surface, cx);
        let input = volume_length_input(&surface, cx);
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.set_volume_dialog_average_enabled(true, cx);
                surface.set_volume_dialog_average_kind(VolumeAverageKind::Exponential, cx);
                surface.set_volume_dialog_placement(VolumePlacement::OwnPane, cx);
                surface.set_volume_dialog_color(VolumeColorTarget::Up, "#123456", cx);
                surface.set_volume_dialog_color(VolumeColorTarget::Average, "#ff9800", cx);
                surface.chart_persistence_dirty = false;
            });
            input.update(cx, |input, cx| input.set_value(" 50 ", window, cx));
            surface.update(cx, WorkspaceSurface::apply_volume_dialog);
        });
        let applied = VolumeSettings {
            up_color: Some("#123456".to_string()),
            down_color: None,
            average: VolumeAverageSettings {
                enabled: true,
                kind: VolumeAverageKind::Exponential,
                length: 50,
                color: Some("#FF9800".to_string()),
            },
            placement: VolumePlacement::OwnPane,
        };
        cx.read(|cx| {
            let surface = surface.read(cx);
            assert!(surface.volume_dialog().is_none());
            assert!(surface.chart_persistence_dirty);
        });
        assert!(cx.debug_bounds("volume_dialog").is_none());
        assert_eq!(chart_volume(&surface, cx), Some(applied.clone()));

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.open_volume_dialog(window, cx);
                assert_eq!(
                    surface.volume_dialog().map(|dialog| &dialog.draft),
                    Some(&applied)
                );
                surface.reset_volume_dialog(window, cx);
                assert_eq!(
                    surface.volume_dialog().map(|dialog| &dialog.draft),
                    Some(&VolumeSettings::default())
                );
            });
        });
        let input = volume_length_input(&surface, cx);
        cx.read(|cx| assert_eq!(input.read(cx).value(), "20"));
        cx.update(|_, cx| surface.update(cx, WorkspaceSurface::close_volume_dialog));
        assert_eq!(
            chart_volume(&surface, cx),
            Some(applied),
            "only Apply changes the chart"
        );
    }

    fn chart_order_flow(
        surface: &Entity<WorkspaceSurface>,
        cx: &VisualTestContext,
    ) -> OrderFlowSettings {
        cx.read(|cx| {
            surface
                .read(cx)
                .chart_order_flow_settings(cx)
                .expect("chart is installed")
        })
    }

    /// A chart showing CVD, the delta histogram and a hidden order book, with the CVD dialog
    /// refused while it is not added.
    fn install_order_flow_chart(surface: &Entity<WorkspaceSurface>, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.chart = Some(cx.new(|_| AerisChartView::new()));
                surface.open_order_flow_dialog(OrderFlowIndicator::CumulativeDelta, window, cx);
                assert!(
                    surface.order_flow_dialog().is_none(),
                    "CVD is not added yet"
                );
                assert!(surface.set_chart_order_flow_settings(
                    OrderFlowSettings {
                        show_cumulative_delta: true,
                        show_delta_histogram: true,
                        order_book_depth: Some(OrderBookDepthSettings {
                            visible: false,
                            ..OrderBookDepthSettings::default()
                        }),
                        ..OrderFlowSettings::default()
                    },
                    cx,
                ));
            });
        });
    }

    #[gpui::test]
    fn order_flow_study_dialogs_apply_only_the_draft_of_their_own_indicator(
        cx: &mut TestAppContext,
    ) {
        let (surface, _requests, _publications, cx) = harness(cx);
        install_order_flow_chart(&surface, cx);
        let before = chart_order_flow(&surface, cx);
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.open_order_flow_dialog(OrderFlowIndicator::CumulativeDelta, window, cx);
                let dialog = surface.order_flow_dialog().expect("CVD dialog");
                assert_eq!(
                    dialog.effective_color(OrderFlowColorTarget::CumulativeDeltaLine),
                    dialog.defaults.cumulative_delta_line,
                    "an unset color shows the color the chart draws"
                );
                surface.set_order_flow_dialog_style(CumulativeDeltaStyle::Candles, cx);
                surface.set_order_flow_dialog_reset(CumulativeDeltaReset::Continuous, cx);
                surface.toggle_order_flow_dialog_color_picker(
                    OrderFlowColorTarget::CumulativeDeltaUp,
                    window,
                    cx,
                );
                surface.set_order_flow_dialog_color(
                    OrderFlowColorTarget::CumulativeDeltaUp,
                    "not a color",
                    cx,
                );
                let dialog = surface.order_flow_dialog().expect("CVD dialog");
                assert!(
                    dialog
                        .color_picker
                        .as_ref()
                        .is_some_and(|picker| picker.error.is_some()),
                    "an invalid color is reported in its picker"
                );
                surface.set_order_flow_dialog_color(
                    OrderFlowColorTarget::CumulativeDeltaUp,
                    "#123456",
                    cx,
                );
                surface.set_order_flow_dialog_color(
                    OrderFlowColorTarget::DeltaPositive,
                    "#654321",
                    cx,
                );
            });
        });
        assert!(cx.debug_bounds("order_flow_dialog").is_some());
        assert_eq!(
            chart_order_flow(&surface, cx),
            before,
            "drafts wait for Apply"
        );
        cx.update(|_, cx| {
            surface.update(cx, |surface, cx| {
                surface.chart_persistence_dirty = false;
                surface.apply_order_flow_dialog(cx);
            });
        });
        let expected_cvd = CumulativeDeltaSettings {
            style: CumulativeDeltaStyle::Candles,
            reset: CumulativeDeltaReset::Continuous,
            up_color: ChartColor::from_hex("#123456"),
            ..CumulativeDeltaSettings::default()
        };
        cx.read(|cx| {
            let surface = surface.read(cx);
            assert!(surface.order_flow_dialog().is_none());
            assert!(surface.chart_persistence_dirty);
        });
        assert!(cx.debug_bounds("order_flow_dialog").is_none());
        let applied = chart_order_flow(&surface, cx);
        assert_eq!(
            applied,
            OrderFlowSettings {
                cumulative_delta: expected_cvd,
                ..before
            },
            "a color of another indicator is not part of the CVD draft"
        );

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.open_order_flow_dialog(OrderFlowIndicator::CumulativeDelta, window, cx);
                surface.reset_order_flow_dialog(window, cx);
                let dialog = surface.order_flow_dialog().expect("CVD dialog");
                assert!(matches!(
                    dialog.draft,
                    OrderFlowDialogDraft::CumulativeDelta(draft)
                        if draft == CumulativeDeltaSettings::default()
                ));
                surface.close_order_flow_dialog(cx);
            });
        });
        assert_eq!(
            chart_order_flow(&surface, cx).cumulative_delta,
            expected_cvd,
            "reset changes only the draft"
        );
    }

    #[gpui::test]
    fn delta_histogram_dialog_cancel_discards_and_apply_changes_only_its_colors(
        cx: &mut TestAppContext,
    ) {
        let (surface, _requests, _publications, cx) = harness(cx);
        install_order_flow_chart(&surface, cx);
        let before = chart_order_flow(&surface, cx);
        let pick_negative = |surface: &mut WorkspaceSurface,
                             window: &mut Window,
                             cx: &mut Context<WorkspaceSurface>| {
            surface.open_order_flow_dialog(OrderFlowIndicator::DeltaHistogram, window, cx);
            surface.set_order_flow_dialog_color(
                OrderFlowColorTarget::DeltaNegative,
                "#ABCDEF80",
                cx,
            );
        };
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                pick_negative(surface, window, cx);
                surface.close_order_flow_dialog(cx);
            });
        });
        assert_eq!(chart_order_flow(&surface, cx), before, "Cancel discards");
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                pick_negative(surface, window, cx);
                surface.apply_order_flow_dialog(cx);
                assert!(surface.order_flow_dialog().is_none());
            });
        });
        assert_eq!(
            chart_order_flow(&surface, cx),
            OrderFlowSettings {
                delta_histogram: DeltaHistogramSettings {
                    up_color: None,
                    down_color: ChartColor::from_hex("#ABCDEF80"),
                },
                ..before
            }
        );
    }

    #[gpui::test]
    fn order_book_dialog_validates_its_numbers_and_keeps_visibility(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness(cx);
        install_order_flow_chart(&surface, cx);
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.open_order_flow_dialog(OrderFlowIndicator::OrderBook, window, cx);
            });
        });
        assert!(cx.debug_bounds("order_flow_dialog").is_some());
        let inputs = cx.read(|cx| {
            let dialog = surface.read(cx).order_flow_dialog().expect("book dialog");
            let OrderFlowDialogDraft::OrderBook(draft) = &dialog.draft else {
                panic!("the order book draft");
            };
            assert_eq!(draft.levels_per_side.read(cx).value(), "40");
            assert_eq!(draft.max_width.read(cx).value(), "120");
            assert_eq!(draft.opacity_percent.read(cx).value(), "50");
            [
                draft.levels_per_side.clone(),
                draft.max_width.clone(),
                draft.opacity_percent.clone(),
            ]
        });
        let [levels, width, opacity] = inputs;
        cx.update(|window, cx| {
            levels.update(cx, |input, cx| input.set_value("0", window, cx));
            surface.update(cx, WorkspaceSurface::apply_order_flow_dialog);
        });
        cx.read(|cx| {
            let dialog = surface
                .read(cx)
                .order_flow_dialog()
                .expect("dialog stays open");
            assert!(
                dialog
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("1 to 200")),
                "{:?}",
                dialog.message
            );
        });
        assert_eq!(
            chart_order_flow(&surface, cx).order_book_depth,
            Some(OrderBookDepthSettings {
                visible: false,
                ..OrderBookDepthSettings::default()
            })
        );

        cx.update(|window, cx| {
            levels.update(cx, |input, cx| input.set_value(" 80 ", window, cx));
            width.update(cx, |input, cx| input.set_value("200", window, cx));
            opacity.update(cx, |input, cx| input.set_value("70", window, cx));
            surface.update(cx, |surface, cx| {
                surface.set_order_flow_dialog_show_sizes(false, cx);
                surface.set_order_flow_dialog_color(OrderFlowColorTarget::Bid, "#112233", cx);
                surface.apply_order_flow_dialog(cx);
                assert!(surface.order_flow_dialog().is_none());
            });
        });
        let applied = OrderBookDepthSettings {
            visible: false,
            levels_per_side: 80,
            max_width: 200,
            opacity_percent: 70,
            show_sizes: false,
            bid_color: ChartColor::from_hex("#112233"),
            ask_color: None,
        };
        assert_eq!(
            chart_order_flow(&surface, cx).order_book_depth,
            Some(applied)
        );

        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.open_order_flow_dialog(OrderFlowIndicator::OrderBook, window, cx);
                surface.reset_order_flow_dialog(window, cx);
            });
        });
        cx.read(|cx| {
            let dialog = surface.read(cx).order_flow_dialog().expect("book dialog");
            let OrderFlowDialogDraft::OrderBook(draft) = &dialog.draft else {
                panic!("the order book draft");
            };
            assert_eq!(
                draft.settings,
                OrderBookDepthSettings {
                    visible: false,
                    ..OrderBookDepthSettings::default()
                },
                "reset keeps the legend's visibility"
            );
            assert_eq!(draft.levels_per_side.read(cx).value(), "40");
            assert_eq!(draft.max_width.read(cx).value(), "120");
            assert_eq!(draft.opacity_percent.read(cx).value(), "50");
        });
        cx.update(|_, cx| surface.update(cx, WorkspaceSurface::close_order_flow_dialog));
        assert_eq!(
            chart_order_flow(&surface, cx).order_book_depth,
            Some(applied)
        );
    }

    fn update_terminal<R>(
        terminal: &Entity<TerminalApp>,
        cx: &mut VisualTestContext,
        update: impl FnOnce(&mut TerminalApp, &mut Window, &mut Context<TerminalApp>) -> R,
    ) -> R {
        cx.update(|window, cx| terminal.update(cx, |terminal, cx| update(terminal, window, cx)))
    }

    /// Turns autosave off for the layout of the tab at `index` and saves a different version
    /// of it, so the tab has unsaved changes. Returns the saved version.
    fn make_layout_unsaved(
        terminal: &Entity<TerminalApp>,
        index: usize,
        cx: &mut VisualTestContext,
    ) -> WorkspaceTabState {
        update_terminal(terminal, cx, |terminal, _, _| {
            let layout_id = terminal.workspaces[index].saved_layout_id;
            let library = &mut terminal.layout_library;
            library
                .set_autosave(layout_id, false)
                .expect("manual layout");
            let mut saved = library
                .get(layout_id)
                .and_then(|layout| layout.tab.clone())
                .expect("saved arrangement");
            saved.panes[0].interval = "1d".to_string();
            library
                .save(layout_id, &saved, Vec::new(), 1)
                .expect("saved version");
            library
                .get(layout_id)
                .and_then(|layout| layout.tab.clone())
                .expect("saved arrangement")
        })
    }

    fn tab_layouts(
        terminal: &Entity<TerminalApp>,
        cx: &mut VisualTestContext,
    ) -> Vec<(u64, String)> {
        cx.read(|cx| {
            terminal
                .read(cx)
                .workspaces
                .iter()
                .map(|workspace| (workspace.saved_layout_id, workspace.label.clone()))
                .collect()
        })
    }

    #[gpui::test]
    fn the_start_page_is_one_transient_tab_that_a_new_layout_replaces(cx: &mut TestAppContext) {
        let (terminal, _additions, cx) = tabbed_terminal_harness(cx);
        let page = |cx: &mut VisualTestContext| {
            cx.read(|cx| {
                let layouts = &terminal.read(cx).layouts_ui;
                (layouts.start_page_open(), layouts.start_page_shown())
            })
        };
        let tab_count =
            |cx: &mut VisualTestContext| cx.read(|cx| terminal.read(cx).workspaces.len());
        cx.update(|window, cx| terminal.read(cx).chrome_focus.clone().focus(window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("ctrl-t");
        assert_eq!(page(cx), (true, true), "Ctrl+T shows the start page");
        update_terminal(&terminal, cx, TerminalApp::open_start_page);
        assert_eq!(page(cx), (true, true), "there is one placeholder");
        assert_eq!(tab_count(cx), 1, "the placeholder is not a workspace");
        assert!(!cx.update(|window, cx| terminal.read(cx).workspace_keyboard_focused(window, cx)));

        let first = cx.read(|cx| terminal.read(cx).workspaces[0].id);
        update_terminal(&terminal, cx, |terminal, _, cx| {
            terminal.select_workspace_id(first, cx);
        });
        assert_eq!(
            page(cx),
            (true, false),
            "a workspace tab hides the placeholder"
        );
        update_terminal(&terminal, cx, TerminalApp::open_start_page);
        assert_eq!(page(cx), (true, true), "the placeholder shows again");

        update_terminal(&terminal, cx, TerminalApp::create_layout_from_start_page);
        assert_eq!(
            page(cx),
            (false, false),
            "the new layout replaces the placeholder"
        );
        cx.read(|cx| {
            let terminal = terminal.read(cx);
            assert_eq!(terminal.workspace_error, None);
            assert_eq!(terminal.workspaces.len(), 2);
            assert_eq!(terminal.active, 1);
            let created = &terminal.workspaces[1];
            assert!(
                created
                    .label
                    .starts_with(layout_library::UNTITLED_LAYOUT_NAME)
            );
            assert_eq!(
                terminal
                    .layout_library
                    .get(created.saved_layout_id)
                    .map(|layout| layout.name.as_str()),
                Some(created.label.as_str())
            );
        });

        update_terminal(&terminal, cx, TerminalApp::open_start_page);
        update_terminal(&terminal, cx, TerminalApp::close_start_page);
        assert_eq!(page(cx), (false, false));
        assert_eq!(
            tab_count(cx),
            2,
            "closing the placeholder closes no workspace"
        );
    }

    #[gpui::test]
    fn saved_layouts_open_from_the_start_page_even_when_a_tab_shows_them(cx: &mut TestAppContext) {
        let (terminal, _additions, cx) = tabbed_terminal_harness(cx);
        let (first_layout, first_name) = tab_layouts(&terminal, cx)[0].clone();
        let copy = update_terminal(&terminal, cx, |terminal, _, cx| {
            terminal.copy_saved_layout(first_layout, cx)
        })
        .expect("a copy");
        for layout_id in [copy, first_layout] {
            update_terminal(&terminal, cx, TerminalApp::open_start_page);
            update_terminal(&terminal, cx, |terminal, window, cx| {
                terminal.open_layout_from_start_page(layout_id, window, cx);
            });
        }
        cx.read(|cx| {
            let terminal = terminal.read(cx);
            assert_eq!(terminal.workspace_error, None);
            assert!(!terminal.layouts_ui.start_page_open());
            assert_eq!(terminal.active, 2);
        });
        assert_eq!(
            tab_layouts(&terminal, cx),
            vec![
                (first_layout, first_name.clone()),
                (copy, format!("{first_name} copy")),
                (first_layout, first_name),
            ]
        );
    }

    #[gpui::test]
    fn renaming_a_layout_is_validated_and_relabels_every_tab_showing_it(cx: &mut TestAppContext) {
        let (terminal, _additions, cx) = tabbed_terminal_harness(cx);
        let first_layout = tab_layouts(&terminal, cx)[0].0;
        update_terminal(&terminal, cx, |terminal, window, cx| {
            terminal.add_workspace("Second", window, cx)
        })
        .expect("a second tab");
        update_terminal(&terminal, cx, |terminal, window, cx| {
            terminal.open_saved_layout(first_layout, window, cx)
        })
        .expect("the first layout in a third tab");
        let rename = |name: &str, cx: &mut VisualTestContext| {
            update_terminal(&terminal, cx, |terminal, window, cx| {
                terminal.open_layout_name_dialog(
                    LayoutNamePurpose::Rename(first_layout),
                    window,
                    cx,
                );
                let input = terminal
                    .layouts_ui
                    .name_input()
                    .expect("name field")
                    .clone();
                input.update(cx, |input, cx| {
                    input.set_value(name.to_string(), window, cx);
                });
                terminal.confirm_layout_name(window, cx);
                terminal
                    .layouts_ui
                    .name_dialog()
                    .map(|(_, error)| error.unwrap_or_default().to_string())
            })
        };
        let labels = |cx: &mut VisualTestContext| {
            tab_layouts(&terminal, cx)
                .into_iter()
                .map(|(_, label)| label)
                .collect::<Vec<_>>()
        };
        let original = labels(cx);
        for refused in [" second ".to_string(), "   ".to_string(), "x".repeat(65)] {
            let error = rename(&refused, cx).expect("the dialog stays open");
            assert!(!error.is_empty(), "{refused:?} is refused with a reason");
            assert_eq!(labels(cx), original);
        }
        assert_eq!(
            rename("  Scalping  ", cx),
            None,
            "a valid name closes the dialog"
        );
        assert_eq!(labels(cx), ["Scalping", "Second", "Scalping"]);
        assert_eq!(
            rename("SCALPING", cx),
            None,
            "a layout may change its own case"
        );
        assert_eq!(
            cx.read(|cx| {
                terminal
                    .read(cx)
                    .layout_library
                    .get(first_layout)
                    .map(|layout| layout.name.clone())
            }),
            Some("SCALPING".to_string())
        );
    }

    #[gpui::test]
    fn the_layout_name_dialog_checks_names_while_typing_and_answers_enter_and_escape(
        cx: &mut TestAppContext,
    ) {
        let (terminal, cx) = terminal_harness(cx);
        let (layout_id, name) = tab_layouts(&terminal, cx)[0].clone();
        update_terminal(&terminal, cx, |terminal, _, cx| {
            terminal.copy_saved_layout(layout_id, cx)
        })
        .expect("a copy");
        let dialog_open = |cx: &mut VisualTestContext| {
            cx.read(|cx| terminal.read(cx).layouts_ui.name_dialog().is_some())
        };
        let open_rename = |cx: &mut VisualTestContext| {
            update_terminal(&terminal, cx, |terminal, window, cx| {
                terminal.open_layout_name_dialog(LayoutNamePurpose::Rename(layout_id), window, cx);
            });
            cx.run_until_parked();
        };

        open_rename(cx);
        cx.simulate_keystrokes("escape");
        assert!(!dialog_open(cx), "Escape cancels");
        assert_eq!(tab_layouts(&terminal, cx), [(layout_id, name.clone())]);

        open_rename(cx);
        cx.simulate_input(&format!("{name} copy"));
        assert!(
            cx.read(|cx| {
                terminal
                    .read(cx)
                    .layouts_ui
                    .name_dialog()
                    .is_some_and(|(_, error)| error.is_some())
            }),
            "a taken name shows its error while typing"
        );
        update_terminal(&terminal, cx, |terminal, window, cx| {
            let input = terminal
                .layouts_ui
                .name_input()
                .expect("name field")
                .clone();
            input.update(cx, |input, cx| input.set_value("Swing", window, cx));
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        assert!(!dialog_open(cx), "Enter confirms");
        assert_eq!(
            tab_layouts(&terminal, cx),
            [(layout_id, "Swing".to_string())]
        );
    }

    #[gpui::test]
    fn an_open_layout_cannot_be_deleted_and_a_closed_one_asks_first(cx: &mut TestAppContext) {
        let (terminal, cx) = terminal_harness(cx);
        let open = tab_layouts(&terminal, cx)[0].0;
        let copy = update_terminal(&terminal, cx, |terminal, _, cx| {
            terminal.copy_saved_layout(open, cx)
        })
        .expect("a copy");
        let state = |cx: &mut VisualTestContext| {
            cx.read(|cx| {
                let terminal = terminal.read(cx);
                (
                    terminal.layouts_ui.delete_confirmation(),
                    terminal.layout_library.get(open).is_some(),
                    terminal.layout_library.get(copy).is_some(),
                )
            })
        };

        update_terminal(&terminal, cx, |terminal, _, cx| {
            terminal.request_layout_delete(open, cx);
        });
        assert_eq!(
            state(cx),
            (None, true, true),
            "an open layout is not deleted"
        );
        let error = cx.read(|cx| terminal.read(cx).workspace_error.clone());
        assert!(
            error
                .as_deref()
                .is_some_and(|error| error.contains("is open in a tab")),
            "{error:?}"
        );

        update_terminal(&terminal, cx, |terminal, _, cx| {
            terminal.request_layout_delete(copy, cx);
        });
        assert_eq!(state(cx), (Some(copy), true, true), "deleting asks first");
        update_terminal(&terminal, cx, TerminalApp::cancel_layout_modal);
        assert_eq!(state(cx), (None, true, true), "cancel keeps the layout");

        update_terminal(&terminal, cx, |terminal, _, cx| {
            terminal.request_layout_delete(copy, cx);
        });
        update_terminal(&terminal, cx, TerminalApp::confirm_layout_delete);
        assert_eq!(state(cx), (None, true, false), "confirming deletes it");
    }

    #[gpui::test]
    fn a_full_library_refuses_new_layouts_without_evicting_any(cx: &mut TestAppContext) {
        let (terminal, _additions, cx) = tabbed_terminal_harness(cx);
        let open = tab_layouts(&terminal, cx)[0].0;
        update_terminal(&terminal, cx, |terminal, _, cx| {
            while terminal.layout_library.states().len() < layout_library::MAXIMUM_SAVED_LAYOUTS {
                terminal
                    .copy_saved_layout(open, cx)
                    .expect("room for another copy");
            }
        });
        let full = layout_library::full_library_message();
        assert_eq!(
            update_terminal(&terminal, cx, |terminal, _, cx| {
                terminal.copy_saved_layout(open, cx)
            }),
            Err(full.clone())
        );

        update_terminal(&terminal, cx, TerminalApp::open_start_page);
        update_terminal(&terminal, cx, TerminalApp::create_layout_from_start_page);
        cx.read(|cx| {
            let terminal = terminal.read(cx);
            assert_eq!(terminal.workspace_error.as_ref(), Some(&full));
            assert_eq!(terminal.workspaces.len(), 1, "no tab opens");
            assert!(terminal.layouts_ui.start_page_shown());
        });

        update_terminal(&terminal, cx, |terminal, window, cx| {
            terminal.open_layout_name_dialog(LayoutNamePurpose::Create, window, cx);
            terminal.confirm_layout_name(window, cx);
        });
        cx.read(|cx| {
            let terminal = terminal.read(cx);
            assert_eq!(
                terminal.layouts_ui.name_dialog(),
                Some((LayoutNamePurpose::Create, Some(full.as_str())))
            );
            assert_eq!(terminal.workspaces.len(), 1);
            assert_eq!(
                terminal.layout_library.states().len(),
                layout_library::MAXIMUM_SAVED_LAYOUTS
            );
            assert!(terminal.layout_library.get(open).is_some());
        });
    }

    #[gpui::test]
    fn ctrl_s_saves_a_layout_with_unsaved_changes(cx: &mut TestAppContext) {
        let (terminal, cx) = terminal_harness(cx);
        let surface = cx.read(|cx| terminal.read(cx).active_surface());
        publish_snapshot(&surface, cx);
        let unsaved =
            |cx: &mut VisualTestContext| cx.read(|cx| terminal.read(cx).layout_unsaved_flags(cx));
        let confirming = |cx: &mut VisualTestContext| {
            cx.read(|cx| terminal.read(cx).keyboard_trading.confirming())
        };
        assert_eq!(unsaved(cx), [false]);
        make_layout_unsaved(&terminal, 0, cx);
        assert_eq!(
            unsaved(cx),
            [true],
            "a manual layout shows its unsaved changes"
        );

        cx.update(|window, cx| terminal.read(cx).chrome_focus.clone().focus(window, cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("ctrl-s");
        assert_eq!(unsaved(cx), [false], "Ctrl+S saves the layout");
        assert!(!confirming(cx), "saving is not a trade");
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("s").expect("key"),
        });

        make_layout_unsaved(&terminal, 0, cx);
        cx.simulate_keystrokes("ctrl-shift-s");
        assert_eq!(unsaved(cx), [true], "Ctrl+Shift+S does not save");

        make_layout_unsaved(&terminal, 0, cx);
        update_terminal(&terminal, cx, |terminal, _, cx| {
            terminal.set_active_layout_autosave(true, cx)
        })
        .expect("autosave on");
        assert_eq!(
            unsaved(cx),
            [false],
            "a layout with autosave on never has unsaved changes"
        );
    }

    #[gpui::test]
    fn closing_a_tab_with_unsaved_changes_asks_to_save_or_discard(cx: &mut TestAppContext) {
        let (terminal, _additions, cx) = tabbed_terminal_harness(cx);
        let prompt =
            |cx: &mut VisualTestContext| cx.read(|cx| terminal.read(cx).layouts_ui.close_prompt());
        let open_tab = |name: &str, cx: &mut VisualTestContext| {
            update_terminal(&terminal, cx, |terminal, window, cx| {
                terminal.add_workspace(name, window, cx).expect("a new tab");
                let workspace = &terminal.workspaces[terminal.active];
                (workspace.id, workspace.saved_layout_id)
            })
        };
        let close = |tab_id: u64, cx: &mut VisualTestContext| {
            update_terminal(&terminal, cx, |terminal, window, cx| {
                terminal.close_workspace(tab_id, window, cx);
            });
        };
        let saved = |layout_id: u64, cx: &mut VisualTestContext| {
            cx.read(|cx| {
                terminal
                    .read(cx)
                    .layout_library
                    .get(layout_id)
                    .and_then(|layout| layout.tab.clone())
            })
        };

        let (clean, _) = open_tab("Clean", cx);
        close(clean, cx);
        assert_eq!(
            prompt(cx),
            None,
            "a tab without unsaved changes closes at once"
        );
        assert_eq!(tab_layouts(&terminal, cx).len(), 1);

        let (kept, kept_layout) = open_tab("Kept", cx);
        let edited = make_layout_unsaved(&terminal, 1, cx);
        close(kept, cx);
        assert_eq!(prompt(cx), Some(ClosePrompt::Tab(kept)));
        update_terminal(&terminal, cx, TerminalApp::cancel_layout_modal);
        assert_eq!(prompt(cx), None);
        assert_eq!(tab_layouts(&terminal, cx).len(), 2, "cancel keeps the tab");
        close(kept, cx);
        update_terminal(&terminal, cx, |terminal, window, cx| {
            terminal.finish_layout_close(true, window, cx);
        });
        assert_eq!(tab_layouts(&terminal, cx).len(), 1);
        assert_ne!(
            saved(kept_layout, cx),
            Some(edited),
            "save and close saves the tab"
        );

        let (discarded, discarded_layout) = open_tab("Discarded", cx);
        let edited = make_layout_unsaved(&terminal, 1, cx);
        close(discarded, cx);
        assert_eq!(prompt(cx), Some(ClosePrompt::Tab(discarded)));
        update_terminal(&terminal, cx, |terminal, window, cx| {
            terminal.finish_layout_close(false, window, cx);
        });
        assert_eq!(prompt(cx), None);
        assert_eq!(tab_layouts(&terminal, cx).len(), 1);
        assert_eq!(
            saved(discarded_layout, cx),
            Some(edited),
            "discard and close keeps the saved version"
        );
    }

    #[gpui::test]
    fn closing_the_window_asks_once_about_unsaved_layouts(cx: &mut TestAppContext) {
        let (terminal, cx) = terminal_harness(cx);
        let layout_id = tab_layouts(&terminal, cx)[0].0;
        let edited = make_layout_unsaved(&terminal, 0, cx);
        let state = |cx: &mut VisualTestContext| {
            cx.read(|cx| {
                let terminal = terminal.read(cx);
                (terminal.layouts_ui.close_prompt(), terminal.closing)
            })
        };
        let should_close = |cx: &mut VisualTestContext| {
            update_terminal(&terminal, cx, TerminalApp::window_should_close)
        };

        assert!(
            !should_close(cx),
            "a window with unsaved layouts stays open to ask"
        );
        assert_eq!(state(cx), (Some(ClosePrompt::Window), false));
        update_terminal(&terminal, cx, TerminalApp::cancel_layout_modal);
        assert_eq!(state(cx), (None, false), "cancel keeps the window open");

        assert!(!should_close(cx));
        update_terminal(&terminal, cx, |terminal, window, cx| {
            terminal.finish_layout_close(false, window, cx);
        });
        assert_eq!(
            state(cx),
            (None, true),
            "discard and close claims the close"
        );
        assert_eq!(
            cx.read(|cx| {
                terminal
                    .read(cx)
                    .layout_library
                    .get(layout_id)
                    .and_then(|layout| layout.tab.clone())
            }),
            Some(edited),
            "the saved version is what the next start opens"
        );
    }

    #[gpui::test]
    fn a_window_without_unsaved_layouts_closes_at_once(cx: &mut TestAppContext) {
        let (terminal, cx) = terminal_harness(cx);
        assert!(update_terminal(
            &terminal,
            cx,
            TerminalApp::window_should_close
        ));
        assert!(cx.read(|cx| terminal.read(cx).closing));
        assert_eq!(
            cx.read(|cx| terminal.read(cx).layouts_ui.close_prompt()),
            None
        );
        assert!(
            update_terminal(&terminal, cx, TerminalApp::window_should_close),
            "a claimed close is not claimed again"
        );
    }
}
#[test]
fn chart_execution_time_converts_runtime_nanoseconds_to_utc_seconds_exactly() {
    assert_eq!(
        chart_time_seconds_from_unix_nanos(1_725_000_037_823_456_789),
        1_725_000_037
    );
    assert_eq!(
        chart_time_seconds_from_unix_nanos(1_725_000_037_000_000_000),
        1_725_000_037
    );
}

#[test]
fn workspace_split_ratio_tracks_the_active_axis_and_clamps_safe_bounds() {
    assert_eq!(
        workspace_split_ratio(
            ChartSplitDirection::Horizontal,
            350.0,
            0.0,
            100.0,
            0.0,
            500.0,
            200.0,
        ),
        Some(0.5)
    );
    assert_eq!(
        workspace_split_ratio(
            ChartSplitDirection::Vertical,
            0.0,
            325.0,
            0.0,
            25.0,
            500.0,
            400.0,
        ),
        Some(0.75)
    );
    let minimum = workspace_split_ratio(
        ChartSplitDirection::Horizontal,
        -100.0,
        0.0,
        0.0,
        0.0,
        500.0,
        200.0,
    )
    .expect("finite horizontal bounds produce a ratio");
    assert!((minimum - 0.05).abs() < f64::from(f32::EPSILON));
    assert_eq!(
        workspace_split_ratio(
            ChartSplitDirection::Vertical,
            0.0,
            100.0,
            0.0,
            0.0,
            500.0,
            0.0,
        ),
        None
    );
}

#[test]
fn workspace_alt_click_maximizes_and_restores_without_mutating_single_pane_layouts() {
    assert_eq!(
        workspace_maximize_transition(None, 7, 1),
        WorkspaceMaximizeTransition::Ignore
    );
    assert_eq!(
        workspace_maximize_transition(None, 7, 2),
        WorkspaceMaximizeTransition::Set(Some(7))
    );
    assert_eq!(
        workspace_maximize_transition(Some(7), 7, 2),
        WorkspaceMaximizeTransition::Set(None)
    );
    assert_eq!(
        workspace_maximize_transition(Some(7), 9, 2),
        WorkspaceMaximizeTransition::Set(Some(9))
    );
}

#[cfg(feature = "diagnostics")]
#[test]
fn foreground_interaction_samples_are_bounded_per_handler() {
    let mut diagnostics = ForegroundInteractionDiagnostics::default();
    for sample in 0..=FOREGROUND_INTERACTION_SAMPLE_CAPACITY {
        let elapsed = u64::try_from(sample).unwrap_or(u64::MAX);
        diagnostics.record_symbol_input(false, elapsed);
        diagnostics.record_symbol_input(true, elapsed);
        diagnostics.record_instrument_selection(elapsed);
        diagnostics.record_interval_selection(elapsed);
    }
    assert_eq!(
        diagnostics.symbol_input_change.len(),
        FOREGROUND_INTERACTION_SAMPLE_CAPACITY
    );
    assert_eq!(
        diagnostics.symbol_input_submit.len(),
        FOREGROUND_INTERACTION_SAMPLE_CAPACITY
    );
    assert_eq!(
        diagnostics.instrument_selection.len(),
        FOREGROUND_INTERACTION_SAMPLE_CAPACITY
    );
    assert_eq!(
        diagnostics.interval_selection.len(),
        FOREGROUND_INTERACTION_SAMPLE_CAPACITY
    );
}

#[test]
fn escape_exits_fullscreen_without_stealing_regular_escape() {
    assert_eq!(
        fullscreen_escape_command("escape", true),
        Some(WindowCommand::ToggleFullscreen)
    );
    assert_eq!(fullscreen_escape_command("escape", false), None);
    assert_eq!(fullscreen_escape_command("enter", true), None);
}

#[test]
fn ctrl_s_saves_the_layout_and_ctrl_shift_s_sells_at_market() {
    let bindings = desktop_key_bindings().expect("the registry shortcuts bind");
    let actions = |chord: &str| {
        let typed = [gpui::Keystroke::parse(chord).expect("chord")];
        bindings
            .iter()
            .filter(|binding| binding.match_keystrokes(&typed) == Some(false))
            .map(|binding| binding.action().name())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        actions("ctrl-s"),
        [gpui::Action::name(&super::SaveActiveLayout)]
    );
    assert_eq!(
        actions("ctrl-shift-s"),
        [gpui::Action::name(&super::TradingSellMarket)]
    );
}

#[test]
fn every_registry_shortcut_binds_and_stays_off_the_chart_keys() {
    let bindings = desktop_key_bindings().expect("the registry shortcuts bind");
    let chord_count = aeris_desktop::command_registry::COMMANDS
        .iter()
        .map(|spec| spec.chords.len())
        .sum::<usize>();
    assert_eq!(
        bindings.len(),
        chord_count,
        "one binding per registry chord"
    );
}

#[test]
fn watchlist_steps_wrap_and_start_from_the_ends() {
    assert_eq!(watchlist_step(Some(0), 3, true), Some(1));
    assert_eq!(watchlist_step(Some(2), 3, true), Some(0));
    assert_eq!(watchlist_step(Some(0), 3, false), Some(2));
    assert_eq!(watchlist_step(None, 3, true), Some(0));
    assert_eq!(watchlist_step(None, 3, false), Some(2));
    assert_eq!(watchlist_step(None, 0, true), None);
}

#[test]
fn caption_pointer_ownership_is_exclusive_per_platform() {
    assert_eq!(
        caption_pointer_owner(CaptionPlatform::Windows),
        CaptionPointerOwner::Native
    );
    assert_eq!(
        caption_pointer_owner(CaptionPlatform::Linux),
        CaptionPointerOwner::Application
    );
    assert_eq!(
        caption_pointer_owner(CaptionPlatform::MacOs),
        CaptionPointerOwner::System
    );
    assert_eq!(
        caption_pointer_owner(CaptionPlatform::Other),
        CaptionPointerOwner::Application
    );
}

#[test]
fn caption_keyboard_activation_accepts_only_button_activation_keys() {
    assert!(caption_keyboard_activates("enter"));
    assert!(caption_keyboard_activates("space"));
    assert!(!caption_keyboard_activates("escape"));
    assert!(!caption_keyboard_activates("tab"));
}

#[test]
fn window_move_waits_for_a_pressed_pointer_move_and_cancels_cleanly() {
    assert_eq!(
        window_move_gesture_transition(false, WindowMoveGestureEvent::Press),
        WindowMoveGestureTransition {
            pending: true,
            start_move: false,
        }
    );
    assert_eq!(
        window_move_gesture_transition(true, WindowMoveGestureEvent::Move { left_pressed: true },),
        WindowMoveGestureTransition {
            pending: false,
            start_move: true,
        }
    );
    assert_eq!(
        window_move_gesture_transition(
            true,
            WindowMoveGestureEvent::Move {
                left_pressed: false,
            },
        ),
        WindowMoveGestureTransition {
            pending: false,
            start_move: false,
        }
    );
    assert_eq!(
        window_move_gesture_transition(true, WindowMoveGestureEvent::Cancel),
        WindowMoveGestureTransition {
            pending: false,
            start_move: false,
        }
    );
}

#[test]
fn side_panel_width_clamps_to_renderable_watchlist_bounds() {
    assert!((clamped_side_panel_width(400.0) - 400.0).abs() < f32::EPSILON);
    assert!((clamped_side_panel_width(900.0) - 480.0).abs() < f32::EPSILON);
    assert!((clamped_side_panel_width(100.0) - 360.0).abs() < f32::EPSILON);
}

#[test]
fn context_panel_height_restores_default_and_clamps_to_resizable_bounds() {
    use super::{
        CONTEXT_PANEL_INITIAL_HEIGHT, CONTEXT_PANEL_MAXIMUM_HEIGHT, CONTEXT_PANEL_MINIMUM_HEIGHT,
        WorkspaceChartState, clamped_context_panel_height, workspace_surface,
    };
    let restored = |height| WorkspaceChartState {
        context_panel_height: height,
        ..WorkspaceChartState::default()
    };
    let height = workspace_surface::restored_context_panel_height;
    assert!((height(None) - CONTEXT_PANEL_INITIAL_HEIGHT).abs() < f32::EPSILON);
    // Workspaces saved before the panel was resizable persist zero.
    assert!((height(Some(&restored(0))) - CONTEXT_PANEL_INITIAL_HEIGHT).abs() < f32::EPSILON);
    assert!((height(Some(&restored(320))) - 320.0).abs() < f32::EPSILON);
    assert!((height(Some(&restored(9_000))) - CONTEXT_PANEL_MAXIMUM_HEIGHT).abs() < f32::EPSILON);
    assert!(
        (clamped_context_panel_height(10.0) - CONTEXT_PANEL_MINIMUM_HEIGHT).abs() < f32::EPSILON
    );
}

#[test]
fn window_close_retirement_is_claimed_exactly_once() {
    let mut closing = false;
    assert!(claim_once(&mut closing));
    assert!(!claim_once(&mut closing));
}

#[test]
fn workspace_tabs_switch_one_surface_and_preserve_stable_labels() {
    assert_eq!(workspace_switch(0, 1, 2), Some((0, 1)));
    assert_eq!(workspace_switch(1, 1, 2), None);
    assert_eq!(workspace_switch(0, 2, 2), None);
    assert_eq!(wrapped_workspace_index(0, 3, -1), Some(2));
    assert_eq!(wrapped_workspace_index(2, 3, 1), Some(0));
    assert_eq!(wrapped_workspace_index(1, 3, -1), Some(0));
    assert_eq!(wrapped_workspace_index(1, 3, 1), Some(2));
    assert_eq!(wrapped_workspace_index(0, 0, 1), None);
}

#[test]
fn development_chart_splitting_disables_the_product_restriction() {
    const {
        assert!(!super::CHART_PANE_PRODUCT_RESTRICTIONS_ENABLED);
        assert!(super::CHART_PANE_CAPACITY > super::PRODUCTION_CHART_PANE_LIMIT);
    }
    assert_eq!(
        super::CHART_PANE_CAPACITY,
        super::DEVELOPMENT_CHART_PANE_SAFETY_CEILING
    );

    let mut workspace = super::AerisChartWorkspace::new(1, super::CHART_PANE_CAPACITY)
        .expect("chart workspace root");
    for pane_id in 2..=8 {
        assert!(
            workspace
                .split(1, ChartSplitDirection::Horizontal, pane_id)
                .is_ok()
        );
    }
    assert_eq!(workspace.layout().leaf_ids().len(), 8);
}

#[test]
fn workspace_tabs_reorder_and_close_without_changing_active_identity() {
    let mut ids = vec![1, 2, 3];
    assert!(reorder_workspace_ids(&mut ids, 1, 2));
    assert_eq!(ids, vec![2, 3, 1]);
    assert!(reorder_workspace_ids(&mut ids, 1, 0));
    assert_eq!(ids, vec![1, 2, 3]);
    assert!(reorder_workspace_ids(&mut ids, 1, 1));
    assert_eq!(ids, vec![2, 1, 3]);
    assert!(reorder_workspace_ids(&mut ids, 1, 0));
    assert_eq!(ids, vec![1, 2, 3]);
    assert!(reorder_workspace_ids(&mut ids, 3, 0));
    assert_eq!(ids, vec![3, 1, 2]);
    let trailing_index = ids.len();
    assert!(reorder_workspace_ids(&mut ids, 3, trailing_index));
    assert_eq!(ids, vec![1, 2, 3]);
    let trailing_index = ids.len();
    assert!(!reorder_workspace_ids(&mut ids, 3, trailing_index));
    assert!(!reorder_workspace_ids(&mut ids, 2, 1));
    assert!(!reorder_workspace_ids(&mut ids, 99, 0));

    assert_eq!(active_workspace_after_close(&ids, 2, 1), Some(2));
    assert_eq!(active_workspace_after_close(&ids, 2, 2), Some(3));
    assert_eq!(active_workspace_after_close(&ids, 3, 3), Some(2));
    assert_eq!(active_workspace_after_close(&[1], 1, 1), None);
}

fn saved_workspace_boot_fixture() -> WorkspaceState {
    let mut workspace: WorkspaceState = super::local_state::default_workspace();
    workspace.active_workspace_id = 42;
    workspace.workspace_revision = 9;
    workspace.layout_generation = 17;
    let tab = workspace
        .workspace_tabs
        .first_mut()
        .expect("default workspace tab");
    tab.workspace_id = 42;
    tab.label = "Execution".to_string();
    tab.split_axis = WorkspaceSplitAxis::Vertical as i32;
    tab.active_pane_id = 8;
    tab.generation = 6;

    let first = tab.panes.first_mut().expect("default workspace pane");
    first.pane_id = 7;
    first.consumer_id = 17;
    first.size_basis_points = 4_000;
    first.generation = 6;
    first.viewport_start_unix_nanos = Some(100);
    first.viewport_end_unix_nanos = Some(200);
    let first_instrument = first.instrument.as_mut().expect("default instrument");
    first_instrument.instrument_id = "hyperliquid:perp:ETH".to_string();
    first_instrument.provider_symbol = "ETH".to_string();
    first_instrument.display_symbol = "ETH-USDC".to_string();
    first.interval = ChartInterval::Minute5.label().to_string();

    let mut second: WorkspacePaneState = first.clone();
    second.pane_id = 8;
    second.consumer_id = 18;
    second.size_basis_points = 6_000;
    second.viewport_start_unix_nanos = Some(300);
    second.viewport_end_unix_nanos = Some(400);
    let second_instrument = second.instrument.as_mut().expect("second instrument");
    second_instrument.instrument_id = "hyperliquid:perp:SOL".to_string();
    second_instrument.provider_symbol = "SOL".to_string();
    second_instrument.display_symbol = "SOL-USDC".to_string();
    second.interval = ChartInterval::Hour1.label().to_string();
    tab.panes.push(second);
    tab.layout = Some(WorkspaceLayoutState {
        pane_id: 0,
        split_axis: WorkspaceSplitAxis::Vertical as i32,
        ratio_basis_points: 4_000,
        first: Some(Box::new(WorkspaceLayoutState {
            pane_id: 7,
            split_axis: WorkspaceSplitAxis::Horizontal as i32,
            ratio_basis_points: 10_000,
            first: None,
            second: None,
        })),
        second: Some(Box::new(WorkspaceLayoutState {
            pane_id: 8,
            split_axis: WorkspaceSplitAxis::Horizontal as i32,
            ratio_basis_points: 10_000,
            first: None,
            second: None,
        })),
    });
    workspace
}

#[test]
fn fresh_boot_consumes_saved_symbol_timeframe_layout_and_active_pane() {
    let unique = format!(
        "aeris-workspace-boot-consumption-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_nanos()
    );
    let directory = std::env::temp_dir().join(unique);
    let path = directory.join("workspace-state.pb");
    let workspace = saved_workspace_boot_fixture();

    super::local_state::save_workspace_fixture(&workspace, &path).expect("workspace fixture saves");
    let restored =
        super::local_state::load_workspace_fixture(&path).expect("workspace fixture loads");
    let shell_plan = super::terminal_view::restored_workspace_shell_plan(&restored);
    let plan = super::engine_market_worker::restored_workspace_boot_plan(&restored)
        .expect("restored workspace builds the production boot plan");

    assert_eq!(shell_plan.active_workspace_id, 42);
    assert_eq!(shell_plan.workspace_revision, 9);
    assert_eq!(shell_plan.layout_generation, 17);
    assert_eq!(shell_plan.workspace_tabs[0].active_pane_id, 8);
    assert_eq!(
        shell_plan.workspace_tabs[0].layout,
        Some(WorkspaceLayoutState {
            pane_id: 0,
            split_axis: WorkspaceSplitAxis::Vertical as i32,
            ratio_basis_points: 4_000,
            first: Some(Box::new(WorkspaceLayoutState {
                pane_id: 7,
                split_axis: WorkspaceSplitAxis::Horizontal as i32,
                ratio_basis_points: 10_000,
                first: None,
                second: None,
            })),
            second: Some(Box::new(WorkspaceLayoutState {
                pane_id: 8,
                split_axis: WorkspaceSplitAxis::Horizontal as i32,
                ratio_basis_points: 10_000,
                first: None,
                second: None,
            })),
        })
    );
    assert_eq!(plan.panes.len(), 2);

    let eth = &plan.panes[0];
    assert_eq!(eth.workspace_id, 42);
    assert_eq!(eth.pane_id, 7);
    assert_eq!(eth.consumer_id, 17);
    assert_eq!(eth.instrument.instrument_id, "hyperliquid:perp:ETH");
    assert_eq!(eth.instrument.display_symbol, "ETH-USDC");
    assert_eq!(eth.interval, ChartInterval::Minute5);
    assert_eq!(eth.restored_viewport, Some((100, 200)));
    assert_eq!(eth.generation, 6);

    let sol = &plan.panes[1];
    assert_eq!(sol.workspace_id, 42);
    assert_eq!(sol.pane_id, 8);
    assert_eq!(sol.consumer_id, 18);
    assert_eq!(sol.instrument.instrument_id, "hyperliquid:perp:SOL");
    assert_eq!(sol.instrument.display_symbol, "SOL-USDC");
    assert_eq!(sol.interval, ChartInterval::Hour1);
    assert_eq!(sol.restored_viewport, Some((300, 400)));
    assert_eq!(sol.generation, 6);

    std::fs::remove_dir_all(&directory).expect("workspace boot fixture cleanup");
}

#[test]
fn workspace_viewport_persistence_ignores_automatic_live_scrolling() {
    let restored = Some((100, 200));
    let current = Some((300, 400));
    assert_eq!(
        durable_workspace_viewport(restored, false, false, current),
        restored
    );
    assert_eq!(
        durable_workspace_viewport(restored, true, true, current),
        None
    );
    assert_eq!(
        durable_workspace_viewport(restored, true, false, current),
        current
    );
}

#[test]
fn workspace_drag_reflows_at_neighbor_slot_boundaries() {
    let strip_left = 100.0;
    let widths = [120.0, 176.0, 96.0];
    let cursor_offset = widths[0] / 2.0;
    let first_center = strip_left + WORKSPACE_TAB_STRIP_PADDING_LEFT + cursor_offset;
    let second_center = first_center + widths[0] + WORKSPACE_TAB_GAP;
    let third_center = second_center + widths[1] + WORKSPACE_TAB_GAP;

    assert_eq!(
        workspace_drag_destination(first_center, strip_left, cursor_offset, &widths),
        Some(0)
    );
    assert_eq!(
        workspace_drag_destination(second_center, strip_left, cursor_offset, &widths),
        Some(1)
    );
    assert_eq!(
        workspace_drag_destination(third_center, strip_left, cursor_offset, &widths),
        Some(2)
    );
    assert_eq!(
        workspace_drag_destination(strip_left - 500.0, strip_left, cursor_offset, &widths),
        Some(0)
    );
    assert_eq!(
        workspace_drag_destination(third_center + 500.0, strip_left, cursor_offset, &widths),
        Some(2)
    );
    assert_eq!(
        workspace_drag_destination(first_center, strip_left, cursor_offset, &[]),
        None
    );
    assert_eq!(
        workspace_drag_destination(f32::NAN, strip_left, cursor_offset, &widths),
        None
    );

    let drag = WorkspaceDragState {
        tab_id: 7,
        cursor_offset_x: cursor_offset,
        pointer_x: Some(second_center + 10.0),
        strip_left,
    };
    assert_eq!(
        workspace_drag_translation(Some(drag), 7, 1, &widths),
        Some(10.0)
    );
    assert_eq!(workspace_drag_translation(Some(drag), 8, 1, &widths), None);
    assert_eq!(
        workspace_drag_translation(
            Some(WorkspaceDragState {
                pointer_x: Some(strip_left - 500.0),
                ..drag
            }),
            7,
            1,
            &widths
        ),
        Some(-(widths[0] + WORKSPACE_TAB_GAP))
    );
    assert_eq!(
        workspace_drag_translation(
            Some(WorkspaceDragState {
                pointer_x: Some(third_center + 500.0),
                ..drag
            }),
            7,
            1,
            &widths
        ),
        Some(widths[2] + WORKSPACE_TAB_GAP)
    );
}

#[test]
fn watchlist_drag_reflows_continuously_at_row_midpoints() {
    let body_top = 100.0;
    let cursor_offset = super::WATCHLIST_ROW_HEIGHT / 2.0;
    assert_eq!(
        watchlist_drag_destination(body_top + cursor_offset, body_top, 0.0, cursor_offset, 5,),
        Some(0)
    );
    assert_eq!(
        watchlist_drag_destination(
            body_top + cursor_offset + super::WATCHLIST_ROW_HEIGHT / 2.0,
            body_top,
            0.0,
            cursor_offset,
            5,
        ),
        Some(1)
    );
    let drag = WatchlistDragState {
        provider: "hyperliquid".to_string(),
        instrument_id: "hyperliquid:perp:BTC".to_string(),
        cursor_offset_y: cursor_offset,
        pointer_y: Some(body_top + cursor_offset + super::WATCHLIST_ROW_HEIGHT + 7.0),
        body_top,
        scroll_offset_y: 0.0,
    };
    assert_eq!(
        watchlist_drag_translation(Some(&drag), "hyperliquid", "hyperliquid:perp:BTC", 1,),
        Some(7.0)
    );
    assert_eq!(
        watchlist_drag_translation(Some(&drag), "hyperliquid", "hyperliquid:perp:ETH", 1),
        None
    );

    let scroll_offset = -60.0;
    assert_eq!(
        watchlist_drag_destination(
            body_top + scroll_offset + 3.0 * super::WATCHLIST_ROW_HEIGHT + cursor_offset,
            body_top,
            scroll_offset,
            cursor_offset,
            5,
        ),
        Some(3)
    );
}

#[test]
fn publication_is_ready_only_after_bridge_acceptance_without_recovery() {
    assert_eq!(publication_chart_state(true, false), ChartState::Ready);
    assert_eq!(publication_chart_state(false, true), ChartState::Recovering);
    assert_eq!(publication_chart_state(true, true), ChartState::Recovering);
}

#[test]
fn catalog_rejections_preserve_search_and_selection_generation_order_bookains() {
    assert_eq!(
        catalog_rejection_order_bookain(ProviderCatalogCommand::Search),
        CatalogCommandOrderBookain::Search
    );
    assert_eq!(
        catalog_rejection_order_bookain(ProviderCatalogCommand::Selection),
        CatalogCommandOrderBookain::Selection
    );
}

#[test]
fn symbol_input_searches_only_on_change_events() {
    assert_eq!(
        symbol_input_action(&InputEvent::Change),
        SymbolInputAction::Search
    );
    assert_eq!(
        symbol_input_action(&InputEvent::PressEnter {
            secondary: false,
            shift: false,
        }),
        SymbolInputAction::Submit
    );
    assert_eq!(
        symbol_input_action(&InputEvent::Focus),
        SymbolInputAction::Ignore
    );
    assert_eq!(
        symbol_input_action(&InputEvent::Blur),
        SymbolInputAction::Ignore
    );
}

#[test]
fn enter_closes_only_for_an_available_provider_selection() {
    assert_eq!(
        symbol_submit_decision(3, 1),
        SymbolSubmitDecision::Select(1)
    );
    assert_eq!(symbol_submit_decision(0, 0), SymbolSubmitDecision::Search);
    assert_eq!(symbol_submit_decision(1, 2), SymbolSubmitDecision::Search);
    assert_eq!(
        symbol_submit_decision(2, 0),
        SymbolSubmitDecision::Select(0)
    );
}

#[test]
fn instrument_menu_highlights_only_the_live_market_until_keyboard_moves() {
    assert!(instrument_row_highlighted(true, 4, 0, false));
    assert!(
        !instrument_row_highlighted(false, 0, 0, false),
        "opening the menu must not paint catalog index 0 as the live stream"
    );
    assert!(instrument_row_highlighted(false, 0, 0, true));
    assert!(!instrument_row_highlighted(true, 4, 0, true));
    assert!(instrument_row_highlighted(true, 4, 4, true));
}

fn instrument_menu_entry(
    label: &str,
    checked: bool,
    action: InstrumentMenuAction,
) -> InstrumentMenuEntry {
    InstrumentMenuEntry {
        provider: TerminalProvider::Rithmic,
        label: label.into(),
        description: None,
        detail: None,
        checked,
        action,
    }
}

const fn search_result(index: usize) -> InstrumentMenuSelection {
    InstrumentMenuSelection::SearchResult {
        provider: TerminalProvider::Rithmic,
        index,
    }
}

#[test]
fn instrument_menu_index_follows_the_checked_live_market() {
    let entries = [
        instrument_menu_entry(
            "BTC/USD",
            false,
            InstrumentMenuAction::Select(search_result(0)),
        ),
        instrument_menu_entry(
            "ETH/USD",
            true,
            InstrumentMenuAction::Select(search_result(1)),
        ),
    ];
    assert_eq!(current_instrument_menu_index(&entries), Some(1));
    assert_eq!(
        current_instrument_menu_index(&[instrument_menu_entry(
            "AAVE/USD",
            false,
            InstrumentMenuAction::Select(search_result(0)),
        )]),
        None
    );
}

#[test]
fn the_first_selectable_instrument_skips_product_rows() {
    let product = crate::desktop::symbol_browser::ProductKey {
        provider: TerminalProvider::Rithmic,
        code: "NQ".into(),
        exchange: "CME".into(),
    };
    let entries = [
        instrument_menu_entry("NQ", false, InstrumentMenuAction::OpenProduct(product)),
        instrument_menu_entry(
            "NQZ6",
            false,
            InstrumentMenuAction::Select(search_result(0)),
        ),
    ];
    assert_eq!(first_selectable_instrument_index(&entries), Some(1));
    assert_eq!(first_selectable_instrument_index(&entries[..1]), None);
}

#[test]
fn listing_counts_name_their_unit() {
    assert_eq!(counted(1, "product", "products"), "1 product");
    assert_eq!(counted(12, "product", "products"), "12 products");
}

#[test]
fn hyperliquid_catalog_rejections_never_name_rithmic() {
    for reason in [
        ProviderCatalogRejectionReason::SearchRejected,
        ProviderCatalogRejectionReason::SupersededSearch,
        ProviderCatalogRejectionReason::InstrumentUnavailable,
        ProviderCatalogRejectionReason::SubscriptionRejected,
        ProviderCatalogRejectionReason::DispatchUnavailable,
        ProviderCatalogRejectionReason::SearchTimedOut,
        ProviderCatalogRejectionReason::SelectionTimedOut,
        ProviderCatalogRejectionReason::Unspecified,
    ] {
        for command in [
            ProviderCatalogCommand::Search,
            ProviderCatalogCommand::Selection,
        ] {
            let message = catalog_rejection_message(reason, command, TerminalProvider::Hyperliquid);
            assert!(!message.contains("Rithmic"), "{message}");
        }
    }
    assert_eq!(
        catalog_rejection_message(
            ProviderCatalogRejectionReason::SearchTimedOut,
            ProviderCatalogCommand::Search,
            TerminalProvider::Rithmic,
        ),
        "The Rithmic market search timed out; try again"
    );
    assert_eq!(
        catalog_rejection_message(
            ProviderCatalogRejectionReason::SelectionTimedOut,
            ProviderCatalogCommand::Selection,
            TerminalProvider::Rithmic,
        ),
        "The Rithmic market selection timed out; try again"
    );
}

#[test]
fn persisted_calendar_intervals_use_stable_chart_interval_labels() {
    assert_eq!(
        aeris_market_runtime::persisted_interval(ChartInterval::Week1.label()),
        Some(ChartInterval::Week1)
    );
    assert_eq!(
        aeris_market_runtime::persisted_interval(ChartInterval::Month1.label()),
        Some(ChartInterval::Month1)
    );
    assert!(super::provider_intervals(TerminalProvider::Rithmic).contains(&ChartInterval::Month1));
}

#[test]
fn bridge_recovery_replaces_ready_after_deferred_gap_validation() {
    assert_eq!(
        reconciled_bridge_state(ChartState::Ready, true),
        ChartState::Recovering
    );
    assert_eq!(
        reconciled_bridge_state(ChartState::Stale, true),
        ChartState::Stale
    );
}

#[test]
fn disconnected_provider_keeps_retained_chart_visible_and_stale() {
    let presentation =
        ProviderConnectionPresentation::from_connection(FeedConnectionState::Disconnected);
    assert_eq!(presentation, ProviderConnectionPresentation::Offline);
    assert_eq!(presentation.chart_state(true), Some(ChartState::Stale));
    assert_eq!(presentation.chart_state(false), None);
    let mut retained_chart_state = presentation
        .chart_state(true)
        .expect("offline retained chart becomes stale");
    assert_eq!(retained_chart_state, ChartState::Stale);
    retained_chart_state =
        ProviderConnectionPresentation::from_connection(FeedConnectionState::Recovering)
            .chart_state(true)
            .expect("the same retained chart advances to reconnecting");
    assert_eq!(retained_chart_state, ChartState::Recovering);
    assert_eq!(
        ProviderConnectionPresentation::from_connection(FeedConnectionState::Recovering)
            .chart_state(true),
        Some(ChartState::Recovering)
    );
    assert_eq!(
        ProviderConnectionPresentation::from_connection(FeedConnectionState::Streaming),
        ProviderConnectionPresentation::None
    );
}

#[test]
fn terminal_session_stop_is_a_truthful_chart_error_with_or_without_data() {
    let stopped = ProviderConnectionPresentation::from_connection(FeedConnectionState::Stopped);
    assert_eq!(stopped.chart_state(true), Some(ChartState::Error));
    assert_eq!(stopped.chart_state(false), Some(ChartState::Error));
    assert_eq!(
        chart_surface_notice(
            stopped.chart_state(true).expect("stopped chart state"),
            true,
            false,
            "Rithmic market worker stopped",
        )
        .expect("retained chart error notice")
        .placement,
        ChartNoticePlacement::Corner
    );
    assert_eq!(
        chart_surface_notice(
            stopped.chart_state(false).expect("stopped chart state"),
            false,
            false,
            "Rithmic market worker stopped",
        )
        .expect("empty chart error notice")
        .placement,
        ChartNoticePlacement::Center
    );
}

#[test]
fn dead_rithmic_worker_stop_transition_is_applied_once() {
    assert!(should_apply_rithmic_worker_stop(
        true,
        Some(FeedConnectionState::Streaming)
    ));
    assert!(!should_apply_rithmic_worker_stop(
        true,
        Some(FeedConnectionState::Stopped)
    ));
    assert!(!should_apply_rithmic_worker_stop(
        false,
        Some(FeedConnectionState::Streaming)
    ));
    assert!(!should_apply_rithmic_worker_stop(true, None));
}

#[test]
fn rithmic_catalog_ready_autoloads_once_without_owning_reconnects() {
    let event = aeris_market_runtime::MarketRuntimeEvent::CatalogReady {
        provider: "rithmic".into(),
    };
    assert!(matches!(
        project_provider_catalog_event(event),
        Some(ProviderCatalogEvent::CatalogReady { provider }) if provider == "rithmic"
    ));
}

#[test]
fn catalog_rejection_lane_comes_from_the_runtime_not_the_reason() {
    for (selection, expected) in [
        (false, ProviderCatalogCommand::Search),
        (true, ProviderCatalogCommand::Selection),
    ] {
        let event = aeris_market_runtime::MarketRuntimeEvent::ProviderCatalogRejected {
            rejection: aeris_contracts::ProviderCatalogRejected {
                consumer_id: 1,
                provider: "rithmic".into(),
                provider_generation: None,
                command_generation: 3,
                reason: ProviderCatalogRejectionReason::DispatchUnavailable,
            },
            selection,
        };
        assert!(matches!(
            project_provider_catalog_event(event),
            Some(ProviderCatalogEvent::CommandRejected {
                command,
                ..
            }) if command == expected
        ));
    }
}

#[test]
fn first_rithmic_connection_is_not_presented_as_recovery_and_autoloads_catalog() {
    let shell =
        crate::desktop::rithmic_shell::RithmicShellState::local().expect("fixed Rithmic profile");
    let discovering =
        stabilized_connection_state(Some(shell.connection()), FeedConnectionState::Discovering);
    assert_eq!(discovering, FeedConnectionState::Discovering);
    let ready = stabilized_connection_state(Some(discovering), FeedConnectionState::Authenticating);
    assert_eq!(ready, FeedConnectionState::Authenticating);
}

#[test]
fn gpui_theme_attachment_preserves_alpha() {
    let attached = gpui_color(ThemeColor::from_rgb8(240, 240, 240).with_alpha(19.0 / 255.0));
    assert!((attached.a - 19.0 / 255.0).abs() < f32::EPSILON);
}

/// A switch leaves the previous chart on screen so the surface never goes
/// blank. That chart is real market data from the market the trader just
/// left, so it has to be covered and named — a corner spinner over live
/// candles reads as the new selection already streaming.
#[test]
fn a_superseded_chart_is_covered_and_named_not_left_looking_current() {
    let switching =
        chart_surface_notice(ChartState::Loading, true, true, "Loading 5m market history")
            .expect("a switch in flight is announced");
    assert_eq!(switching.placement, ChartNoticePlacement::Center);
    assert_eq!(
        switching.detail.as_deref(),
        Some("Loading 5m market history")
    );

    // A repair behind the chart the trader is actually looking at is
    // different: it stays out of the way.
    let repairing = chart_surface_notice(ChartState::Loading, true, false, "repairing coverage")
        .expect("a repair is announced");
    assert_eq!(repairing.placement, ChartNoticePlacement::Corner);
}

#[test]
fn engine_switch_keeps_partial_replacement_covered_until_handoff_is_current() {
    let mut pane = super::chart_pane_controller::ChartPaneController::default();
    pane.chart_installed();
    pane.request(2);
    assert!(!pane.chart_needs_replacement());
    assert!(pane.selection_in_progress());
    assert!(pane.accept(2));
    pane.remember_previous_selection(None, ChartInterval::Minute1);
    assert!(pane.chart_needs_replacement());
    assert!(pane.selection_in_progress());
    pane.chart_installed();
    assert!(!pane.chart_needs_replacement());
    assert!(pane.selection_in_progress());
    pane.finish_replacement();
    assert!(!pane.selection_in_progress());
}

#[test]
fn worker_stop_preserves_a_concrete_startup_error() {
    assert_eq!(
        chart_status_detail(
            ChartState::Error,
            FeedConnectionState::Stopped,
            Some(ChartFailure::UnsupportedProvider),
            None,
        ),
        ChartFailure::UnsupportedProvider.detail()
    );
    assert_eq!(
        chart_status_detail(ChartState::Error, FeedConnectionState::Stopped, None, None),
        "Market worker stopped."
    );
}

#[test]
fn connection_indicator_is_transport_only() {
    let theme = super::AerisTheme::dark();
    let colors = theme.colors;
    let live = connection_presentation(
        TerminalProvider::Rithmic,
        FeedConnectionState::Streaming,
        None,
    );
    assert_eq!(live.provider, "Rithmic");
    assert_eq!(live.status, "Live");
    assert_eq!(live.latency, "Measuring…");
    assert_eq!((live.color)(&theme), colors.positive);

    let recovering = connection_presentation(
        TerminalProvider::Rithmic,
        FeedConnectionState::Recovering,
        Some(18_000_000),
    );
    assert_eq!(recovering.status, "Reconnecting");
    assert_eq!(recovering.latency, "Measuring…");
    assert_eq!((recovering.color)(&theme), colors.warning);

    let offline = connection_presentation(
        TerminalProvider::Rithmic,
        FeedConnectionState::Disconnected,
        Some(18_000_000),
    );
    assert_eq!(offline.status, "Offline");
    assert_eq!(offline.latency, "Measuring…");
    assert_eq!((offline.color)(&theme), colors.danger);

    for state in [
        FeedConnectionState::Discovering,
        FeedConnectionState::Authenticating,
    ] {
        let connecting = connection_presentation(TerminalProvider::Rithmic, state, None);
        assert_eq!((connecting.color)(&theme), colors.warning);
    }
    let stopped = connection_presentation(
        TerminalProvider::Rithmic,
        FeedConnectionState::Stopped,
        None,
    );
    assert_eq!((stopped.color)(&theme), colors.danger);
}

#[test]
fn header_market_dot_shows_session_only_while_the_feed_is_live() {
    use aeris_contracts::{MarketSessionPhase, MarketSessionSource, MarketSessionStatus};
    const MINUTE: i64 = 60_000_000_000;
    let theme = super::AerisTheme::dark();
    let colors = theme.colors;
    let mut status = MarketSessionStatus {
        instrument_id: "rithmic:Future:ESZ6:CME".into(),
        phase: MarketSessionPhase::Regular,
        source: MarketSessionSource::ProviderCalendar,
        session_start_unix_nanos: Some(0),
        session_end_unix_nanos: Some(134 * MINUTE),
        next_open_unix_nanos: Some(1_440 * MINUTE),
    };
    let present = |state, status: &MarketSessionStatus| {
        connection_presentation(TerminalProvider::Rithmic, state, None).with_market(
            state == FeedConnectionState::Streaming,
            Some(status),
            aeris_chart_integration::DEFAULT_TIME_ZONE,
            0,
        )
    };

    let open = present(FeedConnectionState::Streaming, &status);
    assert_eq!((open.color)(&theme), colors.positive);
    assert_eq!(
        open.market_rows,
        vec![
            ("Market", "Open".to_string()),
            ("Session", "00:00–02:14 Etc/UTC".to_string()),
            ("Closes in", "2h 14m".to_string()),
        ]
    );
    // Feed health outranks the market phase.
    let reconnecting = present(FeedConnectionState::Recovering, &status);
    assert_eq!((reconnecting.color)(&theme), colors.warning);
    assert_eq!(
        (present(FeedConnectionState::Stopped, &status).color)(&theme),
        colors.danger
    );

    status.phase = MarketSessionPhase::PreMarket;
    assert_eq!(
        (present(FeedConnectionState::Streaming, &status).color)(&theme),
        colors.warning
    );
    status.phase = MarketSessionPhase::Closed;
    let closed = present(FeedConnectionState::Streaming, &status);
    assert_eq!((closed.color)(&theme), colors.text_muted);
    assert_eq!(
        closed.market_rows.last(),
        Some(&("Opens in", "24h 0m".to_string()))
    );
    status.phase = MarketSessionPhase::AlwaysOpen;
    let always = present(FeedConnectionState::Streaming, &status);
    assert_eq!((always.color)(&theme), colors.positive);
    assert_eq!(always.market_rows[0], ("Market", "Open 24/7".to_string()));
}

#[test]
fn reconnect_retry_states_do_not_flicker_back_to_offline() {
    assert_eq!(
        stabilized_connection_state(
            Some(FeedConnectionState::Streaming),
            FeedConnectionState::Disconnected,
        ),
        FeedConnectionState::Disconnected
    );
    assert_eq!(
        stabilized_connection_state(
            Some(FeedConnectionState::Disconnected),
            FeedConnectionState::Discovering,
        ),
        FeedConnectionState::Recovering
    );
    assert_eq!(
        stabilized_connection_state(
            Some(FeedConnectionState::Recovering),
            FeedConnectionState::Disconnected,
        ),
        FeedConnectionState::Recovering
    );
    assert_eq!(
        stabilized_connection_state(
            Some(FeedConnectionState::Recovering),
            FeedConnectionState::Streaming,
        ),
        FeedConnectionState::Streaming
    );
    assert_eq!(
        connectivity_chart_state(ChartState::Ready, FeedConnectionState::Recovering, true,),
        ChartState::Recovering
    );
    assert_eq!(
        connectivity_chart_state(ChartState::Error, FeedConnectionState::Disconnected, true,),
        ChartState::Stale
    );
}

#[test]
fn header_lifecycle_values_are_truthfully_labeled() {
    let rithmic = connection_presentation(
        TerminalProvider::Rithmic,
        FeedConnectionState::Streaming,
        Some(18_400_000),
    );
    assert_eq!(rithmic.provider, "Rithmic");
    assert_eq!(rithmic.status, "Live");
    assert_eq!(rithmic.latency, "18.4 ms RTT");

    let hyperliquid = connection_presentation(
        TerminalProvider::Hyperliquid,
        FeedConnectionState::Streaming,
        Some(900_000),
    );
    assert_eq!(hyperliquid.provider, "Hyperliquid");
    assert_eq!(hyperliquid.status, "Live");
    assert_eq!(hyperliquid.latency, "0.9 ms RTT");
    let controls = HeaderControls::from_state(true, true).with_chart_controls(true);
    assert!(controls.enabled(HeaderControls::INSTRUMENT));
    assert!(controls.enabled(HeaderControls::SERIES));
    assert!(controls.enabled(HeaderControls::INDICATOR));
    assert!(controls.enabled(HeaderControls::CHART_TYPE));
}

#[test]
fn chart_controls_follow_retained_data_instead_of_transient_chart_state() {
    let retained_chart_controls = HeaderControls::from_state(true, false).with_chart_controls(true);
    assert!(retained_chart_controls.enabled(HeaderControls::INDICATOR));
    assert!(retained_chart_controls.enabled(HeaderControls::CHART_TYPE));

    let empty_chart_controls = HeaderControls::from_state(true, false).with_chart_controls(false);
    assert!(!empty_chart_controls.enabled(HeaderControls::INDICATOR));
    assert!(!empty_chart_controls.enabled(HeaderControls::CHART_TYPE));
}

#[test]
fn chart_context_menu_stays_inside_the_window() {
    let viewport = size(px(800.0), px(600.0));
    let width = px(CHART_CONTEXT_MENU_WIDTH);
    let overflow = clamp_chart_context_menu_origin(point(px(2000.0), px(2000.0)), viewport);
    assert!(overflow.x + width <= px(800.0) - px(OVERLAY_EDGE_MARGIN));
    assert!(overflow.y <= px(600.0) - px(OVERLAY_EDGE_MARGIN));
    assert_eq!(
        clamp_chart_context_menu_origin(point(px(-20.0), px(-20.0)), size(px(800.0), px(600.0))),
        point(px(OVERLAY_EDGE_MARGIN), px(OVERLAY_EDGE_MARGIN))
    );

    // Context menus keep their 1x design size on large screens.
    let large = size(px(3840.0), px(2160.0));
    let overflow = clamp_chart_context_menu_origin(point(px(9000.0), px(9000.0)), large);
    assert_eq!(
        overflow.x,
        px(3840.0) - width - px(OVERLAY_EDGE_MARGIN),
        "a large viewport does not widen the menu"
    );
}

#[test]
fn capture_chart_submenu_opens_beside_its_row_inside_the_window() {
    let viewport = size(px(1200.0), px(800.0));
    let root = point(px(100.0), px(100.0));
    let right = super::clamp_chart_capture_flyout_origin(root, viewport);
    assert_eq!(
        right.x,
        root.x + px(CHART_CONTEXT_MENU_WIDTH) + px(super::PRICE_AXIS_FLYOUT_GAP)
    );
    assert_eq!(
        right.y,
        root.y
            + px(super::CHART_CONTEXT_MENU_ROW_HEIGHT * 2.0
                + super::CHART_CONTEXT_MENU_SEPARATOR_HEIGHT)
    );

    let near_right_edge = point(px(1200.0 - CHART_CONTEXT_MENU_WIDTH - 8.0), px(100.0));
    let left = super::clamp_chart_capture_flyout_origin(near_right_edge, viewport);
    assert_eq!(
        left.x,
        near_right_edge.x
            - px(super::CHART_CAPTURE_FLYOUT_WIDTH)
            - px(super::PRICE_AXIS_FLYOUT_GAP)
    );

    let bottom = point(px(100.0), px(790.0));
    let clamped = super::clamp_chart_capture_flyout_origin(bottom, viewport);
    assert!(clamped.y + px(super::overlay_height(2.0, 0.0)) <= px(800.0 - OVERLAY_EDGE_MARGIN));
}

#[test]
fn chart_context_remove_actions_use_destructive_color() {
    assert!(super::ChartContextAction::ClearDrawings.is_destructive());
    assert!(super::ChartContextAction::ClearIndicators.is_destructive());
    assert!(!super::ChartContextAction::CopyPrice.is_destructive());
    assert!(!super::ChartContextAction::Reset.is_destructive());
    assert!(!super::ChartContextAction::Close.is_destructive());
    assert!(!super::ChartContextAction::Settings.is_destructive());
    let items = super::chart_context_menu_items(super::ChartContextMenuState {
        pane_count: 1,
        flags: 0,
    });
    assert_eq!(items[0].icon, super::HugeIcon::RefreshV2);
    assert_eq!(items[1].icon, super::HugeIcon::Copy);
    assert_eq!(items[1].label, "Copy price");
    assert!(!items[1].enabled);
    assert_eq!(items[1].action, super::ChartContextAction::CopyPrice);
    let copy_ready = super::chart_context_menu_items(super::ChartContextMenuState {
        pane_count: 1,
        flags: super::ChartContextMenuState::COPY_PRICE,
    });
    assert!(copy_ready[1].enabled);
    assert_eq!(items[2].action, super::ChartContextAction::CaptureMenu);
    assert_eq!(items[2].label, "Capture chart");
    assert!(!items[2].enabled);
    let ready = super::ChartContextMenuState {
        pane_count: 1,
        flags: super::ChartContextMenuState::READY,
    };
    assert!(super::chart_context_menu_items(ready)[2].enabled);
    let capture_menu = super::chart_capture_menu_items(ready);
    assert_eq!(
        capture_menu.map(|item| item.action),
        [
            super::ChartContextAction::CopyCapture,
            super::ChartContextAction::SaveCapture
        ]
    );
    assert!(capture_menu.iter().all(|item| item.enabled));
    let trash = items
        .into_iter()
        .filter(|item| item.action.is_destructive())
        .map(|item| item.icon)
        .collect::<Vec<_>>();
    assert_eq!(trash, [super::HugeIcon::Trash, super::HugeIcon::Trash]);
}

#[test]
fn copy_feedback_generation_fences_stale_close_timers() {
    let menu = super::ChartContextMenu {
        workspace_id: 1,
        pane_id: 2,
        position: point(px(20.0), px(20.0)),
        kind: super::ChartContextKind::Pane,
        flyout: super::PriceAxisMenuFlyout::None,
        capture_flyout_open: true,
        copy_price: Some("123.45".into()),
        copy_feedback: Some(super::ChartCopyFeedback {
            action: super::ChartContextAction::CopyCapture,
            generation: 7,
        }),
    };

    assert!(super::TerminalApp::chart_context_copy_feedback_is_current(
        Some(&menu),
        7
    ));
    assert!(!super::TerminalApp::chart_context_copy_feedback_is_current(
        Some(&menu),
        6
    ));
    assert!(!super::TerminalApp::chart_context_copy_feedback_is_current(
        None, 7
    ));
}

#[test]
fn chrome_menus_shrink_to_fit_a_small_viewport() {
    let chrome_height = 44.0;
    let roomy = chrome_menu_extent(size(px(1440.0), px(900.0)), chrome_height);
    assert!((roomy.width - CHROME_MENU_WIDTH).abs() < f32::EPSILON);
    assert!((roomy.list_height - CHROME_MENU_LIST_HEIGHT).abs() < f32::EPSILON);

    let cramped = chrome_menu_extent(size(px(800.0), px(600.0)), chrome_height);
    assert!(cramped.width < CHROME_MENU_WIDTH);
    assert!(cramped.width + OVERLAY_EDGE_MARGIN * 2.0 <= 800.0);
    assert!(cramped.list_height < CHROME_MENU_LIST_HEIGHT);
    let drawn = CHROME_MENU_SEARCH_HEIGHT + cramped.list_height;
    assert!(drawn + chrome_height + OVERLAY_EDGE_MARGIN * 2.0 <= 600.0);
    assert!(drawn <= CHROME_MENU_MAX_HEIGHT);
}

#[test]
fn chrome_menus_keep_their_design_size_on_a_large_viewport() {
    let large = chrome_menu_extent(size(px(3840.0), px(2160.0)), 44.0);
    assert!((large.width - CHROME_MENU_WIDTH).abs() < f32::EPSILON);
    assert!((large.list_height - CHROME_MENU_LIST_HEIGHT).abs() < f32::EPSILON);
}

#[test]
fn chrome_menus_never_exceed_a_tiny_viewport() {
    let tiny = chrome_menu_extent(size(px(240.0), px(180.0)), 44.0);
    assert!(tiny.width <= 240.0);
    assert!(tiny.list_height >= 0.0);
    assert!(CHROME_MENU_SEARCH_HEIGHT + tiny.list_height <= 180.0 - 44.0);
}

#[test]
fn header_history_controls_gate_on_their_own_half_of_the_stack() {
    use super::{DrawingHistoryControl, DrawingHistoryState};

    let empty = DrawingHistoryState::default();
    assert!(!DrawingHistoryControl::Undo.enabled(empty));
    assert!(!DrawingHistoryControl::Redo.enabled(empty));

    let undo_only = DrawingHistoryState {
        can_undo: true,
        can_redo: false,
    };
    assert!(DrawingHistoryControl::Undo.enabled(undo_only));
    assert!(
        !DrawingHistoryControl::Redo.enabled(undo_only),
        "redo must stay disabled while nothing has been reversed"
    );

    assert_ne!(
        DrawingHistoryControl::Undo.id(),
        DrawingHistoryControl::Redo.id()
    );
    assert_eq!(DrawingHistoryControl::Undo.icon(), super::HugeIcon::Undo);
    assert_eq!(DrawingHistoryControl::Redo.icon(), super::HugeIcon::Redo);
}

#[test]
fn anchored_menus_slide_back_inside_the_window() {
    let viewport = size(px(800.0), px(600.0));
    let flush_right = clamp_anchored_menu_left(px(760.0), viewport, TIMEFRAME_MENU_WIDTH);
    assert!(
        flush_right + px(TIMEFRAME_MENU_WIDTH) <= px(800.0) - px(OVERLAY_EDGE_MARGIN),
        "a trigger near the right edge must not push the panel off-screen"
    );
    assert_eq!(
        clamp_anchored_menu_left(px(120.0), viewport, TIMEFRAME_MENU_WIDTH),
        px(120.0),
        "a panel that already fits keeps its anchored position"
    );
    assert_eq!(
        clamp_anchored_menu_left(px(40.0), size(px(100.0), px(600.0)), TIMEFRAME_MENU_WIDTH),
        px(0.0),
        "a panel wider than the window pins to the left edge rather than going negative"
    );
}

#[test]
fn price_axis_menu_stays_inside_the_window() {
    let overflow = clamp_price_axis_menu_origin(
        point(px(2000.0), px(2000.0)),
        size(px(800.0), px(600.0)),
        false,
    );
    let width = px(CHART_CONTEXT_MENU_WIDTH);
    assert!(overflow.x >= px(OVERLAY_EDGE_MARGIN));
    assert!(overflow.y >= px(OVERLAY_EDGE_MARGIN));
    assert!(overflow.x + width <= px(800.0) - px(OVERLAY_EDGE_MARGIN));
    assert!(overflow.y <= px(600.0) - px(OVERLAY_EDGE_MARGIN));
}

#[test]
fn price_axis_menu_opens_into_the_chart() {
    let width = px(CHART_CONTEXT_MENU_WIDTH);
    let right_axis = clamp_price_axis_menu_origin(
        point(px(780.0), px(200.0)),
        size(px(800.0), px(600.0)),
        false,
    );
    assert!(right_axis.x + width + px(PRICE_AXIS_MENU_GAP) <= px(780.0));
    assert!(right_axis.x >= px(OVERLAY_EDGE_MARGIN));

    let left_axis =
        clamp_price_axis_menu_origin(point(px(24.0), px(200.0)), size(px(800.0), px(600.0)), true);
    assert!(left_axis.x >= px(24.0) + px(PRICE_AXIS_MENU_GAP));
    assert!(left_axis.x + width <= px(800.0) - px(OVERLAY_EDGE_MARGIN));
}

#[test]
fn price_axis_menu_compacts_labels_and_lines_into_flyouts() {
    let state = PriceAxisMenuState {
        flags: PriceAxisMenuState::PRICE_LINE
            | PriceAxisMenuState::LAST_VALUE
            | PriceAxisMenuState::TITLE
            | PriceAxisMenuState::COUNTDOWN
            | PriceAxisMenuState::INDICATOR_NAMES
            | PriceAxisMenuState::INDICATOR_VALUES
            | PriceAxisMenuState::INDICATOR_PRICE_LINES
            | PriceAxisMenuState::AUTO_SCALE
            | PriceAxisMenuState::ALIGN_LABELS,
        mode: 0,
        left: false,
        precision: None,
    };
    assert_eq!(
        price_axis_root_rows(PriceAxisMenuFlyout::None, state).map(PriceAxisMenuRow::label),
        [
            "Labels",
            "Lines",
            "Auto scale",
            "Invert scale",
            "Scale mode",
            "Y-axis",
            "Precision",
        ]
    );
    let labels = price_axis_flyout_rows(PriceAxisMenuFlyout::Labels, state);
    assert_eq!(
        labels
            .iter()
            .copied()
            .map(PriceAxisMenuRow::label)
            .collect::<Vec<_>>(),
        [
            "Symbol name label",
            "Symbol last price label",
            "Symbol previous day close price label",
            "Pre/post/night market price label",
            "High and low price labels",
            "Bid and ask labels",
            "Indicators and financials name labels",
            "Indicators and financials value labels",
            "Countdown to bar close",
            "No overlapping labels",
        ]
    );
    assert!(labels.iter().all(|row| {
        ![
            "Symbol previous day close price label",
            "Pre/post/night market price label",
            "High and low price labels",
        ]
        .contains(&row.label())
            || !row.enabled()
    }));
    let lines = price_axis_flyout_rows(PriceAxisMenuFlyout::Lines, state);
    assert_eq!(
        lines
            .iter()
            .copied()
            .map(PriceAxisMenuRow::label)
            .collect::<Vec<_>>(),
        [
            "Price line",
            "Previous day close price line",
            "Pre/post/night market price line",
            "High and low price lines",
            "Bid and ask lines",
            "Indicators and financials price lines",
        ]
    );
    assert!(lines.iter().all(|row| {
        ![
            "Previous day close price line",
            "Pre/post/night market price line",
            "High and low price lines",
        ]
        .contains(&row.label())
            || !row.enabled()
    }));
}

#[test]
fn side_panel_controls_keep_stable_labels_and_explicit_destinations() {
    assert_eq!(SidePanel::OrderBook.title(), "Order Book");
    assert_eq!(
        aeris_desktop::command_registry::command(
            aeris_desktop::command_registry::CommandId::ToggleOrderBook
        )
        .title,
        "Order book"
    );
    assert_eq!(SidePanel::Watchlist.title(), "Watchlist");
    assert_eq!(
        aeris_desktop::command_registry::command(
            aeris_desktop::command_registry::CommandId::ToggleWatchlist
        )
        .title,
        "Watchlist"
    );
}

#[test]
fn time_sales_is_an_independent_market_panel_with_a_stable_persisted_bit() {
    assert_eq!(SidePanel::TimeSales.title(), "Time & Sales");
    assert_eq!(
        aeris_desktop::command_registry::command(
            aeris_desktop::command_registry::CommandId::ToggleTimeSales
        )
        .title,
        "Time & Sales"
    );
    assert!(SidePanel::TimeSales.requires_market());
    assert!(!SidePanel::Watchlist.requires_market());

    let mut panels = SidePanelVisibility::default();
    panels.set(SidePanel::TimeSales, true);
    assert!(panels.any(), "Time & Sales docks without the order book");
    assert!(!panels.contains(SidePanel::OrderBook));
    // Layouts saved while Time & Sales lived inside the order book used bit 4.
    assert_eq!(SidePanelVisibility::from_persisted(4), panels);
    assert_eq!(
        SidePanelVisibility::from_persisted(u32::MAX),
        SidePanelVisibility(0b1111)
    );
}

#[test]
fn order_entry_is_its_own_market_panel_that_keeps_the_book_demanded() {
    assert_eq!(SidePanel::OrderEntry.title(), "Order Entry");
    assert_eq!(
        aeris_desktop::command_registry::command(
            aeris_desktop::command_registry::CommandId::ToggleOrderEntry
        )
        .title,
        "Order entry"
    );
    assert!(SidePanel::OrderEntry.requires_market());

    let mut panels = SidePanelVisibility::default();
    panels.set(SidePanel::OrderEntry, true);
    assert!(!panels.contains(SidePanel::OrderBook));
    assert_eq!(SidePanelVisibility::from_persisted(8), panels);
    // The ticket trades on the live book frame, so it holds depth demand without the ladder.
    assert!(panels.needs_order_book());

    let mut tape_and_watchlist = SidePanelVisibility::default();
    tape_and_watchlist.set(SidePanel::TimeSales, true);
    tape_and_watchlist.set(SidePanel::Watchlist, true);
    assert!(!tape_and_watchlist.needs_order_book());
}

#[test]
fn time_sales_filter_reset_restores_every_default_atomically() {
    let mut filter = TimeSalesFilter {
        side: TimeSalesSideFilter::Sell,
        minimum_quantity: 100.0,
        price_range_ticks: Some(50),
    };
    assert!(filter.reset());
    assert_eq!(filter, TimeSalesFilter::default());
    assert!(!filter.reset(), "resetting defaults is an idempotent no-op");
}

#[test]
fn closing_the_symbol_menu_preserves_a_pending_watchlist_selection() {
    assert_eq!(
        instrument_target_after_close(SymbolSelectionTarget::Watchlist, true),
        SymbolSelectionTarget::Watchlist
    );
    assert_eq!(
        instrument_target_after_close(SymbolSelectionTarget::Watchlist, false),
        SymbolSelectionTarget::Chart
    );
}

#[test]
fn market_price_presentation_trims_storage_padding_without_losing_precision() {
    assert_eq!(super::market_price_text(13_100_000_000, 8), "131.00");
    assert_eq!(super::market_price_text(7_654_500_000_000, 8), "76545.00");
    assert_eq!(super::market_price_text(9_881_800_000, 8), "98.818");
    assert_eq!(super::market_price_text(-16_500_000, 8), "-0.165");
    assert_eq!(super::market_price_text(123, 0), "123");
}

fn tick_grid_instrument(price_increment: i64) -> InstallProviderInstrument {
    let mut instrument = super::local_state::default_workspace().watchlist_entries[0]
        .instrument
        .clone()
        .expect("default watchlist instrument");
    instrument.price_increment = Some(price_increment);
    instrument
}

#[test]
fn watchlist_prices_keep_the_tick_grid_precision() {
    let equity = tick_grid_instrument(1_000_000);
    assert_eq!(
        super::market_summary_price(21_550_390_000, &equity),
        "215.50"
    );
    assert_eq!(
        super::market_summary_price(36_089_500_000, &equity),
        "360.90"
    );
    assert_eq!(
        super::market_summary_price(19_060_000_000, &equity),
        "190.60"
    );
    assert_eq!(super::market_summary_change(72_130_000, &equity), "+0.72");
    assert_eq!(super::market_summary_change(-36_770_000, &equity), "-0.37");
    assert_eq!(super::market_summary_change(400_000, &equity), "0.00");

    let future = tick_grid_instrument(25_000_000);
    assert_eq!(
        super::market_summary_price(773_800_000_000, &future),
        "7738.00"
    );
    assert_eq!(
        super::market_summary_change(3_375_000_000, &future),
        "+33.75"
    );

    let ungridded = super::local_state::default_workspace().watchlist_entries[0]
        .instrument
        .clone()
        .expect("default watchlist instrument");
    assert_eq!(
        super::market_summary_change(13_100_000_000, &ungridded),
        "+131.00"
    );
}

#[test]
fn watchlist_tick_colors_only_the_changed_presented_digits() {
    let equity = tick_grid_instrument(1_000_000);
    let tick = |last, reference| {
        let text = super::market_summary_price(last, &equity);
        super::market_summary_tick(&text, last, reference, &equity)
    };
    let up = tick(24_102_000_000, 24_101_000_000).expect("up tick");
    assert_eq!(up.direction, std::cmp::Ordering::Greater);
    assert_eq!(up.changed_from, 5);
    let down = tick(9_338_000_000, 9_340_000_000).expect("down tick");
    assert_eq!(down.direction, std::cmp::Ordering::Less);
    assert_eq!(down.changed_from, 3);
    let widened = tick(10_000_000_000, 9_999_000_000).expect("new digit");
    assert_eq!(widened.changed_from, 0);
    assert_eq!(tick(21_550_390_000, 21_550_000_000), None);
}

#[test]
fn watchlist_tick_reference_ignores_sub_grid_prints() {
    let mut entry = MarketSummaryEntry::new(
        tick_grid_instrument(1_000_000),
        None,
        None,
        ConsumerResourceClass::Background,
    );
    let bar = |close| MarketBar {
        source_sequence: 1,
        exchange_timestamp_seconds: 100,
        exchange_timestamp_unix_nanos: 100_000_000_000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 10,
    };
    entry.apply_bar(bar(21_550_000_000));
    assert_eq!(entry.tick_reference, None);
    entry.apply_bar(bar(21_550_390_000));
    assert_eq!(entry.tick_reference, None);
    entry.apply_bar(bar(21_545_000_000));
    assert_eq!(entry.tick_reference, Some(21_550_390_000));
}

#[test]
fn watchlist_tracks_current_and_previous_daily_closes_in_timestamp_order() {
    let instrument = super::local_state::default_workspace().watchlist_entries[0]
        .instrument
        .clone()
        .expect("default watchlist instrument");
    let mut entry =
        MarketSummaryEntry::new(instrument, None, None, ConsumerResourceClass::Background);
    assert_eq!(entry.resource_class, ConsumerResourceClass::Background);
    entry.set_resource_class(ConsumerResourceClass::Foreground);
    assert_eq!(entry.resource_class, ConsumerResourceClass::Foreground);
    let bar = |sequence, timestamp, close| MarketBar {
        source_sequence: sequence,
        exchange_timestamp_seconds: timestamp,
        exchange_timestamp_unix_nanos: timestamp * 1_000_000_000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 10,
    };

    entry.apply_bar(bar(1, 100, 10_000));
    entry.apply_bar(bar(2, 200, 10_500));
    entry.apply_bar(bar(3, 200, 10_600));
    entry.apply_bar(bar(4, 150, 9_000));

    assert_eq!(entry.previous_close, Some(10_000));
    assert_eq!(entry.last.expect("current bar").close, 10_600);
    let values = entry.values();
    assert_eq!(values.last, Some(10_600));
    assert_eq!(values.change, Some(600));
    assert_eq!(values.change_percent, Some(6.0));

    entry.apply_bar(bar(5, 300, 11_000));
    assert_eq!(entry.previous_close, Some(10_600));
    assert_eq!(entry.last.expect("next daily bar").close, 11_000);
    let values = entry.values();
    assert_eq!(values.change, Some(400));
    assert!((values.change_percent.expect("daily change") - 3.773_584_905_660_377_4).abs() < 1e-12);

    assert_eq!(
        super::market_summary_values(entry.last, Some(0)).change_percent,
        None
    );
}

#[test]
fn market_summary_accepts_only_current_fenced_canonical_bars() {
    use aeris_desktop::test_support::{FixtureMarketEvents, FixtureSeries};
    use aeris_market_runtime::SeriesTailOperation;

    let instrument = super::local_state::default_workspace().watchlist_entries[0]
        .instrument
        .clone()
        .expect("default watchlist instrument");
    let route = FixtureSeries {
        series: super::engine_market_worker::series_key(
            &instrument,
            aeris_market_data::ChartInterval::Day1,
        )
        .expect("valid summary series"),
        provider_generation: instrument.session_generation.max(1),
        price_scale: u8::try_from(instrument.price_scale).expect("price scale"),
        quantity_scale: u8::try_from(instrument.quantity_scale).expect("quantity scale"),
    };
    let mut entry =
        MarketSummaryEntry::new(instrument, None, None, ConsumerResourceClass::Background);
    let descriptor = entry.descriptor.as_mut().expect("fixture descriptor");
    descriptor.asset_class = aeris_instruments::InstrumentClass::Future;
    descriptor.currency = Some("USDC".into());
    let events = FixtureMarketEvents::default();
    let bar = |sequence, close| MarketBar {
        source_sequence: sequence,
        exchange_timestamp_seconds: i64::try_from(sequence).expect("sequence") * 86_400,
        exchange_timestamp_unix_nanos: i64::try_from(sequence).expect("sequence")
            * 86_400_000_000_000,
        open: close,
        high: close,
        low: close,
        close,
        volume: 10,
    };

    entry.apply_runtime_publication(events.snapshot(&route, 1, 1, [bar(1, 10_000)]), 1);
    assert_eq!(entry.last.expect("snapshot").close, 10_000);
    entry.apply_runtime_publication(
        events.update(
            &route,
            2,
            2,
            SeriesTailOperation::Append,
            bar(2, 99_000),
            false,
        ),
        2,
    );
    assert_eq!(entry.last.expect("retired selection ignored").close, 10_000);
    entry.apply_runtime_publication(
        events.update(
            &route,
            1,
            2,
            SeriesTailOperation::Append,
            bar(2, 10_500),
            false,
        ),
        3,
    );
    assert_eq!(entry.last.expect("current tail").close, 10_500);
    assert_eq!(entry.previous_close, Some(10_000));
}

#[test]
fn tab_and_watchlist_share_one_daily_summary_requirement() {
    let instrument = super::local_state::default_workspace().watchlist_entries[0]
        .instrument
        .clone()
        .expect("default watchlist instrument");

    let shared = super::workspace_tabs::market_summary_requirements(
        [instrument.clone(), instrument.clone()],
        std::slice::from_ref(&instrument),
        false,
    );
    assert_eq!(
        shared.len(),
        1,
        "duplicate tab/watchlist references must share one daily summary owner"
    );
    assert!(
        shared.values().next().expect("shared summary").1,
        "a visible tab keeps the shared daily summary foreground when the watchlist is hidden"
    );

    let hidden_watchlist_only = super::workspace_tabs::market_summary_requirements(
        std::iter::empty(),
        std::slice::from_ref(&instrument),
        false,
    );
    assert_eq!(hidden_watchlist_only.len(), 1);
    assert!(
        !hidden_watchlist_only
            .values()
            .next()
            .expect("watchlist summary")
            .1,
        "a hidden watchlist alone may release live daily-summary demand"
    );

    let tab_only = super::workspace_tabs::market_summary_requirements([instrument], &[], false);
    assert_eq!(tab_only.len(), 1);
    assert!(
        tab_only.values().next().expect("tab summary").1,
        "a workspace tab requires its daily summary even when the watchlist has no matching row"
    );
}

#[test]
fn pending_market_switch_keeps_the_committed_summary_until_the_marker_arrives() {
    let current = super::local_state::default_workspace().watchlist_entries[0]
        .instrument
        .clone()
        .expect("default watchlist instrument");
    let mut previous = current.clone();
    previous.instrument_id = format!("{}:previous", previous.instrument_id);

    assert_eq!(
        super::workspace_tabs::market_summary_instrument_for_switch(Some(&current), None, true),
        Some(current.clone()),
        "pending-before-marker must not retire the still-authoritative current summary"
    );
    assert_eq!(
        super::workspace_tabs::market_summary_instrument_for_switch(
            Some(&current),
            Some(&previous),
            true,
        ),
        Some(previous),
        "after the marker, retained pixels and summary demand both belong to the prior selection"
    );
    assert_eq!(
        super::workspace_tabs::market_summary_instrument_for_switch(Some(&current), None, false),
        Some(current),
    );
}

#[test]
fn watchlist_reordering_moves_both_directions_and_clamps_the_destination() {
    let mut symbols = vec!["BTC", "ETH", "SOL", "DOGE"];
    assert!(super::workspace_tabs::move_item(&mut symbols, 0, 2));
    assert_eq!(symbols, vec!["ETH", "SOL", "BTC", "DOGE"]);
    assert!(super::workspace_tabs::move_item(&mut symbols, 3, 0));
    assert_eq!(symbols, vec!["DOGE", "ETH", "SOL", "BTC"]);
    assert!(super::workspace_tabs::move_item(
        &mut symbols,
        1,
        usize::MAX
    ));
    assert_eq!(symbols, vec!["DOGE", "SOL", "BTC", "ETH"]);
    assert!(!super::workspace_tabs::move_item(&mut symbols, 3, 3));
}

#[test]
fn chart_notice_distinguishes_empty_loading_from_retained_recovery() {
    let loading = chart_surface_notice(
        ChartState::Loading,
        false,
        false,
        "discovering Rithmic Test systems",
    )
    .expect("loading notice");
    assert_eq!(loading.label, "Loading chart");
    assert_eq!(
        loading.detail.as_deref(),
        Some("discovering Rithmic Test systems")
    );
    assert_eq!(loading.placement, ChartNoticePlacement::Center);
    assert_eq!(loading.tone, ChartNoticeTone::Muted);

    let recovery = chart_surface_notice(
        ChartState::Recovering,
        true,
        false,
        "Rithmic Test session will retry",
    )
    .expect("recovery notice");
    assert_eq!(recovery.label, "Reconnecting chart");
    assert_eq!(recovery.placement, ChartNoticePlacement::Corner);
    assert_eq!(recovery.tone, ChartNoticeTone::Warning);
    assert!(chart_surface_notice(ChartState::Ready, true, false, "current").is_none());
}

#[test]
fn chart_error_and_stale_notices_use_truthful_severity() {
    let stale = chart_surface_notice(ChartState::Stale, true, false, "trade stream is silent")
        .expect("stale notice");
    assert_eq!(stale.label, "Chart stale");
    assert_eq!(stale.tone, ChartNoticeTone::Warning);

    let error = chart_surface_notice(
        ChartState::Error,
        false,
        false,
        "Rithmic Test authentication was rejected",
    )
    .expect("error notice");
    assert_eq!(error.label, "Chart unavailable");
    assert_eq!(
        error.detail.as_deref(),
        Some("Rithmic Test authentication was rejected")
    );
    assert_eq!(error.placement, ChartNoticePlacement::Center);
    assert_eq!(error.tone, ChartNoticeTone::Loss);
}

#[test]
fn history_unavailable_notice_distinguishes_live_bars_from_waiting() {
    let waiting = chart_surface_notice(
        ChartState::HistoryUnavailable,
        false,
        false,
        "Waiting for live trades; history will backfill when the provider answers.",
    )
    .expect("waiting notice");
    assert_eq!(waiting.label, "History unavailable");
    assert_eq!(
        waiting.detail.as_deref(),
        Some("Waiting for live trades; history will backfill when the provider answers.")
    );
    assert_eq!(waiting.placement, ChartNoticePlacement::Center);
    assert_eq!(waiting.tone, ChartNoticeTone::Warning);

    let live = chart_surface_notice(ChartState::HistoryUnavailable, true, false, "ignored")
        .expect("live-bars notice");
    assert_eq!(live.label, "History unavailable");
    assert_eq!(
        live.detail.as_deref(),
        Some("Showing live bars; history will backfill when the provider answers.")
    );
    assert_eq!(live.placement, ChartNoticePlacement::Corner);
    assert_eq!(live.tone, ChartNoticeTone::Warning);

    assert_eq!(
        chart_status_detail(
            ChartState::HistoryUnavailable,
            FeedConnectionState::Streaming,
            None,
            None,
        ),
        "Waiting for live trades; history will backfill when the provider answers."
    );
    assert!(
        ChartState::HistoryUnavailable.awaits_engine_readiness(),
        "a live-only bar publication alone must not clear the notice"
    );
}

#[test]
fn contextual_labels_keep_contract_and_pending_series_truthful() {
    assert_eq!(instrument_selector_label(None), "Contract");
    assert_eq!(
        instrument_selector_label(Some(("MNQU6", "CME"))),
        "MNQU6 / CME"
    );
    assert_eq!(series_selector_label(ChartInterval::Minute1), "1m");
    assert_eq!(series_selector_label(ChartInterval::Minute5), "5m");
    assert_eq!(series_selector_label(ChartInterval::Hour4), "4h");
}

#[test]
fn provider_catalog_keeps_display_symbol_separate_from_wire_symbol() {
    let instrument = ProviderInstrumentSummary {
        kind: ProviderInstrumentKind::Instrument,
        symbol: "BTC".to_string(),
        display_symbol: "BTC-USDC".to_string(),
        exchange: "Hyperliquid".to_string(),
        name: None,
        product_code: None,
        instrument_type: Some("perpetual".to_string()),
        expiration_date: None,
        asset_class: None,
    };
    assert_eq!(instrument.display_symbol, "BTC-USDC");
    assert_eq!(instrument.symbol, "BTC");
}

#[test]
fn chart_detail_prefers_connection_context_until_streaming() {
    assert_eq!(
        chart_status_detail(
            ChartState::Loading,
            FeedConnectionState::Authenticating,
            None,
            None,
        ),
        "Authenticating market data."
    );
    assert_eq!(
        chart_status_detail(
            ChartState::Recovering,
            FeedConnectionState::Streaming,
            None,
            None,
        ),
        "Waiting for a covering market snapshot."
    );
    assert_eq!(
        chart_status_detail(
            ChartState::Error,
            FeedConnectionState::Discovering,
            Some(ChartFailure::Market),
            None,
        ),
        ChartFailure::Market.detail(),
        "a chart error is not hidden behind connection progress"
    );
    assert_eq!(
        chart_status_detail(ChartState::Error, FeedConnectionState::Stopped, None, None,),
        "Market worker stopped."
    );
    assert_eq!(
        chart_status_detail(
            ChartState::Loading,
            FeedConnectionState::Streaming,
            None,
            Some(ChartInterval::Minute5)
        ),
        "Loading 5m history."
    );
}

#[test]
fn chart_detail_is_bounded_and_suppresses_generic_duplicates() {
    assert_eq!(
        bounded_status_detail(" Chart unavailable ", "Chart unavailable"),
        None
    );
    let detail = bounded_status_detail(&"x".repeat(200), "Chart unavailable")
        .expect("long detail remains visible");
    assert_eq!(detail.chars().count(), 161);
    assert!(detail.ends_with('…'));
}

#[test]
fn chart_appearance_persists_theme_provenance_and_migrates_flattened_defaults() {
    use super::workspace_surface::{
        persisted_chart_appearance, restored_chart_appearance, restored_chart_axis,
    };
    use aeris_chart_integration::{
        AerisChartTheme, ChartAppearanceColor, ChartAppearanceSettings, ChartAxisAppearance,
        ChartThemeColors,
    };

    let appearance = ChartAppearanceSettings {
        up_color: ChartAppearanceColor::Custom("#abcdef".to_string()),
        background_color: ChartAppearanceColor::Custom("#0b1220".to_string()),
        ..ChartAppearanceSettings::default()
    };
    let axis = ChartAxisAppearance {
        border_color: ChartAppearanceColor::Custom("#475569".to_string()),
        text_color: ChartAppearanceColor::Theme,
    };
    let persisted = persisted_chart_appearance(&appearance, &axis);
    assert_eq!(
        (
            persisted.axis_border_color.as_str(),
            persisted.axis_text_color.as_str()
        ),
        ("#475569", "theme")
    );
    assert_eq!(restored_chart_axis(&persisted), axis);
    assert_eq!(persisted.grid_color, "theme");
    assert_eq!(persisted.text_color, "theme");
    assert_eq!(persisted.up_color, "#abcdef");
    assert_eq!(persisted.background_color, "#0b1220");
    assert_eq!(
        restored_chart_appearance(&persisted).expect("restored appearance"),
        appearance
    );

    let mut legacy = persisted;
    legacy.down_color = ChartThemeColors::for_theme(AerisChartTheme::Light)
        .bearish
        .to_string();
    legacy.grid_color = ChartThemeColors::for_theme(AerisChartTheme::Dark)
        .grid
        .to_string();
    legacy.background_color.clear();
    legacy.text_color.clear();
    legacy.axis_border_color.clear();
    legacy.axis_text_color.clear();
    assert_eq!(restored_chart_axis(&legacy), ChartAxisAppearance::default());
    let restored = restored_chart_appearance(&legacy).expect("legacy appearance");
    assert_eq!(restored.down_color, ChartAppearanceColor::Theme);
    assert_eq!(restored.grid_color, ChartAppearanceColor::Theme);
    assert_eq!(restored.background_color, ChartAppearanceColor::Theme);
    assert_eq!(restored.text_color, ChartAppearanceColor::Theme);
    assert_eq!(
        restored.up_color,
        ChartAppearanceColor::Custom("#abcdef".to_string())
    );
}

#[test]
fn saved_crosshair_style_follows_the_chart_default_unless_the_user_chose_one() {
    use super::workspace_surface::{persisted_chart_appearance, restored_chart_appearance};
    use aeris_chart_integration::{ChartAppearanceSettings, ChartAxisAppearance};

    let default_style = ChartAppearanceSettings::default().crosshair_style;
    let persisted = persisted_chart_appearance(
        &ChartAppearanceSettings::default(),
        &ChartAxisAppearance::default(),
    );
    assert_eq!(persisted.crosshair_style_custom, Some(false));

    // A layout saved before style provenance stored the former dashed default.
    let mut legacy = persisted.clone();
    legacy.crosshair_style = 2;
    legacy.crosshair_style_custom = None;
    let restored = restored_chart_appearance(&legacy).expect("legacy appearance");
    assert_eq!(restored.crosshair_style, default_style);

    // Any other legacy style was the user's pick.
    legacy.crosshair_style = 3;
    let restored = restored_chart_appearance(&legacy).expect("legacy pick");
    assert_eq!(restored.crosshair_style, 3);

    // With provenance, an explicit pick other than the default survives.
    let picked_style = (default_style + 1) % 5;
    let picked = ChartAppearanceSettings {
        crosshair_style: picked_style,
        ..ChartAppearanceSettings::default()
    };
    let persisted = persisted_chart_appearance(&picked, &ChartAxisAppearance::default());
    assert_eq!(persisted.crosshair_style_custom, Some(true));
    assert_eq!(
        restored_chart_appearance(&persisted)
            .expect("picked appearance")
            .crosshair_style,
        picked_style
    );
}

#[test]
fn indicator_menu_keyboard_rows_skip_group_headings_across_both_groups() {
    use super::workspace_surface::{
        IndicatorMenuEntry, OrderFlowMenuStudy, indicator_menu_scroll_item,
    };

    let sma = &super::chart_chrome::INDICATOR_SPECS[0];
    let ema = &super::chart_chrome::INDICATOR_SPECS[1];
    let both = [
        IndicatorMenuEntry::OrderFlow(OrderFlowMenuStudy::Delta),
        IndicatorMenuEntry::OrderFlow(OrderFlowMenuStudy::BigTrades),
        IndicatorMenuEntry::Indicator(sma),
        IndicatorMenuEntry::Indicator(ema),
    ];
    // Children: "Order flow", Delta, Big trades, "Indicators", SMA, EMA.
    assert_eq!(indicator_menu_scroll_item(&both, 0), Some(1));
    assert_eq!(indicator_menu_scroll_item(&both, 1), Some(2));
    assert_eq!(indicator_menu_scroll_item(&both, 2), Some(4));
    assert_eq!(indicator_menu_scroll_item(&both, 3), Some(5));
    assert_eq!(indicator_menu_scroll_item(&both, 4), None);

    let natives_only = [
        IndicatorMenuEntry::Indicator(sma),
        IndicatorMenuEntry::Indicator(ema),
    ];
    assert_eq!(indicator_menu_scroll_item(&natives_only, 0), Some(1));
    assert_eq!(indicator_menu_scroll_item(&natives_only, 1), Some(2));
}

#[test]
fn study_line_colors_persist_only_user_picks_by_output_index() {
    use super::StudyOutputColorDraft;
    use super::workspace_surface::{persisted_output_colors, persisted_study_output_colors};
    use aeris_contracts::WorkspaceChartStudyState;

    let draft = |output_index: usize, color: Option<&str>| StudyOutputColorDraft {
        output_index,
        label: format!("EMA {output_index}"),
        color: color.map(str::to_string),
        default_color: "#2962ff".to_string(),
    };

    // Every line on the chart default stores nothing, so later engine defaults apply.
    let defaults = [draft(0, None), draft(2, None)];
    assert_eq!(persisted_output_colors(5, &defaults), Vec::<String>::new());

    let picked = [
        draft(0, None),
        draft(2, Some("#ff6d00")),
        draft(4, Some("#00bcd4")),
    ];
    let stored = persisted_output_colors(5, &picked);
    assert_eq!(stored, ["", "", "#ff6d00", "", "#00bcd4"]);

    let persisted = WorkspaceChartStudyState {
        output_colors: stored,
        ..WorkspaceChartStudyState::default()
    };
    assert_eq!(
        persisted_study_output_colors(&persisted),
        [
            None,
            None,
            Some("#ff6d00".to_string()),
            None,
            Some("#00bcd4".to_string())
        ]
    );
}

#[test]
fn saved_area_fill_follows_the_translucent_default_instead_of_a_pinned_fallback() {
    use super::workspace_surface::{persisted_chart_appearance, restored_chart_appearance};
    use aeris_chart_integration::{
        ChartAppearanceColor, ChartAppearanceSettings, ChartAxisAppearance,
    };

    let persisted = persisted_chart_appearance(
        &ChartAppearanceSettings::default(),
        &ChartAxisAppearance::default(),
    );
    for color in [
        &persisted.line_color,
        &persisted.area_top_color,
        &persisted.area_bottom_color,
        &persisted.baseline_top_color,
        &persisted.baseline_bottom_color,
    ] {
        assert_eq!(color, "theme");
    }

    // A layout saved before these colors carried provenance pinned the engine's opaque
    // fallbacks and has no bottom fill.
    let mut legacy = persisted.clone();
    legacy.line_color = "#089981".to_string();
    legacy.area_top_color = "#089981".to_string();
    legacy.area_bottom_color.clear();
    legacy.baseline_top_color = "#089981".to_string();
    legacy.baseline_bottom_color = "#F7525F".to_string();
    let restored = restored_chart_appearance(&legacy).expect("legacy appearance");
    assert_eq!(restored, ChartAppearanceSettings::default());

    // A user's own pick survives, including one equal to a former fallback once saved with
    // provenance.
    let mut picked = persisted;
    picked.area_top_color = "#089981".to_string();
    picked.area_bottom_color = "#2962FF00".to_string();
    let restored = restored_chart_appearance(&picked).expect("picked appearance");
    assert_eq!(
        restored.area_top_color,
        ChartAppearanceColor::Custom("#089981".to_string())
    );
    assert_eq!(
        restored.area_bottom_color,
        ChartAppearanceColor::Custom("#2962FF00".to_string())
    );
}

#[test]
fn ladder_clicks_rest_limits_passively_and_place_stops_on_breakout() {
    use super::ladder_click_order;
    use aeris_terminal_ui::OrderBookLevelSide;
    use aeris_trading::{OrderSide, OrderType};
    assert_eq!(
        ladder_click_order(OrderBookLevelSide::Bid, OrderType::Limit),
        (OrderSide::Buy, OrderType::Limit)
    );
    assert_eq!(
        ladder_click_order(OrderBookLevelSide::Ask, OrderType::Market),
        (OrderSide::Sell, OrderType::Limit)
    );
    assert_eq!(
        ladder_click_order(OrderBookLevelSide::Ask, OrderType::Stop),
        (OrderSide::Buy, OrderType::Stop)
    );
    assert_eq!(
        ladder_click_order(OrderBookLevelSide::Bid, OrderType::StopLimit),
        (OrderSide::Sell, OrderType::StopLimit)
    );
}

#[test]
fn working_orders_are_drawn_on_the_row_side_their_click_created() {
    use super::{ladder_click_order, order_book_row_price};
    use aeris_terminal_ui::OrderBookLevelSide;
    use aeris_trading::{
        ClientOrderId, FixedPoint, Order, OrderId, OrderStatus, OrderType, TimeInForce,
        TradingAccountId, TradingProvenance,
    };
    let price = FixedPoint::try_new(10_000, 2).expect("price");
    for row_side in [OrderBookLevelSide::Bid, OrderBookLevelSide::Ask] {
        for selected in [OrderType::Limit, OrderType::Stop, OrderType::StopLimit] {
            let (side, order_type) = ladder_click_order(row_side, selected);
            let (limit_price, stop_price) = match order_type {
                OrderType::Market => (None, None),
                OrderType::Limit | OrderType::RestingAt => (Some(price), None),
                OrderType::Stop => (None, Some(price)),
                OrderType::StopLimit => (Some(price), Some(price)),
            };
            let order = Order {
                id: OrderId::try_new("sim-order-1").expect("order id"),
                client_order_id: ClientOrderId::try_new("ui-1").expect("client id"),
                account_id: TradingAccountId::try_new("aeris-sim-1").expect("account"),
                instrument_id: aeris_instruments::InstrumentId::try_new("instrument:fixture:X")
                    .expect("instrument"),
                side,
                order_type,
                time_in_force: TimeInForce::Day,
                quantity: FixedPoint::try_new(1, 0).expect("quantity"),
                filled_quantity: FixedPoint::try_new(0, 0).expect("filled"),
                limit_price,
                stop_price,
                status: OrderStatus::Working,
                submitted_unix_nanos: 1,
                revision: 1,
                accepted_unix_nanos: 1,
                updated_unix_nanos: 1,
                eligible_after_unix_nanos: 1,
                triggered_unix_nanos: None,
                expires_at_unix_nanos: None,
                terminal_unix_nanos: None,
                provenance: TradingProvenance {
                    venue_id: "aeris-sim".to_string(),
                    provider_id: "fixture".to_string(),
                    session_generation: 1,
                    source_sequence: 1,
                    observed_unix_nanos: 1,
                },
            };
            assert_eq!(order_book_row_price(&order), Some((row_side, price)));
        }
    }
}

fn simulated_chart_fixture(
    account: aeris_trading::TradingAccountId,
) -> aeris_trading::TradingAccount {
    aeris_trading::TradingAccount {
        id: account,
        display_name: "SIM • Chart fixture".to_string(),
        environment: aeris_trading::AccountEnvironment::Simulated,
        venue_id: "aeris-sim".to_string(),
        broker_ref: None,
        currency: "USD".to_string(),
        currency_scale: 2,
        starting_equity: Some(aeris_trading::FixedPoint::try_new(5_000_000, 2).expect("equity")),
    }
}

/// A practice service with one account and one instrument on a 0.01 tick, for chart order
/// projection tests.
struct ChartOrderFixture {
    service: aeris_trading_runtime::TradingService,
    account: aeris_trading::TradingAccountId,
    instrument_id: aeris_instruments::InstrumentId,
    directory: std::path::PathBuf,
}

struct ChartFeedSink;

impl aeris_trading::market::QuoteInterestSink for ChartFeedSink {
    fn set_interest(
        &self,
        _interest: aeris_trading::market::QuoteInterest,
    ) -> Result<(), aeris_trading::market::MarketFeedError> {
        Ok(())
    }
}

fn attach_chart_test_feed(
    service: &aeris_trading_runtime::TradingService,
    instrument_id: &aeris_instruments::InstrumentId,
) {
    use aeris_trading::market::{
        FeedState, InstrumentSession, InstrumentTerms, MarketFeedEvent, MarketRoute, TradingQuote,
    };
    let route = MarketRoute {
        provider_id: "fixture".into(),
        instrument_id: instrument_id.clone(),
        provider_symbol: "BTC".into(),
        display_symbol: "BTC".into(),
        venue_id: "fixture".into(),
        entitlement_id: None,
        price_scale: 2,
        quantity_scale: 0,
    };
    let feed = service
        .attach_market_feed(std::sync::Arc::new(ChartFeedSink))
        .expect("fixture market feed");
    feed.push(MarketFeedEvent::Terms(InstrumentTerms {
        route,
        instrument_class: aeris_instruments::InstrumentClass::CryptoSpot,
        price_grid: aeris_instruments::PriceGrid::Fixed(
            aeris_instruments::InstrumentDecimal::try_new(1, 2).expect("tick"),
        ),
        point_value: Some(
            aeris_instruments::InstrumentDecimal::try_new(1, 0).expect("point value"),
        ),
        quantity_increment: None,
        currency: "USD".into(),
        expiry: None,
        first_notice: None,
        last_trade: None,
        session_hours: Vec::new(),
        session_generation: 1,
    }))
    .expect("terms");
    feed.push(MarketFeedEvent::Session(InstrumentSession {
        instrument_id: instrument_id.clone(),
        provider_generation: 1,
        phase: aeris_instruments::SessionPhase::AlwaysOpen,
        source: aeris_instruments::CalendarSourceKind::AlwaysOpen,
        session_end_unix_nanos: None,
        next_open_unix_nanos: None,
    }))
    .expect("session");
    feed.push(MarketFeedEvent::Feed {
        instrument_id: instrument_id.clone(),
        state: FeedState::Live,
        provider_generation: 1,
    })
    .expect("feed");
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos(),
    )
    .expect("timestamp");
    feed.push(MarketFeedEvent::Quote(TradingQuote {
        instrument_id: instrument_id.clone(),
        bid: aeris_trading::FixedPoint::try_new(10_000, 2).expect("bid"),
        ask: aeris_trading::FixedPoint::try_new(10_025, 2).expect("ask"),
        bid_size: None,
        ask_size: None,
        last: None,
        provider_id: "fixture".into(),
        provider_generation: 1,
        source_sequence: Some(1),
        exchange_unix_nanos: None,
        received_unix_nanos: now,
    }))
    .expect("quote");
    service.status().expect("fixture facts applied");
}

impl ChartOrderFixture {
    fn start(name: &str) -> Self {
        use aeris_instruments::{
            ContractMetadata, InstrumentDecimal, InstrumentId, InstrumentMetadataProvenance,
        };
        use aeris_trading_runtime::{
            TradingInstrument, TradingRetention, TradingService, TradingServiceConfig,
        };
        let directory = std::env::temp_dir().join(format!(
            "aeris-chart-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let service = TradingService::start(TradingServiceConfig {
            database_path: directory.join("trading.sqlite3"),
            retention: TradingRetention::default(),
            clock: std::sync::Arc::new(aeris_trading_runtime::clock::SystemClock::default()),
            execution: aeris_trading_runtime::ExecutionPolicy::default(),
        })
        .expect("trading service");
        let instrument_id = InstrumentId::try_new("instrument:fixture:BTC").expect("instrument");
        service
            .register_instrument(TradingInstrument {
                instrument_id: instrument_id.clone(),
                price_scale: 2,
                quantity_scale: 0,
                contract: ContractMetadata {
                    tick_size: Some(InstrumentDecimal::try_new(1, 2).expect("tick")),
                    price_grid: aeris_instruments::PriceGrid::Fixed(
                        InstrumentDecimal::try_new(1, 2).expect("tick"),
                    ),
                    instrument_class: aeris_instruments::InstrumentClass::CryptoSpot,
                    point_value: Some(InstrumentDecimal::try_new(1, 0).expect("point value")),
                    order_quantity_increment: None,
                    currency: "USD".to_string(),
                    expiry: None,
                    first_notice: None,
                    last_trade: None,
                    session_hours: Vec::new(),
                    provenance: InstrumentMetadataProvenance {
                        provider_id: "fixture".to_string(),
                        provider_symbol: "BTC".to_string(),
                        display_symbol: "BTC".to_string(),
                        session_generation: 1,
                    },
                },
            })
            .expect("register instrument");
        attach_chart_test_feed(&service, &instrument_id);
        let account = aeris_trading::TradingAccountId::try_new("aeris-sim-1").expect("account");
        service
            .register_account(simulated_chart_fixture(account.clone()))
            .expect("register account");
        Self {
            service,
            account,
            instrument_id,
            directory,
        }
    }

    /// A one-lot buy limit at 100.00.
    fn limit(&self, id: &str) -> aeris_trading_runtime::PlaceOrder {
        use aeris_trading::{
            ClientOrderId, FixedPoint, OrderSide, OrderType, TimeInForce, TradingProvenance,
        };
        aeris_trading_runtime::PlaceOrder {
            client_order_id: ClientOrderId::try_new(id).expect("client id"),
            account_id: self.account.clone(),
            instrument_id: self.instrument_id.clone(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::GoodTillCancelled,
            quantity: FixedPoint::try_new(1, 0).expect("quantity"),
            limit_price: Some(FixedPoint::try_new(10_000, 2).expect("price")),
            stop_price: None,
            submitted_unix_nanos: 1,
            provenance: TradingProvenance {
                venue_id: "aeris-sim".to_string(),
                provider_id: "fixture".to_string(),
                session_generation: 1,
                source_sequence: 1,
                observed_unix_nanos: 1,
            },
        }
    }

    fn chart_orders(&self) -> Vec<super::ChartWorkingOrder> {
        let snapshot = self.service.snapshot().expect("snapshot");
        super::chart_working_orders(&snapshot, &self.account, self.instrument_id.as_str(), None)
    }

    fn finish(self) {
        self.service
            .shutdown(std::time::Duration::from_secs(2))
            .expect("shutdown");
        let _ = std::fs::remove_dir_all(self.directory);
    }
}

#[test]
fn chart_receives_only_open_orders_so_cancelled_lines_leave_the_chart() {
    let fixture = ChartOrderFixture::start("open-orders");
    fixture
        .service
        .place_order(fixture.limit("working"))
        .expect("working order");
    fixture
        .service
        .place_order(fixture.limit("cancelled"))
        .expect("second order");
    fixture
        .service
        .cancel_order(aeris_trading::ClientOrderId::try_new("cancelled").expect("client id"))
        .expect("cancel");
    let orders = fixture.chart_orders();
    assert_eq!(
        orders
            .iter()
            .map(|order| order.id.as_str())
            .collect::<Vec<_>>(),
        ["working"]
    );
    assert!(orders.iter().all(|order| order.annotations.is_empty()));
    fixture.finish();
}

#[test]
fn a_pending_bracket_entry_carries_its_stop_and_target_on_the_chart() {
    use super::{ChartOrderId, ChartOrderRole, ChartOrderSide, ChartOrderStatus};
    let fixture = ChartOrderFixture::start("pending-bracket");
    fixture
        .service
        .place_inline_bracket(aeris_trading_runtime::PlaceInlineBracket {
            entry: fixture.limit("bracket-entry"),
            template: aeris_trading_runtime::BracketStrategyTemplate {
                template_id: "chart-bracket-entry".to_string(),
                revision: 1,
                name: "Chart bracket".to_string(),
                stop_offset_ticks: 50,
                targets: vec![aeris_trading_runtime::BracketTarget {
                    offset_ticks: 100,
                    quantity_percent: 100,
                }],
                trailing_stop: None,
                break_even: None,
                enabled: true,
            },
        })
        .expect("bracket");
    let legs = fixture
        .chart_orders()
        .iter()
        .filter(|order| {
            order.parent_order_id.as_ref().map(ChartOrderId::as_str) == Some("bracket-entry")
        })
        .map(|order| (order.role, order.side, order.status, order.price))
        .collect::<Vec<_>>();
    assert_eq!(
        legs,
        [
            (
                ChartOrderRole::StopLoss,
                ChartOrderSide::Sell,
                ChartOrderStatus::Working,
                99.5
            ),
            (
                ChartOrderRole::TakeProfit,
                ChartOrderSide::Sell,
                ChartOrderStatus::Working,
                101.0
            ),
        ],
        "a pending bracket entry carries its stop and target so the chart offers no TP/SL to add"
    );
    fixture.finish();
}

/// Runtime providers the desktop intentionally does not offer. Empty today; a new built-in
/// provider must be added to `TerminalProvider` or listed here, never left to a fallback.
const HIDDEN_RUNTIME_PROVIDERS: &[&str] = &[];

#[test]
fn every_built_in_provider_maps_to_a_desktop_provider_or_is_hidden() {
    for descriptor in aeris_market_runtime::built_in_provider_presentations() {
        assert!(
            super::known_terminal_provider(descriptor.id).is_some()
                || HIDDEN_RUNTIME_PROVIDERS.contains(&descriptor.id),
            "runtime provider {} has no desktop provider",
            descriptor.id
        );
    }
    for provider in super::TerminalProvider::ALL {
        assert!(
            super::provider_presentation(provider).is_some(),
            "{provider:?} has no runtime descriptor"
        );
    }
}

#[test]
fn watchlist_activity_covers_open_orders_and_positions_of_the_selected_account_only() {
    use aeris_trading::{
        ClientOrderId, FixedPoint, Order, OrderId, OrderSide, OrderStatus, OrderType, TimeInForce,
        TradingAccountId, TradingProvenance,
    };
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    let other = TradingAccountId::try_new("aeris-sim-2").expect("account");
    let point = |units, scale| FixedPoint::try_new(units, scale).expect("point");
    let instrument = |id: &str| aeris_instruments::InstrumentId::try_new(id).expect("id");
    let order = |id: &str, account: &TradingAccountId, instrument_id: &str, status| Order {
        id: OrderId::try_new(id).expect("order id"),
        client_order_id: ClientOrderId::try_new(id).expect("client id"),
        account_id: account.clone(),
        instrument_id: instrument(instrument_id),
        side: OrderSide::Buy,
        order_type: OrderType::Limit,
        time_in_force: TimeInForce::Day,
        quantity: point(1, 0),
        filled_quantity: point(0, 0),
        limit_price: Some(point(10_000, 2)),
        stop_price: None,
        status,
        submitted_unix_nanos: 1,
        revision: 1,
        accepted_unix_nanos: 1,
        updated_unix_nanos: 1,
        eligible_after_unix_nanos: 1,
        triggered_unix_nanos: None,
        expires_at_unix_nanos: None,
        terminal_unix_nanos: None,
        provenance: TradingProvenance {
            venue_id: "aeris-sim".to_string(),
            provider_id: "fixture".to_string(),
            session_generation: 1,
            source_sequence: 1,
            observed_unix_nanos: 1,
        },
    };
    let mut state = super::TradingPnlState {
        orders: vec![
            order(
                "working",
                &account,
                "instrument:fixture:A",
                OrderStatus::Working,
            ),
            order(
                "cancelled",
                &account,
                "instrument:fixture:B",
                OrderStatus::Cancelled,
            ),
            order(
                "elsewhere",
                &other,
                "instrument:fixture:C",
                OrderStatus::Working,
            ),
        ],
        positions: [("instrument:fixture:D", -2), ("instrument:fixture:E", 0)]
            .into_iter()
            .map(|(instrument_id, net)| aeris_trading_runtime::PositionPnl {
                position: aeris_trading::Position {
                    account_id: account.clone(),
                    instrument_id: instrument(instrument_id),
                    net_quantity: point(net, 0),
                    average_entry_price: Some(point(689_625, 2)),
                    realized_pnl: point(0, 2),
                    unrealized_pnl: point(0, 2),
                    last_fill_unix_nanos: 1,
                },
                realized_ticks: None,
                unrealized_ticks: None,
                point_value: None,
                currency_scale: 2,
            })
            .collect(),
        ..super::TradingPnlState::default()
    };
    assert!(state.instruments_with_activity().is_empty());

    state.order_entry.selected_account_id = Some(account);
    let mut active = state
        .instruments_with_activity()
        .into_iter()
        .collect::<Vec<_>>();
    active.sort_unstable();
    assert_eq!(active, ["instrument:fixture:A", "instrument:fixture:D"]);
}

#[test]
fn desktop_provider_labels_come_from_runtime_descriptors() {
    assert_eq!(
        super::terminal_provider_display(super::TerminalProvider::Rithmic),
        "Rithmic"
    );
    assert_eq!(
        super::terminal_provider_display(super::TerminalProvider::Hyperliquid),
        "Hyperliquid"
    );
    // Every provider id round-trips; an id the build does not expose maps to none.
    for provider in super::TerminalProvider::ALL {
        assert_eq!(
            super::known_terminal_provider(super::terminal_provider_id(provider)),
            Some(provider)
        );
    }
    assert_eq!(super::known_terminal_provider("binance"), None);
    for provider in [
        super::TerminalProvider::Rithmic,
        super::TerminalProvider::Hyperliquid,
    ] {
        assert!(super::provider_chart_types(provider).contains(&super::ChartType::Footprint));
    }
    assert!(
        !super::provider_intervals(super::TerminalProvider::Rithmic)
            .contains(&ChartInterval::Tick100)
    );
    assert!(
        super::provider_intervals(super::TerminalProvider::Hyperliquid)
            .contains(&ChartInterval::Day3)
    );
    let product = InstallProviderInstrument {
        provider_symbol: "MNQ".to_string(),
        display_symbol: "MNQZ6".to_string(),
        ..Default::default()
    };
    assert_eq!(
        super::provider_ready_message(super::TerminalProvider::Rithmic, &product),
        "MNQ · Rithmic"
    );
    assert_eq!(
        super::provider_ready_message(super::TerminalProvider::Hyperliquid, &product),
        "MNQ · Hyperliquid"
    );
}

#[test]
fn switching_a_seconds_chart_to_a_minute_bar_provider_opens_it_at_one_minute() {
    let instrument = |provider: super::TerminalProvider| InstallProviderInstrument {
        provider: super::terminal_provider_id(provider).to_string(),
        ..Default::default()
    };
    let hyperliquid = instrument(super::TerminalProvider::Hyperliquid);
    let rithmic = instrument(super::TerminalProvider::Rithmic);
    assert_eq!(
        super::instrument_interval(&hyperliquid, ChartInterval::Second30),
        ChartInterval::Minute1
    );
    assert_eq!(
        super::instrument_interval(&hyperliquid, ChartInterval::Second15),
        ChartInterval::Minute1
    );
    assert_eq!(
        super::instrument_interval(&hyperliquid, ChartInterval::Hour4),
        ChartInterval::Hour4,
        "an offered interval is kept"
    );
    assert_eq!(
        super::instrument_interval(&rithmic, ChartInterval::Second30),
        ChartInterval::Second30,
        "a provider with second bars keeps the seconds chart"
    );
}
