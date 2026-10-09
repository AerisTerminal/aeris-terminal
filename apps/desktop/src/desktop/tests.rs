#![cfg(test)]

use super::{
    CHART_CONTEXT_MENU_ROW_HEIGHT, CHART_CONTEXT_MENU_WIDTH, CHROME_MENU_LIST_HEIGHT,
    CHROME_MENU_MAX_HEIGHT, CHROME_MENU_SEARCH_HEIGHT, CHROME_MENU_WIDTH, CaptionPlatform,
    CaptionPointerOwner, ChartNoticePlacement, ChartNoticeTone, ChartShortcut, ChartState,
    ChromeOverlayPhase, ConsumerResourceClass, HeaderControls, InputEvent, InstrumentMenuEntry,
    InstrumentMenuSelection, MarketSummaryEntry, OVERLAY_EDGE_MARGIN, PRICE_AXIS_MENU_GAP,
    PriceAxisMenuFlyout, PriceAxisMenuRow, ProviderCatalogCommand, ProviderConnectionPresentation,
    RithmicSwitchState, SidePanel, SidePanelVisibility, SymbolInputAction, SymbolSelectionTarget,
    SymbolSubmitDecision, TIMEFRAME_FLYOUT_GAP, TIMEFRAME_FLYOUT_WIDTH, TIMEFRAME_MENU_WIDTH,
    TerminalProvider, TimeSalesFilter, TimeSalesSideFilter, TimeframeMenuGroup, WORKSPACE_TAB_GAP,
    WORKSPACE_TAB_STRIP_PADDING_LEFT, WatchlistDragState, WindowCommand, WindowMoveGestureEvent,
    WindowMoveGestureTransition, WorkspaceDragState, WorkspaceMaximizeTransition,
    active_workspace_after_close, aeris_chart_theme, bounded_status_detail,
    caption_keyboard_activates, caption_pointer_owner, catalog_rejection_message,
    chart_position_id, chart_shortcut, chart_status_detail, chart_surface_notice,
    chart_time_seconds_from_unix_nanos, chrome_menu_extent, chrome_overlay_progress,
    chrome_typeahead_char_from, claim_once, clamp_anchored_menu_left,
    clamp_chart_context_menu_origin, clamp_price_axis_menu_origin, clamped_side_panel_width,
    connection_presentation, connectivity_chart_state, current_instrument_menu_index,
    default_rithmic_contract_index, durable_workspace_viewport, fullscreen_escape_command,
    gpui_color, instrument_listing_refresh_needed, instrument_row_highlighted,
    instrument_selector_label, instrument_target_after_close, overlay_height,
    price_axis_flyout_rows, price_axis_root_rows, publication_chart_state,
    ready_state_can_complete_switch, reconciled_bridge_state, reorder_workspace_ids,
    series_selector_label, should_autoload_rithmic_catalog, should_finish_chrome_overlay_close,
    stabilized_connection_state, stable_connection_message, stopped_worker_chart_detail,
    switch_requires_chart_cover, symbol_input_action, symbol_submit_decision,
    timeframe_flyout_height, timeframe_flyout_offset, timeframe_flyout_row_is_active,
    timeframe_group_intervals, timeframe_interval_group, timeframe_menu_groups,
    timeframe_menu_row_label, timeframe_overlay_extent, timeframe_overlay_left,
    watchlist_drag_destination, watchlist_drag_translation, watchlist_step,
    window_move_gesture_transition, workspace_drag_destination, workspace_drag_translation,
    workspace_label, workspace_maximize_transition, workspace_series, workspace_split_ratio,
    workspace_switch, wrapped_workspace_index,
};
#[cfg(feature = "diagnostics")]
use super::{FOREGROUND_INTERACTION_SAMPLE_CAPACITY, ForegroundInteractionDiagnostics};
use aeris_chart_integration::{AerisChartTheme, ChartSplitDirection, PriceAxisMenuState};
use aeris_contracts::{
    InstallProviderInstrument, ProviderCatalogRejectionReason, ProviderInstrumentSummary,
    SeriesCadence, WorkspaceLayoutState, WorkspacePaneState, WorkspaceSplitAxis, WorkspaceState,
};

pub(super) fn test_trading_service() -> aeris_trading_runtime::TradingService {
    aeris_trading_runtime::TradingService::start(aeris_trading_runtime::TradingServiceConfig {
        database_path: std::path::PathBuf::from(":memory:"),
        retention: aeris_trading_runtime::TradingRetention::default(),
    })
    .expect("test trading service starts")
}
use aeris_design_system::{ThemeColor, ThemeMode};
use aeris_market_data::{ChartInterval, MarketBar};
use aeris_observability::FeedConnectionState;
use gpui::{Bounds, point, px, size};

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
fn selected_account_lock_is_derived_from_the_runtime_meter() {
    let account = aeris_trading::TradingAccountId::try_new("aeris-sim-2").expect("account");
    let mut state = super::TradingPnlState::default();
    state.order_entry.selected_account_id = Some(account.clone());
    state.risk_locks.push(aeris_trading_runtime::RiskLock {
        account_id: account,
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
fn instrument_menu_requests_a_default_listing_only_when_idle_and_empty() {
    use crate::desktop::rithmic_shell::RithmicSymbolBrowser;
    use std::num::NonZeroUsize;

    let startup = NonZeroUsize::MIN;
    let mut browser = RithmicSymbolBrowser::rithmic_catalog_awaiting_search(startup, "");
    assert!(
        !instrument_listing_refresh_needed(&browser, false),
        "startup search is already pending"
    );

    let result = ProviderInstrumentSummary {
        symbol: "BTC-USD".to_string(),
        display_symbol: "BTC-USD".to_string(),
        exchange: "rithmic".to_string(),
        name: Some("BTC/USD".to_string()),
        product_code: Some("BTC-USD".to_string()),
        instrument_type: Some("spot".to_string()),
        expiration_date: None,
        asset_class: None,
    };
    assert!(browser.apply_results(startup, vec![result]));
    assert!(
        !instrument_listing_refresh_needed(&browser, false),
        "populated listing needs no refresh"
    );

    let selection = browser.select(0).expect("catalog result is selectable");
    assert!(
        !instrument_listing_refresh_needed(&browser, true),
        "in-flight selection defers the refresh"
    );
    assert!(browser.confirm_selection(selection.generation));
    assert!(browser.consume_completed_search(selection.search_generation));
    assert!(
        instrument_listing_refresh_needed(&browser, false),
        "consumed selection authorization reopens as a fresh default listing"
    );
    let refreshed = browser
        .begin_search("")
        .expect("runtime provider catalog permits a fresh empty listing query");
    assert_eq!(refreshed.query, "");
    assert!(browser.search_pending());
    assert!(browser.reject_search(refreshed.request_id));

    browser
        .retain_latest_search("ETH")
        .expect("typed query validates");
    assert!(
        !instrument_listing_refresh_needed(&browser, false),
        "a retained typed query outranks the default listing"
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
fn timeframe_menu_uses_dual_group_and_interval_containers() {
    assert_eq!(
        timeframe_menu_groups(&ChartInterval::ALL),
        [
            TimeframeMenuGroup::Ticks,
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
    use super::super::*;
    use aeris_desktop::market_worker::{
        EngineWorkerStartup, MarketWorkerCommand, MarketWorkerSender, market_worker_channel,
    };
    use gpui::{TestAppContext, VisualTestContext};
    use std::num::NonZeroUsize;
    use std::sync::{atomic::AtomicU64, mpsc};

    struct TimeframeHarness(Entity<WorkspaceSurface>);

    impl Render for TimeframeHarness {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let surface = self.0.read(cx);
            let input = surface.timeframe_input.clone();
            let quick = surface.chrome_overlay == Some(ChromeOverlay::QuickTimeframe);
            let practice_name = surface
                .trading_pnl
                .account_creator
                .as_ref()
                .map(|creator| creator.name.clone());
            let big_trades_dialog = surface.big_trades_dialog.as_ref().map(|dialog| {
                div()
                    .size_full()
                    .debug_selector(|| "big_trades_dialog".into())
                    .child(big_trades_dialog_layer(
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
                        surface.on_terminal_key_down(event, window, cx)
                    }) {
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }))
                .when(quick, |root| root.child(Input::new(&input)))
                .when_some(practice_name, |root, input| root.child(Input::new(&input)))
                .children(big_trades_dialog)
        }
    }

    fn harness(
        cx: &mut TestAppContext,
    ) -> (
        Entity<WorkspaceSurface>,
        mpsc::Receiver<MarketWorkerCommand>,
        MarketWorkerSender,
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
        MarketWorkerSender,
        &mut VisualTestContext,
    ) {
        let (commands, requests) = mpsc::sync_channel(8);
        let (publications, messages) = market_worker_channel(NonZeroUsize::MIN);
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

    fn ctrader_product() -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: "ctrader".to_string(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: "ctrader:demo:1001:1".to_string(),
            provider_symbol: "EURUSD".to_string(),
            display_symbol: "EURUSD".to_string(),
            venue_id: "ctrader".to_string(),
            price_scale: 5,
            quantity_scale: 2,
            entitlement_id: "ctrader-authorized".to_string(),
            ..Default::default()
        }
    }

    #[gpui::test]
    fn restored_ctrader_chart_loads_without_trade_print_features(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness_with(cx, ctrader_product());
        cx.read(|cx| {
            let surface = surface.read(cx);
            assert_eq!(surface.provider, TerminalProvider::Ctrader);
            assert_eq!(surface.chart_state, ChartState::Loading);
            assert_eq!(
                surface.connection_message.as_deref(),
                Some("Connecting to cTrader hosted markets")
            );
            assert!(
                !super::super::provider_chart_types(surface.provider)
                    .contains(&ChartType::Footprint)
            );
        });
        // A footprint request (palette, menu or template) is refused, not drawn blank.
        surface.update(cx, |surface, cx| {
            surface.set_chart_type(ChartType::Footprint, cx);
            assert_ne!(surface.chart_chrome.chart_type, ChartType::Footprint);
            assert!(
                surface
                    .indicator_message
                    .as_deref()
                    .is_some_and(|message| message.contains("cTrader does not publish"))
            );
        });
    }

    #[gpui::test]
    fn restored_chart_from_an_unavailable_provider_fails_explicitly(cx: &mut TestAppContext) {
        let product = InstallProviderInstrument {
            provider: "retired".to_string(),
            ..ctrader_product()
        };
        let (surface, _requests, _publications, cx) = harness_with(cx, product);
        cx.read(|cx| {
            let surface = surface.read(cx);
            // Never silently presented as another provider's market.
            assert_eq!(surface.chart_state, ChartState::Error);
            assert!(
                surface
                    .chart_state_message
                    .contains("retired charts are not available")
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
                surface.read(cx).rithmic_pending_interval,
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
                .send(MarketWorkerMessage::EngineSwitchMarker {
                    sequence: selections[0].sequence,
                })
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
    fn practice_account_name_input_does_not_open_symbol_typeahead(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness(cx);
        cx.update(|window, cx| {
            let name = cx.new(|input_cx| InputState::new(window, input_cx));
            let equity = cx.new(|input_cx| InputState::new(window, input_cx));
            name.update(cx, |input, input_cx| input.focus(window, input_cx));
            surface.update(cx, |surface, surface_cx| {
                surface.trading_pnl.account_creator =
                    Some(PracticeAccountDialogState { name, equity });
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
                .expect("practice dialog remains open");
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
                .big_trades_dialog
                .as_ref()
                .expect("big trades dialog is open")
                .minimum_volume
                .clone()
        })
    }

    #[gpui::test]
    fn big_trades_settings_open_only_while_the_indicator_is_on_the_chart(cx: &mut TestAppContext) {
        let (surface, _requests, _publications, cx) = harness(cx);
        install_chart(&surface, None, cx);
        cx.update(|window, cx| {
            surface.update(cx, |surface, cx| {
                surface.open_big_trades_dialog(window, cx);
                assert!(surface.big_trades_dialog.is_none());
                assert!(
                    surface.indicator_message.is_some(),
                    "a settings request for a removed indicator is reported"
                );
                assert!(
                    surface
                        .add_order_flow_study(workspace_surface::OrderFlowMenuStudy::BigTrades, cx)
                );
                surface.open_big_trades_dialog(window, cx);
                assert!(surface.big_trades_dialog.is_some());
                assert!(surface.indicator_message.is_none());
            });
        });
        assert_eq!(
            chart_big_trades(&surface, cx),
            Some(BigTradesSettings::default()),
            "the indicator menu adds the default indicator"
        );
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
            let dialog = surface.read(cx).big_trades_dialog.as_ref().expect("dialog");
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
                .big_trades_dialog
                .as_ref()
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
            assert!(surface.big_trades_dialog.is_none());
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
            let dialog = surface.read(cx).big_trades_dialog.as_ref().expect("dialog");
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
            let dialog = surface.read(cx).big_trades_dialog.as_ref().expect("dialog");
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
fn chart_keys_step_the_watchlist_and_toggle_fullscreen_like_tradingview() {
    let shift = gpui::Modifiers {
        shift: true,
        ..gpui::Modifiers::default()
    };
    let control = gpui::Modifiers {
        control: true,
        ..gpui::Modifiers::default()
    };
    assert_eq!(
        chart_shortcut("space", gpui::Modifiers::default()),
        Some(ChartShortcut::NextWatchlistSymbol)
    );
    assert_eq!(
        chart_shortcut("space", shift),
        Some(ChartShortcut::PreviousWatchlistSymbol)
    );
    assert_eq!(
        chart_shortcut("f", shift),
        Some(ChartShortcut::ToggleChartFullscreen)
    );
    assert_eq!(
        chart_shortcut("f", gpui::Modifiers::default()),
        None,
        "a plain F still starts symbol search"
    );
    assert_eq!(chart_shortcut("space", control), None);
    assert_eq!(
        chart_shortcut(
            "f",
            gpui::Modifiers {
                control: true,
                shift: true,
                ..gpui::Modifiers::default()
            }
        ),
        None,
        "Ctrl+Shift+F stays the flatten command"
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
    assert_eq!(workspace_label(0), "Workspace 1");
    assert_eq!(workspace_label(7), "Workspace 8");
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
    let first_series = first.series.as_mut().expect("default series");
    first_series.instrument_id = first_instrument.instrument_id.clone();
    first_series.cadence = SeriesCadence::FixedSeconds as i32;
    first_series.cadence_value = 300;

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
    let second_instrument_id = second_instrument.instrument_id.clone();
    let second_series = second.series.as_mut().expect("second series");
    second_series.instrument_id = second_instrument_id;
    second_series.cadence = SeriesCadence::FixedSeconds as i32;
    second_series.cadence_value = 3_600;
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
fn shell_theme_maps_only_to_aeris_charts_theme_selection() {
    assert_eq!(aeris_chart_theme(ThemeMode::Light), AerisChartTheme::Light);
    assert_eq!(aeris_chart_theme(ThemeMode::Dark), AerisChartTheme::Dark);
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
        symbol_submit_decision(TerminalProvider::Rithmic, 3, 1),
        SymbolSubmitDecision::Select(1)
    );
    assert_eq!(
        symbol_submit_decision(TerminalProvider::Rithmic, 0, 0),
        SymbolSubmitDecision::Search
    );
    assert_eq!(
        symbol_submit_decision(TerminalProvider::Rithmic, 1, 2),
        SymbolSubmitDecision::Search
    );
    assert_eq!(
        symbol_submit_decision(TerminalProvider::Rithmic, 2, 0),
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

#[test]
fn instrument_menu_index_follows_the_checked_live_market() {
    let entries = [
        InstrumentMenuEntry {
            label: "BTC/USD".into(),
            asset_class: None,
            checked: false,
            selection: InstrumentMenuSelection(0),
        },
        InstrumentMenuEntry {
            label: "ETH/USD".into(),
            asset_class: None,
            checked: true,
            selection: InstrumentMenuSelection(1),
        },
    ];
    assert_eq!(current_instrument_menu_index(&entries), Some(1));
    assert_eq!(
        current_instrument_menu_index(&[InstrumentMenuEntry {
            label: "AAVE/USD".into(),
            asset_class: None,
            checked: false,
            selection: InstrumentMenuSelection(0),
        }]),
        None
    );
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
fn persisted_calendar_series_keep_week_and_month_identity() {
    let instrument = InstallProviderInstrument {
        provider: "rithmic".to_string(),
        instrument_id: "instrument:rithmic:btc:usd".to_string(),
        entitlement_id: super::provider_presentation(super::TerminalProvider::Rithmic)
            .expect("Rithmic descriptor")
            .selection_entitlement_id
            .to_string(),
        ..InstallProviderInstrument::default()
    };
    let week = workspace_series(ChartInterval::Week1, &instrument);
    let month = workspace_series(ChartInterval::Month1, &instrument);
    assert_eq!(week.cadence, SeriesCadence::CalendarWeeks as i32);
    assert_eq!(month.cadence, SeriesCadence::CalendarMonths as i32);
    assert_eq!(week.cadence_value, 1);
    assert_eq!(month.cadence_value, 1);
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
fn default_rithmic_contract_skips_continuous_and_spread_symbols() {
    let result = |symbol: &str, expiration: &str| ProviderInstrumentSummary {
        symbol: symbol.to_string(),
        display_symbol: symbol.to_string(),
        exchange: "CME-Delayed".to_string(),
        name: None,
        product_code: Some("MNQ".to_string()),
        instrument_type: Some("FUTURE".to_string()),
        expiration_date: Some(expiration.to_string()),
        asset_class: None,
    };
    let results = vec![
        result("MNQ", "20260918"),
        result("MNQU6-MNQZ6", "20260918"),
        result("MNQZ6", "20261218"),
        result("MNQU6", "20260918"),
        ProviderInstrumentSummary {
            symbol: "NQ".to_string(),
            display_symbol: "NQ".to_string(),
            exchange: "CME-Delayed".to_string(),
            name: None,
            product_code: Some("NQ".to_string()),
            instrument_type: Some("FUTURE".to_string()),
            expiration_date: None,
            asset_class: None,
        },
    ];
    assert_eq!(default_rithmic_contract_index(&results), Some(3));
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
        ChartNoticePlacement::BottomRight
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
    assert!(should_autoload_rithmic_catalog(
        FeedConnectionState::Authenticating,
        "Rithmic Test session is ready for instrument search",
        false,
    ));
    assert!(!should_autoload_rithmic_catalog(
        FeedConnectionState::Authenticating,
        "Rithmic Test session is ready for instrument search",
        true,
    ));
    assert!(!should_autoload_rithmic_catalog(
        FeedConnectionState::Recovering,
        "Rithmic realtime is recovering; retained history remains visible",
        false,
    ));
}

#[test]
fn first_rithmic_connection_is_not_presented_as_recovery_and_autoloads_catalog() {
    let shell = crate::desktop::rithmic_shell::RithmicShellState::local()
        .expect("fixed Rithmic Test profile");
    let discovering =
        stabilized_connection_state(Some(shell.connection()), FeedConnectionState::Discovering);
    assert_eq!(discovering, FeedConnectionState::Discovering);
    let ready = stabilized_connection_state(Some(discovering), FeedConnectionState::Authenticating);
    assert_eq!(ready, FeedConnectionState::Authenticating);
    assert!(should_autoload_rithmic_catalog(
        ready,
        crate::desktop::engine_market_worker::RITHMIC_CATALOG_READY_MESSAGE,
        false,
    ));
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
    assert_eq!(repairing.placement, ChartNoticePlacement::BottomRight);
}

#[test]
fn engine_switch_keeps_partial_replacement_covered_until_handoff_is_current() {
    assert!(!switch_requires_chart_cover(
        false,
        RithmicSwitchState::Pending
    ));
    assert!(!switch_requires_chart_cover(true, RithmicSwitchState::Idle));
    assert!(switch_requires_chart_cover(
        true,
        RithmicSwitchState::Pending
    ));
    assert!(switch_requires_chart_cover(
        true,
        RithmicSwitchState::Swapping
    ));
    assert!(switch_requires_chart_cover(
        true,
        RithmicSwitchState::Initializing
    ));
}

#[test]
fn old_ready_cannot_cancel_a_pending_symbol_or_timeframe_switch() {
    assert!(ready_state_can_complete_switch(RithmicSwitchState::Idle));
    assert!(!ready_state_can_complete_switch(
        RithmicSwitchState::Pending
    ));
    assert!(!ready_state_can_complete_switch(
        RithmicSwitchState::Swapping
    ));
    assert!(ready_state_can_complete_switch(
        RithmicSwitchState::Initializing
    ));
}

#[test]
fn worker_stop_preserves_a_concrete_startup_error() {
    assert_eq!(
        stopped_worker_chart_detail(
            ChartState::Error,
            "provider instrument selection is stale",
            "Hyperliquid market worker stopped",
        ),
        "provider instrument selection is stale"
    );
    assert_eq!(
        stopped_worker_chart_detail(
            ChartState::Loading,
            "Loading Hyperliquid history",
            "Hyperliquid market worker stopped",
        ),
        "Hyperliquid market worker stopped"
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
        instrument_id: "tastytrade:Future:/ESZ6".into(),
        phase: MarketSessionPhase::Regular,
        source: MarketSessionSource::ProviderCalendar,
        session_start_unix_nanos: Some(0),
        session_end_unix_nanos: Some(134 * MINUTE),
        next_open_unix_nanos: Some(1_440 * MINUTE),
    };
    let present = |state, status: &MarketSessionStatus| {
        connection_presentation(TerminalProvider::Tastytrade, state, None).with_market(
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
        stable_connection_message(
            FeedConnectionState::Recovering,
            "Rithmic realtime disconnected".to_string(),
        ),
        "Reconnecting market data"
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
    assert!(controls.enabled(HeaderControls::MARKET_PANELS));
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
        SidePanelVisibility(0b111)
    );
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
    assert_eq!(super::market_summary_change(13_100_000_000, 8), "+131.00");
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
            Some(&(Some(previous.clone()), ChartInterval::Minute1)),
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
    assert_eq!(recovery.placement, ChartNoticePlacement::BottomRight);
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
fn contextual_labels_keep_contract_and_pending_series_truthful() {
    assert_eq!(instrument_selector_label(None, false), "Contract");
    assert_eq!(
        instrument_selector_label(Some(("MNQU6", "CME")), false),
        "MNQU6 / CME"
    );
    assert_eq!(
        instrument_selector_label(Some(("MNQU6", "CME")), true),
        "MNQU6 / CME"
    );
    assert_eq!(series_selector_label(ChartInterval::Minute1), "1m");
    assert_eq!(series_selector_label(ChartInterval::Minute5), "5m");
    assert_eq!(series_selector_label(ChartInterval::Hour4), "4h");
}

#[test]
fn provider_catalog_keeps_display_symbol_separate_from_wire_symbol() {
    let instrument = ProviderInstrumentSummary {
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
            "waiting for chart",
            Some("Rithmic Test agreements require attention"),
        ),
        "Rithmic Test agreements require attention"
    );
    assert_eq!(
        chart_status_detail(
            ChartState::Recovering,
            FeedConnectionState::Streaming,
            "history is covering a gap",
            Some("feed is streaming"),
        ),
        "history is covering a gap"
    );
    assert_eq!(
        chart_status_detail(
            ChartState::Error,
            FeedConnectionState::Discovering,
            "Provider startup catalog refresh was rejected",
            Some("Connecting to market data"),
        ),
        "Provider startup catalog refresh was rejected",
        "a chart error is not hidden behind connection progress"
    );
    assert_eq!(
        chart_status_detail(
            ChartState::Error,
            FeedConnectionState::Stopped,
            "Loading tastytrade history",
            Some("tastytrade market worker stopped"),
        ),
        "tastytrade market worker stopped"
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
    use super::workspace_surface::{persisted_chart_appearance, restored_chart_appearance};
    use aeris_chart_integration::{
        AerisChartTheme, ChartAppearanceColor, ChartAppearanceSettings, ChartThemeColors,
    };

    let appearance = ChartAppearanceSettings {
        up_color: ChartAppearanceColor::Custom("#abcdef".to_string()),
        ..ChartAppearanceSettings::default()
    };
    let persisted = persisted_chart_appearance(&appearance);
    assert_eq!(persisted.grid_color, "theme");
    assert_eq!(persisted.up_color, "#abcdef");
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
    let restored = restored_chart_appearance(&legacy).expect("legacy appearance");
    assert_eq!(restored.down_color, ChartAppearanceColor::Theme);
    assert_eq!(restored.grid_color, ChartAppearanceColor::Theme);
    assert_eq!(
        restored.up_color,
        ChartAppearanceColor::Custom("#abcdef".to_string())
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
                OrderType::Limit => (Some(price), None),
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

#[test]
fn chart_receives_only_open_orders_so_cancelled_lines_leave_the_chart() {
    use aeris_instruments::{
        ContractMetadata, InstrumentDecimal, InstrumentId, InstrumentMetadataProvenance,
    };
    use aeris_trading::{
        ClientOrderId, FixedPoint, OrderSide, OrderType, TimeInForce, TradingAccountId,
        TradingProvenance,
    };
    use aeris_trading_runtime::{
        PlaceOrder, TradingInstrument, TradingRetention, TradingService, TradingServiceConfig,
    };
    let directory = std::env::temp_dir().join(format!(
        "aeris-chart-open-orders-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let service = TradingService::start(TradingServiceConfig {
        database_path: directory.join("trading.sqlite3"),
        retention: TradingRetention::default(),
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
    let account = TradingAccountId::try_new("aeris-sim-1").expect("account");
    service
        .register_account(simulated_chart_fixture(account.clone()))
        .expect("register account");
    let limit = |id: &str| PlaceOrder {
        client_order_id: ClientOrderId::try_new(id).expect("client id"),
        account_id: account.clone(),
        instrument_id: instrument_id.clone(),
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
    };
    service
        .place_order(limit("working"))
        .expect("working order");
    service
        .place_order(limit("cancelled"))
        .expect("second order");
    service
        .cancel_order(ClientOrderId::try_new("cancelled").expect("client id"))
        .expect("cancel");
    let snapshot = service.snapshot().expect("snapshot");
    let orders = super::chart_working_orders(&snapshot, &account, instrument_id.as_str(), None);
    assert_eq!(
        orders
            .iter()
            .map(|order| order.id.as_str())
            .collect::<Vec<_>>(),
        ["working"]
    );
    assert!(orders.iter().all(|order| order.annotations.is_empty()));
    service
        .shutdown(std::time::Duration::from_secs(2))
        .expect("shutdown");
    let _ = std::fs::remove_dir_all(directory);
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
fn broker_exposure_nets_hedged_positions_and_knows_a_single_entry() {
    let account = aeris_trading::TradingAccountId::try_new("ctrader-demo-1001").expect("id");
    let point = |units, scale| aeris_trading::FixedPoint::try_new(units, scale).expect("point");
    let position = |id: &str, side, quantity| aeris_trading::BrokerPosition {
        account_id: account.clone(),
        instrument_id: aeris_instruments::InstrumentId::try_new("ctrader:demo:1001:1")
            .expect("instrument"),
        broker_position_id: id.into(),
        side,
        quantity: point(quantity, 2),
        entry_price: point(108_250, 5),
        stop_loss: None,
        take_profit: None,
        swap: point(0, 2),
        commission: point(0, 2),
        gross_unrealized: point(0, 2),
        net_unrealized: point(0, 2),
        opened_unix_nanos: 1,
    };
    let single = [position("77", aeris_trading::OrderSide::Buy, 100_000)];
    assert_eq!(
        super::broker_exposure(&single, &account, "ctrader:demo:1001:1"),
        Some(super::BrokerExposure {
            net_units: 100_000,
            scale: 2,
            entry_price: Some(point(108_250, 5)),
        })
    );
    let hedged = [
        position("77", aeris_trading::OrderSide::Buy, 100_000),
        position("78", aeris_trading::OrderSide::Sell, 30_000),
    ];
    let exposure = super::broker_exposure(&hedged, &account, "ctrader:demo:1001:1").expect("net");
    assert_eq!((exposure.net_units, exposure.entry_price), (70_000, None));
    let flat = [
        position("77", aeris_trading::OrderSide::Buy, 100_000),
        position("78", aeris_trading::OrderSide::Sell, 100_000),
    ];
    assert_eq!(
        super::broker_exposure(&flat, &account, "ctrader:demo:1001:1"),
        None
    );
    assert_eq!(
        super::broker_exposure(&single, &account, "ctrader:demo:1001:2"),
        None
    );
}

#[test]
fn desktop_provider_labels_and_default_queries_come_from_runtime_descriptors() {
    assert_eq!(
        super::terminal_provider_display(super::TerminalProvider::Rithmic),
        "Rithmic"
    );
    assert_eq!(
        super::terminal_provider_display(super::TerminalProvider::Hyperliquid),
        "Hyperliquid"
    );
    assert_eq!(
        super::terminal_provider_display(super::TerminalProvider::Tastytrade),
        "tastytrade"
    );
    assert_eq!(
        super::default_listing_query(super::TerminalProvider::Rithmic),
        "MNQ"
    );
    assert_eq!(
        super::default_listing_query(super::TerminalProvider::Hyperliquid),
        ""
    );
    assert_eq!(
        super::default_listing_query(super::TerminalProvider::Tastytrade),
        "/ES"
    );
    assert_eq!(
        super::provider_connection_message(super::TerminalProvider::Tastytrade),
        "Connecting to tastytrade hosted markets"
    );
    assert_eq!(
        super::provider_connection_message(super::TerminalProvider::Hyperliquid),
        "Connecting to Hyperliquid public markets"
    );
    assert_eq!(
        super::terminal_provider_display(super::TerminalProvider::Ctrader),
        "cTrader"
    );
    assert_eq!(
        super::provider_connection_message(super::TerminalProvider::Ctrader),
        "Connecting to cTrader hosted markets"
    );
    // Every provider id round-trips; an id the build does not expose maps to none.
    for provider in super::TerminalProvider::ALL {
        assert_eq!(
            super::known_terminal_provider(super::terminal_provider_id(provider)),
            Some(provider)
        );
    }
    assert_eq!(super::known_terminal_provider("binance"), None);
    // Only providers that stream trade prints offer a footprint.
    assert!(!super::provider_trades_available(
        super::TerminalProvider::Ctrader
    ));
    for provider in [
        super::TerminalProvider::Rithmic,
        super::TerminalProvider::Hyperliquid,
        super::TerminalProvider::Tastytrade,
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
    assert!(
        super::provider_intervals(super::TerminalProvider::Tastytrade)
            .contains(&ChartInterval::Month1)
    );
    let product = InstallProviderInstrument {
        provider_symbol: "MNQ".to_string(),
        display_symbol: "MNQZ6".to_string(),
        ..Default::default()
    };
    assert_eq!(
        super::provider_ready_message(super::TerminalProvider::Rithmic, &product),
        "MNQ · Rithmic spot"
    );
    assert_eq!(
        super::provider_ready_message(super::TerminalProvider::Hyperliquid, &product),
        "MNQ · Hyperliquid"
    );
    assert_eq!(
        super::provider_ready_message(super::TerminalProvider::Tastytrade, &product),
        "MNQZ6 · tastytrade"
    );
}
