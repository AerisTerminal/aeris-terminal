use super::*;
use aeris_desktop::command_registry::{self, CommandId};
use gpui::Focusable;

fn active_header_state(
    workspace: &WorkspaceSurface,
    theme: &AerisTheme,
    chart_has_market_data: bool,
    cx: &App,
) -> HeaderState {
    HeaderState {
        theme: *theme,
        provider: workspace.provider,
        instrument_label: terminal_instrument_label(workspace),
        series_label: series_selector_label(workspace.selected_interval()),
        chart_type: workspace.chart_type(cx),
        chart_type_label: workspace.chart_type(cx).label().to_string(),
        instruments: workspace.instrument_entries(cx),
        symbol_input: workspace.symbol_input.clone(),
        indicator_input: workspace.indicator_input.clone(),
        indicator_message: workspace.indicator_message.clone(),
        series_message: workspace.series_message.clone(),
        pending: HeaderPendingState {
            symbol_selection: workspace.market_state.symbol_selection_pending,
            series: workspace.rithmic_switch.in_progress(),
        },
        drawing_history: workspace.drawing_history_state(cx),
        controls: HeaderControls::from_state(
            workspace.symbol_input.is_some() || !workspace.symbol_browser.results().is_empty(),
            workspace.has_market_selection(),
        )
        .with_chart_controls(chart_has_market_data),
        order_book_visible: workspace.side_panels.contains(SidePanel::OrderBook),
        watchlist_visible: workspace.side_panels.contains(SidePanel::Watchlist),
        context_visible: workspace.context_panel_visible,
        chart_link_group: workspace.chart_link_group,
        connection_state: workspace
            .connection_state
            .unwrap_or(FeedConnectionState::Disconnected),
        transport_rtt_nanos: workspace.provider_transport_rtt_nanos,
        instrument_scroll: workspace.scrolls.instrument.clone(),
    }
}

fn due_economic_event_triggers(
    snapshot: &ContextSnapshot,
    maximum_lead_seconds: u32,
    observed_unix_nanos: i64,
    prior_dispatches: &BTreeMap<String, i64>,
    redispatch_interval_nanos: i64,
    event_grace_nanos: i64,
    limit: usize,
) -> Vec<(String, aeris_trading_runtime::EconomicEventRiskTrigger)> {
    let lead_nanos = i64::from(maximum_lead_seconds).saturating_mul(1_000_000_000);
    snapshot
        .economic_events
        .iter()
        .take(128)
        .filter_map(|event| {
            let scheduled = event.scheduled_unix_seconds.checked_mul(1_000_000_000)?;
            if observed_unix_nanos < scheduled.saturating_sub(lead_nanos)
                || observed_unix_nanos > scheduled.saturating_add(event_grace_nanos)
            {
                return None;
            }
            let key = format!("{}:{scheduled}", event.id);
            if prior_dispatches.get(&key).is_some_and(|last| {
                observed_unix_nanos.saturating_sub(*last) < redispatch_interval_nanos
            }) {
                return None;
            }
            let importance = match event.importance {
                aeris_context_runtime::EventImportance::Low => {
                    aeris_trading_runtime::EconomicEventRiskImportance::Low
                }
                aeris_context_runtime::EventImportance::Medium => {
                    aeris_trading_runtime::EconomicEventRiskImportance::Medium
                }
                aeris_context_runtime::EventImportance::High => {
                    aeris_trading_runtime::EconomicEventRiskImportance::High
                }
            };
            let source_release_unix_nanos = event
                .provenance
                .release_unix_seconds
                .checked_mul(1_000_000_000)?;
            Some((
                key,
                aeris_trading_runtime::EconomicEventRiskTrigger {
                    event_id: event.id.clone(),
                    title: event.title.clone(),
                    source: event.provenance.source.label().to_string(),
                    importance,
                    scheduled_unix_nanos: scheduled,
                    source_release_unix_nanos,
                    observed_unix_nanos,
                },
            ))
        })
        .take(limit)
        .collect()
}

impl TerminalApp {
    fn open_command_palette(
        &mut self,
        _: &OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.command_palette_input.update(cx, |input, input_cx| {
            input.set_value("", window, input_cx);
            input.focus_handle(input_cx).focus(window, input_cx);
        });
        self.command_palette_open = true;
        self.command_palette_selection = 0;
        self.command_palette_message = None;
        cx.notify();
    }

    pub(super) fn close_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_palette_open = false;
        self.command_palette_message = None;
        self.chrome_focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn execute_registered_command(
        &mut self,
        command: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let surface = self.active_surface();
        match command {
            CommandId::OpenPalette => self.open_command_palette(&OpenCommandPalette, window, cx),
            CommandId::ToggleContext => surface.update(cx, WorkspaceSurface::toggle_context_panel),
            CommandId::ToggleOrderBook => surface.update(cx, WorkspaceSurface::toggle_order_book),
            CommandId::ToggleWatchlist => surface.update(cx, WorkspaceSurface::toggle_watchlist),
            CommandId::ChartCandles
            | CommandId::ChartBars
            | CommandId::ChartLine
            | CommandId::ChartArea
            | CommandId::ChartBaseline
            | CommandId::ChartFootprint => {
                let chart_type = match command {
                    CommandId::ChartCandles => ChartType::Candles,
                    CommandId::ChartBars => ChartType::Bars,
                    CommandId::ChartLine => ChartType::Line,
                    CommandId::ChartArea => ChartType::Area,
                    CommandId::ChartBaseline => ChartType::Baseline,
                    CommandId::ChartFootprint => ChartType::Footprint,
                    _ => unreachable!(),
                };
                surface.update(cx, |surface, surface_cx| {
                    surface.set_chart_type(chart_type, surface_cx);
                });
            }
            CommandId::Interval1Minute
            | CommandId::Interval5Minutes
            | CommandId::Interval15Minutes
            | CommandId::Interval1Hour
            | CommandId::Interval1Day => {
                let interval = match command {
                    CommandId::Interval1Minute => ChartInterval::Minute1,
                    CommandId::Interval5Minutes => ChartInterval::Minute5,
                    CommandId::Interval15Minutes => ChartInterval::Minute15,
                    CommandId::Interval1Hour => ChartInterval::Hour1,
                    CommandId::Interval1Day => ChartInterval::Day1,
                    _ => unreachable!(),
                };
                surface.update(cx, |surface, surface_cx| {
                    surface.select_interval(interval, surface_cx);
                });
            }
            CommandId::NewWorkspace => self.new_workspace(&NewWorkspace, window, cx),
            CommandId::SplitHorizontal => {
                self.split_pane_horizontal(&SplitPaneHorizontal, window, cx);
            }
            CommandId::SplitVertical => self.split_pane_vertical(&SplitPaneVertical, window, cx),
            CommandId::BuyMarket => {
                self.chrome_focus.focus(window, cx);
                self.trading_buy_market(&TradingBuyMarket, window, cx);
            }
            CommandId::SellMarket => {
                self.chrome_focus.focus(window, cx);
                self.trading_sell_market(&TradingSellMarket, window, cx);
            }
            CommandId::CancelAll => {
                self.chrome_focus.focus(window, cx);
                self.trading_cancel_all(&TradingCancelAll, window, cx);
            }
            CommandId::FlattenAccount => {
                self.chrome_focus.focus(window, cx);
                self.trading_flatten_account(&TradingFlattenAccount, window, cx);
            }
            CommandId::KillSwitch => {
                self.chrome_focus.focus(window, cx);
                self.trading_kill_switch(&TradingKillSwitch, window, cx);
            }
        }
    }

    pub(super) fn execute_command_palette_query(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let query = self.command_palette_input.read(cx).value().to_string();
        if let Some(mnemonic) = command_registry::parse_mnemonic(&query) {
            if let Some(chart) = mnemonic.chart {
                self.execute_registered_command(chart, window, cx);
            }
            if let Some(interval) = mnemonic.interval {
                self.execute_registered_command(interval, window, cx);
            }
            if let Some(symbol) = mnemonic.symbol {
                let dispatched = self.active_surface().update(cx, |surface, surface_cx| {
                    surface.search_mnemonic_symbol(symbol, window, surface_cx)
                });
                if !dispatched {
                    self.command_palette_message =
                        Some("Symbol search could not start".to_string());
                    cx.notify();
                    return;
                }
            }
            self.close_command_palette(window, cx);
            return;
        }
        let results = command_registry::search(&query, 8);
        let Some(command) = results
            .get(self.command_palette_selection)
            .map(|spec| spec.id)
        else {
            self.command_palette_message = Some("No matching command".to_string());
            cx.notify();
            return;
        };
        self.execute_registered_command(command, window, cx);
        self.close_command_palette(window, cx);
    }

    fn dispatch_due_economic_event_rules(&mut self, cx: &mut Context<Self>) {
        const CHECK_INTERVAL: Duration = Duration::from_secs(5);
        const REDISPATCH_INTERVAL_NANOS: i64 = 15_000_000_000;
        const EVENT_GRACE_NANOS: i64 = 15 * 60 * 1_000_000_000;
        const MAXIMUM_EVENTS_PER_CHECK: usize = 16;
        let now_instant = Instant::now();
        if now_instant < self.next_event_risk_check {
            return;
        }
        self.next_event_risk_check = now_instant + CHECK_INTERVAL;
        let surface = self.active_surface();
        let (snapshot, maximum_lead_seconds, observation) = {
            let surface = surface.read(cx);
            let maximum_lead_seconds = surface
                .trading_pnl
                .risk_profiles
                .iter()
                .filter(|profile| profile.enabled)
                .filter_map(|profile| profile.economic_event_rule)
                .map(|rule| rule.lead_seconds)
                .max()
                .unwrap_or(0);
            let observation = surface
                .order_book
                .read(cx)
                .frame()
                .and_then(aeris_desktop::trading::prepare_flatten)
                .map(|(_, observation)| observation);
            (
                Arc::clone(&surface.context_snapshot),
                maximum_lead_seconds,
                observation,
            )
        };
        if maximum_lead_seconds == 0 {
            return;
        }
        let observed_unix_nanos = aeris_desktop::trading::now();
        self.event_risk_dispatches.retain(|_, scheduled| {
            scheduled
                .checked_add(EVENT_GRACE_NANOS)
                .is_some_and(|expires| expires >= observed_unix_nanos)
        });
        let due = due_economic_event_triggers(
            &snapshot,
            maximum_lead_seconds,
            observed_unix_nanos,
            &self.event_risk_dispatches,
            REDISPATCH_INTERVAL_NANOS,
            EVENT_GRACE_NANOS,
            MAXIMUM_EVENTS_PER_CHECK,
        );
        let Some(service) = aeris_desktop::trading::handle() else {
            return;
        };
        for (key, _) in &due {
            self.event_risk_dispatches
                .insert(key.clone(), observed_unix_nanos);
        }
        if due.is_empty() {
            return;
        }
        let dispatch = cx.background_executor().spawn(async move {
            for (_, event) in due {
                service.apply_economic_event_risk(event, observation.clone())?;
            }
            Ok::<_, String>(())
        });
        cx.spawn(async move |_, cx| {
            let result = dispatch.await;
            surface.update(cx, |surface, surface_cx| {
                surface.economic_event_risk_message = result
                    .err()
                    .map(|error| format!("Economic-event risk action unavailable: {error}"));
                surface_cx.notify();
            });
        })
        .detach();
    }

    fn absorb_render_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.absorb_pane_activate_requests(cx);
        self.absorb_watchlist_requests(cx);
        self.absorb_chart_context_menu_requests(cx);
        self.absorb_study_settings_requests(window, cx);
        self.absorb_study_remove_requests(cx);
    }

    fn prepare_terminal_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_market_wake_listener(window, cx);
        self.schedule_market_frame(window, cx);
        self.reconcile_active_drags(cx);
        self.track_window_activation(window, cx);
        self.absorb_render_requests(window, cx);
        self.synchronize_linked_charts(cx);
        self.dispatch_due_economic_event_rules(cx);
        self.sync_market_summaries(cx);
    }

    fn rendered_command_palette(&self, terminal: &Entity<Self>, cx: &App) -> Option<AnyElement> {
        self.command_palette_open.then(|| {
            command_palette_layer(
                terminal,
                &self.command_palette_input,
                self.command_palette_selection,
                self.command_palette_message.clone(),
                &self.theme,
                cx,
            )
        })
    }

    fn rendered_title_bar(
        &self,
        terminal: &Entity<Self>,
        window: &Window,
        fullscreen: bool,
        cx: &App,
    ) -> Option<impl IntoElement + use<>> {
        workspace_title_bar_visible(fullscreen).then(|| {
            workspace_title_bar(
                terminal,
                &WorkspaceTabBarState {
                    workspaces: &self.workspaces,
                    market_summaries: &self.market_summaries,
                    active: self.active,
                    enabled: self.workspace_factory.is_some(),
                    error: self.workspace_error.as_deref(),
                    workspace_drag: self.workspace_drag,
                    theme: self.theme,
                },
                window,
                cx,
            )
        })
    }

    fn rendered_about_dialog(&self, terminal: &Entity<Self>) -> Option<AnyElement> {
        self.about_dialog_open
            .then(|| about_dialog_layer(terminal, self.update_presentation(), &self.theme))
    }

    fn rendered_header(
        &self,
        terminal: &Entity<Self>,
        surface: &Entity<WorkspaceSurface>,
        cx: &App,
    ) -> impl IntoElement + use<> {
        let workspace = surface.read(cx);
        let has_data = workspace
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).has_market_data());
        terminal_header(
            terminal,
            surface,
            active_header_state(workspace, &self.theme, has_data, cx),
        )
    }
}

impl Render for TerminalApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare_terminal_frame(window, cx);
        let terminal = cx.entity();
        let pane_count = self.workspaces[self.active].panes.len();
        let active = self.active_surface();
        let workspace = active.read(cx);
        let chart_has_market_data = workspace
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).has_market_data());
        let fullscreen = window.is_fullscreen();
        let overlay = chrome_overlay_layer(
            workspace,
            &active,
            &self.theme,
            chart_chrome::CHART_CHROME_HEIGHT
                + if fullscreen {
                    0.0
                } else {
                    WORKSPACE_TITLE_BAR_HEIGHT
                },
            window.viewport_size(),
            cx,
        );
        let (context_menu, settings_menu) = self.chart_surface_menus(
            &terminal,
            pane_count,
            chart_has_market_data,
            window.viewport_size(),
            cx,
        );
        let account_menu = self.account_menu_overlay(&terminal, window.viewport_size());
        let about_dialog = self.rendered_about_dialog(&terminal);
        let command_palette = self.rendered_command_palette(&terminal, cx);
        let title_bar = self.rendered_title_bar(&terminal, window, fullscreen, cx);
        let header = self.rendered_header(&terminal, &active, cx);
        let watchlist = self.watchlist_panel_state(cx);
        let market = workspace_market_area(
            &terminal,
            &self.workspaces[self.active],
            &active,
            self.drawing_toolbar.is_collapsed(),
            watchlist,
            &self.theme,
            cx,
        );
        let fullscreen_focus = self.chrome_focus.clone();
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .track_focus(&self.chrome_focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|terminal, _, window, cx| {
                    terminal.handle_window_move_gesture(WindowMoveGestureEvent::Cancel, window);
                    terminal.end_workspace_drag(cx);
                    terminal.end_watchlist_drag(cx);
                }),
            )
            .on_action(|_: &MinimizeWindow, window, _| window.minimize_window())
            .on_action(|_: &ZoomWindow, window, _| {
                WindowCommand::MaximizeOrRestore.execute(window);
            })
            .map(|root| workspace_action_handlers(root, cx))
            .on_action(move |_: &ToggleFullscreen, window, cx| {
                window.toggle_fullscreen();
                fullscreen_focus.focus(window, cx);
            })
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(Self::trading_buy_market))
            .on_action(cx.listener(Self::trading_sell_market))
            .on_action(cx.listener(Self::trading_cancel_all))
            .on_action(cx.listener(Self::trading_flatten_account))
            .on_action(cx.listener(Self::trading_kill_switch))
            .on_action(cx.listener(Self::open_command_palette))
            .bg(gpui_color(self.theme.colors.surface))
            .text_color(gpui_color(self.theme.colors.text_primary))
            .font_family(aeris_design_system::platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .children(title_bar)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .bg(gpui_color(self.theme.colors.surface))
                    .child(market),
            )
            .children(overlay)
            .children(context_menu)
            .children(settings_menu)
            .children(account_menu)
            .children(about_dialog)
            .children(command_palette)
    }
}

impl TerminalApp {
    fn trading_hotkeys_enabled(&self, window: &Window) -> bool {
        self.chrome_focus.is_focused(window)
    }

    fn trading_order_frame(&self, cx: &App) -> Option<aeris_market_data::OrderBookFrame> {
        self.active_surface()
            .read(cx)
            .order_book
            .read(cx)
            .frame()
            .cloned()
    }

    fn trading_order_entry(
        &self,
        cx: &App,
    ) -> (
        Option<String>,
        u64,
        aeris_trading::OrderType,
        aeris_trading::TimeInForce,
    ) {
        let entry = &self.active_surface().read(cx).trading_pnl.order_entry;
        (
            entry
                .selected_account_id
                .as_ref()
                .map(|id| id.as_str().to_string()),
            entry.quantity,
            entry.order_type,
            entry.time_in_force,
        )
    }

    fn trading_order_entry_locked(&self, cx: &App) -> bool {
        super::selected_account_lock_reason(&self.active_surface().read(cx).trading_pnl).is_some()
    }

    fn trading_buy_market(
        &mut self,
        _: &TradingBuyMarket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.trading_hotkeys_enabled(window) || self.trading_order_entry_locked(cx) {
            return;
        }
        let Some(frame) = self.trading_order_frame(cx) else {
            return;
        };
        let (account_id, quantity, order_type, time_in_force) = self.trading_order_entry(cx);
        aeris_desktop::trading::dispatch_simulated_order(
            &frame,
            aeris_trading::OrderSide::Buy,
            account_id,
            quantity,
            order_type,
            time_in_force,
            cx,
        );
    }

    fn trading_sell_market(
        &mut self,
        _: &TradingSellMarket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.trading_hotkeys_enabled(window) || self.trading_order_entry_locked(cx) {
            return;
        }
        let Some(frame) = self.trading_order_frame(cx) else {
            return;
        };
        let (account_id, quantity, order_type, time_in_force) = self.trading_order_entry(cx);
        aeris_desktop::trading::dispatch_simulated_order(
            &frame,
            aeris_trading::OrderSide::Sell,
            account_id,
            quantity,
            order_type,
            time_in_force,
            cx,
        );
    }

    fn trading_cancel_all(
        &mut self,
        _: &TradingCancelAll,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.trading_hotkeys_enabled(window) {
            return;
        }
        aeris_desktop::trading::cancel_simulated_accounts(cx);
    }

    fn trading_flatten_account(
        &mut self,
        _: &TradingFlattenAccount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.trading_hotkeys_enabled(window)
            && let Some(frame) = self.trading_order_frame(cx)
        {
            aeris_desktop::trading::flatten_simulated_account_for(
                &frame,
                self.trading_order_entry(cx).0,
                cx,
            );
        }
    }

    fn trading_kill_switch(
        &mut self,
        _: &TradingKillSwitch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.trading_hotkeys_enabled(window) {
            return;
        }
        aeris_desktop::trading::kill_simulated_accounts(cx);
    }
}

fn workspace_action_handlers(root: Div, cx: &mut Context<TerminalApp>) -> Div {
    root.on_action(cx.listener(TerminalApp::new_workspace))
        .on_action(cx.listener(TerminalApp::select_next_workspace))
        .on_action(cx.listener(TerminalApp::select_previous_workspace))
        .on_action(cx.listener(TerminalApp::move_workspace_left))
        .on_action(cx.listener(TerminalApp::move_workspace_right))
        .on_action(cx.listener(TerminalApp::close_active_workspace))
        .on_action(cx.listener(TerminalApp::split_pane_horizontal))
        .on_action(cx.listener(TerminalApp::split_pane_vertical))
        .on_action(cx.listener(TerminalApp::close_active_pane))
}

#[derive(Clone)]
struct WorkspaceTabDrag {
    tab_id: u64,
}

impl Render for WorkspaceTabDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

#[derive(Clone)]
pub(super) struct WorkspaceSplitDrag {
    pub(super) workspace_id: u64,
    pub(super) left_pane_id: u64,
    pub(super) right_pane_id: u64,
    pub(super) direction: ChartSplitDirection,
}

impl Render for WorkspaceSplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

#[derive(Clone, Copy)]
struct WorkspaceTabRenderState {
    index: usize,
    active: usize,
    workspace_count: usize,
    drag_enabled: bool,
    drag_translation: Option<f32>,
    theme: AerisTheme,
}

const fn workspace_tab_close_drag_enabled(workspace_count: usize) -> bool {
    workspace_count > 1
}

fn workspace_tab_close_button(
    terminal: Entity<TerminalApp>,
    tab_id: u64,
    label: &str,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let key_terminal = terminal.clone();
    div()
        .id(("close_workspace", tab_id))
        .occlude()
        .size(px(WORKSPACE_TAB_ICON_HIT))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .text_color(gpui_color(colors.icon))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(format!("Close {label}"))
        .tab_index(0)
        .hover(move |close| {
            close
                .bg(gpui_color(colors.danger))
                .text_color(gpui_color(colors.danger_foreground))
        })
        .focus_visible(move |close| close.border_2().border_color(gpui_color(colors.ring)))
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            terminal.update(cx, |terminal, cx| {
                terminal.close_workspace(tab_id, window, cx);
            });
            cx.stop_propagation();
        })
        .on_key_down(move |event, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                key_terminal.update(cx, |terminal, cx| {
                    terminal.close_workspace(tab_id, window, cx);
                });
                cx.stop_propagation();
            }
        })
        .child(header_icon(HugeIcon::Close).with_size(px(WORKSPACE_TAB_ICON_GLYPH)))
}

fn handle_workspace_tab_key(
    terminal: &Entity<TerminalApp>,
    tab_id: u64,
    index: usize,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut App,
) {
    let key = event.keystroke.key.as_str();
    if !matches!(
        key,
        "left" | "right" | "home" | "end" | "enter" | "space" | "delete"
    ) {
        return;
    }
    terminal.update(cx, |terminal, cx| match key {
        "left" => terminal.select_relative_workspace(index, -1, window, cx),
        "right" => terminal.select_relative_workspace(index, 1, window, cx),
        "home" => terminal.select_and_focus_workspace(0, window, cx),
        "end" => {
            let last = terminal.workspaces.len().saturating_sub(1);
            terminal.select_and_focus_workspace(last, window, cx);
        }
        "enter" | "space" => terminal.select_workspace_id(tab_id, cx),
        "delete" => terminal.close_workspace(tab_id, window, cx),
        _ => {}
    });
    cx.stop_propagation();
}

fn workspace_add_button(
    terminal: Entity<TerminalApp>,
    enabled: bool,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let key_terminal = terminal.clone();
    div()
        .id("add_workspace")
        .occlude()
        .size(px(WORKSPACE_TAB_ICON_HIT))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .text_color(gpui_color(if enabled {
            colors.icon
        } else {
            colors.text_muted
        }))
        .role(Role::Button)
        .aria_label("Create workspace")
        .tab_index(0)
        .tab_stop(enabled)
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .hover(move |button| {
                    button.bg(gpui_color(colors.hover_bg.over(colors.surface_secondary)))
                })
                .focus_visible(move |button| {
                    button.border_2().border_color(gpui_color(colors.ring))
                })
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    terminal.update(cx, |terminal, cx| terminal.add_workspace(window, cx));
                    cx.stop_propagation();
                })
                .on_key_down(move |event, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        key_terminal.update(cx, |terminal, cx| terminal.add_workspace(window, cx));
                        cx.stop_propagation();
                    }
                })
        })
        .child(header_icon(HugeIcon::Add).with_size(px(WORKSPACE_TAB_ICON_GLYPH)))
}

fn workspace_tab_content(
    workspace: &WorkspaceTab,
    market_summaries: &BTreeMap<MarketSummaryKey, MarketSummaryEntry>,
    theme: &AerisTheme,
    cx: &App,
) -> (String, String, Div, Option<String>) {
    let surface = workspace.panes[workspace.active_pane].surface.read(cx);
    let label = terminal_instrument_label(surface);
    let summary = (!surface.showing_superseded_series())
        .then(|| {
            surface.product.as_ref().and_then(|instrument| {
                market_summaries.get(&MarketSummaryKey::from_instrument(instrument))
            })
        })
        .flatten();
    let values = summary.map_or_else(MarketSummaryValues::default, MarketSummaryEntry::values);
    let change_label = workspace_tab_change_label(values);
    let aria_label = workspace_tab_aria_label(&label, change_label.as_deref());
    let change_color = values
        .change
        .map_or(theme.colors.text_muted, |value| match value.cmp(&0) {
            std::cmp::Ordering::Less => theme.colors.text_negative,
            std::cmp::Ordering::Greater => theme.colors.text_positive,
            std::cmp::Ordering::Equal => theme.colors.text_secondary,
        });
    let exchange = match surface.provider {
        TerminalProvider::Rithmic => assets::ExchangeLogo::Rithmic,
        TerminalProvider::Hyperliquid => assets::ExchangeLogo::Hyperliquid,
    };
    let content = div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_1()
        .child(exchange_mark(exchange, px(16.0), false, &theme.colors))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(label.clone()),
        )
        .children(change_label.clone().map(|change| {
            div()
                .flex_none()
                .text_xs()
                .whitespace_nowrap()
                .font_features(platform_tabular_numerals())
                .text_color(gpui_color(change_color))
                .child(change)
        }));
    (label, aria_label, content, change_label)
}

fn workspace_tab_width(
    label: &str,
    change: Option<&str>,
    show_close: bool,
    window: &Window,
) -> f32 {
    let mut font = gpui::font(aeris_design_system::platform_font_family());
    font.weight = platform_font_weight(TypographyRole::Normal);
    let measure = |text: &str, size: f32, font: gpui::Font| {
        f32::from(
            window
                .text_system()
                .layout_line(
                    text,
                    px(size),
                    &[gpui::TextRun {
                        len: text.len(),
                        font,
                        ..Default::default()
                    }],
                    None,
                )
                .width,
        )
    };
    let rem = f32::from(window.rem_size());
    let gap = rem * 0.25;
    let mut width = rem * 0.75 + 16.0 + gap + measure(label, rem * 0.875, font.clone()) + gap;
    if let Some(change) = change {
        font.features = platform_tabular_numerals();
        width += gap + measure(change, rem * 0.75, font);
    }
    if show_close {
        width += gap + WORKSPACE_TAB_ICON_HIT;
    }
    width.ceil().min(WORKSPACE_TAB_MAX_WIDTH)
}

fn workspace_tab_widths(
    visuals: &[(String, String, Div, Option<String>)],
    show_close: bool,
    window: &Window,
) -> Vec<f32> {
    visuals
        .iter()
        .map(|(label, _, _, change)| {
            workspace_tab_width(label, change.as_deref(), show_close, window)
        })
        .collect()
}

fn workspace_tab_change_label(values: MarketSummaryValues) -> Option<String> {
    values.change_percent.map(|value| format!("{value:+.2}%"))
}

fn workspace_tab_aria_label(label: &str, change: Option<&str>) -> String {
    change.map_or_else(
        || label.to_string(),
        |change| format!("{label}, change {change}"),
    )
}

fn workspace_tab(
    terminal: &Entity<TerminalApp>,
    workspace: &WorkspaceTab,
    state: &WorkspaceTabRenderState,
    visual: (String, String, Div, Option<String>),
    width: f32,
) -> AnyElement {
    let (label, aria_label, content, _) = visual;
    let index = state.index;
    let drag_enabled = state.drag_enabled;
    let theme = state.theme;
    let tab_id = workspace.id;
    let selected = index == state.active;
    let select_terminal = terminal.clone();
    let key_terminal = terminal.clone();
    let middle_click_terminal = terminal.clone();
    let drag_terminal = terminal.clone();
    let drag = WorkspaceTabDrag { tab_id };
    let tab_focus = workspace.focus.clone();
    let mouse_focus = workspace.focus.clone();
    Tab::new(("workspace_tab", tab_id), &theme)
        .selected(selected)
        .w(px(width))
        .h(px(chart_chrome::CHART_CONTROL_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .pl_3()
        .pr_1()
        .text_sm()
        .track_focus(&tab_focus)
        .aria_label(aria_label)
        .aria_position_in_set(index + 1)
        .aria_size_of_set(state.workspace_count)
        .when_some(state.drag_translation, |tab, translation| {
            tab.relative().left(px(translation)).shadow_md()
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            mouse_focus.focus(window, cx);
            select_terminal.update(cx, |terminal, cx| {
                terminal.select_workspace_id(tab_id, cx);
            });
        })
        .on_aux_click(move |event, window, cx| {
            if event.is_middle_click() {
                middle_click_terminal.update(cx, |terminal, cx| {
                    terminal.close_workspace(tab_id, window, cx);
                });
                cx.stop_propagation();
            }
        })
        .on_key_down(move |event, window, cx| {
            handle_workspace_tab_key(&key_terminal, tab_id, index, event, window, cx);
        })
        .when(drag_enabled, |tab| {
            tab.on_drag(drag, move |drag, cursor_offset, _, cx| {
                drag_terminal.update(cx, |terminal, cx| {
                    terminal.begin_workspace_drag(
                        drag.tab_id,
                        f32::from(cursor_offset.x),
                        width,
                        cx,
                    );
                });
                cx.new(|_| drag.clone())
            })
        })
        .child(content)
        .children(
            workspace_tab_close_drag_enabled(state.workspace_count)
                .then(|| workspace_tab_close_button(terminal.clone(), tab_id, &label, &theme)),
        )
        .into_any_element()
}

pub(super) fn workspace_tab_strip(
    terminal: &Entity<TerminalApp>,
    state: &WorkspaceTabBarState<'_>,
    window: &Window,
    cx: &App,
) -> impl IntoElement + use<> {
    let workspaces = state.workspaces;
    let enabled = state.enabled;
    let theme = state.theme;
    let colors = theme.colors;
    let close_drag_enabled = workspace_tab_close_drag_enabled(workspaces.len());
    let visuals: Vec<_> = workspaces
        .iter()
        .map(|workspace| workspace_tab_content(workspace, state.market_summaries, &theme, cx))
        .collect();
    let tab_widths = workspace_tab_widths(&visuals, close_drag_enabled, window);
    let drag_widths = tab_widths.clone();
    let tabs = workspaces
        .iter()
        .zip(visuals)
        .enumerate()
        .map(|(index, (workspace, visual))| {
            workspace_tab(
                terminal,
                workspace,
                &WorkspaceTabRenderState {
                    index,
                    active: state.active,
                    workspace_count: workspaces.len(),
                    drag_enabled: enabled && close_drag_enabled,
                    drag_translation: close_drag_enabled
                        .then(|| {
                            workspace_drag_translation(
                                state.workspace_drag,
                                workspace.id,
                                index,
                                &tab_widths,
                            )
                        })
                        .flatten(),
                    theme,
                },
                visual,
                tab_widths[index],
            )
        });
    let add_terminal = terminal.clone();
    let move_terminal = terminal.clone();
    let end_terminal = terminal.clone();
    let add_enabled = enabled && workspaces.len() < MAXIMUM_OPEN_WORKSPACES;
    div()
        .id("workspace_tab_list")
        .h_full()
        .min_w_0()
        .flex_none()
        .flex()
        .items_center()
        .gap_0p5()
        .pl_2()
        .overflow_x_hidden()
        .role(Role::TabList)
        .aria_label("Workspaces")
        .aria_orientation(Orientation::Horizontal)
        .tab_group()
        .on_drag_move::<WorkspaceTabDrag>(move |event, _, cx| {
            let tab_id = event.drag(cx).tab_id;
            move_terminal.update(cx, |terminal, cx| {
                terminal.move_workspace_drag(
                    tab_id,
                    f32::from(event.event.position.x),
                    f32::from(event.bounds.left()),
                    &drag_widths,
                    cx,
                );
            });
        })
        .on_drop(move |_: &WorkspaceTabDrag, _, cx| {
            end_terminal.update(cx, TerminalApp::end_workspace_drag);
        })
        .children(enabled.then_some(tabs).into_iter().flatten())
        .children(enabled.then(|| {
            chrome_tooltip(
                "add_workspace",
                "Create workspace",
                workspace_add_button(add_terminal, add_enabled, &theme),
                &theme,
            )
        }))
        .children(enabled.then(|| {
            div()
                .id("workspace_tab_drop_target")
                .h(px(chart_chrome::CHART_CONTROL_SIZE))
                .min_w(px(16.0))
        }))
        .children(state.error.map(|error| {
            chrome_tooltip(
                "workspace_creation_error",
                error.to_string(),
                div()
                    .size(px(7.0))
                    .rounded_full()
                    .bg(gpui_color(colors.danger)),
                &theme,
            )
        }))
}

pub(super) fn terminal_root(
    bootstrap: MarketWorkerStartup,
    market_worker: MarketDataWorker,
    workspace_factory: Option<engine_market_worker::WorkspaceMarketFactory>,
    lifecycle: &DesktopLifecycle,
    chart_chrome: chart_chrome::ChartChromePreferences,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalApp> {
    let surface = workspace_surface_entity(
        bootstrap,
        market_worker,
        lifecycle,
        chart_chrome,
        WorkspaceSurfaceRestore::default(),
        window,
        cx,
    );
    terminal_shell_root(
        TerminalShellInit {
            workspaces: vec![WorkspaceTab {
                id: 1,
                label: workspace_label(0),
                panes: vec![WorkspacePane {
                    id: 1,
                    consumer_id: 1,
                    surface,
                    focus: cx.focus_handle(),
                }],
                active_pane: 0,
                layout: AerisChartWorkspace::new(1, MAXIMUM_PANES_PER_WORKSPACE),
                generation: 1,
                focus: cx.focus_handle(),
            }],
            active_workspace_id: Some(1),
            workspace_revision: 0,
            layout_generation: 1,
            workspace_factory,
            workspace_shell: WorkspaceShellKind::Window,
            chart_chrome,
            chart_settings_templates: Vec::new(),
            watchlist_entries: Vec::new(),
        },
        lifecycle,
        window,
        cx,
    )
}

pub(super) struct TerminalShellInit {
    pub(super) workspaces: Vec<WorkspaceTab>,
    pub(super) active_workspace_id: Option<u64>,
    pub(super) workspace_revision: u64,
    pub(super) layout_generation: u64,
    pub(super) workspace_factory: Option<engine_market_worker::WorkspaceMarketFactory>,
    pub(super) workspace_shell: WorkspaceShellKind,
    pub(super) chart_chrome: chart_chrome::ChartChromePreferences,
    pub(super) chart_settings_templates: Vec<WorkspaceChartSettingsTemplateState>,
    pub(super) watchlist_entries: Vec<WorkspaceWatchlistEntryState>,
}

fn terminal_shell_root(
    init: TerminalShellInit,
    lifecycle: &DesktopLifecycle,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalApp> {
    let terminal_lifecycle = lifecycle.clone();
    let command_palette_input = cx.new(|input_cx| {
        InputState::new(window, input_cx).placeholder("Search commands or type ES footprint 5m")
    });
    let terminal_input = command_palette_input.clone();
    let terminal = cx.new(move |cx| TerminalApp::new(init, terminal_lifecycle, terminal_input, cx));
    let palette_terminal = terminal.clone();
    window
        .subscribe(
            &command_palette_input,
            cx,
            move |_, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    palette_terminal.update(cx, |terminal, terminal_cx| {
                        terminal.command_palette_selection = 0;
                        terminal.command_palette_message = None;
                        terminal_cx.notify();
                    });
                }
            },
        )
        .detach();
    let closing_terminal = terminal.clone();
    window.on_window_should_close(cx, move |_, cx| {
        closing_terminal.update(cx, |terminal, cx| {
            terminal.claim_close(cx);
        });
        true
    });
    let focus_terminal = terminal.clone();
    window.on_next_frame(move |window, cx| {
        focus_terminal.update(cx, |terminal, cx| {
            terminal.chrome_focus.focus(window, cx);
        });
    });
    terminal
}

pub(super) struct RestoredWorkspaceShellPlan<'a> {
    pub(super) active_workspace_id: u64,
    pub(super) workspace_revision: u64,
    pub(super) layout_generation: u64,
    pub(super) workspace_tabs: &'a [WorkspaceTabState],
}

pub(super) fn restored_workspace_shell_plan(
    restored: &WorkspaceState,
) -> RestoredWorkspaceShellPlan<'_> {
    RestoredWorkspaceShellPlan {
        active_workspace_id: restored.active_workspace_id,
        workspace_revision: restored.workspace_revision,
        layout_generation: restored.layout_generation,
        workspace_tabs: &restored.workspace_tabs,
    }
}

pub(super) fn workspace_tabs_root(
    mut market_panes: Vec<engine_market_worker::WorkspaceMarketPane>,
    restored: &WorkspaceState,
    workspace_factory: engine_market_worker::WorkspaceMarketFactory,
    lifecycle: &DesktopLifecycle,
    chart_chrome: chart_chrome::ChartChromePreferences,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalApp> {
    let plan = restored_workspace_shell_plan(restored);
    let mut workspaces = Vec::with_capacity(plan.workspace_tabs.len());
    for tab in plan.workspace_tabs {
        let mut panes = Vec::with_capacity(tab.panes.len());
        for persisted in &tab.panes {
            let Some(index) = market_panes.iter().position(|pane| {
                pane.workspace_id == tab.workspace_id && pane.pane_id == persisted.pane_id
            }) else {
                continue;
            };
            let pane = market_panes.remove(index);
            panes.push(WorkspacePane {
                id: pane.pane_id,
                consumer_id: pane.consumer_id,
                surface: workspace_surface_entity(
                    pane.startup,
                    pane.worker,
                    lifecycle,
                    chart_chrome,
                    WorkspaceSurfaceRestore {
                        chart: persisted.chart.clone(),
                        side_panel: Some((
                            persisted.side_panel_visibility,
                            persisted.side_panel_width,
                            persisted.side_panel_split_basis_points,
                        )),
                    },
                    window,
                    cx,
                ),
                focus: cx.focus_handle(),
            });
        }
        if panes.is_empty() {
            continue;
        }
        let Some(layout) = tab.layout.as_ref().and_then(chart_workspace_layout) else {
            continue;
        };
        let pane_order = layout.pane_ids();
        panes.sort_by_key(|pane| {
            pane_order
                .iter()
                .position(|pane_id| *pane_id == pane.id)
                .unwrap_or(usize::MAX)
        });
        let active_pane = panes
            .iter()
            .position(|pane| pane.id == tab.active_pane_id)
            .unwrap_or(0);
        let Ok(layout) = AerisChartWorkspace::restore(&layout, MAXIMUM_PANES_PER_WORKSPACE) else {
            continue;
        };
        workspaces.push(WorkspaceTab {
            id: tab.workspace_id,
            label: tab.label.clone(),
            panes,
            active_pane,
            layout,
            generation: tab.generation,
            focus: cx.focus_handle(),
        });
    }
    terminal_shell_root(
        TerminalShellInit {
            workspaces,
            active_workspace_id: Some(plan.active_workspace_id),
            workspace_revision: plan.workspace_revision,
            layout_generation: plan.layout_generation,
            workspace_factory: Some(workspace_factory),
            workspace_shell: WorkspaceShellKind::Tabs,
            chart_chrome,
            chart_settings_templates: restored.chart_settings_templates.clone(),
            watchlist_entries: restored.watchlist_entries.clone(),
        },
        lifecycle,
        window,
        cx,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        MarketSummaryValues, workspace_tab_aria_label, workspace_tab_change_label,
        workspace_tab_close_drag_enabled,
    };

    #[test]
    fn workspace_tab_close_and_drag_require_multiple_tabs() {
        assert!(!workspace_tab_close_drag_enabled(0));
        assert!(!workspace_tab_close_drag_enabled(1));
        assert!(workspace_tab_close_drag_enabled(2));
        assert!(workspace_tab_close_drag_enabled(3));
    }

    #[test]
    fn workspace_tab_stat_is_percentage_change_without_last_price() {
        let change = workspace_tab_change_label(MarketSummaryValues {
            last: Some(123_456),
            change: Some(600),
            change_percent: Some(6.0),
        });
        assert_eq!(change.as_deref(), Some("+6.00%"));
        assert_eq!(
            workspace_tab_aria_label("BTC-USD", change.as_deref()),
            "BTC-USD, change +6.00%"
        );
    }
}
