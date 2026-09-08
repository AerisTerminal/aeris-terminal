//! Workspace surface.

use super::*;

impl WorkspaceSurface {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        cx: &mut Context<Self>,
        startup: MarketWorkerStartup,
        market_worker: MarketDataWorker,
        lifecycle: DesktopLifecycle,
        symbol_input: Option<Entity<InputState>>,
        indicator_input: Entity<InputState>,
        timeframe_input: Entity<InputState>,
        chart_chrome: chart_chrome::ChartChromePreferences,
    ) -> Self {
        let theme = AxiusflowTheme::dark();
        let restored_rithmic = match &startup {
            MarketWorkerStartup::Loading(startup) => {
                Some((startup.interval, startup.restored_viewport))
            }
            MarketWorkerStartup::Rithmic => None,
        };
        let TerminalStartupState {
            chart,
            chart_state,
            chart_state_message,
            replay_label,
            worker_label,
            subscription_id,
            connection_state,
            connection_message,
            provider,
            product,
        } = terminal_startup_state(startup, cx);
        initialize_chart_chrome(chart.as_ref(), chart_chrome, cx);
        let bridge_label = chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| bridge_status(chart.read(cx).replay_bridge_metrics()),
        );
        observe_chart(chart.as_ref(), cx);
        let order_book = cx.new(move |_| ReadOnlyOrderBookView::new(theme));
        Self {
            chart,
            order_book,
            side_panel: None,
            side_panel_width: SIDE_PANEL_INITIAL_WIDTH,
            side_panel_resize: None,
            menu_state: WorkspaceMenuState::default(),
            scrolls: WorkspaceScrollHandles::default(),
            chart_state,
            chart_state_message,
            theme,
            replay_label,
            worker_label,
            subscription_id,
            bridge_label,
            market_worker,
            lifecycle,
            pending_ui_diagnostics: None,
            connection_state,
            connection_message,
            provider_transport_rtt_nanos: None,
            // Both resident-engine providers support the empty catalog query
            // used to populate the instrument menu. A successful selection
            // consumes its one-shot search authorization, so reopening the
            // menu must be able to issue another empty listing request instead
            // of leaving Hyperliquid with an empty, non-refreshable browser.
            symbol_browser: rithmic_shell::RithmicSymbolBrowser::rithmic_catalog_awaiting_search(
                std::num::NonZeroUsize::MIN,
                "",
            ),
            symbol_message: initial_symbol_message(provider),
            market_state: WorkspaceMarketState::default(),
            series_browser: rithmic_history::RithmicSeriesBrowser::default(),
            series_message: "Select a symbol before choosing a series".to_string(),
            rithmic_reconnect: RithmicReconnectState::Idle,
            symbol_input,
            indicator_input,
            timeframe_input,
            indicator_message: None,
            chrome_overlay: None,
            chrome_overlay_phase: ChromeOverlayPhase::Opening,
            chrome_overlay_generation: 0,
            timeframe_menu_flyout: None,
            timeframe_flyout_close_token: 0,
            timeframe_hover_regions: 0,
            timeframe_trigger_bounds: None,
            chart_type_trigger_bounds: None,
            chrome_selection: 0,
            chrome_focus: cx.focus_handle().tab_stop(true),
            instrument_exchange: InstrumentExchangeUi::Idle(match provider {
                TerminalProvider::Rithmic => assets::ExchangeLogo::Rithmic,
                TerminalProvider::Hyperliquid => assets::ExchangeLogo::Hyperliquid,
            }),
            provider,
            product,
            rithmic_switch: RithmicSwitchState::Idle,
            interval: restored_rithmic.map_or(ChartInterval::Minute1, |restored| restored.0),
            rithmic_pending_interval: None,
            rithmic_pending_product: None,
            rithmic_pending_sequence: None,
            rithmic_previous_selection: None,
            restored_viewport: restored_rithmic.and_then(|restored| restored.1),
            last_persisted_viewport: None,
            pending_chart_context_menu: None,
            pending_pane_activate: PaneActivationRequest::None,
            resource_class: ConsumerResourceClass::Foreground,
            chart_chrome,
            retained_chart_presentation: RetainedChartPresentation::default(),
            #[cfg(feature = "diagnostics")]
            foreground_interactions: ForegroundInteractionDiagnostics::default(),
            #[cfg(feature = "diagnostics")]
            live_evidence_enabled: std::env::var_os("AXIUSFLOW_LIVE_EVIDENCE").is_some(),
            #[cfg(feature = "diagnostics")]
            live_evidence_publications: 0,
        }
    }

    pub(super) fn retire_market_worker(&mut self, cx: &App) {
        if let Some(retirement) = self.market_worker.begin_retirement() {
            self.lifecycle.retire_market_worker(retirement, cx);
        }
    }

    pub(super) fn set_market_resource_class(&mut self, resource_class: ConsumerResourceClass) {
        self.resource_class = resource_class;
        let _ = self
            .market_worker
            .try_set_market_resource_class(resource_class);
    }

    pub(super) fn set_market_message_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.market_worker.set_message_wake(wake);
    }

    pub(super) fn should_poll_market(&self) -> bool {
        self.resource_class == ConsumerResourceClass::Foreground
    }

    pub(super) fn available_intervals(&self) -> &'static [ChartInterval] {
        match self.provider {
            TerminalProvider::Rithmic => RITHMIC_INTERVALS,
            TerminalProvider::Hyperliquid => HYPERLIQUID_INTERVALS,
        }
    }

    pub(super) fn quick_timeframe_matches(&self, cx: &App) -> Vec<ChartInterval> {
        let query = self.timeframe_input.read(cx).value();
        self.available_intervals()
            .iter()
            .copied()
            .filter(|interval| interval.matches_typeahead(query.as_ref()))
            .collect()
    }

    fn sync_timeframe_menu_selection(&mut self) {
        self.timeframe_menu_flyout = None;
        self.menu_state.timeframe_flyout_keyboard = false;
        self.timeframe_hover_regions = 0;
        let selected_group = timeframe_interval_group(self.selected_interval());
        self.chrome_selection = timeframe_menu_groups(self.available_intervals())
            .iter()
            .position(|group| *group == selected_group)
            .unwrap_or(0);
    }

    pub(super) fn open_timeframe_group(
        &mut self,
        group: TimeframeMenuGroup,
        from_keyboard: bool,
        cx: &mut Context<Self>,
    ) {
        self.retain_timeframe_flyout();
        let already_open = self.timeframe_menu_flyout == Some(group);
        if !already_open {
            self.timeframe_menu_flyout = Some(group);
            self.chrome_selection = timeframe_group_intervals(group, self.available_intervals())
                .iter()
                .position(|interval| *interval == self.selected_interval())
                .unwrap_or(0);
        }
        if self.menu_state.timeframe_flyout_keyboard != from_keyboard || !already_open {
            self.menu_state.timeframe_flyout_keyboard = from_keyboard;
            cx.notify();
        }
    }

    fn retain_timeframe_flyout(&mut self) {
        self.timeframe_flyout_close_token = self.timeframe_flyout_close_token.saturating_add(1);
    }

    fn arm_timeframe_flyout_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.retain_timeframe_flyout();
        let token = self.timeframe_flyout_close_token;
        cx.spawn_in(window, async move |app, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let _ = app.update_in(cx, |app, _, app_cx| {
                if app.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && app.timeframe_flyout_close_token == token
                {
                    app.close_timeframe_flyout(app_cx);
                }
            });
        })
        .detach();
    }

    pub(super) fn hover_timeframe_menu_region(
        &mut self,
        hovered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            self.timeframe_hover_regions = self.timeframe_hover_regions.saturating_add(1);
            self.retain_timeframe_flyout();
            return;
        }
        if self.timeframe_hover_regions == 0 {
            return;
        }
        self.timeframe_hover_regions -= 1;
        if self.timeframe_hover_regions == 0 {
            self.arm_timeframe_flyout_close(window, cx);
        }
    }

    fn close_timeframe_flyout(&mut self, cx: &mut Context<Self>) {
        let Some(group) = self.timeframe_menu_flyout.take() else {
            return;
        };
        self.menu_state.timeframe_flyout_keyboard = false;
        self.timeframe_hover_regions = 0;
        self.chrome_selection = timeframe_menu_groups(self.available_intervals())
            .iter()
            .position(|item| *item == group)
            .unwrap_or(0);
        cx.notify();
    }

    fn timeframe_menu_keyboard_count(&self) -> usize {
        if let Some(group) = self.timeframe_menu_flyout {
            timeframe_group_intervals(group, self.available_intervals()).len()
        } else {
            timeframe_menu_groups(self.available_intervals()).len()
        }
    }

    fn sync_chart_type_menu_selection(&mut self, cx: &App) {
        let selected = self.chart_type(cx);
        self.chrome_selection = ChartType::ALL
            .iter()
            .position(|chart_type| *chart_type == selected)
            .unwrap_or(0);
    }

    fn apply_highlighted_chart_type(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(chart_type) = ChartType::ALL.get(self.chrome_selection).copied() {
            self.set_chart_type(chart_type, cx);
            self.close_chrome_overlay(window, cx);
        }
    }

    pub(super) fn sync_quick_timeframe_selection(&mut self, cx: &App) {
        let query = self.timeframe_input.read(cx).value();
        let intervals = self.quick_timeframe_matches(cx);
        self.chrome_selection = intervals
            .iter()
            .position(|interval| interval.label() == query.as_ref())
            .or_else(|| {
                intervals
                    .iter()
                    .position(|interval| *interval == self.selected_interval())
            })
            .unwrap_or(0);
    }

    pub(super) fn apply_highlighted_interval(
        &mut self,
        intervals: &[ChartInterval],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(interval) = intervals.get(self.chrome_selection).copied()
            && self.select_interval(interval, cx)
        {
            self.close_chrome_overlay(window, cx);
        }
    }

    pub(super) fn selected_interval(&self) -> ChartInterval {
        self.interval
    }

    pub(super) fn select_interval(
        &mut self,
        interval: ChartInterval,
        cx: &mut Context<Self>,
    ) -> bool {
        let starter_interval = matches!(
            interval,
            ChartInterval::Minute1
                | ChartInterval::Minute3
                | ChartInterval::Minute5
                | ChartInterval::Minute15
                | ChartInterval::Minute30
                | ChartInterval::Hour1
                | ChartInterval::Hour2
                | ChartInterval::Hour4
                | ChartInterval::Hour8
                | ChartInterval::Hour12
                | ChartInterval::Day1
        );
        if !current_plan_limits().extended_timeframes && !starter_interval {
            self.series_message = "This timeframe requires a paid plan".to_string();
            cx.notify();
            return false;
        }
        #[cfg(feature = "diagnostics")]
        let started = Instant::now();
        let selected = (|| {
            if self.interval == interval && self.rithmic_pending_interval.is_none() {
                return true;
            }
            if self.rithmic_pending_interval == Some(interval) {
                return true;
            }
            let Some(product) = self.product.clone() else {
                let display = terminal_provider_display(self.provider);
                self.series_message = format!("{display} market selection is unavailable");
                cx.notify();
                return false;
            };
            let Ok(sequence) = self.market_worker.try_select_engine(product, interval) else {
                self.series_message = format!("{} history could not start", interval.label());
                cx.notify();
                return false;
            };
            self.rithmic_pending_interval = Some(interval);
            self.rithmic_pending_sequence = Some(sequence);
            self.rithmic_switch = RithmicSwitchState::Pending;
            self.chart_state = ChartState::Loading;
            self.chart_state_message = format!("Loading {} history", interval.label());
            self.series_message = format!("Switching to {}", interval.label());
            cx.notify();
            true
        })();
        #[cfg(feature = "diagnostics")]
        self.foreground_interactions
            .record_interval_selection(elapsed_nanos(started));
        selected
    }

    pub(super) fn instrument_entries(&self, _cx: &App) -> Vec<InstrumentMenuEntry> {
        self.symbol_browser
            .results()
            .iter()
            .enumerate()
            .map(|(index, instrument)| InstrumentMenuEntry {
                symbol: instrument.symbol.clone(),
                checked: self.symbol_browser.selected().is_some_and(|selected| {
                    selected.instrument.symbol == instrument.symbol
                        && selected.instrument.exchange == instrument.exchange
                }),
                selection: match self.provider {
                    TerminalProvider::Rithmic => InstrumentMenuSelection::Rithmic(index),
                    TerminalProvider::Hyperliquid => InstrumentMenuSelection::Hyperliquid(index),
                },
            })
            .collect()
    }

    fn activate_chrome_list_keyboard(&mut self) {
        if matches!(
            self.chrome_overlay,
            Some(ChromeOverlay::Instrument | ChromeOverlay::Indicator)
        ) {
            self.menu_state.chrome_list_keyboard = true;
        }
    }

    fn sync_instrument_menu_keyboard(&mut self, cx: &App) {
        if self.menu_state.chrome_list_keyboard {
            return;
        }
        self.chrome_selection =
            current_instrument_menu_index(&self.instrument_entries(cx)).unwrap_or(0);
    }

    pub(super) fn select_instrument(
        &mut self,
        selection: InstrumentMenuSelection,
        cx: &mut Context<Self>,
    ) -> bool {
        #[cfg(feature = "diagnostics")]
        let started = Instant::now();
        let selected = (|| match selection {
            InstrumentMenuSelection::Rithmic(index)
            | InstrumentMenuSelection::Hyperliquid(index) => {
                let provider = terminal_provider_id(self.provider);
                let entitlement_id = match self.provider {
                    TerminalProvider::Rithmic => RITHMIC_ENTITLEMENT_ID,
                    TerminalProvider::Hyperliquid => HYPERLIQUID_ENTITLEMENT_ID,
                };
                let display = terminal_provider_display(self.provider);
                let Some(selection) = self.symbol_browser.select(index) else {
                    return false;
                };
                let request = SelectProviderInstrument {
                    consumer_id: 0,
                    selection_generation: selection.generation.get() as u64,
                    search_generation: selection.search_generation.get() as u64,
                    provider: provider.to_string(),
                    symbol: selection.instrument.symbol.clone(),
                    exchange: selection.instrument.exchange.clone(),
                    entitlement_id: entitlement_id.to_string(),
                };
                if self.market_worker.try_select_provider(request).is_err() {
                    self.symbol_browser.reject_selection(selection.generation);
                    self.symbol_message = format!("{display} market selection is busy; try again");
                    cx.notify();
                    return false;
                }
                self.market_state.symbol_selection_pending = true;
                self.symbol_message = format!("Selecting {}", selection.instrument.symbol);
                cx.notify();
                true
            }
        })();
        #[cfg(feature = "diagnostics")]
        self.foreground_interactions
            .record_instrument_selection(elapsed_nanos(started));
        selected
    }

    pub(super) fn open_chrome_overlay(
        &mut self,
        overlay: ChromeOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chrome_overlay_generation = self.chrome_overlay_generation.saturating_add(1);
        self.chrome_overlay_phase = ChromeOverlayPhase::Opening;
        self.chrome_overlay = Some(overlay);
        if overlay != ChromeOverlay::QuickTimeframe {
            self.timeframe_input.update(cx, |input, input_cx| {
                input.set_value("", window, input_cx);
            });
        }
        if overlay != ChromeOverlay::Timeframe {
            self.timeframe_menu_flyout = None;
            self.menu_state.timeframe_flyout_keyboard = false;
            self.timeframe_hover_regions = 0;
        }
        self.menu_state.chrome_list_keyboard = false;
        self.chrome_selection = match overlay {
            ChromeOverlay::Timeframe => {
                self.sync_timeframe_menu_selection();
                self.chrome_selection
            }
            ChromeOverlay::ChartType => {
                self.sync_chart_type_menu_selection(cx);
                self.chrome_selection
            }
            ChromeOverlay::QuickTimeframe => {
                self.sync_quick_timeframe_selection(cx);
                self.chrome_selection
            }
            ChromeOverlay::Instrument => {
                current_instrument_menu_index(&self.instrument_entries(cx)).unwrap_or(0)
            }
            ChromeOverlay::Indicator => 0,
        };
        match overlay {
            ChromeOverlay::Instrument => {
                self.instrument_exchange = InstrumentExchangeUi::Idle(match self.provider {
                    TerminalProvider::Rithmic => assets::ExchangeLogo::Rithmic,
                    TerminalProvider::Hyperliquid => assets::ExchangeLogo::Hyperliquid,
                });
                if let Some(input) = &self.symbol_input {
                    input.update(cx, |input, input_cx| input.focus(window, input_cx));
                }
                self.refresh_default_instrument_listing(cx);
                self.sync_instrument_menu_keyboard(cx);
            }
            ChromeOverlay::Indicator => {
                self.indicator_input
                    .update(cx, |input, input_cx| input.focus(window, input_cx));
            }
            ChromeOverlay::QuickTimeframe => {
                self.timeframe_input
                    .update(cx, |input, input_cx| input.focus(window, input_cx));
            }
            ChromeOverlay::Timeframe | ChromeOverlay::ChartType => {
                self.chrome_focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn close_chrome_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chrome_overlay.is_none() || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
        {
            return;
        }
        match self.chrome_overlay {
            Some(ChromeOverlay::Indicator) => {
                self.indicator_input.update(cx, |input, input_cx| {
                    input.set_value("", window, input_cx);
                });
            }
            Some(ChromeOverlay::QuickTimeframe) => {
                self.timeframe_input.update(cx, |input, input_cx| {
                    input.set_value("", window, input_cx);
                });
            }
            Some(
                ChromeOverlay::Instrument | ChromeOverlay::Timeframe | ChromeOverlay::ChartType,
            )
            | None => {}
        }
        self.chrome_focus.focus(window, cx);
        if cx.reduce_motion() {
            self.chrome_overlay = None;
            self.timeframe_menu_flyout = None;
            self.menu_state.timeframe_flyout_keyboard = false;
            self.menu_state.chrome_list_keyboard = false;
            self.timeframe_hover_regions = 0;
            cx.notify();
            return;
        }

        self.chrome_overlay_generation = self.chrome_overlay_generation.saturating_add(1);
        self.chrome_overlay_phase = ChromeOverlayPhase::Closing;
        let generation = self.chrome_overlay_generation;
        cx.spawn_in(window, async move |app, cx| {
            cx.background_executor()
                .timer(CHROME_OVERLAY_TRANSITION_DURATION)
                .await;
            let _ = app.update_in(cx, |app, _, app_cx| {
                if should_finish_chrome_overlay_close(
                    app.chrome_overlay_phase,
                    app.chrome_overlay_generation,
                    generation,
                ) {
                    app.chrome_overlay = None;
                    app.timeframe_menu_flyout = None;
                    app.menu_state.timeframe_flyout_keyboard = false;
                    app.menu_state.chrome_list_keyboard = false;
                    app.timeframe_hover_regions = 0;
                    app.chrome_overlay_phase = ChromeOverlayPhase::Opening;
                    app_cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn toggle_instrument_exchange_menu(&mut self, cx: &mut Context<Self>) {
        self.instrument_exchange = match self.instrument_exchange {
            InstrumentExchangeUi::Idle(exchange) => InstrumentExchangeUi::Menu(exchange),
            InstrumentExchangeUi::Menu(exchange) => InstrumentExchangeUi::Idle(exchange),
        };
        cx.notify();
    }

    pub(super) fn set_instrument_catalog_exchange(
        &mut self,
        exchange: assets::ExchangeLogo,
        cx: &mut Context<Self>,
    ) {
        self.instrument_exchange = InstrumentExchangeUi::Idle(exchange);
        self.chrome_selection = 0;
        cx.notify();
    }

    pub(super) fn on_terminal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(command) =
            fullscreen_escape_command(event.keystroke.key.as_str(), window.is_fullscreen())
        {
            command.execute(window);
            return true;
        }
        if self.consume_chrome_typeahead(event, window, cx) {
            return true;
        }
        if self.chrome_overlay.is_none() || self.chrome_overlay_phase == ChromeOverlayPhase::Closing
        {
            return false;
        }
        match event.keystroke.key.as_str() {
            "escape" => {
                if let InstrumentExchangeUi::Menu(exchange) = self.instrument_exchange {
                    self.instrument_exchange = InstrumentExchangeUi::Idle(exchange);
                    cx.notify();
                } else if self.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && self.timeframe_menu_flyout.is_some()
                {
                    self.close_timeframe_flyout(cx);
                } else {
                    self.close_chrome_overlay(window, cx);
                }
            }
            "up" => {
                if self.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && self.timeframe_menu_flyout.is_some()
                    && !self.menu_state.timeframe_flyout_keyboard
                {
                    self.menu_state.timeframe_flyout_keyboard = true;
                    cx.notify();
                    return true;
                }
                self.activate_chrome_list_keyboard();
                self.chrome_selection = self.chrome_selection.saturating_sub(1);
                cx.notify();
            }
            "down" => {
                if self.chrome_overlay == Some(ChromeOverlay::Timeframe)
                    && self.timeframe_menu_flyout.is_some()
                    && !self.menu_state.timeframe_flyout_keyboard
                {
                    self.menu_state.timeframe_flyout_keyboard = true;
                    cx.notify();
                    return true;
                }
                let count = match self.chrome_overlay {
                    Some(ChromeOverlay::Instrument) => self.instrument_entries(cx).len(),
                    Some(ChromeOverlay::Indicator) => chart_chrome::filter_indicator_specs(
                        self.indicator_input.read(cx).value().as_ref(),
                    )
                    .len(),
                    Some(ChromeOverlay::Timeframe) => self.timeframe_menu_keyboard_count(),
                    Some(ChromeOverlay::ChartType) => ChartType::ALL.len(),
                    Some(ChromeOverlay::QuickTimeframe) => self.quick_timeframe_matches(cx).len(),
                    None => 0,
                };
                self.activate_chrome_list_keyboard();
                self.chrome_selection = (self.chrome_selection + 1).min(count.saturating_sub(1));
                cx.notify();
            }
            "left" if self.chrome_overlay == Some(ChromeOverlay::Timeframe) => {
                self.close_timeframe_flyout(cx);
            }
            "right" if self.chrome_overlay == Some(ChromeOverlay::Timeframe) => {
                if self.timeframe_menu_flyout.is_none()
                    && let Some(group) = timeframe_menu_groups(self.available_intervals())
                        .get(self.chrome_selection)
                        .copied()
                {
                    self.open_timeframe_group(group, true, cx);
                }
            }
            "enter" => return self.handle_chrome_enter(window, cx),
            _ => return false,
        }
        true
    }

    fn handle_chrome_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.chrome_overlay {
            Some(ChromeOverlay::Timeframe) => {
                if let Some(group) = self.timeframe_menu_flyout {
                    if self.menu_state.timeframe_flyout_keyboard {
                        let intervals =
                            timeframe_group_intervals(group, self.available_intervals());
                        self.apply_highlighted_interval(&intervals, window, cx);
                    } else {
                        self.menu_state.timeframe_flyout_keyboard = true;
                        cx.notify();
                    }
                } else if let Some(group) = timeframe_menu_groups(self.available_intervals())
                    .get(self.chrome_selection)
                    .copied()
                {
                    self.open_timeframe_group(group, true, cx);
                }
            }
            Some(ChromeOverlay::ChartType) => self.apply_highlighted_chart_type(window, cx),
            Some(ChromeOverlay::QuickTimeframe) => {
                let intervals = self.quick_timeframe_matches(cx);
                self.apply_highlighted_interval(&intervals, window, cx);
            }
            Some(ChromeOverlay::Instrument | ChromeOverlay::Indicator) | None => return false,
        }
        true
    }

    fn consume_chrome_typeahead(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if chrome_typeahead_blocked(event) {
            return false;
        }
        if self.chrome_overlay_phase == ChromeOverlayPhase::Closing {
            return false;
        }
        if self.drawing_toolbar_state(cx).active_tool == ChartDrawingTool::Text {
            return false;
        }
        if self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).is_editing_text())
        {
            return false;
        }
        let Some(typed) = chrome_typeahead_char(event) else {
            return false;
        };
        if event.is_held && self.chrome_overlay.is_none() {
            return false;
        }
        match self.chrome_overlay {
            None if typed.is_ascii_digit() => {
                self.begin_quick_timeframe(typed, window, cx);
                true
            }
            None if typed.is_ascii_alphabetic() => self.begin_symbol_typeahead(typed, window, cx),
            Some(
                ChromeOverlay::Instrument
                | ChromeOverlay::Indicator
                | ChromeOverlay::Timeframe
                | ChromeOverlay::QuickTimeframe
                | ChromeOverlay::ChartType,
            )
            | None => false,
        }
    }

    fn begin_quick_timeframe(&mut self, typed: char, window: &mut Window, cx: &mut Context<Self>) {
        self.timeframe_input.update(cx, |input, input_cx| {
            input.set_value(typed.to_string(), window, input_cx);
            input.focus(window, input_cx);
        });
        self.open_chrome_overlay(ChromeOverlay::QuickTimeframe, window, cx);
    }

    fn begin_symbol_typeahead(
        &mut self,
        typed: char,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(input) = self.symbol_input.clone() else {
            return false;
        };
        let query = typed.to_string();
        input.update(cx, |input, input_cx| {
            input.set_value(&query, window, input_cx);
            input.focus(window, input_cx);
        });
        self.search_symbol_query(&query, cx);
        self.open_chrome_overlay(ChromeOverlay::Instrument, window, cx);
        true
    }

    #[cfg(feature = "diagnostics")]
    fn record_live_evidence_publication(&mut self, update: &ReplayStreamUpdate) {
        if !self.live_evidence_enabled || self.live_evidence_publications >= 256 {
            return;
        }
        self.live_evidence_publications = self.live_evidence_publications.saturating_add(1);
        match update {
            ReplayStreamUpdate::Snapshot(snapshot) => {
                let interval_nanos = (snapshot.bar_definition().interval_seconds > 0).then(|| {
                    i64::from(snapshot.bar_definition().interval_seconds)
                        .saturating_mul(1_000_000_000)
                });
                let interior_gaps = interval_nanos.map(|interval_nanos| {
                    snapshot
                        .bars()
                        .windows(2)
                        .filter(|pair| {
                            pair[1]
                                .provenance()
                                .exchange_timestamp_unix_nanos
                                .saturating_sub(pair[0].provenance().exchange_timestamp_unix_nanos)
                                != interval_nanos
                        })
                        .count()
                });
                let first_timestamp = snapshot
                    .bars()
                    .first()
                    .map_or(0, |bar| bar.provenance().exchange_timestamp_unix_nanos);
                let last_timestamp = snapshot
                    .bars()
                    .last()
                    .map_or(0, |bar| bar.provenance().exchange_timestamp_unix_nanos);
                let interior_gaps =
                    interior_gaps.map_or_else(|| "null".to_string(), |value| value.to_string());
                let interval_nanos =
                    interval_nanos.map_or_else(|| "null".to_string(), |value| value.to_string());
                eprintln!(
                    "AXIUSFLOW_LIVE_SNAPSHOT {{\"bar_count\":{},\"first_timestamp\":{first_timestamp},\"last_timestamp\":{last_timestamp},\"interior_gaps\":{interior_gaps},\"interval_nanos\":{interval_nanos}}}",
                    snapshot.bars().len()
                );
            }
            ReplayStreamUpdate::Delta(delta) => eprintln!(
                "AXIUSFLOW_LIVE_UPDATE {{\"kind\":\"delta\",\"timestamp\":{}}}",
                delta.item().provenance().exchange_timestamp_unix_nanos
            ),
            ReplayStreamUpdate::Tail(tail) => eprintln!(
                "AXIUSFLOW_LIVE_UPDATE {{\"kind\":\"tail\",\"timestamp\":{},\"forming\":{}}}",
                tail.item().provenance().exchange_timestamp_unix_nanos,
                tail.forming()
            ),
        }
    }

    pub(super) fn apply_publication(
        &mut self,
        publication: MarketWorkerPublication,
        cx: &mut Context<Self>,
    ) {
        #[cfg(feature = "diagnostics")]
        self.record_live_evidence_publication(&publication.update);
        let MarketWorkerPublication {
            update,
            generation,
            subscription_id,
            worker_label,
            ui_diagnostics,
        } = publication;
        self.worker_label = worker_label;
        self.subscription_id = subscription_id;
        self.replay_label =
            generation_status(&self.worker_label, &self.subscription_id, generation);
        // A switch that has been committed but not yet drawn keeps the previous
        // chart on screen. That chart belongs to the previous series, so the
        // replacement's incremental updates must not reach it; only its covering
        // snapshot may, and that snapshot is what swaps the chart.
        let swapping = self.rithmic_switch.is_swapping();
        let next_state = match (&self.chart, update) {
            (existing, axiusflow_application::ReplayStreamUpdate::Snapshot(snapshot))
                if existing.is_none() || swapping =>
            {
                let chart_theme = nucleus_chart_theme(self.theme.mode);
                let chart = cx
                    .new(move |_| NucleusChartView::with_replay_and_theme(&snapshot, chart_theme));
                self.apply_chart_chrome_to_chart(&chart, cx);
                self.apply_retained_indicators_to_chart(&chart, cx);
                if let Some((start, end)) = self.restored_viewport {
                    chart.update(cx, |chart, _| {
                        chart.set_visible_time_range_unix_nanos(start, end);
                    });
                }
                observe_chart(Some(&chart), cx);
                self.chart = Some(chart);
                self.rithmic_switch = RithmicSwitchState::Initializing;
                ChartState::Ready
            }
            (Some(_), _) if swapping => {
                // The replacement has not arrived yet; the previous chart stays
                // as it is rather than being fed another series' bars.
                return;
            }
            (Some(chart), update) => {
                let (accepted, recovery_pending) = chart.update(cx, |chart, _| {
                    let accepted = chart.try_queue_replay_update(update).is_ok();
                    if !accepted {
                        eprintln!("bounded chart queue overflowed; fixture resnapshot required");
                    }
                    (accepted, chart.replay_bridge_metrics().recovery_pending)
                });
                publication_chart_state(accepted, recovery_pending)
            }
            (None, _) => {
                self.reject_incremental_publication(ui_diagnostics, cx);
                return;
            }
        };
        if let Some(diagnostics) = ui_diagnostics
            && let Some(replaced) = self.pending_ui_diagnostics.replace(diagnostics)
        {
            self.market_worker
                .send_ui_diagnostics(UiDiagnosticsFeedback::Coalesced {
                    generation: replaced.generation(),
                });
        }
        // A publication says bars arrived, not that they are current. The engine
        // reports readiness separately, and while it is still loading current
        // coverage the chart is showing retained history — promoting it here is
        // what presented a stale chart as ready for the seconds before the
        // provider page and the live handoff landed.
        if next_state == ChartState::Ready && self.chart_state == ChartState::Loading {
            cx.notify();
        } else if next_state == ChartState::Ready {
            self.chart_state = ChartState::Ready;
            self.chart_state_message = "market snapshot is current".to_string();
            if self.provider == TerminalProvider::Rithmic {
                self.market_state.symbol_selection_pending = false;
                self.symbol_message = self.product.as_ref().map_or_else(
                    || "Rithmic market ready".to_string(),
                    |product| format!("{} · Rithmic spot", product.provider_symbol),
                );
            } else if self.provider == TerminalProvider::Hyperliquid {
                self.market_state.symbol_selection_pending = false;
                self.symbol_message = self.product.as_ref().map_or_else(
                    || "Hyperliquid market ready".to_string(),
                    |product| format!("{} · Hyperliquid", product.display_symbol),
                );
            }
        } else {
            self.set_chart_state(
                ChartState::Recovering,
                "chart update requires a correlated covering snapshot".to_string(),
                cx,
            );
        }
    }

    fn reject_incremental_publication(
        &mut self,
        diagnostics: Option<PendingUiDiagnostics>,
        cx: &mut Context<Self>,
    ) {
        if let Some(diagnostics) = diagnostics {
            self.market_worker
                .send_ui_diagnostics(UiDiagnosticsFeedback::Coalesced {
                    generation: diagnostics.generation(),
                });
        }
        self.set_chart_state(
            ChartState::Error,
            "market update arrived before the initial covering snapshot".to_string(),
            cx,
        );
    }

    fn apply_recovery(
        &mut self,
        request_id: u64,
        result: Result<MarketWorkerBootstrap, String>,
        cx: &mut Context<Self>,
    ) {
        let bootstrap = match result {
            Ok(bootstrap) => bootstrap,
            Err(error) => {
                let current = self.chart.as_ref().is_some_and(|chart| {
                    chart.update(cx, |chart, chart_cx| {
                        let current = chart.mark_replay_recovery_failed(request_id);
                        if current {
                            chart_cx.notify();
                        }
                        current
                    })
                });
                if !current {
                    return;
                }
                self.apply_market_state_message(ChartState::Error, error.clone(), cx);
                eprintln!("fixture recovery {request_id} failed: {error}");
                return;
            }
        };
        let Some(chart) = &self.chart else {
            self.set_chart_state(
                ChartState::Error,
                "recovery response arrived before the initial snapshot".to_string(),
                cx,
            );
            return;
        };
        let install = chart.update(cx, |chart, chart_cx| {
            let installed = chart.install_replay_recovery(request_id, &bootstrap.snapshot);
            chart_cx.notify();
            installed
        });
        match install {
            Ok(true) => {
                self.replay_label = generation_status(
                    &self.worker_label,
                    &bootstrap.subscription_id,
                    MarketPublicationGeneration::from_generation(&bootstrap.generation),
                );
                self.apply_market_state_message(
                    ChartState::Ready,
                    "market snapshot is current".to_string(),
                    cx,
                );
            }
            Ok(false) => {
                let metrics = chart.read(cx).replay_bridge_metrics();
                if metrics.snapshot_required && !metrics.recovery_pending {
                    self.apply_market_state_message(
                        ChartState::Error,
                        "chart recovery exhausted its snapshot retry budget".to_string(),
                        cx,
                    );
                }
            }
            Err(error) => {
                chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(request_id);
                    chart_cx.notify();
                });
                self.apply_market_state_message(ChartState::Error, error.to_string(), cx);
                eprintln!("fixture recovery {request_id} was rejected: {error}");
            }
        }
    }

    fn mark_market_stream_invalid(&mut self, message: &str, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.mark_replay_stream_invalid();
                chart_cx.notify();
            });
        }
        eprintln!("market worker invalidated the stream: {message}");
    }

    /// Reports whether an engine selection handoff must keep the chart covered.
    ///
    /// Before the replacement snapshot arrives, the pixels belong to the series
    /// the trader just left. After that first snapshot arrives, the replacement
    /// can still be retained/partial history until the provider-history/live
    /// handoff declares it current. Both phases are deliberately hidden behind
    /// the neutral loading surface so a stale range cannot appear to "wake up"
    /// and jump when current coverage lands.
    pub(super) fn showing_superseded_series(&self) -> bool {
        match self.provider {
            TerminalProvider::Rithmic | TerminalProvider::Hyperliquid => {
                switch_requires_chart_cover(self.chart.is_some(), self.rithmic_switch)
            }
        }
    }

    fn set_chart_state(&mut self, state: ChartState, message: String, cx: &mut Context<Self>) {
        if matches!(state, ChartState::Stale | ChartState::Recovering) {
            self.mark_market_stream_invalid(&message, cx);
        }
        self.chart_state = state;
        self.chart_state_message = message;
        cx.notify();
    }

    fn dispatch_recovery(&mut self, cx: &mut Context<Self>) {
        if !self.market_worker.is_connected() {
            return;
        }
        let Some(chart) = &self.chart else {
            return;
        };
        let worker = &self.market_worker;
        let dispatch = chart.update(cx, |chart, chart_cx| {
            let result =
                chart.try_dispatch_replay_recovery(|command| worker.try_send_recovery(command));
            if result.as_ref().is_ok_and(|dispatched| *dispatched) {
                chart_cx.notify();
            }
            result
        });
        match dispatch {
            Ok(_) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(command)) => {
                self.market_worker.mark_disconnected();
                chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(command.request_id);
                    chart_cx.notify();
                });
            }
        }
    }

    fn apply_market_worker_message(
        &mut self,
        message: MarketWorkerMessage,
        cx: &mut Context<Self>,
    ) {
        match message {
            MarketWorkerMessage::Update(publication) => {
                self.apply_publication(publication, cx);
            }
            MarketWorkerMessage::Diagnostics(snapshot) => {
                #[cfg(feature = "diagnostics")]
                eprintln!("desktop market diagnostics: {snapshot:?}");
                #[cfg(not(feature = "diagnostics"))]
                drop(snapshot);
            }
            MarketWorkerMessage::Recovery { request_id, result } => {
                self.apply_recovery(request_id, result, cx);
            }
            MarketWorkerMessage::State { state, message } => {
                self.apply_market_state_message(state, message, cx);
            }
            MarketWorkerMessage::EngineSwitchMarker { sequence } => {
                self.apply_rithmic_switch_marker(sequence, cx);
            }
            MarketWorkerMessage::Connection {
                state,
                message,
                transport_rtt_nanos,
            } => {
                self.apply_market_connection_message(state, message, transport_rtt_nanos, cx);
            }
            MarketWorkerMessage::ProviderCatalog(event) => {
                self.apply_provider_catalog_event(event, cx);
            }
            MarketWorkerMessage::RithmicHistory {
                selection_generation,
                series_generation,
                result,
            } => {
                self.apply_rithmic_history(selection_generation, series_generation, result, cx);
            }
            MarketWorkerMessage::RithmicLive {
                selection_generation,
                series_generation,
                update,
            } => {
                self.apply_rithmic_live(selection_generation, series_generation, update, cx);
            }
            MarketWorkerMessage::OrderBook(frame) => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.replace_frame(frame, order_book_cx)
                });
            }
            MarketWorkerMessage::ChartViewport {
                start_unix_nanos,
                end_unix_nanos,
            } => {
                let viewport = (start_unix_nanos, end_unix_nanos);
                self.restored_viewport = Some(viewport);
                self.last_persisted_viewport = Some(viewport);
                if let Some(chart) = &self.chart {
                    chart.update(cx, |chart, chart_cx| {
                        if chart.set_visible_time_range_unix_nanos(start_unix_nanos, end_unix_nanos)
                        {
                            chart_cx.notify();
                        }
                    });
                }
            }
        }
    }

    fn apply_market_state_message(
        &mut self,
        state: ChartState,
        message: String,
        cx: &mut Context<Self>,
    ) {
        // The pending-switch tracker is provider-neutral: both engine
        // providers resolve selections through the same marker flow.
        let engine_provider = matches!(
            self.provider,
            TerminalProvider::Rithmic | TerminalProvider::Hyperliquid
        );
        if self.stale_ready_during_engine_switch(state, engine_provider) {
            return;
        }
        if state == ChartState::Error && engine_provider {
            let swapping = self.rithmic_switch.is_swapping();
            self.rithmic_switch = RithmicSwitchState::Idle;
            self.rithmic_pending_interval = None;
            self.rithmic_pending_product = None;
            self.rithmic_pending_sequence = None;
            self.market_state.symbol_selection_pending = false;
            if swapping {
                self.restore_rithmic_selection_after_failure(&message, cx);
            } else {
                self.rithmic_previous_selection = None;
            }
        } else if self.provider == TerminalProvider::Rithmic && state == ChartState::Ready {
            self.rithmic_switch = RithmicSwitchState::Idle;
            self.rithmic_previous_selection = None;
            self.market_state.symbol_selection_pending = false;
            self.symbol_message = self.product.as_ref().map_or_else(
                || "Rithmic market ready".to_string(),
                |product| format!("{} · Rithmic spot", product.provider_symbol),
            );
        } else if self.provider == TerminalProvider::Hyperliquid && state == ChartState::Ready {
            self.rithmic_switch = RithmicSwitchState::Idle;
            self.rithmic_previous_selection = None;
            self.market_state.symbol_selection_pending = false;
            self.symbol_message = self.product.as_ref().map_or_else(
                || "Hyperliquid market ready".to_string(),
                |product| format!("{} · Hyperliquid", product.display_symbol),
            );
        }
        self.set_chart_state(state, message, cx);
    }

    fn apply_market_connection_message(
        &mut self,
        state: FeedConnectionState,
        message: String,
        transport_rtt_nanos: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        self.provider_transport_rtt_nanos = if state == FeedConnectionState::Streaming {
            transport_rtt_nanos
        } else {
            None
        };
        self.apply_connection_state(state, message, cx);
    }

    fn stale_ready_during_engine_switch(&self, state: ChartState, engine_provider: bool) -> bool {
        // The previous series can report one last Ready after the catalog
        // response but before the worker processes the new EngineSelect
        // command. Keep the switch pending until its marker/snapshot lands.
        state == ChartState::Ready
            && engine_provider
            && !ready_state_can_complete_switch(self.rithmic_switch)
    }

    fn apply_provider_catalog_event(
        &mut self,
        event: ProviderCatalogEvent,
        cx: &mut Context<Self>,
    ) {
        self.apply_catalog_event(event, cx);
    }

    fn apply_catalog_results(
        &mut self,
        generation: u64,
        instruments: Vec<ProviderInstrumentSummary>,
    ) -> Option<usize> {
        let generation = usize_generation(generation)?;
        let count = instruments.len();
        self.symbol_browser
            .apply_results(generation, instruments)
            .then_some(count)
    }

    fn confirm_catalog_selection(&mut self, command_generation: u64) -> bool {
        usize_generation(command_generation)
            .is_some_and(|generation| self.symbol_browser.confirm_selection(generation))
    }

    fn consume_catalog_search_authorization(&mut self) {
        if let Some(search_generation) = self
            .symbol_browser
            .selected()
            .map(|selection| selection.search_generation)
        {
            self.symbol_browser
                .consume_completed_search(search_generation);
        }
    }

    /// Commits a Rithmic switch's identity without touching the chart.
    ///
    /// The marker only says "everything after this belongs to the new
    /// selection". The chart the trader is looking at is left on screen — still
    /// its own series, still correct — under a loading notice, and is replaced
    /// in `apply_publication` when the replacement's covering snapshot arrives.
    /// Dropping it here is what produced the blank surface on every switch.
    fn apply_rithmic_switch_marker(&mut self, sequence: u64, cx: &mut Context<Self>) {
        if !matches!(
            self.provider,
            TerminalProvider::Rithmic | TerminalProvider::Hyperliquid
        ) || !self.rithmic_switch.is_pending()
            || self.rithmic_pending_sequence != Some(sequence)
        {
            return;
        }
        self.rithmic_previous_selection = Some((self.product.clone(), self.interval));
        if let Some(interval) = self.rithmic_pending_interval.take() {
            self.interval = interval;
        }
        if let Some(product) = self.rithmic_pending_product.take() {
            self.product = Some(product);
            // Price levels belong to one instrument: a product switch drops
            // the old book back to loading instead of showing BTC levels
            // under an ETH selection. Interval-only switches keep the book.
            self.order_book.update(cx, |order_book, order_book_cx| {
                order_book.clear(order_book_cx);
            });
        }
        self.rithmic_pending_sequence = None;
        self.rithmic_switch = if self.chart.is_some() {
            RithmicSwitchState::Swapping
        } else {
            RithmicSwitchState::Idle
        };
        self.retain_chart_presentation(cx);
        self.restored_viewport = None;
        self.last_persisted_viewport = None;
        self.chart_state = ChartState::Loading;
        cx.notify();
    }

    /// Restores the selection a failed switch was replacing.
    ///
    /// The chart on screen is still the previous series, so restoring means
    /// re-stating its demand and reporting an actionable error over it — never
    /// leaving the trader on a surface with no data and no way back.
    fn restore_rithmic_selection_after_failure(&mut self, detail: &str, cx: &mut Context<Self>) {
        let Some((product, interval)) = self.rithmic_previous_selection.take() else {
            return;
        };
        self.product.clone_from(&product);
        self.interval = interval;
        self.rithmic_pending_interval = None;
        self.rithmic_pending_product = None;
        self.rithmic_pending_sequence = None;
        self.rithmic_switch = RithmicSwitchState::Idle;
        self.order_book.update(cx, |order_book, order_book_cx| {
            order_book.clear(order_book_cx);
        });
        let restored = product
            .and_then(|product| self.market_worker.try_select_engine(product, interval).ok());
        if let Some(sequence) = restored {
            self.rithmic_pending_sequence = Some(sequence);
            self.rithmic_pending_interval = Some(interval);
            self.rithmic_switch = RithmicSwitchState::Pending;
        }
        self.series_message = format!("{detail} — showing {}", interval.label());
        cx.notify();
    }

    pub(super) fn poll_market_worker(&mut self, cx: &mut Context<Self>) -> usize {
        let chart_was_missing = self.chart.is_none();
        let (messages, disconnected) = self
            .market_worker
            .drain_messages_up_to(MARKET_MESSAGES_PER_FRAME);
        let applied = messages.len();
        let chart_update_received = messages
            .iter()
            .any(|message| matches!(message, MarketWorkerMessage::Update(_)));
        for message in messages {
            self.apply_market_worker_message(message, cx);
        }
        if matches!(
            self.provider,
            TerminalProvider::Rithmic | TerminalProvider::Hyperliquid
        ) && disconnected
            && !matches!(self.connection_state, Some(FeedConnectionState::Stopped))
        {
            let display = terminal_provider_display(self.provider);
            let fallback = format!("{display} market worker stopped");
            let detail =
                stopped_worker_chart_detail(self.chart_state, &self.chart_state_message, &fallback);
            self.apply_connection_state(FeedConnectionState::Stopped, fallback, cx);
            if self.chart_state == ChartState::Error && self.chart_state_message != detail {
                self.chart_state_message = detail;
                cx.notify();
            }
        } else if disconnected && self.chart_state != ChartState::Error {
            let message = match self.provider {
                TerminalProvider::Rithmic => "Rithmic market worker stopped",
                TerminalProvider::Hyperliquid => "Hyperliquid market worker stopped",
            }
            .to_string();
            self.connection_state = Some(FeedConnectionState::Stopped);
            self.connection_message = Some(message.clone());
            self.set_chart_state(ChartState::Error, message, cx);
        }
        self.dispatch_recovery(cx);
        self.dispatch_retained_symbol_search(cx);

        let status = self.chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| {
                let metrics = chart.read(cx).replay_bridge_metrics();
                let reconciled =
                    reconciled_bridge_state(self.chart_state, metrics.recovery_pending);
                if reconciled != self.chart_state {
                    self.chart_state = reconciled;
                    self.chart_state_message =
                        "chart validation requires a correlated covering snapshot".to_string();
                    cx.notify();
                }
                bridge_status(metrics)
            },
        );
        if self.bridge_label != status {
            self.bridge_label = status;
            cx.notify();
        }
        let loading = self.chart_state == ChartState::Loading;
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.set_asset_loading(loading) {
                    chart_cx.notify();
                }
            });
        }
        if applied > 0 {
            if chart_was_missing && self.chart.is_some() {
                cx.notify();
            } else if chart_update_received && let Some(chart) = &self.chart {
                chart.update(cx, |_, chart_cx| chart_cx.notify());
            }
        }
        applied + usize::from(disconnected)
    }

    fn apply_connection_state(
        &mut self,
        state: FeedConnectionState,
        message: String,
        cx: &mut Context<Self>,
    ) {
        let state = stabilized_connection_state(self.connection_state, state);
        if state != FeedConnectionState::Streaming {
            self.provider_transport_rtt_nanos = None;
        }
        let retirement = RithmicSessionRetirement::from_connection(state);
        let retained_market_data = self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).has_market_data());
        // The legacy Rithmic session is re-driven from the desktop on
        // retirement. The resident engine owns Hyperliquid recovery
        // end to end (reconnect, resubscribe, history refresh), so a
        // Hyperliquid disconnect must not tear down selections here.
        let legacy_session = self.provider == TerminalProvider::Rithmic;
        match retirement {
            RithmicSessionRetirement::Offline | RithmicSessionRetirement::Recovering
                if legacy_session =>
            {
                self.begin_rithmic_reconnect(cx);
            }
            RithmicSessionRetirement::Stopped if legacy_session => {
                self.rithmic_reconnect = RithmicReconnectState::Idle;
                self.retire_rithmic_session(cx);
            }
            RithmicSessionRetirement::Offline
            | RithmicSessionRetirement::Recovering
            | RithmicSessionRetirement::Stopped
            | RithmicSessionRetirement::None => {}
        }
        if let Some(chart_state) = retirement.chart_state(retained_market_data) {
            self.chart_state = chart_state;
            self.chart_state_message.clone_from(&message);
        }
        self.connection_state = Some(state);
        // Depth follows the same honesty rule as the empty panel: a fresh
        // demand restarts from loading, and only a concrete stop marks the
        // book unavailable. An engine replacement additionally clears books
        // from the dead incarnation, whose reset generations would fence
        // every new frame out forever.
        match state {
            FeedConnectionState::Disconnected => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.set_connection_state(
                        axiusflow_terminal_ui::OrderBookConnectionState::Offline,
                        order_book_cx,
                    );
                });
            }
            FeedConnectionState::Discovering
            | FeedConnectionState::Authenticating
            | FeedConnectionState::Recovering => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    if message == engine_market_worker::ENGINE_RESTARTED_MESSAGE {
                        order_book.clear(order_book_cx);
                    }
                    order_book.set_connection_state(
                        axiusflow_terminal_ui::OrderBookConnectionState::Recovering,
                        order_book_cx,
                    );
                });
            }
            FeedConnectionState::Streaming => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.set_connection_state(
                        axiusflow_terminal_ui::OrderBookConnectionState::Online,
                        order_book_cx,
                    );
                });
            }
            FeedConnectionState::Stopped => {
                self.order_book.update(cx, |order_book, order_book_cx| {
                    order_book.mark_unavailable(order_book_cx);
                });
            }
        }
        let ready_action = rithmic_ready_action(
            state,
            &message,
            &self.rithmic_reconnect,
            self.market_state.rithmic_autoload_started,
        );
        self.connection_message = Some(stable_connection_message(state, message));
        match ready_action {
            RithmicReadyAction::Reconnect(symbol) => {
                if self.search_symbol_query(&symbol, cx)
                    && let RithmicReconnectState::AwaitingSearch(target) = &self.rithmic_reconnect
                {
                    self.rithmic_reconnect = RithmicReconnectState::SearchInFlight(target.clone());
                }
            }
            RithmicReadyAction::Autoload => {
                self.market_state.rithmic_autoload_started = true;
                let _ = self.search_symbol_query(DEFAULT_RITHMIC_LISTING_QUERY, cx);
            }
            RithmicReadyAction::None => {}
        }
        cx.notify();
    }

    pub(super) fn apply_theme(&mut self, theme: &AxiusflowTheme, cx: &mut Context<Self>) {
        self.order_book.update(cx, |order_book, order_book_cx| {
            order_book.set_theme(*theme, order_book_cx);
        });
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_theme(nucleus_chart_theme(theme.mode));
                chart_cx.notify();
            });
        }
        self.theme = *theme;
        cx.notify();
    }

    pub(super) fn apply_indicator_chrome_preferences(
        &mut self,
        names: bool,
        values: bool,
        price_lines: bool,
        cx: &mut Context<Self>,
    ) {
        self.chart_chrome.indicator_name_labels_visible = names;
        self.chart_chrome.indicator_value_labels_visible = values;
        self.chart_chrome.indicator_price_lines_visible = price_lines;
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.apply_indicator_chrome_preferences(names, values, price_lines);
                chart_cx.notify();
            });
        }
    }

    fn apply_chart_chrome_to_chart(
        &self,
        chart: &Entity<NucleusChartView>,
        cx: &mut Context<Self>,
    ) {
        chart.update(cx, |chart, _| {
            chart.apply_indicator_chrome_preferences(
                self.chart_chrome.indicator_name_labels_visible,
                self.chart_chrome.indicator_value_labels_visible,
                self.chart_chrome.indicator_price_lines_visible,
            );
            chart.set_chart_type(self.chart_chrome.chart_type);
            let _ = chart.apply_price_axis_menu_action(
                0,
                false,
                PriceAxisMenuAction::SetPrecision(self.retained_chart_presentation.price_precision),
            );
        });
    }

    fn retain_chart_presentation(&mut self, cx: &App) {
        if let Some(chart) = &self.chart
            && chart.read(cx).has_market_data()
        {
            self.retained_chart_presentation.indicators = chart.read(cx).indicator_states();
            self.retained_chart_presentation.price_precision =
                chart.read(cx).selected_price_precision();
        }
    }

    fn apply_retained_indicators_to_chart(
        &self,
        chart: &Entity<NucleusChartView>,
        cx: &mut Context<Self>,
    ) {
        if self.retained_chart_presentation.indicators.is_empty() {
            return;
        }
        let states = self.retained_chart_presentation.indicators.clone();
        let result = chart.update(cx, |chart, _| chart.restore_indicator_states(&states));
        if let Err(error) = result {
            eprintln!("Axiusflow chart indicators could not be restored: {error}");
        }
    }

    pub(super) fn chart_type(&self, cx: &App) -> ChartType {
        self.chart
            .as_ref()
            .map_or(self.chart_chrome.chart_type, |chart| {
                chart.read(cx).chart_type()
            })
    }

    pub(super) fn set_chart_type(&mut self, chart_type: ChartType, cx: &mut Context<Self>) {
        self.chart_chrome.chart_type = chart_type;
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_chart_type(chart_type);
                chart_cx.notify();
            });
        }
        let preferences = self.chart_chrome;
        cx.background_executor()
            .spawn(async move {
                if let Err(error) = chart_chrome::save_chart_chrome_preferences(preferences) {
                    eprintln!("Axiusflow chart chrome could not be saved: {error}");
                }
            })
            .detach();
        cx.notify();
    }

    fn refresh_default_instrument_listing(&mut self, cx: &mut Context<Self>) {
        if !instrument_listing_refresh_needed(
            &self.symbol_browser,
            self.market_state.symbol_selection_pending,
        ) {
            return;
        }
        let _ = self.search_symbol_query("", cx);
    }

    fn search_symbol_query(&mut self, query: &str, cx: &mut Context<Self>) -> bool {
        if self.symbol_browser.search_pending() {
            match self.symbol_browser.retain_latest_search(query) {
                Ok(already_dispatched) => {
                    if !already_dispatched {
                        let display = terminal_provider_display(self.provider);
                        self.symbol_message =
                            format!("Waiting to search the latest {display} query");
                    }
                    cx.notify();
                    return already_dispatched;
                }
                Err(message) => {
                    self.symbol_message = message.to_string();
                    cx.notify();
                }
            }
            return false;
        }
        let request = match self.symbol_browser.begin_search(query) {
            Ok(request) => request,
            Err(message) => {
                self.symbol_message = message.to_string();
                cx.notify();
                return false;
            }
        };
        self.dispatch_symbol_search(request, cx)
    }

    fn dispatch_symbol_search(
        &mut self,
        request: rithmic_shell::RithmicSymbolSearchRequest,
        cx: &mut Context<Self>,
    ) -> bool {
        let provider = terminal_provider_id(self.provider);
        let retained_query = request.query.clone();
        let request_id = request.request_id;
        let search = SearchProviderInstruments {
            consumer_id: 0,
            search_generation: u64::try_from(request_id.get()).unwrap_or(u64::MAX),
            provider: provider.to_string(),
            query: request.query,
            maximum_results: u32::try_from(self.symbol_browser.maximum_results())
                .unwrap_or(u32::MAX),
        };
        let dispatched = if self.market_worker.try_search_provider(search).is_ok() {
            self.symbol_message = match self.provider {
                TerminalProvider::Rithmic => "Searching Rithmic spot markets".to_string(),
                TerminalProvider::Hyperliquid => "Searching Hyperliquid markets".to_string(),
            };
            true
        } else {
            self.symbol_browser.reject_search(request_id);
            let _ = self.symbol_browser.retain_latest_search(&retained_query);
            self.symbol_message = "Symbol search is busy; try again".to_string();
            false
        };
        cx.notify();
        dispatched
    }

    fn dispatch_retained_symbol_search(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(request) = self.symbol_browser.begin_retained_search() {
            return self.dispatch_symbol_search(request, cx);
        }
        false
    }

    pub(super) fn search_symbol_input(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(input) = &self.symbol_input else {
            return false;
        };
        let query = input.read(cx).value().to_string();
        self.search_symbol_query(&query, cx)
    }

    pub(super) fn submit_symbol_input(&mut self, cx: &mut Context<Self>) -> bool {
        let entries = self.instrument_entries(cx);
        match symbol_submit_decision(self.provider, entries.len(), self.chrome_selection) {
            SymbolSubmitDecision::Select(index) => entries
                .get(index)
                .is_some_and(|entry| self.select_instrument(entry.selection, cx)),
            SymbolSubmitDecision::Search => {
                self.search_symbol_input(cx);
                false
            }
        }
    }

    fn begin_rithmic_reconnect(&mut self, cx: &mut Context<Self>) {
        let series = self
            .series_browser
            .selected()
            .map_or(rithmic_history::RithmicSeries::Minute1, |request| {
                request.series
            });
        let no_retired_selection = if self.rithmic_reconnect == RithmicReconnectState::Idle {
            if let Some(selection) = self.symbol_browser.selected().cloned() {
                self.rithmic_reconnect =
                    RithmicReconnectState::AwaitingSearch(RithmicReconnectTarget {
                        symbol: selection.instrument.symbol,
                        exchange: selection.instrument.exchange,
                        series,
                    });
                false
            } else {
                true
            }
        } else {
            false
        };
        if no_retired_selection {
            self.market_state.rithmic_autoload_started = false;
        }
        self.retire_rithmic_session(cx);
    }

    fn retire_rithmic_session(&mut self, cx: &mut Context<Self>) {
        self.market_state.symbol_selection_pending = false;
        self.symbol_browser.invalidate_session();
        self.series_browser.reset();
        self.order_book
            .update(cx, axiusflow_terminal_ui::ReadOnlyOrderBookView::clear);
    }

    fn select_rithmic_symbol(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let Some(selection) = self.symbol_browser.select(index) else {
            return false;
        };
        let entitlement_id = format!(
            "rithmic-test:{}:{}",
            selection.instrument.exchange, selection.instrument.symbol
        );
        let request = SelectProviderInstrument {
            consumer_id: 0,
            selection_generation: u64::try_from(selection.generation.get()).unwrap_or(u64::MAX),
            search_generation: u64::try_from(selection.search_generation.get()).unwrap_or(u64::MAX),
            provider: "rithmic".to_string(),
            symbol: selection.instrument.symbol.clone(),
            exchange: selection.instrument.exchange.clone(),
            entitlement_id,
        };
        let dispatched = if self.market_worker.try_select_provider(request).is_ok() {
            self.market_state.symbol_selection_pending = true;
            self.order_book
                .update(cx, axiusflow_terminal_ui::ReadOnlyOrderBookView::clear);
            self.symbol_message = format!(
                "Selecting {} · {}",
                selection.instrument.symbol, selection.instrument.exchange
            );
            true
        } else {
            self.symbol_browser.reject_selection(selection.generation);
            self.symbol_message = "Symbol selection is busy; try again".to_string();
            false
        };
        cx.notify();
        dispatched
    }

    fn apply_catalog_event(&mut self, event: ProviderCatalogEvent, cx: &mut Context<Self>) {
        if provider_catalog_event_provider(&event) != terminal_provider_id(self.provider) {
            return;
        }
        let rithmic = self.provider == TerminalProvider::Rithmic;
        let hyperliquid = self.provider == TerminalProvider::Hyperliquid;
        match event {
            ProviderCatalogEvent::SearchCompleted(result) => {
                self.apply_search_completed(result, cx);
            }
            ProviderCatalogEvent::SelectionInstalled {
                command_generation,
                instrument,
            } => {
                // Both engine providers resolve selections through the same
                // switch flow: the pending product replaces the chart only
                // when its covering snapshot arrives.
                if !self.confirm_catalog_selection(command_generation) {
                    return;
                }
                self.consume_catalog_search_authorization();
                let display = terminal_provider_display(self.provider);
                let interval = self.rithmic_pending_interval.unwrap_or(self.interval);
                let Ok(sequence) = self
                    .market_worker
                    .try_select_engine(instrument.clone(), interval)
                else {
                    self.market_state.symbol_selection_pending = false;
                    self.symbol_message = format!("{display} market history could not start");
                    return;
                };
                self.rithmic_pending_product = Some(instrument);
                self.rithmic_pending_interval = Some(interval);
                self.rithmic_pending_sequence = Some(sequence);
                self.rithmic_switch = RithmicSwitchState::Pending;
                self.chart_state = ChartState::Loading;
                self.chart_state_message = format!("Loading {} market history", interval.label());
                self.symbol_message = format!("Loading the selected {display} market");
            }
            ProviderCatalogEvent::CommandRejected { rejection, command } => {
                self.apply_catalog_rejection(&rejection, command, rithmic, hyperliquid, cx);
            }
        }
        cx.notify();
    }

    fn apply_search_completed(
        &mut self,
        result: ProviderInstrumentSearchResult,
        cx: &mut Context<Self>,
    ) {
        let Some(count) = self.apply_catalog_results(result.search_generation, result.instruments)
        else {
            return;
        };
        self.symbol_message = match self.provider {
            TerminalProvider::Rithmic => {
                format!("{count} active Rithmic spot markets")
            }
            TerminalProvider::Hyperliquid => {
                format!("{count} Hyperliquid markets")
            }
        };
        if self.symbol_browser.has_retained_search() {
            self.dispatch_retained_symbol_search(cx);
            cx.notify();
            return;
        }
        // The legacy Rithmic session re-drives selection itself after a
        // reconnect or autoload; the resident engine owns Hyperliquid
        // recovery end to end, so Hyperliquid results only need display.
        if self.provider == TerminalProvider::Rithmic
            && self.rithmic_reconnect != RithmicReconnectState::Idle
        {
            if let Some(index) = self
                .rithmic_reconnect
                .target()
                .and_then(|target| reconnect_contract_index(self.symbol_browser.results(), target))
            {
                self.select_rithmic_symbol(index, cx);
            } else {
                self.rithmic_reconnect = RithmicReconnectState::Idle;
                self.symbol_message =
                    "The previous Rithmic contract is unavailable after reconnect".to_string();
            }
        } else if self.provider == TerminalProvider::Rithmic
            && self.market_state.rithmic_autoload_started
            && self.symbol_browser.selected().is_none()
            && let Some(index) = default_rithmic_contract_index(self.symbol_browser.results())
        {
            self.select_rithmic_symbol(index, cx);
        }
        self.dispatch_retained_symbol_search(cx);
        if self.chrome_overlay == Some(ChromeOverlay::Instrument) {
            self.sync_instrument_menu_keyboard(cx);
        }
    }

    fn apply_catalog_rejection(
        &mut self,
        rejection: &ProviderCatalogRejected,
        command: ProviderCatalogCommand,
        rithmic: bool,
        hyperliquid: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(generation) = usize_generation(rejection.command_generation) else {
            return;
        };
        let selection = command == ProviderCatalogCommand::Selection;
        let rejected = if selection {
            self.symbol_browser.reject_selection(generation)
        } else {
            self.symbol_browser.reject_search(generation)
        };
        if !rejected {
            return;
        }
        if rithmic || hyperliquid || selection {
            self.market_state.symbol_selection_pending = false;
        }
        let reason = ProviderCatalogRejectionReason::try_from(rejection.reason)
            .unwrap_or(ProviderCatalogRejectionReason::Unspecified);
        self.symbol_message = catalog_rejection_message(reason, command, self.provider).to_string();
        if rithmic && let Some(target) = self.rithmic_reconnect.target().cloned() {
            self.rithmic_reconnect = RithmicReconnectState::AwaitingSearch(target);
            self.retire_rithmic_session(cx);
        }
        self.dispatch_retained_symbol_search(cx);
    }

    fn restore_rithmic_series_after_failure(&mut self) {
        let Some(selected) = self.series_browser.selected() else {
            return;
        };
        let _ = self
            .market_worker
            .try_request_engine_series(EngineSeriesRequest {
                selection_generation: selected.selection_generation,
                series_generation: selected.series_generation,
                interval: selected.series.interval(),
            });
    }

    fn apply_rithmic_history(
        &mut self,
        selection_generation: std::num::NonZeroUsize,
        series_generation: std::num::NonZeroUsize,
        result: Result<Box<MarketWorkerBootstrap>, String>,
        cx: &mut Context<Self>,
    ) {
        let bootstrap = match result {
            Ok(bootstrap) => bootstrap,
            Err(error) => {
                if self.series_browser.reject(series_generation) {
                    let (series_message, chart_message) =
                        rithmic_engine_history::history_failure_messages(&error);
                    self.series_message = series_message;
                    self.set_chart_state(ChartState::Error, chart_message, cx);
                    // The chart on screen is still the previous series, so its
                    // demand is restated rather than abandoned: the trader keeps
                    // a live chart and an actionable error, not an empty surface.
                    self.restore_rithmic_series_after_failure();
                }
                return;
            }
        };
        if !self
            .series_browser
            .accept(selection_generation, series_generation)
        {
            return;
        }
        let replay_label = generation_status(
            &bootstrap.worker_label,
            &bootstrap.subscription_id,
            MarketPublicationGeneration::from_generation(&bootstrap.generation),
        );
        let snapshot = bootstrap.snapshot;
        let visible_bar_count = snapshot.bars().len();
        let chart_theme = nucleus_chart_theme(self.theme.mode);
        let chart =
            cx.new(move |_| NucleusChartView::with_replay_and_theme(&snapshot, chart_theme));
        self.apply_chart_chrome_to_chart(&chart, cx);
        self.apply_retained_indicators_to_chart(&chart, cx);
        self.chart = Some(chart);
        observe_chart(self.chart.as_ref(), cx);
        self.worker_label = bootstrap.worker_label;
        self.subscription_id = bootstrap.subscription_id;
        self.replay_label = replay_label;
        self.bridge_label = self.chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| bridge_status(chart.read(cx).replay_bridge_metrics()),
        );
        self.series_message = format!("{visible_bar_count} visible bars are current");
        self.set_chart_state(
            ChartState::Ready,
            "Rithmic visible history is current".to_string(),
            cx,
        );
    }

    fn apply_rithmic_live(
        &mut self,
        selection_generation: std::num::NonZeroUsize,
        series_generation: std::num::NonZeroUsize,
        update: axiusflow_application::ReplayStreamUpdate,
        cx: &mut Context<Self>,
    ) {
        let Some(selected) = self.series_browser.selected() else {
            return;
        };
        if selected.selection_generation != selection_generation
            || selected.series_generation != series_generation
        {
            return;
        }
        let Some(chart) = &self.chart else {
            return;
        };
        if chart
            .update(cx, |chart, _| {
                chart.try_queue_replay_update(update).map_err(|_| ())
            })
            .is_err()
        {
            self.set_chart_state(
                ChartState::Recovering,
                "Rithmic live chart requires a covering snapshot".to_string(),
                cx,
            );
            return;
        }
        self.series_message = "Live candle is current".to_string();
        self.chart_state = ChartState::Ready;
        self.chart_state_message = "Rithmic live candle is current".to_string();
    }

    /// Whether a market is selected. The header enables the Order Book toggle on this
    /// and `toggle_order_book` opens on it, so the two cannot drift apart again.
    pub(super) fn has_market_selection(&self) -> bool {
        self.symbol_browser.selected().is_some() || self.product.is_some()
    }

    pub(super) fn toggle_order_book(&mut self, cx: &mut Context<Self>) {
        if self.has_market_selection() {
            self.side_panel =
                (self.side_panel != Some(SidePanel::OrderBook)).then_some(SidePanel::OrderBook);
            if self.side_panel.is_none() {
                self.menu_state.order_book_column_open = false;
            }
            cx.notify();
        }
    }

    pub(super) fn toggle_order_book_column_menu(&mut self, cx: &mut Context<Self>) {
        self.menu_state.order_book_column_open = !self.menu_state.order_book_column_open;
        cx.notify();
    }

    pub(super) fn close_order_book_column_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu_state.order_book_column_open {
            self.menu_state.order_book_column_open = false;
            cx.notify();
        }
    }

    pub(super) fn close_side_panel(&mut self, cx: &mut Context<Self>) {
        self.side_panel_resize = None;
        self.menu_state.order_book_column_open = false;
        if self.side_panel.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn begin_side_panel_resize(&mut self, pointer_x: f32) {
        self.side_panel_resize = Some(SidePanelResize {
            pointer_x,
            width: self.side_panel_width,
        });
    }

    pub(super) fn update_side_panel_resize(
        &mut self,
        pointer_x: f32,
        left_pressed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(resize) = self.side_panel_resize else {
            return;
        };
        if !left_pressed {
            self.side_panel_resize = None;
            return;
        }
        let width = resized_side_panel_width(resize, pointer_x);
        if (width - self.side_panel_width).abs() > f32::EPSILON {
            self.side_panel_width = width;
            cx.notify();
        }
    }

    pub(super) fn end_side_panel_resize(&mut self) {
        self.side_panel_resize = None;
    }

    pub(super) fn reset_chart_view(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.reset_view();
                chart_cx.notify();
            });
        }
    }

    pub(super) fn select_drawing_tool(&mut self, tool: ChartDrawingTool, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_drawing_tool(tool);
                chart_cx.notify();
            });
            cx.notify();
        }
    }

    pub(super) fn remove_selected_chart_object(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.remove_selected_chart_object() {
                    chart_cx.notify();
                }
            });
            self.retain_chart_presentation(cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_selected_drawing_lock(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                let locked = chart.selected_drawing_locked();
                if chart.set_selected_drawing_locked(!locked) {
                    chart_cx.notify();
                }
            });
            cx.notify();
        }
    }

    pub(super) fn undo_drawing(&mut self, cx: &mut Context<Self>) {
        self.step_drawing_history(true, cx);
    }

    pub(super) fn redo_drawing(&mut self, cx: &mut Context<Self>) {
        self.step_drawing_history(false, cx);
    }

    fn step_drawing_history(&mut self, undo: bool, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            let stepped = chart.update(cx, |chart, chart_cx| {
                let stepped = if undo {
                    chart.undo_drawing()
                } else {
                    chart.redo_drawing()
                };
                if stepped {
                    chart_cx.notify();
                }
                stepped
            });
            if stepped {
                cx.notify();
            }
        }
    }

    pub(super) fn clear_drawings(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.clear_drawings();
                chart.cancel_drawing();
                chart_cx.notify();
            });
            cx.notify();
        }
    }

    pub(super) fn clear_indicators(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.clear_indicators() {
                    chart_cx.notify();
                }
            });
            self.retain_chart_presentation(cx);
            cx.notify();
        }
    }

    pub(super) fn add_indicator(
        &mut self,
        indicator: ChartIndicator,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(chart) = self.chart.clone() else {
            self.indicator_message = Some("Chart data is not available yet".to_string());
            cx.notify();
            return false;
        };
        let maximum = current_plan_limits().indicators_per_chart;
        if chart.read(cx).indicator_states().len() >= maximum {
            self.indicator_message = Some(format!(
                "Your plan supports at most {maximum} indicators per chart"
            ));
            cx.notify();
            return false;
        }
        let result = chart.update(cx, |chart, chart_cx| {
            let result = chart.add_indicator(indicator);
            if result.is_ok() {
                chart_cx.notify();
            }
            result
        });
        match result {
            Ok(_) => {
                self.indicator_message = None;
                self.retain_chart_presentation(cx);
                true
            }
            Err(error) => {
                self.indicator_message = Some(error.to_string());
                cx.notify();
                false
            }
        }
    }

    /// Reports what the header's undo and redo controls may offer for the active chart.
    pub(super) fn drawing_history_state(&self, cx: &App) -> DrawingHistoryState {
        self.chart
            .as_ref()
            .map_or_else(DrawingHistoryState::default, |chart| {
                let chart = chart.read(cx);
                DrawingHistoryState {
                    can_undo: chart.can_undo_drawing(),
                    can_redo: chart.can_redo_drawing(),
                }
            })
    }

    pub(super) fn drawing_toolbar_state(&self, cx: &App) -> DrawingToolbarState {
        self.chart
            .as_ref()
            .map_or_else(DrawingToolbarState::default, |chart| {
                DrawingToolbarState::from_chart(chart.read(cx))
            })
    }
}
