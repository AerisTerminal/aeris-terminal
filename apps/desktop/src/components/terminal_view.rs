use super::trading_hotkeys::TradingHotkey;
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
        instruments: workspace.instrument_entries(cx),
        symbol_input: workspace.symbol_input.clone(),
        indicator_input: workspace.indicator_input.clone(),
        time_zone_id: workspace.chart_time_zone_id().to_string(),
        time_zone_clock: workspace.chart_time_zone_clock(),
        now_unix_nanos: workspace.lifecycle.clock().wall_unix_nanos(),
        account_label: workspace
            .trading_pnl
            .order_entry
            .selected_account_id
            .as_ref()
            .and_then(|id| {
                workspace
                    .lifecycle
                    .trading_store()?
                    .read(cx)
                    .current()?
                    .accounts
                    .iter()
                    .find(|account| &account.id == id)
                    .map(|account| account.display_name.clone())
            }),
        pending: HeaderPendingState {
            symbol_selection: workspace.market_state.symbol_selection_pending,
            series: workspace.chart_pane_controller.selection_in_progress(),
        },
        drawing_history: workspace.drawing_history_state(cx),
        controls: HeaderControls::from_state(
            workspace.symbol_input.is_some() || workspace.symbol_browser.has_results(),
            workspace.has_market_selection(),
        )
        .with_chart_controls(chart_has_market_data),
        chart_link_group: workspace.chart_link_group,
        connection_state: workspace
            .connection_state
            .unwrap_or(FeedConnectionState::Disconnected),
        transport_rtt_nanos: workspace.provider_transport_rtt_nanos,
        market_session: workspace
            .market_session_status
            .as_ref()
            .filter(|status| {
                workspace
                    .product
                    .as_ref()
                    .is_some_and(|product| product.instrument_id == status.instrument_id)
            })
            .cloned(),
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
    fn refresh_broker_connections(
        &mut self,
        _: &RefreshBrokerConnections,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.check_provider_logins(window, cx);
    }

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
            CommandId::ToggleOrderEntry => surface.update(cx, WorkspaceSurface::toggle_order_entry),
            CommandId::ToggleTimeSales => surface.update(cx, WorkspaceSurface::toggle_time_sales),
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
            CommandId::ToggleFullscreen => {
                window.toggle_fullscreen();
                self.chrome_focus.focus(window, cx);
            }
            CommandId::MinimizeWindow => window.minimize_window(),
            CommandId::ZoomWindow => WindowCommand::MaximizeOrRestore.execute(window),
            CommandId::CloseWindow => self.close_window(&CloseWindow, window, cx),
            CommandId::NewWorkspace => self.new_workspace(&NewWorkspace, window, cx),
            CommandId::SelectNextWorkspace => {
                self.select_next_workspace(&SelectNextWorkspace, window, cx);
            }
            CommandId::SelectPreviousWorkspace => {
                self.select_previous_workspace(&SelectPreviousWorkspace, window, cx);
            }
            CommandId::MoveWorkspaceLeft => {
                self.move_workspace_left(&MoveWorkspaceLeft, window, cx);
            }
            CommandId::MoveWorkspaceRight => {
                self.move_workspace_right(&MoveWorkspaceRight, window, cx);
            }
            CommandId::CloseWorkspace => {
                self.close_active_workspace(&CloseWorkspace, window, cx);
            }
            CommandId::SplitHorizontal => {
                self.split_pane_horizontal(&SplitPaneHorizontal, window, cx);
            }
            CommandId::SplitVertical => self.split_pane_vertical(&SplitPaneVertical, window, cx),
            CommandId::ClosePane => self.close_active_pane(&ClosePane, window, cx),
            CommandId::SaveLayout
            | CommandId::ToggleLayoutAutosave
            | CommandId::CopyLayout
            | CommandId::OpenRecentLayout => self.run_layout_command(command, window, cx),
            CommandId::ToggleOneClickTrading => {
                self.toggle_one_click_trading(&ToggleOneClickTrading, window, cx);
            }
            CommandId::BuyMarket
            | CommandId::SellMarket
            | CommandId::CancelAll
            | CommandId::FlattenAccount
            | CommandId::KillSwitch => {
                let hotkey = match command {
                    CommandId::BuyMarket => TradingHotkey::BuyMarket,
                    CommandId::SellMarket => TradingHotkey::SellMarket,
                    CommandId::CancelAll => TradingHotkey::CancelAll,
                    CommandId::FlattenAccount => TradingHotkey::FlattenAccount,
                    _ => TradingHotkey::KillSwitch,
                };
                self.chrome_focus.focus(window, cx);
                self.run_trading_hotkey(hotkey, window, cx);
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
        let (snapshot, maximum_lead_seconds) = {
            let surface = surface.read(cx);
            let maximum_lead_seconds = self
                .trading_store
                .read(cx)
                .current()
                .and_then(|set| {
                    set.risk_profiles
                        .iter()
                        .filter(|profile| profile.enabled)
                        .filter_map(|profile| profile.economic_event_rule)
                        .map(|rule| rule.lead_seconds)
                        .max()
                })
                .unwrap_or(0);
            (
                Arc::clone(surface.context_panel.snapshot()),
                maximum_lead_seconds,
            )
        };
        if maximum_lead_seconds == 0 {
            return;
        }
        let observed_unix_nanos = self.lifecycle.clock().wall_unix_nanos();
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
        let service = self.lifecycle.trading().clone();
        for (key, _) in &due {
            self.event_risk_dispatches
                .insert(key.clone(), observed_unix_nanos);
        }
        if due.is_empty() {
            return;
        }
        let dispatch = cx.background_executor().spawn(async move {
            for (_, event) in due {
                service.apply_economic_event_risk(event)?;
            }
            Ok::<_, String>(())
        });
        cx.spawn(async move |_, cx| {
            let result = dispatch.await;
            surface.update(cx, |surface, surface_cx| {
                surface.context_panel.set_risk_message(
                    result
                        .err()
                        .map(|error| format!("Economic-event risk action unavailable: {error}")),
                );
                surface_cx.notify();
            });
        })
        .detach();
    }

    fn absorb_render_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.absorb_pane_activate_requests(cx);
        self.absorb_watchlist_requests(cx);
        self.absorb_chart_context_menu_requests(cx);
        self.absorb_settings_requests(window, cx);
        self.absorb_study_remove_requests(cx);
    }

    fn prepare_terminal_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.start_market_wake_listener(window, cx);
        self.schedule_market_frame(window, cx);
        self.reconcile_active_drags(cx);
        self.track_window_activation(window, cx);
        self.track_fullscreen_hint(window, cx);
        self.update_toasts(window, cx);
        self.track_chart_fullscreen(window);
        self.absorb_render_requests(window, cx);
        self.synchronize_linked_charts(cx);
        self.sync_layout_drawings(cx);
        self.sync_drawing_context_toolbar(cx);
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

    fn rendered_title_bar(&self, terminal: &Entity<Self>, window: &Window, cx: &App) -> AnyElement {
        let unsaved = self.layout_unsaved_flags(cx);
        workspace_title_bar(
            terminal,
            &WorkspaceTabBarState {
                workspaces: &self.workspaces,
                unsaved: &unsaved,
                start_page: self
                    .layouts_ui
                    .start_page_open()
                    .then(|| self.layouts_ui.start_page_shown()),
                market_summaries: &self.market_summaries,
                active: self.active,
                enabled: self.workspace_factory.is_some(),
                error: self.workspace_error.as_deref(),
                workspace_drag: self.workspace_drag,
                app_view: self.pages.view,
                account: self.lifecycle.account(),
                theme: self.theme,
            },
            window,
            cx,
        )
        .into_any_element()
    }

    /// The title bar row above the workspace, and for a frameless window the top-edge strip
    /// that grows the row back. Docked windows always show the row; fullscreen shows neither.
    fn rendered_title_bars(
        &self,
        terminal: &Entity<Self>,
        window: &Window,
        placement: TitleBarPlacement,
        now: Instant,
        cx: &App,
    ) -> (Option<AnyElement>, Option<AnyElement>) {
        match placement {
            TitleBarPlacement::Docked => {
                (Some(self.rendered_title_bar(terminal, window, cx)), None)
            }
            TitleBarPlacement::Frameless => (
                frameless_title_bar_row(
                    terminal,
                    &self.frameless_title_bar,
                    || self.rendered_title_bar(terminal, window, cx),
                    now,
                ),
                frameless_reveal_zone(terminal, &self.frameless_title_bar),
            ),
            TitleBarPlacement::Hidden => (None, None),
        }
    }

    fn rendered_bottom_panel(
        &self,
        terminal: &Entity<Self>,
        active: &Entity<WorkspaceSurface>,
        cx: &App,
    ) -> impl IntoElement + use<> {
        let surface = active.read(cx);
        bottom_panel::bottom_panel(
            bottom_panel::BottomPanelView {
                terminal,
                state: &self.bottom_panel,
                chart: surface.chart.clone(),
                attribution: terminal_chrome::attribution_for_session(
                    aeris_market_runtime::built_in_provider_presentations(),
                    surface.provider,
                    surface
                        .connection_state
                        .unwrap_or(FeedConnectionState::Disconnected),
                ),
            },
            &self.theme,
        )
    }

    fn rendered_chrome_overlay(
        &self,
        terminal: &Entity<Self>,
        active: &Entity<WorkspaceSurface>,
        title_bar_row_height: f32,
        window: &Window,
        cx: &App,
    ) -> Option<AnyElement> {
        chrome_overlay_layer(
            active.read(cx),
            active,
            accounts_panel::ConnectionsView {
                terminal,
                data: &self.data_connections,
                provider_login: &self.provider_login,
            },
            &self.theme,
            chart_chrome::CHART_CHROME_HEIGHT + title_bar_row_height,
            window.viewport_size(),
            cx,
        )
    }

    fn rendered_market_area(
        &self,
        terminal: &Entity<Self>,
        active: &Entity<WorkspaceSurface>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let watchlist = self.watchlist_panel_state(cx);
        workspace_market_area(
            WorkspaceMarketArea {
                terminal,
                trading_store: &self.trading_store,
                workspace: &self.workspaces[self.active],
                active_surface: active,
                expanded_drawing_toolbar: self.chart_chrome.drawing_toolbar.shown().then_some(
                    DrawingSidebar {
                        menu: &self.drawing_tool_menu,
                        favorites: self.chart_chrome.drawing_favorites,
                    },
                ),
                watchlist,
                toaster: &self.toaster,
                theme: &self.theme,
            },
            cx,
        )
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
        let layout_menu = self.layout_menu_button(
            terminal,
            self.layout_tab_has_unsaved_changes(self.active, cx),
        );
        terminal_header(
            surface,
            active_header_state(workspace, &self.theme, has_data, cx),
            layout_menu,
        )
    }
}

impl Render for TerminalApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare_terminal_frame(window, cx);
        let terminal = cx.entity();
        if let Some(account) = self.lifecycle.account()
            && !account.authenticated()
        {
            // No workspace action, shortcut or trading hotkey handler is
            // attached while the gate covers the window.
            return sign_in_gate::sign_in_page(&terminal, account, window, &self.theme)
                .into_any_element();
        }
        let page = match self.pages.view {
            market_screener::AppView::Terminal => {
                self.rendered_terminal_page(&terminal, window, cx)
            }
            market_screener::AppView::Screener => {
                self.rendered_screener_page(&terminal, window, cx)
            }
        };
        let fullscreen_focus = self.chrome_focus.clone();
        page.track_focus(&self.chrome_focus)
            .on_key_down(cx.listener(Self::on_key_down))
            // Captured so a chart that consumes the release cannot leave a shortcut latched.
            .capture_key_up(cx.listener(|terminal, _, _, _| {
                terminal.keyboard_trading.release_key();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|terminal, _, window, cx| {
                    terminal.handle_window_move_gesture(WindowMoveGestureEvent::Cancel, window);
                    terminal.end_workspace_drag(window, cx);
                    terminal.end_watchlist_drag(cx);
                    terminal.end_drawing_favorites_move(cx);
                    terminal.end_drawing_context_toolbar_move(cx);
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
            .on_action(cx.listener(Self::save_active_layout_shortcut))
            .on_action(cx.listener(Self::toggle_one_click_trading))
            .on_action(cx.listener(Self::trading_buy_market))
            .on_action(cx.listener(Self::trading_sell_market))
            .on_action(cx.listener(Self::trading_cancel_all))
            .on_action(cx.listener(Self::trading_flatten_account))
            .on_action(cx.listener(Self::trading_kill_switch))
            .on_action(cx.listener(Self::open_command_palette))
            .on_action(cx.listener(Self::connect_provider))
            .on_action(cx.listener(Self::cancel_provider_login))
            .on_action(cx.listener(Self::disconnect_provider))
            .on_action(cx.listener(Self::refresh_broker_connections))
            .into_any_element()
    }
}

impl TerminalApp {
    /// The window root every page shares: surface colors and platform typography.
    fn page_root(&self) -> Div {
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(gpui_color(self.theme.colors.surface))
            .text_color(gpui_color(self.theme.colors.text_default))
            .font_family(aeris_design_system::platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
    }

    /// The screener under the title bar. Only shell-level overlays apply on this page.
    fn rendered_screener_page(&self, terminal: &Entity<Self>, window: &Window, cx: &App) -> Div {
        let placement = title_bar_placement(window.is_fullscreen(), self.chart_chrome.window_frame);
        let (title_bar, frameless_reveal_zone) =
            self.rendered_title_bars(terminal, window, placement, Instant::now(), cx);
        self.page_root()
            .children(title_bar)
            .child(market_screener_view::market_screener_page(
                terminal,
                &self.pages.screener,
                &self.theme,
            ))
            .children(fullscreen_hint_layer(&self.fullscreen_hint, &self.theme))
            .children(frameless_reveal_zone)
            .children(self.platform_menu_overlay(terminal, window.viewport_size(), cx))
            .children(self.app_navigation_overlay(terminal, window.viewport_size()))
            .children(self.keyboard_shortcuts_layer(terminal))
            .children(self.rendered_command_palette(terminal, cx))
            .children(self.trading_confirmation_layer(terminal))
            .children(self.layouts_dialog_layer(terminal, cx))
    }

    /// The start page under the title bar, in place of the active workspace. Only
    /// shell-level overlays apply.
    fn rendered_start_page_view(
        &self,
        terminal: &Entity<Self>,
        start_page: AnyElement,
        window: &Window,
        cx: &App,
    ) -> Div {
        let placement = title_bar_placement(window.is_fullscreen(), self.chart_chrome.window_frame);
        let (title_bar, frameless_reveal_zone) =
            self.rendered_title_bars(terminal, window, placement, Instant::now(), cx);
        self.page_root()
            .children(title_bar)
            .child(start_page)
            .children(fullscreen_hint_layer(&self.fullscreen_hint, &self.theme))
            .children(frameless_reveal_zone)
            .children(self.platform_menu_overlay(terminal, window.viewport_size(), cx))
            .children(self.app_navigation_overlay(terminal, window.viewport_size()))
            .children(self.keyboard_shortcuts_layer(terminal))
            .children(self.rendered_command_palette(terminal, cx))
            .children(self.trading_confirmation_layer(terminal))
            .children(self.layouts_dialog_layer(terminal, cx))
    }

    fn rendered_terminal_page(
        &self,
        terminal: &Entity<Self>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        if let Some(start_page) = self.rendered_start_page(terminal, cx) {
            return self.rendered_start_page_view(terminal, start_page, window, cx);
        }
        let terminal = terminal.clone();
        let pane_count = self.workspaces[self.active].panes.len();
        let active = self.active_surface();
        let workspace = active.read(cx);
        let chart_has_market_data = workspace
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).has_market_data());
        let placement = title_bar_placement(window.is_fullscreen(), self.chart_chrome.window_frame);
        let now = Instant::now();
        // Header popups anchor below the title bar row, which moves while a frameless bar
        // slides; its animation redraws this view every frame, so the anchor follows it.
        let title_bar_row_height =
            placement.row_height(self.frameless_title_bar.visibility_at(now));
        let overlay =
            self.rendered_chrome_overlay(&terminal, &active, title_bar_row_height, window, cx);
        let (context_menu, settings_menu) = self.chart_surface_menus(
            &terminal,
            pane_count,
            chart_has_market_data,
            window.viewport_size(),
            cx,
        );
        let platform_menu = self.platform_menu_overlay(&terminal, window.viewport_size(), cx);
        let drawing_tool_menu = self.drawing_tool_menu_overlay(&terminal, window, cx);
        let chart_top = if self.chart_fullscreen.is_some() {
            0.0
        } else {
            title_bar_row_height + chart_chrome::CHART_CHROME_HEIGHT
        };
        let drawing_favorites = self.drawing_favorites_overlay(&terminal, chart_top, window, cx);
        let drawing_context = self.drawing_context_overlay(&terminal, chart_top, window, cx);
        let command_palette = self.rendered_command_palette(&terminal, cx);
        let keyboard_shortcuts = self.keyboard_shortcuts_layer(&terminal);
        let trading_confirmation = self.trading_confirmation_layer(&terminal);
        let layouts_dialog = self.layouts_dialog_layer(&terminal, cx);
        let (title_bar, frameless_reveal_zone) =
            self.rendered_title_bars(&terminal, window, placement, now, cx);
        let header = self.rendered_header(&terminal, &active, cx);
        let bottom_panel = self.rendered_bottom_panel(&terminal, &active, cx);
        let market = self.rendered_market_area(&terminal, &active, cx);
        let fullscreen_hint = fullscreen_hint_layer(&self.fullscreen_hint, &self.theme);
        let app_navigation = self.app_navigation_overlay(&terminal, window.viewport_size());
        if self.chart_fullscreen.is_some() {
            // Every chart in the workspace layout fills the screen; a pane maximized with
            // Alt+click stays the only one shown, exactly as in the docked workspace.
            let workspace = &self.workspaces[self.active];
            let charts = workspace_pane_grid(&terminal, workspace, &self.toaster, &self.theme, cx);
            return self
                .page_root()
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .bg(gpui_color(self.theme.colors.surface))
                        .child(charts),
                )
                .children(drawing_favorites)
                .children(drawing_context)
                .children(fullscreen_hint)
                .children(overlay)
                .children(context_menu)
                .children(settings_menu)
                .children(drawing_tool_menu)
                .children(keyboard_shortcuts)
                .children(command_palette)
                .children(trading_confirmation)
                .children(layouts_dialog);
        }
        self.page_root()
            .children(title_bar)
            .child(header)
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .bg(gpui_color(self.theme.colors.surface))
                    .child(market),
            )
            .child(bottom_panel)
            .children(drawing_favorites)
            .children(drawing_context)
            .children(fullscreen_hint)
            .children(overlay)
            .children(frameless_reveal_zone)
            .children(context_menu)
            .children(settings_menu)
            .children(drawing_tool_menu)
            .children(platform_menu)
            .children(app_navigation)
            .children(keyboard_shortcuts)
            .children(command_palette)
            .children(trading_confirmation)
            .children(layouts_dialog)
    }
}

impl TerminalApp {
    pub(super) fn trading_hotkeys_enabled(&self, window: &Window, cx: &App) -> bool {
        self.pages.view == market_screener::AppView::Terminal
            && !self.keyboard_shortcuts_open
            && self.workspace_keyboard_focused(window, cx)
    }

    pub(super) fn trading_order_frame(
        &self,
        cx: &App,
    ) -> Option<aeris_market_data::OrderBookFrame> {
        self.active_surface()
            .read(cx)
            .order_book
            .read(cx)
            .frame()
            .cloned()
    }

    pub(super) fn trading_order_entry(
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
            aeris_trading::OrderType::Market,
            aeris_trading::TimeInForce::Day,
        )
    }

    pub(super) fn trading_order_entry_locked(&self, cx: &App) -> bool {
        self.active_surface()
            .read(cx)
            .selected_account_lock_reason(cx)
            .is_some()
    }

    /// A trading key the workspace does not take moves on to the focused element.
    fn trading_key_action(
        &mut self,
        hotkey: TradingHotkey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.run_trading_hotkey(hotkey, window, cx) {
            cx.propagate();
        }
    }

    fn trading_buy_market(
        &mut self,
        _: &TradingBuyMarket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.trading_key_action(TradingHotkey::BuyMarket, window, cx);
    }

    fn trading_sell_market(
        &mut self,
        _: &TradingSellMarket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.trading_key_action(TradingHotkey::SellMarket, window, cx);
    }

    fn trading_cancel_all(
        &mut self,
        _: &TradingCancelAll,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.trading_key_action(TradingHotkey::CancelAll, window, cx);
    }

    fn trading_flatten_account(
        &mut self,
        _: &TradingFlattenAccount,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.trading_key_action(TradingHotkey::FlattenAccount, window, cx);
    }

    fn trading_kill_switch(
        &mut self,
        _: &TradingKillSwitch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.trading_key_action(TradingHotkey::KillSwitch, window, cx);
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
    /// The selected tab; none while the start page is shown.
    active: Option<usize>,
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
) -> Button {
    close_button(("close_workspace", tab_id), theme, move |window, cx| {
        terminal.update(cx, |terminal, cx| {
            terminal.close_workspace(tab_id, window, cx);
        });
    })
    .aria_label(format!("Close {label}"))
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
) -> Button {
    Button::new("add_workspace", theme)
        .variant(ButtonVariant::Ghost)
        .button_size(ButtonSize::Sm)
        .round()
        .icon(header_icon(HugeIcon::Add))
        .aria_label("New tab")
        .disabled(!enabled)
        .on_press(move |_, window, cx| {
            terminal.update(cx, |terminal, cx| terminal.open_start_page(window, cx));
        })
}

/// A tab shows its active chart's exchange mark, instrument and daily change, marked while
/// its layout has unsaved changes.
fn workspace_tab_content(
    workspace: &WorkspaceTab,
    unsaved: bool,
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
    let aria_label = workspace_tab_aria_label(&label, unsaved, change_label.as_deref());
    let change_color = values
        .change
        .map_or(theme.colors.text_muted, |value| match value.cmp(&0) {
            std::cmp::Ordering::Less => theme.colors.text_negative,
            std::cmp::Ordering::Greater => theme.colors.text_positive,
            std::cmp::Ordering::Equal => theme.colors.text_secondary,
        });
    let content = div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_1()
        .child(symbol_menu::provider_exchange_mark(
            surface.provider,
            px(WORKSPACE_TAB_EXCHANGE_GLYPH),
            false,
            &theme.colors,
        ))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(label.clone()),
        )
        .children(unsaved.then(|| layouts_view::unsaved_layout_dot(theme)))
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
    unsaved: bool,
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
    let label_width = measure(label, rem * 0.875, font.clone());
    let change_width = change.map(|change| {
        font.features = platform_tabular_numerals();
        measure(change, rem * 0.75, font)
    });
    let marker_width = unsaved.then_some(layouts_view::UNSAVED_LAYOUT_DOT_SIZE);
    workspace_tab_width_from_measurements(label_width, marker_width, change_width, show_close)
}

fn workspace_tab_width_from_measurements(
    label_width: f32,
    marker_width: Option<f32>,
    change_width: Option<f32>,
    show_close: bool,
) -> f32 {
    let mut width = WORKSPACE_TAB_HORIZONTAL_PADDING * 2.0
        + WORKSPACE_TAB_EXCHANGE_GLYPH
        + WORKSPACE_TAB_CONTENT_GAP
        + label_width;
    if let Some(marker_width) = marker_width {
        width += WORKSPACE_TAB_CONTENT_GAP + marker_width;
    }
    if let Some(change_width) = change_width {
        width += WORKSPACE_TAB_CONTENT_GAP + change_width;
    }
    if show_close {
        // The close control is the shared `close_button`, a small square button.
        width += WORKSPACE_TAB_CLOSE_GAP + ButtonSize::Sm.logical_height();
    }
    width.ceil().min(WORKSPACE_TAB_MAX_WIDTH)
}

fn workspace_tab_widths(
    visuals: &[(String, String, Div, Option<String>)],
    unsaved: &[bool],
    show_close: bool,
    window: &Window,
) -> Vec<f32> {
    visuals
        .iter()
        .enumerate()
        .map(|(index, (label, _, _, change))| {
            let unsaved = unsaved.get(index).copied().unwrap_or(false);
            workspace_tab_width(label, unsaved, change.as_deref(), show_close, window)
        })
        .collect()
}

fn workspace_tab_change_label(values: MarketSummaryValues) -> Option<String> {
    values.change_percent.map(|value| format!("{value:+.2}%"))
}

fn workspace_tab_aria_label(label: &str, unsaved: bool, change: Option<&str>) -> String {
    let label = if unsaved {
        format!("{label}, unsaved changes")
    } else {
        label.to_string()
    };
    change.map_or_else(
        || label.clone(),
        |change| format!("{label}, change {change}"),
    )
}

fn workspace_tab_surface_colors(theme: &AerisTheme) -> (ThemeColor, ThemeColor) {
    (terminal_chrome::title_bar_fill(theme), theme.colors.surface)
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
    let selected = Some(index) == state.active;
    let select_terminal = terminal.clone();
    let key_terminal = terminal.clone();
    let middle_click_terminal = terminal.clone();
    let drag_terminal = terminal.clone();
    let drag = WorkspaceTabDrag { tab_id };
    let tab_focus = workspace.focus.clone();
    let mouse_focus = workspace.focus.clone();
    let (resting_fill, selected_fill) = workspace_tab_surface_colors(&theme);
    Tab::new(("workspace_tab", tab_id), &theme)
        .chrome(resting_fill, selected_fill)
        .selected(selected)
        .w(px(width))
        .h(px(chart_chrome::CHART_CONTROL_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
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
    let close_drag_enabled = workspace_tab_close_drag_enabled(workspaces.len());
    let visuals: Vec<_> = workspaces
        .iter()
        .enumerate()
        .map(|(index, workspace)| {
            let unsaved = state.unsaved.get(index).copied().unwrap_or(false);
            workspace_tab_content(workspace, unsaved, state.market_summaries, &theme, cx)
        })
        .collect();
    let tab_widths = workspace_tab_widths(&visuals, state.unsaved, close_drag_enabled, window);
    let selected_tab = (state.start_page != Some(true)).then_some(state.active);
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
                    active: selected_tab,
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
        .on_drop(move |_: &WorkspaceTabDrag, window, cx| {
            end_terminal.update(cx, |terminal, cx| terminal.end_workspace_drag(window, cx));
        })
        .children(enabled.then_some(tabs).into_iter().flatten())
        .children(
            state
                .start_page
                .filter(|_| enabled)
                .map(|shown| start_page_tab(terminal, shown, &theme)),
        )
        .children(
            enabled
                .then(|| workspace_tab_strip_end(terminal, add_enabled, &theme))
                .into_iter()
                .flatten(),
        )
        .children(
            state
                .error
                .map(|error| workspace_creation_error(error, &theme)),
        )
}

fn workspace_creation_error(error: &str, theme: &AerisTheme) -> AnyElement {
    chrome_tooltip(
        "workspace_creation_error",
        error.to_string(),
        div()
            .size(px(7.0))
            .rounded_full()
            .bg(gpui_color(theme.colors.danger)),
        theme,
    )
    .into_any_element()
}

/// The "New tab" button and the drop target after the last tab.
fn workspace_tab_strip_end(
    terminal: &Entity<TerminalApp>,
    add_enabled: bool,
    theme: &AerisTheme,
) -> [AnyElement; 2] {
    [
        chrome_tooltip(
            "add_workspace",
            "New tab",
            workspace_add_button(terminal.clone(), add_enabled, theme),
            theme,
        )
        .into_any_element(),
        div()
            .id("workspace_tab_drop_target")
            .h(px(chart_chrome::CHART_CONTROL_SIZE))
            .min_w(px(16.0))
            .into_any_element(),
    ]
}

/// The "New tab" placeholder: selected while the start page shows, closable, never a
/// workspace.
fn start_page_tab(terminal: &Entity<TerminalApp>, shown: bool, theme: &AerisTheme) -> AnyElement {
    let (resting_fill, selected_fill) = workspace_tab_surface_colors(theme);
    let show = terminal.clone();
    let close = terminal.clone();
    Tab::new("start_page_tab", theme)
        .chrome(resting_fill, selected_fill)
        .selected(shown)
        .h(px(chart_chrome::CHART_CONTROL_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .text_sm()
        .aria_label("New tab")
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            show.update(cx, |terminal, cx| terminal.open_start_page(window, cx));
        })
        .child(div().whitespace_nowrap().child("New tab"))
        .child(
            close_button("close_start_page", theme, move |window, cx| {
                close.update(cx, |terminal, cx| terminal.close_start_page(window, cx));
            })
            .aria_label("Close New tab"),
        )
        .into_any_element()
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
                label: layout_library::UNTITLED_LAYOUT_NAME.to_string(),
                saved_layout_id: 0,
                autosaved_snapshot: None,
                panes: vec![WorkspacePane {
                    id: 1,
                    consumer_id: 1,
                    surface,
                }],
                active_pane: 0,
                layout: AerisChartWorkspace::new(1, CHART_PANE_CAPACITY)
                    .expect("startup pane identity 1 is a valid chart workspace root"),
                maximized_pane: None,
                swallow_pane_mouse_up: false,
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
            drawing_templates: Vec::new(),
            saved_layouts: Vec::new(),
            workspace_notice: None,
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
    pub(super) drawing_templates: Vec<WorkspaceDrawingTemplateState>,
    /// A tab showing no layout of this library is given a new one.
    pub(super) saved_layouts: Vec<WorkspaceSavedLayoutState>,
    pub(super) workspace_notice: Option<String>,
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
    window.on_window_should_close(cx, move |window, cx| {
        closing_terminal.update(cx, |terminal, cx| terminal.window_should_close(window, cx))
    });
    // A focused element that leaves the screen (a replaced chart, a hidden tab strip, a closed
    // pane or dialog) would otherwise route every key to the bare window root, where no
    // workspace shortcut is registered.
    terminal.update(cx, |_, cx| {
        cx.on_focus_lost(window, |terminal, window, cx| {
            terminal.focus_workspace(window, cx);
            // GPUI does not redraw for focus a focus-lost listener moves. Draw the restored
            // focus, so the next focused element to leave the screen is reported again.
            cx.defer_in(window, |_, window, _| window.refresh());
        })
        .detach();
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

pub(super) struct WorkspaceTabsRestore<'a> {
    pub(super) state: &'a WorkspaceState,
    pub(super) notice: Option<String>,
}

/// Builds the open tab `tab` describes from the market panes started for it, which it takes
/// out of `market_panes`. Every restored or opened tab is built here. `None` when none of its
/// panes started or its layout is invalid; the panes it did take are then dropped with it.
pub(super) fn restored_workspace_tab(
    tab: &WorkspaceTabState,
    market_panes: &mut Vec<engine_market_worker::WorkspaceMarketPane>,
    lifecycle: &DesktopLifecycle,
    chart_chrome: chart_chrome::ChartChromePreferences,
    window: &mut Window,
    cx: &mut App,
) -> Option<WorkspaceTab> {
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
        });
    }
    if panes.is_empty() {
        return None;
    }
    let layout = tab.layout.as_ref().and_then(chart_workspace_layout)?;
    let pane_order = layout.leaf_ids();
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
    let layout = AerisChartWorkspace::restore(&layout, CHART_PANE_CAPACITY).ok()?;
    Some(WorkspaceTab {
        id: tab.workspace_id,
        label: tab.label.clone(),
        saved_layout_id: tab.saved_layout_id,
        autosaved_snapshot: None,
        panes,
        active_pane,
        layout,
        maximized_pane: None,
        swallow_pane_mouse_up: false,
        generation: tab.generation,
        focus: cx.focus_handle(),
    })
}

pub(super) fn workspace_tabs_root(
    mut market_panes: Vec<engine_market_worker::WorkspaceMarketPane>,
    restored: WorkspaceTabsRestore<'_>,
    workspace_factory: engine_market_worker::WorkspaceMarketFactory,
    lifecycle: &DesktopLifecycle,
    chart_chrome: chart_chrome::ChartChromePreferences,
    window: &mut Window,
    cx: &mut App,
) -> Entity<TerminalApp> {
    let plan = restored_workspace_shell_plan(restored.state);
    let workspaces = plan
        .workspace_tabs
        .iter()
        .filter_map(|tab| {
            restored_workspace_tab(tab, &mut market_panes, lifecycle, chart_chrome, window, cx)
        })
        .collect();
    terminal_shell_root(
        TerminalShellInit {
            workspaces,
            active_workspace_id: Some(plan.active_workspace_id),
            workspace_revision: plan.workspace_revision,
            layout_generation: plan.layout_generation,
            workspace_factory: Some(workspace_factory),
            workspace_shell: WorkspaceShellKind::Tabs,
            chart_chrome,
            chart_settings_templates: restored.state.chart_settings_templates.clone(),
            watchlist_entries: restored.state.watchlist_entries.clone(),
            drawing_templates: restored.state.drawing_templates.clone(),
            saved_layouts: restored.state.saved_layouts.clone(),
            workspace_notice: restored.notice,
        },
        lifecycle,
        window,
        cx,
    )
}

#[cfg(test)]
mod tests {
    use aeris_design_system::AerisTheme;

    use super::{
        MarketSummaryValues, WORKSPACE_TAB_MAX_WIDTH, workspace_tab_aria_label,
        workspace_tab_change_label, workspace_tab_close_drag_enabled, workspace_tab_surface_colors,
        workspace_tab_width_from_measurements,
    };

    #[test]
    fn workspace_tab_close_and_drag_require_multiple_tabs() {
        assert!(!workspace_tab_close_drag_enabled(0));
        assert!(!workspace_tab_close_drag_enabled(1));
        assert!(workspace_tab_close_drag_enabled(2));
        assert!(workspace_tab_close_drag_enabled(3));
    }

    #[test]
    fn workspace_tabs_match_the_title_bar_until_selected() {
        for theme in [AerisTheme::light(), AerisTheme::dark()] {
            let (inactive, active) = workspace_tab_surface_colors(&theme);
            assert_eq!(
                inactive,
                theme.colors.surface_secondary.over(theme.colors.surface)
            );
            assert!((inactive.alpha() - 1.0).abs() < f32::EPSILON);
            assert_eq!(active, theme.colors.surface);
        }
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
            workspace_tab_aria_label("Scalping", false, change.as_deref()),
            "Scalping, change +6.00%"
        );
        assert_eq!(
            workspace_tab_aria_label("Scalping", true, None),
            "Scalping, unsaved changes"
        );
    }

    #[test]
    fn workspace_tab_width_tracks_content_and_preserves_trailing_space() {
        let without_close = workspace_tab_width_from_measurements(100.0, None, Some(50.0), false);
        assert!((without_close - 198.0).abs() < f32::EPSILON);
        let with_close = workspace_tab_width_from_measurements(100.0, None, Some(50.0), true);
        assert!((with_close - 230.0).abs() < f32::EPSILON);
        let unsaved = workspace_tab_width_from_measurements(100.0, Some(6.0), Some(50.0), false);
        assert!(unsaved > without_close, "the unsaved marker gets room");
        let capped = workspace_tab_width_from_measurements(1_000.0, None, None, false);
        assert!((capped - WORKSPACE_TAB_MAX_WIDTH).abs() < f32::EPSILON);
    }
}
