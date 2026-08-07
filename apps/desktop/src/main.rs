//! Axiusflow's native GPUI terminal entry point.

mod live_market_worker;
mod market_worker;
mod readiness_conformance;
mod rithmic_history;
mod rithmic_live_chart;
mod rithmic_market_worker;
mod rithmic_shell;
mod windowed_benchmark;

use axiusflow_application::{ReplayProvenance, ReplayStreamUpdate};
use axiusflow_chart_integration::{ChartBridgeMetrics, OriginChartView};
use axiusflow_design_system::{AxiusflowTheme, RadiusToken, ThemeColor};
use axiusflow_observability::{FeedConnectionState, FeedDiagnosticsSnapshot};
use axiusflow_rithmic_protocol_adapter::{
    RithmicCatalogEvent, RithmicCatalogRejection, RithmicInstrumentSelection,
    RithmicReadOnlySubscription, RithmicSymbolSearch, SearchPattern,
};
use axiusflow_terminal_ui::{DomFrame, ReadOnlyDomView};
use gpui::{
    App, Bounds, Context, Entity, Hsla, Render, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};
use gpui_component::{
    Disableable, Root, StyledExt, TitleBar,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    input::{Input, InputState},
    menu::{DropdownMenu, PopupMenu, PopupMenuItem},
    theme::{Theme as ComponentTheme, ThemeMode as ComponentThemeMode, ThemeTokens},
};
use gpui_platform::application;
use market_worker::{
    ChartState, DesktopMarketGeneration, MarketDataWorker, MarketWorkerBootstrap,
    MarketWorkerMessage, MarketWorkerPublication, MarketWorkerStartup, PendingUiDiagnostics,
    UiDiagnosticsFeedback,
};
use std::sync::mpsc::TrySendError;

fn generation_status(
    worker_label: &str,
    subscription_id: &str,
    generation: &DesktopMarketGeneration,
) -> String {
    let (first_sequence, last_sequence) = generation.sequence_range();
    format!(
        "{worker_label} · {subscription_id} · model g{} · {} retained · seq {first_sequence}–{last_sequence}",
        generation.generation(),
        generation.items().len(),
    )
}

fn bridge_status(metrics: ChartBridgeMetrics) -> String {
    format!(
        "bridge q{} · overflows {} · recoveries {}/{}{}",
        metrics.queued_updates,
        metrics.queue_overflows,
        metrics.completed_recoveries,
        metrics.failed_recoveries,
        if metrics.recovery_pending {
            " · snapshot pending"
        } else {
            ""
        }
    )
}

fn publication_chart_state(accepted: bool, recovery_pending: bool) -> ChartState {
    if accepted && !recovery_pending {
        ChartState::Ready
    } else {
        ChartState::Recovering
    }
}

fn reconciled_bridge_state(current: ChartState, recovery_pending: bool) -> ChartState {
    if current == ChartState::Ready && recovery_pending {
        ChartState::Recovering
    } else {
        current
    }
}

fn default_rithmic_contract_index(
    results: &[axiusflow_rithmic_protocol_adapter::SymbolSearchResult],
) -> Option<usize> {
    results
        .iter()
        .enumerate()
        .filter(|(_, result)| {
            result.symbol.starts_with("MNQ")
                && result.symbol != "MNQ"
                && !result.symbol.contains('-')
                && result.expiration_date.is_some()
        })
        .min_by_key(|(_, result)| result.expiration_date.as_deref())
        .map(|(index, _)| index)
}

fn reconnect_contract_index(
    results: &[axiusflow_rithmic_protocol_adapter::SymbolSearchResult],
    target: &RithmicReconnectTarget,
) -> Option<usize> {
    results
        .iter()
        .position(|result| result.symbol == target.symbol && result.exchange == target.exchange)
}

struct TerminalApp {
    chart: Option<Entity<OriginChartView>>,
    dom: Entity<ReadOnlyDomView>,
    side_panel: Option<SidePanel>,
    window_active: bool,
    frame_poll_gate: readiness_conformance::FramePollGate,
    feed_diagnostics: Option<Box<FeedDiagnosticsSnapshot>>,
    chart_state: ChartState,
    chart_state_message: String,
    theme: AxiusflowTheme,
    replay_label: String,
    worker_label: String,
    subscription_id: String,
    bridge_label: String,
    market_worker: MarketDataWorker,
    pending_ui_diagnostics: Option<PendingUiDiagnostics>,
    connection_state: Option<FeedConnectionState>,
    connection_message: Option<String>,
    symbol_browser: rithmic_shell::RithmicSymbolBrowser,
    symbol_message: String,
    series_browser: rithmic_history::RithmicSeriesBrowser,
    series_message: String,
    rithmic_autoload_started: bool,
    rithmic_reconnect: RithmicReconnectState,
    symbol_input: Option<Entity<InputState>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RithmicReconnectTarget {
    symbol: String,
    exchange: String,
    series: rithmic_history::RithmicSeries,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
enum RithmicReconnectState {
    #[default]
    Idle,
    AwaitingSearch(RithmicReconnectTarget),
    SearchInFlight(RithmicReconnectTarget),
}

impl RithmicReconnectState {
    fn target(&self) -> Option<&RithmicReconnectTarget> {
        match self {
            Self::AwaitingSearch(target) | Self::SearchInFlight(target) => Some(target),
            Self::Idle => None,
        }
    }
}

struct HeaderState {
    theme: AxiusflowTheme,
    market_label: String,
    series_label: String,
    instruments: Vec<axiusflow_rithmic_protocol_adapter::SymbolSearchResult>,
    selected_instrument: Option<(String, String)>,
    selected_series: Option<rithmic_history::RithmicSeries>,
    symbol_input: Option<Entity<InputState>>,
    controls: HeaderControls,
    dom_visible: bool,
    health_visible: bool,
    connection_state: FeedConnectionState,
    chart_state: ChartState,
    delayed: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SidePanel {
    Dom,
    Health,
}

#[derive(Clone, Copy)]
struct HeaderControls(u8);

impl HeaderControls {
    const INSTRUMENT: u8 = 1;
    const SERIES: u8 = 2;
    const DOM: u8 = 4;
    const HEALTH: u8 = 8;

    const fn enabled(self, control: u8) -> bool {
        self.0 & control != 0
    }

    fn from_state(instrument: bool, selection: bool, health: bool) -> Self {
        let mut controls = 0;
        if instrument {
            controls |= Self::INSTRUMENT;
        }
        if selection {
            controls |= Self::SERIES | Self::DOM;
        }
        if health {
            controls |= Self::HEALTH;
        }
        Self(controls)
    }
}

impl TerminalApp {
    fn new(
        cx: &mut Context<Self>,
        startup: MarketWorkerStartup,
        market_worker: MarketDataWorker,
        symbol_input: Option<Entity<InputState>>,
    ) -> Self {
        let theme = AxiusflowTheme::dark();
        let (
            chart,
            chart_state,
            chart_state_message,
            replay_label,
            worker_label,
            subscription_id,
            connection_state,
            connection_message,
        ) = match startup {
            MarketWorkerStartup::Shell(shell) => {
                let profile = shell.profile_label();
                let connection = shell.connection();
                let message = shell.message().to_string();
                let chart = cx.new(move |_| OriginChartView::empty(theme));
                (
                    Some(chart),
                    ChartState::Loading,
                    message.clone(),
                    profile.clone(),
                    "Rithmic market worker".to_string(),
                    "Loading chart".to_string(),
                    Some(connection),
                    Some(message),
                )
            }
            MarketWorkerStartup::Loading {
                instrument,
                subscription_id,
                worker_label,
            } => {
                let _ = instrument;
                (
                    None,
                    ChartState::Loading,
                    "waiting for a covering market snapshot".to_string(),
                    "waiting for a covering market snapshot".to_string(),
                    worker_label,
                    subscription_id,
                    None,
                    None,
                )
            }
            MarketWorkerStartup::Ready(bootstrap) => {
                let replay_label = generation_status(
                    &bootstrap.worker_label,
                    &bootstrap.subscription_id,
                    &bootstrap.generation,
                );
                let snapshot = bootstrap.snapshot;
                let chart =
                    cx.new(move |_| OriginChartView::with_theme_and_replay(theme, &snapshot));
                (
                    Some(chart),
                    ChartState::Ready,
                    "market snapshot is current".to_string(),
                    replay_label,
                    bootstrap.worker_label,
                    bootstrap.subscription_id,
                    None,
                    None,
                )
            }
        };
        let bridge_label = chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| bridge_status(chart.read(cx).replay_bridge_metrics()),
        );
        let dom = cx.new(move |_| ReadOnlyDomView::new(theme));
        Self {
            chart,
            dom,
            side_panel: None,
            window_active: true,
            frame_poll_gate: readiness_conformance::FramePollGate::default(),
            feed_diagnostics: None,
            chart_state,
            chart_state_message,
            theme,
            replay_label,
            worker_label,
            subscription_id,
            bridge_label,
            market_worker,
            pending_ui_diagnostics: None,
            connection_state,
            connection_message,
            symbol_browser: rithmic_shell::RithmicSymbolBrowser::default(),
            symbol_message: "Search for an entitled Rithmic Test symbol".to_string(),
            series_browser: rithmic_history::RithmicSeriesBrowser::default(),
            series_message: "Select a symbol before choosing a series".to_string(),
            rithmic_autoload_started: false,
            rithmic_reconnect: RithmicReconnectState::Idle,
            symbol_input,
        }
    }

    fn apply_publication(
        &mut self,
        mut publication: MarketWorkerPublication,
        cx: &mut Context<Self>,
    ) {
        let ui_diagnostics = publication.ui_diagnostics.take();
        self.worker_label = publication.worker_label;
        self.subscription_id = publication.subscription_id;
        self.replay_label = generation_status(
            &self.worker_label,
            &self.subscription_id,
            &publication.generation,
        );
        let (next_state, accepted) = match (&self.chart, publication.update) {
            (None, axiusflow_application::ReplayStreamUpdate::Snapshot(snapshot)) => {
                let theme = self.theme;
                self.chart =
                    Some(cx.new(move |_| OriginChartView::with_theme_and_replay(theme, &snapshot)));
                (ChartState::Ready, true)
            }
            (Some(chart), update) => {
                let (accepted, recovery_pending) = chart.update(cx, |chart, chart_cx| {
                    let accepted = chart.try_queue_replay_update(update).is_ok();
                    if !accepted {
                        eprintln!("bounded chart queue overflowed; fixture resnapshot required");
                    }
                    chart_cx.notify();
                    (accepted, chart.replay_bridge_metrics().recovery_pending)
                });
                (
                    publication_chart_state(accepted, recovery_pending),
                    accepted,
                )
            }
            (None, axiusflow_application::ReplayStreamUpdate::Delta(_)) => {
                self.finish_ui_diagnostics(ui_diagnostics, false);
                self.set_chart_state(
                    ChartState::Error,
                    "market delta arrived before the initial covering snapshot".to_string(),
                    cx,
                );
                return;
            }
        };
        self.finish_ui_diagnostics(ui_diagnostics, accepted);
        if next_state == ChartState::Ready {
            self.chart_state = ChartState::Ready;
            self.chart_state_message = "market snapshot is current".to_string();
        } else {
            self.set_chart_state(
                ChartState::Recovering,
                "chart update requires a correlated covering snapshot".to_string(),
                cx,
            );
        }
        cx.notify();
    }

    fn finish_ui_diagnostics(&mut self, incoming: Option<PendingUiDiagnostics>, accepted: bool) {
        let Some(incoming) = incoming else {
            return;
        };
        if !accepted {
            self.market_worker
                .send_ui_diagnostics(UiDiagnosticsFeedback::Coalesced {
                    generation: incoming.generation(),
                });
            return;
        }
        if let Some(replaced) = self.pending_ui_diagnostics.replace(incoming) {
            self.market_worker
                .send_ui_diagnostics(UiDiagnosticsFeedback::Coalesced {
                    generation: replaced.generation(),
                });
        }
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
                if let Some(chart) = &self.chart {
                    chart.update(cx, |chart, chart_cx| {
                        chart.mark_replay_recovery_failed(request_id);
                        chart_cx.notify();
                    });
                }
                self.set_chart_state(ChartState::Error, error.clone(), cx);
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
                    &bootstrap.generation,
                );
                self.chart_state = ChartState::Ready;
                self.chart_state_message = "market snapshot is current".to_string();
                cx.notify();
            }
            Ok(false) => eprintln!("ignored stale fixture recovery response {request_id}"),
            Err(error) => {
                chart.update(cx, |chart, chart_cx| {
                    chart.mark_replay_recovery_failed(request_id);
                    chart_cx.notify();
                });
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
        eprintln!("fixture market worker invalidated the stream: {message}");
    }

    fn set_chart_state(&mut self, state: ChartState, message: String, cx: &mut Context<Self>) {
        if matches!(state, ChartState::Stale | ChartState::Recovering) {
            self.mark_market_stream_invalid(&message, cx);
        }
        self.chart_state = state;
        self.chart_state_message = message;
        cx.notify();
    }

    fn reset_chart_surface(&mut self, cx: &mut Context<Self>) {
        let theme = self.theme;
        self.chart = Some(cx.new(move |_| OriginChartView::empty(theme)));
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

    fn poll_market_worker(&mut self, cx: &mut Context<Self>) {
        if !self.window_active {
            return;
        }
        let (messages, disconnected) = self.market_worker.drain_messages();
        for message in messages {
            match message {
                MarketWorkerMessage::Update(publication) => {
                    self.apply_publication(publication, cx);
                }
                MarketWorkerMessage::Diagnostics(snapshot) => {
                    self.feed_diagnostics = Some(snapshot);
                    cx.notify();
                }
                MarketWorkerMessage::Recovery { request_id, result } => {
                    self.apply_recovery(request_id, result, cx);
                }
                MarketWorkerMessage::State { state, message } => {
                    self.set_chart_state(state, message, cx);
                }
                MarketWorkerMessage::Connection { state, message } => {
                    self.apply_connection_state(state, message, cx);
                }
                MarketWorkerMessage::RithmicCatalog(event) => {
                    self.apply_rithmic_catalog(event, cx);
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
                    snapshot,
                } => {
                    self.apply_rithmic_live(selection_generation, series_generation, &snapshot, cx);
                }
                MarketWorkerMessage::RithmicDom(frame) => {
                    self.apply_rithmic_dom(frame, cx);
                }
            }
        }
        if disconnected && self.connection_state.is_some() {
            self.connection_state = Some(FeedConnectionState::Stopped);
            self.connection_message = Some("Rithmic market worker stopped".to_string());
            cx.notify();
        } else if disconnected && self.chart_state != ChartState::Error {
            self.set_chart_state(
                ChartState::Error,
                "worker channel disconnected".to_string(),
                cx,
            );
        }
        self.dispatch_recovery(cx);

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
    }

    fn apply_connection_state(
        &mut self,
        state: FeedConnectionState,
        message: String,
        cx: &mut Context<Self>,
    ) {
        if state == FeedConnectionState::Recovering {
            self.begin_rithmic_reconnect();
        }
        self.connection_state = Some(state);
        let ready_for_search = state == FeedConnectionState::Authenticating
            && message.contains("ready for instrument search");
        let should_reconnect = ready_for_search
            && matches!(
                self.rithmic_reconnect,
                RithmicReconnectState::AwaitingSearch(_)
            );
        let should_autoload = ready_for_search
            && self.rithmic_reconnect == RithmicReconnectState::Idle
            && !self.rithmic_autoload_started;
        self.connection_message = Some(message);
        if should_reconnect {
            let symbol = self
                .rithmic_reconnect
                .target()
                .map(|target| target.symbol.clone())
                .unwrap_or_default();
            if self.search_rithmic_query(&symbol, cx)
                && let RithmicReconnectState::AwaitingSearch(target) = &self.rithmic_reconnect
            {
                self.rithmic_reconnect = RithmicReconnectState::SearchInFlight(target.clone());
            }
        } else if should_autoload {
            self.rithmic_autoload_started = true;
            let _ = self.search_rithmic_query("MNQ", cx);
        }
        cx.notify();
    }

    fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = self.theme.toggled();
        sync_component_theme(&theme, Some(window), cx);
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_theme(theme);
                chart_cx.notify();
            });
        }
        self.dom.update(cx, |dom, dom_cx| {
            dom.set_theme(theme, dom_cx);
        });
        self.theme = theme;
        cx.notify();
    }

    fn schedule_diagnostics_frame(&mut self, window: &mut Window) {
        if let Some(mut diagnostics) = self.pending_ui_diagnostics.take()
            && let Some(sender) = self.market_worker.ui_diagnostics_sender()
        {
            diagnostics.mark_frame_submit();
            window.on_next_frame(move |_window, _cx| {
                let _ = sender.send(diagnostics.into_presented());
            });
        }
    }

    fn schedule_market_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.frame_poll_gate.try_schedule(self.window_active) {
            return;
        }
        let app = cx.entity();
        window.on_next_frame(move |_, cx| {
            app.update(cx, |app, cx| {
                app.frame_poll_gate.complete();
                app.poll_market_worker(cx);
                cx.notify();
            });
        });
    }

    fn search_rithmic_query(&mut self, query: &str, cx: &mut Context<Self>) -> bool {
        let request = match self.symbol_browser.begin_search(query) {
            Ok(request) => request,
            Err(message) => {
                self.symbol_message = message.to_string();
                cx.notify();
                return false;
            }
        };
        let search = RithmicSymbolSearch::try_new(
            request.request_id,
            request.query,
            None,
            None,
            None,
            SearchPattern::Equals,
            std::num::NonZeroUsize::new(rithmic_shell::MAXIMUM_SYMBOL_RESULTS)
                .unwrap_or(std::num::NonZeroUsize::MIN),
        );
        let Ok(search) = search else {
            self.symbol_browser.reject_command(request.request_id);
            self.symbol_message = "Symbol search request is invalid".to_string();
            cx.notify();
            return false;
        };
        let dispatched = if self.market_worker.try_search_rithmic(search).is_ok() {
            self.symbol_message = "Searching Rithmic Test symbols".to_string();
            true
        } else {
            self.symbol_browser.reject_command(request.request_id);
            self.symbol_message = "Symbol search is busy; try again".to_string();
            false
        };
        cx.notify();
        dispatched
    }

    fn search_rithmic_input(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.symbol_input else {
            return;
        };
        let query = input.read(cx).value().to_string();
        self.search_rithmic_query(&query, cx);
    }

    fn begin_rithmic_reconnect(&mut self) {
        if self.rithmic_reconnect != RithmicReconnectState::Idle {
            return;
        }
        let Some(selection) = self.symbol_browser.selected().cloned() else {
            return;
        };
        let series = self
            .series_browser
            .selected()
            .map_or(rithmic_history::RithmicSeries::Minute1, |request| {
                request.series
            });
        self.rithmic_reconnect = RithmicReconnectState::AwaitingSearch(RithmicReconnectTarget {
            symbol: selection.instrument.symbol,
            exchange: selection.instrument.exchange,
            series,
        });
        self.symbol_browser.invalidate_session();
        self.series_browser.reset();
    }

    fn select_rithmic_symbol(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(selection) = self.symbol_browser.select(index) else {
            return;
        };
        let entitlement_id = format!(
            "rithmic-test:{}:{}",
            selection.instrument.exchange, selection.instrument.symbol
        );
        let request =
            RithmicReadOnlySubscription::try_new(true, false, true).and_then(|subscription| {
                RithmicInstrumentSelection::try_new(
                    selection.generation,
                    selection.search_generation,
                    selection.instrument.symbol.clone(),
                    selection.instrument.exchange.clone(),
                    entitlement_id,
                    subscription,
                )
            });
        let Ok(request) = request else {
            self.symbol_browser.reject_command(selection.generation);
            self.symbol_message = "Symbol selection is invalid".to_string();
            cx.notify();
            return;
        };
        if self.market_worker.try_select_rithmic(request).is_ok() {
            self.symbol_message = format!(
                "Selecting {} · {}",
                selection.instrument.symbol, selection.instrument.exchange
            );
        } else {
            self.symbol_browser.reject_command(selection.generation);
            self.symbol_message = "Symbol selection is busy; try again".to_string();
        }
        cx.notify();
    }

    fn apply_rithmic_catalog(&mut self, event: RithmicCatalogEvent, cx: &mut Context<Self>) {
        match event {
            RithmicCatalogEvent::SearchCompleted {
                search_generation,
                symbols,
                ..
            } => {
                let result_count = symbols.results.len();
                let applied = self
                    .symbol_browser
                    .apply_results(search_generation, symbols.results);
                if applied {
                    self.symbol_message = format!("{result_count} matching symbols");
                }
                if applied && self.rithmic_reconnect != RithmicReconnectState::Idle {
                    let index = self.rithmic_reconnect.target().and_then(|target| {
                        reconnect_contract_index(self.symbol_browser.results(), target)
                    });
                    if let Some(index) = index {
                        self.select_rithmic_symbol(index, cx);
                    } else {
                        self.rithmic_reconnect = RithmicReconnectState::Idle;
                        self.symbol_message =
                            "The previous Rithmic contract is unavailable after reconnect"
                                .to_string();
                    }
                } else if applied
                    && self.rithmic_autoload_started
                    && self.symbol_browser.selected().is_none()
                {
                    let index = default_rithmic_contract_index(self.symbol_browser.results());
                    if let Some(index) = index {
                        self.select_rithmic_symbol(index, cx);
                    }
                }
            }
            RithmicCatalogEvent::SelectionInstalled {
                selection_generation,
                instrument,
                ..
            } => {
                if self.symbol_browser.confirm_selection(selection_generation) {
                    let recovered_series = self
                        .rithmic_reconnect
                        .target()
                        .map_or(rithmic_history::RithmicSeries::Minute1, |target| {
                            target.series
                        });
                    self.rithmic_reconnect = RithmicReconnectState::Idle;
                    self.series_browser.reset();
                    self.reset_chart_surface(cx);
                    self.bridge_label = "bridge awaiting series selection".to_string();
                    self.replay_label = "Selected instrument · choose a series".to_string();
                    self.subscription_id =
                        format!("{} · {}", instrument.display_symbol, instrument.venue_id);
                    self.symbol_message = format!("Selected {}", instrument.display_symbol);
                    self.series_message = "Choose a chart series".to_string();
                    self.connection_state = Some(FeedConnectionState::Streaming);
                    self.connection_message =
                        Some("Rithmic Test market subscription active".to_string());
                    self.select_rithmic_series(recovered_series, cx);
                }
            }
            RithmicCatalogEvent::CommandRejected {
                command_generation,
                reason,
                ..
            } => {
                if self.symbol_browser.reject_command(command_generation) {
                    self.symbol_message = catalog_rejection_message(reason).to_string();
                    if let Some(target) = self.rithmic_reconnect.target().cloned() {
                        self.rithmic_reconnect = RithmicReconnectState::AwaitingSearch(target);
                        self.symbol_browser.invalidate_session();
                    }
                }
            }
        }
        cx.notify();
    }

    fn select_rithmic_series(
        &mut self,
        series: rithmic_history::RithmicSeries,
        cx: &mut Context<Self>,
    ) {
        let Some(selection) = self.symbol_browser.selected() else {
            self.series_message = "Select a symbol before choosing a series".to_string();
            cx.notify();
            return;
        };
        let request = self.series_browser.select(selection.generation, series);
        if self
            .market_worker
            .try_request_rithmic_history(request)
            .is_ok()
        {
            self.reset_chart_surface(cx);
            self.bridge_label = "bridge awaiting visible history".to_string();
            self.series_message = format!("Loading {} visible history", series.label());
            self.set_chart_state(
                ChartState::Loading,
                format!("loading {} visible history", series.label()),
                cx,
            );
        } else {
            self.series_browser.reject(request.series_generation);
            self.series_message = "Rithmic history worker is busy; try again".to_string();
        }
        cx.notify();
    }

    fn apply_rithmic_history(
        &mut self,
        selection_generation: std::num::NonZeroUsize,
        series_generation: std::num::NonZeroUsize,
        result: Result<Box<MarketWorkerBootstrap>, String>,
        cx: &mut Context<Self>,
    ) {
        let Ok(bootstrap) = result else {
            if self.series_browser.reject(series_generation) {
                self.series_message = "Rithmic visible history is unavailable".to_string();
                self.set_chart_state(
                    ChartState::Error,
                    "Rithmic visible history could not be loaded".to_string(),
                    cx,
                );
            }
            return;
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
            &bootstrap.generation,
        );
        let snapshot = bootstrap.snapshot;
        let theme = self.theme;
        self.chart =
            Some(cx.new(move |_| OriginChartView::with_theme_and_replay(theme, &snapshot)));
        self.worker_label = bootstrap.worker_label;
        self.subscription_id = bootstrap.subscription_id;
        self.replay_label = replay_label;
        self.bridge_label = self.chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| bridge_status(chart.read(cx).replay_bridge_metrics()),
        );
        self.series_message = "Visible history is current".to_string();
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
        snapshot: &axiusflow_application::ReplaySnapshot,
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
            .update(cx, |chart, chart_cx| {
                let result = chart.load_replay(snapshot);
                if result.is_ok() {
                    chart_cx.notify();
                }
                result
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
        cx.notify();
    }

    fn apply_rithmic_dom(&mut self, frame: DomFrame, cx: &mut Context<Self>) {
        let selected_generation = self
            .symbol_browser
            .selected()
            .and_then(|selection| u64::try_from(selection.generation.get()).ok());
        if selected_generation != Some(frame.selection_generation) {
            return;
        }
        self.dom.update(cx, |dom, dom_cx| {
            dom.replace_frame(frame, dom_cx);
        });
    }

    fn toggle_dom(&mut self, cx: &mut Context<Self>) {
        if self.symbol_browser.selected().is_some() {
            self.side_panel = (self.side_panel != Some(SidePanel::Dom)).then_some(SidePanel::Dom);
            cx.notify();
        }
    }

    fn toggle_health(&mut self, cx: &mut Context<Self>) {
        if self.feed_diagnostics.is_some() {
            self.side_panel =
                (self.side_panel != Some(SidePanel::Health)).then_some(SidePanel::Health);
            cx.notify();
        }
    }
}

impl Render for TerminalApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.schedule_diagnostics_frame(window);
        self.window_active = window.is_window_active();
        self.schedule_market_frame(window, cx);
        let theme = self.theme;
        let colors = theme.colors;
        let app = cx.entity();
        let header = terminal_header(
            cx,
            &app,
            HeaderState {
                theme,
                market_label: self.subscription_id.clone(),
                series_label: self
                    .series_browser
                    .selected()
                    .map_or("1m", |request| request.series.label())
                    .to_string(),
                instruments: self.symbol_browser.results().to_vec(),
                selected_instrument: self.symbol_browser.selected().map(|selection| {
                    (
                        selection.instrument.symbol.clone(),
                        selection.instrument.exchange.clone(),
                    )
                }),
                selected_series: self.series_browser.selected().map(|request| request.series),
                symbol_input: self.symbol_input.clone(),
                controls: HeaderControls::from_state(
                    self.symbol_input.is_some() || !self.symbol_browser.results().is_empty(),
                    self.symbol_browser.selected().is_some(),
                    self.feed_diagnostics.is_some(),
                ),
                dom_visible: self.side_panel == Some(SidePanel::Dom),
                health_visible: self.side_panel == Some(SidePanel::Health),
                connection_state: self
                    .connection_state
                    .unwrap_or(FeedConnectionState::Disconnected),
                chart_state: self.chart_state,
                delayed: self
                    .feed_diagnostics
                    .as_deref()
                    .and_then(|snapshot| snapshot.provider_timestamp_age)
                    .is_some_and(|age| age.nanos > 60_000_000_000),
            },
        );

        let chart = div()
            .id("primary_chart")
            .v_flex()
            .flex_1()
            .overflow_hidden()
            .border_1()
            .border_color(gpui_color(colors.border))
            .bg(gpui_color(colors.background))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .children(self.chart.clone())
                    .children(self.chart.is_none().then(|| {
                        div()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_color(gpui_color(colors.muted_foreground))
                            .child(self.chart_state.label())
                    })),
            );

        div()
            .v_flex()
            .size_full()
            .bg(gpui_color(colors.background))
            .text_color(gpui_color(colors.foreground))
            .child(header)
            .child(
                div()
                    .id("market_workspace")
                    .flex()
                    .flex_1()
                    .overflow_hidden()
                    .bg(gpui_color(colors.background))
                    .child(chart)
                    .children((self.side_panel == Some(SidePanel::Dom)).then(|| {
                        div()
                            .id("depth_panel")
                            .w(px(320.0))
                            .h_full()
                            .flex_none()
                            .child(self.dom.clone())
                    }))
                    .children(
                        (self.side_panel == Some(SidePanel::Health))
                            .then(|| feed_health_panel(self.feed_diagnostics.as_deref(), &theme)),
                    ),
            )
    }
}

fn catalog_rejection_message(reason: RithmicCatalogRejection) -> &'static str {
    match reason {
        RithmicCatalogRejection::SearchRejected => "Rithmic Test rejected the symbol search",
        RithmicCatalogRejection::SupersededSearch => "A newer symbol search replaced this one",
        RithmicCatalogRejection::InstrumentUnavailable => {
            "The selected symbol is no longer available"
        }
        RithmicCatalogRejection::SubscriptionRejected => {
            "Rithmic Test rejected the market subscription"
        }
    }
}

fn terminal_header(
    cx: &mut Context<TerminalApp>,
    app: &Entity<TerminalApp>,
    state: HeaderState,
) -> impl IntoElement + use<> {
    let colors = state.theme.colors;
    let active_button = ButtonCustomVariant::new(cx)
        .color(gpui_color(colors.secondary))
        .foreground(gpui_color(colors.secondary_foreground))
        .hover(gpui_color(colors.accent))
        .active(gpui_color(colors.muted));
    let (connection_label, connection_color) =
        connection_presentation(state.connection_state, state.chart_state, state.delayed);
    let instruments = state.instruments;
    let selected_instrument = state.selected_instrument;
    let selected_series = state.selected_series;
    let symbol_input = state.symbol_input;
    let connection = connection_badge(connection_label, connection_color(&state.theme), &colors);
    let dom_toggle = panel_toggle(
        "dom_toggle",
        if state.dom_visible { "Chart" } else { "DOM" },
        active_button,
        state.controls.enabled(HeaderControls::DOM),
        app.clone(),
        TerminalApp::toggle_dom,
    );
    let health_toggle = panel_toggle(
        "health_toggle",
        if state.health_visible {
            "Chart"
        } else {
            "Health"
        },
        active_button,
        state.controls.enabled(HeaderControls::HEALTH),
        app.clone(),
        TerminalApp::toggle_health,
    );
    let theme_toggle = theme_toggle(app.clone(), &state.theme, active_button);
    let series_selector = series_selector(
        app.clone(),
        state.series_label,
        selected_series,
        active_button,
        state.controls.enabled(HeaderControls::SERIES),
    );
    let instrument_selector = instrument_selector(
        app.clone(),
        state.market_label,
        instruments,
        selected_instrument,
        symbol_input,
        active_button,
        state.controls.enabled(HeaderControls::INSTRUMENT),
    );
    TitleBar::new().child(
        div()
            .h_full()
            .flex()
            .flex_1()
            .gap_4()
            .items_center()
            .child(div().text_sm().child("Axiusflow"))
            .child(connection)
            .child(
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(instrument_selector)
                    .child(series_selector)
                    .child(dom_toggle)
                    .child(health_toggle)
                    .child(theme_toggle),
            ),
    )
}

fn instrument_selector(
    app: Entity<TerminalApp>,
    label: String,
    instruments: Vec<axiusflow_rithmic_protocol_adapter::SymbolSearchResult>,
    selected: Option<(String, String)>,
    input: Option<Entity<InputState>>,
    variant: ButtonCustomVariant,
    enabled: bool,
) -> impl IntoElement {
    Button::new("instrument_selector")
        .label(label)
        .dropdown_caret(true)
        .custom(variant)
        .disabled(!enabled)
        .dropdown_menu(move |menu, _, _| {
            instrument_menu(menu, &app, &instruments, selected.as_ref(), input.as_ref())
        })
}

fn instrument_menu(
    menu: PopupMenu,
    app: &Entity<TerminalApp>,
    instruments: &[axiusflow_rithmic_protocol_adapter::SymbolSearchResult],
    selected: Option<&(String, String)>,
    input: Option<&Entity<InputState>>,
) -> PopupMenu {
    let menu = match input {
        Some(input) => menu
            .item(instrument_search_item(input.clone(), app.clone()))
            .separator(),
        None => menu,
    };
    instruments
        .iter()
        .enumerate()
        .fold(menu.scrollable(true), |menu, (index, instrument)| {
            let checked = selected.is_some_and(|(symbol, exchange)| {
                symbol == &instrument.symbol && exchange == &instrument.exchange
            });
            let app = app.clone();
            menu.item(
                PopupMenuItem::new(instrument_menu_label(instrument))
                    .checked(checked)
                    .on_click(move |_, _, cx| {
                        app.update(cx, |app, cx| app.select_rithmic_symbol(index, cx));
                    }),
            )
        })
}

fn instrument_search_item(input: Entity<InputState>, app: Entity<TerminalApp>) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(Input::new(&input).w(px(220.0)))
            .child(
                Button::new("rithmic_header_search")
                    .label("Search")
                    .primary()
                    .on_click({
                        let app = app.clone();
                        move |_, _, cx| {
                            app.update(cx, TerminalApp::search_rithmic_input);
                        }
                    }),
            )
    })
}

fn instrument_menu_label(
    instrument: &axiusflow_rithmic_protocol_adapter::SymbolSearchResult,
) -> String {
    format!("{} / {}", instrument.symbol, instrument.exchange)
}

fn connection_badge(
    label: &'static str,
    color: ThemeColor,
    colors: &axiusflow_design_system::ThemeColors,
) -> impl IntoElement + use<> {
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .text_color(gpui_color(colors.muted_foreground))
        .child(div().size(px(6.0)).rounded_full().bg(gpui_color(color)))
        .child(label)
}

fn panel_toggle(
    id: &'static str,
    label: &'static str,
    variant: ButtonCustomVariant,
    enabled: bool,
    app: Entity<TerminalApp>,
    toggle: fn(&mut TerminalApp, &mut Context<TerminalApp>),
) -> impl IntoElement {
    Button::new(id)
        .label(label)
        .custom(variant)
        .disabled(!enabled)
        .on_click(move |_, _, cx| {
            app.update(cx, toggle);
        })
}

fn theme_toggle(
    app: Entity<TerminalApp>,
    theme: &AxiusflowTheme,
    variant: ButtonCustomVariant,
) -> impl IntoElement + use<> {
    Button::new("theme_toggle")
        .label(theme.mode.toggled().label())
        .custom(variant)
        .on_click(move |_, window, cx| {
            app.update(cx, |app, cx| app.toggle_theme(window, cx));
        })
}

fn series_selector(
    app: Entity<TerminalApp>,
    label: String,
    selected: Option<rithmic_history::RithmicSeries>,
    variant: ButtonCustomVariant,
    enabled: bool,
) -> impl IntoElement {
    Button::new("series_selector")
        .label(label)
        .dropdown_caret(true)
        .custom(variant)
        .disabled(!enabled)
        .dropdown_menu(move |menu, _, _| {
            rithmic_history::RithmicSeries::ALL
                .iter()
                .fold(menu, |menu, series| {
                    let series = *series;
                    let app = app.clone();
                    menu.item(
                        PopupMenuItem::new(series.label())
                            .checked(selected == Some(series))
                            .on_click(move |_, _, cx| {
                                app.update(cx, |app, cx| app.select_rithmic_series(series, cx));
                            }),
                    )
                })
        })
}

type ConnectionColor = fn(&AxiusflowTheme) -> ThemeColor;

fn connection_presentation(
    state: FeedConnectionState,
    chart_state: ChartState,
    delayed: bool,
) -> (&'static str, ConnectionColor) {
    if chart_state == ChartState::Stale {
        return ("Test · Stale", |theme| theme.colors.warning);
    }
    if chart_state == ChartState::Recovering {
        return ("Test · Reconnecting", |theme| theme.colors.warning);
    }
    if chart_state == ChartState::Error && state == FeedConnectionState::Streaming {
        return ("Test · Data error", |theme| theme.colors.loss);
    }
    if state == FeedConnectionState::Streaming && delayed {
        return ("Test · Delayed", |theme| theme.colors.warning);
    }
    match state {
        FeedConnectionState::Disconnected => ("Offline", |theme| theme.colors.loss),
        FeedConnectionState::Discovering => ("Test · Discovering", |theme| theme.colors.info),
        FeedConnectionState::Authenticating => {
            ("Test · Authenticating", |theme| theme.colors.info)
        }
        FeedConnectionState::Streaming => ("Test · Live", |theme| theme.colors.profit),
        FeedConnectionState::Recovering => ("Test · Reconnecting", |theme| theme.colors.warning),
        FeedConnectionState::Stopped => ("Stopped", |theme| theme.colors.loss),
    }
}

fn feed_health_panel(
    snapshot: Option<&FeedDiagnosticsSnapshot>,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let rows = health_rows(snapshot);
    div()
        .id("feed_health_panel")
        .w(px(320.0))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.background))
        .child(
            div()
                .h(px(30.0))
                .flex_none()
                .flex()
                .items_center()
                .px_2()
                .border_b_1()
                .border_color(gpui_color(colors.border))
                .text_xs()
                .text_color(gpui_color(colors.muted_foreground))
                .child("FEED HEALTH"),
        )
        .children(rows.into_iter().map(move |(label, value)| {
            div()
                .h(px(28.0))
                .flex_none()
                .flex()
                .items_center()
                .px_2()
                .border_b_1()
                .border_color(gpui_color(colors.border.with_alpha(0.55)))
                .text_xs()
                .child(
                    div()
                        .flex_1()
                        .text_color(gpui_color(colors.muted_foreground))
                        .child(label),
                )
                .child(value)
        }))
}

fn health_rows(snapshot: Option<&FeedDiagnosticsSnapshot>) -> Vec<(String, String)> {
    snapshot.map_or_else(
        || vec![("State".to_string(), "Awaiting diagnostics".to_string())],
        |snapshot| {
            let queue_high_water = snapshot
                .queues
                .iter()
                .map(|queue| queue.high_water_items)
                .max()
                .unwrap_or(0);
            let local_p99 = snapshot
                .detailed_latency
                .iter()
                .flatten()
                .map(|latency| latency.p99_upper_bound_nanos)
                .max();
            vec![
                (
                    "Feed".to_string(),
                    format!(
                        "{} · {}",
                        snapshot.identity.system(),
                        snapshot.identity.environment()
                    ),
                ),
                (
                    "Session".to_string(),
                    snapshot
                        .session_generation
                        .map_or_else(|| "—".to_string(), |generation| generation.to_string()),
                ),
                (
                    "Messages".to_string(),
                    format!(
                        "{} trades · {} quotes · {} depth",
                        snapshot.counters.trades,
                        snapshot.counters.quotes,
                        snapshot.counters.depth_snapshots
                    ),
                ),
                (
                    "Rates".to_string(),
                    format!(
                        "{} t/s · {} q/s · {} d/s",
                        milli_rate(snapshot.rates.trades_per_second_milli),
                        milli_rate(snapshot.rates.quotes_per_second_milli),
                        milli_rate(snapshot.rates.depth_updates_per_second_milli)
                    ),
                ),
                (
                    "Last message".to_string(),
                    snapshot
                        .last_message_age_nanos
                        .map_or_else(|| "—".to_string(), duration_label),
                ),
                (
                    "Heartbeat".to_string(),
                    snapshot
                        .heartbeat_age_nanos
                        .map_or_else(|| "—".to_string(), duration_label),
                ),
                (
                    "Local processing p99".to_string(),
                    local_p99.map_or_else(|| "Disabled".to_string(), duration_label),
                ),
                (
                    "Provider clock age".to_string(),
                    snapshot.provider_timestamp_age.map_or_else(
                        || "—".to_string(),
                        |age| format!("{} · clock-relative", signed_duration_label(age.nanos)),
                    ),
                ),
                (
                    "Reconnects".to_string(),
                    snapshot.reconnect_count.to_string(),
                ),
                ("Queue high water".to_string(), queue_high_water.to_string()),
                (
                    "Memory".to_string(),
                    format!(
                        "{} / {} KiB",
                        snapshot.memory.current_bytes / 1024,
                        snapshot.memory.configured_bound_bytes / 1024
                    ),
                ),
            ]
        },
    )
}

fn milli_rate(value: u64) -> String {
    format!("{}.{:03}", value / 1_000, value % 1_000)
}

fn duration_label(nanos: u64) -> String {
    if nanos < 1_000_000 {
        format!("{} µs", nanos / 1_000)
    } else if nanos < 1_000_000_000 {
        format!("{} ms", nanos / 1_000_000)
    } else {
        format!(
            "{}.{:01} s",
            nanos / 1_000_000_000,
            nanos / 100_000_000 % 10
        )
    }
}

fn signed_duration_label(nanos: i64) -> String {
    if nanos < 0 {
        format!("-{}", duration_label(nanos.unsigned_abs()))
    } else {
        duration_label(nanos.unsigned_abs())
    }
}

fn sync_component_theme(theme: &AxiusflowTheme, window: Option<&mut Window>, cx: &mut App) {
    let mode = match theme.mode {
        axiusflow_design_system::ThemeMode::Light => ComponentThemeMode::Light,
        axiusflow_design_system::ThemeMode::Dark => ComponentThemeMode::Dark,
    };
    ComponentTheme::change(mode, None, cx);

    let colors = theme.colors;
    let component = ComponentTheme::global_mut(cx);
    component.radius = px(f32::from(RadiusToken::Default.logical_pixels()));
    component.radius_lg = component.radius;
    component.tile_radius = component.radius;

    component.background = gpui_color(colors.background);
    component.foreground = gpui_color(colors.foreground);
    component.border = gpui_color(colors.border);
    component.input = gpui_color(colors.input);
    component.ring = gpui_color(colors.ring);
    component.muted = gpui_color(colors.muted);
    component.muted_foreground = gpui_color(colors.muted_foreground);
    component.accent = gpui_color(colors.accent);
    component.accent_foreground = gpui_color(colors.accent_foreground);
    component.popover = gpui_color(colors.popover);
    component.popover_foreground = gpui_color(colors.popover_foreground);

    component.button = gpui_color(colors.secondary);
    component.button_foreground = gpui_color(colors.secondary_foreground);
    component.button_hover = gpui_color(colors.accent);
    component.button_active = gpui_color(colors.muted);
    component.primary = gpui_color(colors.primary);
    component.primary_foreground = gpui_color(colors.primary_foreground);
    component.primary_hover = gpui_color(colors.primary);
    component.primary_active = gpui_color(colors.primary);
    component.secondary = gpui_color(colors.secondary);
    component.secondary_foreground = gpui_color(colors.secondary_foreground);
    component.secondary_hover = gpui_color(colors.accent);
    component.secondary_active = gpui_color(colors.muted);

    component.chart_1 = gpui_color(colors.chart_palette[0]);
    component.chart_2 = gpui_color(colors.chart_palette[1]);
    component.chart_3 = gpui_color(colors.chart_palette[2]);
    component.chart_4 = gpui_color(colors.chart_palette[3]);
    component.chart_5 = gpui_color(colors.chart_palette[4]);
    component.chart_bullish = gpui_color(colors.chart_candle_up);
    component.chart_bearish = gpui_color(colors.chart_candle_down);
    component.danger = gpui_color(colors.destructive);
    component.danger_foreground = gpui_color(colors.destructive_foreground);
    component.info = gpui_color(colors.info);
    component.success = gpui_color(colors.profit);
    component.warning = gpui_color(colors.warning);

    component.sidebar = gpui_color(colors.background);
    component.sidebar_foreground = gpui_color(colors.card_foreground);
    component.sidebar_border = gpui_color(colors.border);
    component.table = gpui_color(colors.card);
    component.table_head = gpui_color(colors.muted);
    component.table_row_border = gpui_color(colors.border);
    component.title_bar = gpui_color(colors.background);
    component.title_bar_border = gpui_color(colors.border);
    component.status_bar = gpui_color(colors.background);
    component.status_bar_border = gpui_color(colors.border);
    component.tokens = ThemeTokens::from(&component.colors);

    if let Some(window) = window {
        window.refresh();
    }
}

fn gpui_color(color: ThemeColor) -> Hsla {
    let mut resolved: Hsla = rgb(color.rgb_u32()).into();
    resolved.a = color.alpha();
    resolved
}

fn run_coinbase_live_smoke(
    product_id: &str,
    history_root: std::path::PathBuf,
) -> Result<(), String> {
    let (startup, mut worker) = MarketDataWorker::start_coinbase(
        product_id.to_string(),
        history_root,
        std::thread::current().id(),
        false,
    )?;
    if !matches!(startup, MarketWorkerStartup::Loading { .. }) {
        return Err("Coinbase shipping worker bypassed the loading state".to_string());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    let mut local_cache_observed = false;
    let mut last_state = None;
    let mut last_state_message = None;
    let mut last_snapshot_provenance = None;
    loop {
        let (messages, disconnected) = worker.drain_messages();
        for message in messages {
            match message {
                MarketWorkerMessage::Update(publication) => match publication.update {
                    ReplayStreamUpdate::Snapshot(snapshot)
                        if snapshot.provenance() == ReplayProvenance::LiveProvider =>
                    {
                        drop(worker);
                        println!(
                            "coinbase_shipping_live_smoke=passed product={product_id} loading=true local_cache_observed={local_cache_observed} covering_snapshot=true clean_shutdown=true"
                        );
                        return Ok(());
                    }
                    ReplayStreamUpdate::Snapshot(snapshot)
                        if snapshot.provenance() == ReplayProvenance::LocalCache =>
                    {
                        local_cache_observed = true;
                        last_snapshot_provenance = Some(snapshot.provenance());
                    }
                    ReplayStreamUpdate::Snapshot(snapshot) => {
                        last_snapshot_provenance = Some(snapshot.provenance());
                    }
                    ReplayStreamUpdate::Delta(_) => {}
                },
                MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message,
                } => {
                    return Err(format!(
                        "{message} (previous_state_message={last_state_message:?})"
                    ));
                }
                MarketWorkerMessage::State { state, message } => {
                    last_state = Some(state);
                    last_state_message = Some(message);
                }
                _ => {}
            }
        }
        if disconnected {
            return Err("Coinbase shipping worker disconnected before its snapshot".to_string());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "Coinbase shipping worker timed out before its snapshot (local_cache_observed={local_cache_observed}, last_snapshot_provenance={last_snapshot_provenance:?}, last_state={last_state:?}, last_state_message={last_state_message:?})"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn run_coinbase_live_smoke_command(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let usage = "usage: axiusflow_desktop --coinbase-live-smoke <BTC-USD|ETH-USD> <history-root>";
    let product = arguments.next().ok_or_else(|| usage.to_string())?;
    let history_root = arguments.next().ok_or_else(|| usage.to_string())?;
    if arguments.next().is_some() {
        return Err(usage.to_string());
    }
    run_coinbase_live_smoke(
        &product.to_string_lossy(),
        std::path::PathBuf::from(history_root),
    )
}

fn parse_rithmic_test_arguments(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(std::path::PathBuf, bool), String> {
    let usage = "usage: axiusflow_desktop --rithmic-test <history-root> [--detailed-diagnostics]";
    let history_root = arguments.next().ok_or_else(|| usage.to_string())?;
    let detailed_diagnostics = match arguments.next() {
        Some(flag) if flag == "--detailed-diagnostics" => true,
        Some(_) => return Err(usage.to_string()),
        None => false,
    };
    if arguments.next().is_some() {
        return Err(usage.to_string());
    }
    Ok((std::path::PathBuf::from(history_root), detailed_diagnostics))
}

fn run_desktop_readiness_command(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let usage = "usage: axiusflow_desktop --desktop-readiness <report-path>";
    let report_path = arguments.next().ok_or_else(|| usage.to_string())?;
    if arguments.next().is_some() {
        return Err(usage.to_string());
    }
    readiness_conformance::run(std::path::Path::new(&report_path))
        .map_err(|error| error.to_string())
}

fn run_desktop_endurance_command(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let usage = "usage: axiusflow_desktop --desktop-endurance <report-path> <duration-seconds>";
    let report_path = arguments.next().ok_or_else(|| usage.to_string())?;
    let duration_seconds = arguments
        .next()
        .ok_or_else(|| usage.to_string())?
        .to_string_lossy()
        .parse::<u64>()
        .map_err(|_| usage.to_string())?;
    if arguments.next().is_some() {
        return Err(usage.to_string());
    }
    readiness_conformance::run_endurance(
        std::path::Path::new(&report_path),
        std::time::Duration::from_secs(duration_seconds),
    )
    .map_err(|error| error.to_string())
}

fn symbol_input_for_startup(
    startup: &MarketWorkerStartup,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<InputState>> {
    matches!(startup, MarketWorkerStartup::Shell(_))
        .then(|| cx.new(|cx| InputState::new(window, cx).placeholder("Search Rithmic symbols")))
}

fn desktop_window_options(cx: &mut App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitleBar::title_bar_options()),
        ..Default::default()
    }
}

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    let worker = if let Some(argument) = arguments.next() {
        if argument == "--windowed-benchmark" {
            let report_path = arguments
                .next()
                .expect("usage: axiusflow_desktop --windowed-benchmark <report-path>");
            windowed_benchmark::run(std::path::Path::new(&report_path))
                .expect("the windowed benchmark completes");
            return;
        }
        if argument == "--desktop-readiness" {
            run_desktop_readiness_command(arguments).expect("desktop readiness conformance passes");
            return;
        }
        if argument == "--desktop-endurance" {
            run_desktop_endurance_command(arguments).expect("desktop endurance conformance passes");
            return;
        }
        if argument == "--coinbase-live-smoke" {
            run_coinbase_live_smoke_command(arguments)
                .expect("the Coinbase shipping live smoke passes");
            return;
        }
        if argument == "--rithmic-test" {
            let (history_root, detailed_diagnostics) = parse_rithmic_test_arguments(arguments)
                .unwrap_or_else(|usage| {
                    eprintln!("{usage}");
                    std::process::exit(2);
                });
            MarketDataWorker::start_rithmic(
                history_root,
                std::thread::current().id(),
                detailed_diagnostics,
            )
            .unwrap_or_else(|error| {
                eprintln!("Rithmic Test shell could not start: {error}");
                std::process::exit(1);
            })
        } else if argument == "--coinbase-live" {
            let product = arguments.next().unwrap_or_else(|| {
                eprintln!(
                    "usage: axiusflow_desktop --coinbase-live <BTC-USD|ETH-USD> <history-root> [--detailed-diagnostics]"
                );
                std::process::exit(2);
            });
            let history_root = arguments.next().unwrap_or_else(|| {
                eprintln!(
                    "usage: axiusflow_desktop --coinbase-live <BTC-USD|ETH-USD> <history-root> [--detailed-diagnostics]"
                );
                std::process::exit(2);
            });
            let detailed_diagnostics = match arguments.next() {
                Some(flag) if flag == "--detailed-diagnostics" => true,
                Some(_) => {
                    eprintln!(
                        "usage: axiusflow_desktop --coinbase-live <BTC-USD|ETH-USD> <history-root> [--detailed-diagnostics]"
                    );
                    std::process::exit(2);
                }
                None => false,
            };
            if arguments.next().is_some() {
                eprintln!(
                    "usage: axiusflow_desktop --coinbase-live <BTC-USD|ETH-USD> <history-root> [--detailed-diagnostics]"
                );
                std::process::exit(2);
            }
            MarketDataWorker::start_coinbase(
                product.to_string_lossy().into_owned(),
                std::path::PathBuf::from(history_root),
                std::thread::current().id(),
                detailed_diagnostics,
            )
            .expect("the bounded direct Coinbase worker starts")
        } else {
            eprintln!("unsupported argument: {}", argument.to_string_lossy());
            std::process::exit(2);
        }
    } else {
        MarketDataWorker::start().expect("the bounded binary fixture worker bootstraps")
    };
    let (bootstrap, market_worker) = worker;
    application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        sync_component_theme(&AxiusflowTheme::dark(), None, cx);
        let options = desktop_window_options(cx);

        cx.open_window(options, move |window, cx| {
            let symbol_input = symbol_input_for_startup(&bootstrap, window, cx);
            let terminal =
                cx.new(move |cx| TerminalApp::new(cx, bootstrap, market_worker, symbol_input));
            cx.new(|cx| Root::new(terminal, window, cx))
        })
        .expect("the Axiusflow terminal window opens");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::{
        ChartState, HeaderControls, RithmicReconnectTarget, connection_presentation,
        default_rithmic_contract_index, duration_label, gpui_color, milli_rate,
        parse_rithmic_test_arguments, publication_chart_state, reconciled_bridge_state,
        reconnect_contract_index,
    };
    use axiusflow_design_system::ThemeColor;
    use axiusflow_observability::FeedConnectionState;
    use axiusflow_rithmic_protocol_adapter::SymbolSearchResult;
    use std::ffi::OsString;

    #[test]
    fn publication_is_ready_only_after_bridge_acceptance_without_recovery() {
        assert_eq!(publication_chart_state(true, false), ChartState::Ready);
        assert_eq!(publication_chart_state(false, true), ChartState::Recovering);
        assert_eq!(publication_chart_state(true, true), ChartState::Recovering);
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
        let result = |symbol: &str, expiration: &str| SymbolSearchResult {
            symbol: symbol.to_string(),
            exchange: "CME-Delayed".to_string(),
            name: None,
            product_code: Some("MNQ".to_string()),
            instrument_type: Some("FUTURE".to_string()),
            expiration_date: Some(expiration.to_string()),
        };
        let results = vec![
            result("MNQ", "20260918"),
            result("MNQU6-MNQZ6", "20260918"),
            result("MNQZ6", "20261218"),
            result("MNQU6", "20260918"),
            SymbolSearchResult {
                symbol: "NQ".to_string(),
                exchange: "CME-Delayed".to_string(),
                name: None,
                product_code: Some("NQ".to_string()),
                instrument_type: Some("FUTURE".to_string()),
                expiration_date: None,
            },
        ];
        assert_eq!(default_rithmic_contract_index(&results), Some(3));
    }

    #[test]
    fn reconnect_contract_requires_the_exact_symbol_and_exchange() {
        let result = |exchange: &str| SymbolSearchResult {
            symbol: "MNQU6".to_string(),
            exchange: exchange.to_string(),
            name: None,
            product_code: Some("MNQ".to_string()),
            instrument_type: Some("FUTURE".to_string()),
            expiration_date: Some("20260918".to_string()),
        };
        let results = vec![result("CME-Delayed"), result("CME")];
        let target = RithmicReconnectTarget {
            symbol: "MNQU6".to_string(),
            exchange: "CME".to_string(),
            series: crate::rithmic_history::RithmicSeries::Minute5,
        };
        assert_eq!(reconnect_contract_index(&results, &target), Some(1));
        let missing = RithmicReconnectTarget {
            exchange: "CBOT".to_string(),
            ..target
        };
        assert_eq!(reconnect_contract_index(&results, &missing), None);
    }

    #[test]
    fn gpui_theme_attachment_preserves_alpha() {
        let attached = gpui_color(ThemeColor::from_rgb8(240, 240, 240).with_alpha(19.0 / 255.0));
        assert!((attached.a - 19.0 / 255.0).abs() < f32::EPSILON);
    }

    #[test]
    fn rithmic_test_cli_requires_one_history_root_and_only_the_diagnostics_flag() {
        let (root, detailed) = parse_rithmic_test_arguments(
            ["cache", "--detailed-diagnostics"]
                .into_iter()
                .map(OsString::from),
        )
        .expect("valid Rithmic Test arguments parse");
        assert_eq!(root, std::path::PathBuf::from("cache"));
        assert!(detailed);
        assert!(parse_rithmic_test_arguments(std::iter::empty()).is_err());
        assert!(
            parse_rithmic_test_arguments(
                [OsString::from("cache"), OsString::from("--unknown")].into_iter()
            )
            .is_err()
        );
    }

    #[test]
    fn header_lifecycle_and_health_values_are_truthfully_labeled() {
        assert_eq!(
            connection_presentation(
                FeedConnectionState::Disconnected,
                ChartState::Loading,
                false,
            )
            .0,
            "Offline"
        );
        assert_eq!(
            connection_presentation(FeedConnectionState::Recovering, ChartState::Loading, false).0,
            "Test · Reconnecting"
        );
        assert_eq!(
            connection_presentation(FeedConnectionState::Streaming, ChartState::Ready, false).0,
            "Test · Live"
        );
        assert_eq!(
            connection_presentation(FeedConnectionState::Streaming, ChartState::Stale, false).0,
            "Test · Stale"
        );
        assert_eq!(
            connection_presentation(FeedConnectionState::Streaming, ChartState::Ready, true).0,
            "Test · Delayed"
        );
        assert_eq!(milli_rate(12_345), "12.345");
        assert_eq!(duration_label(850_000), "850 µs");
        assert_eq!(duration_label(42_000_000), "42 ms");
        assert_eq!(duration_label(1_500_000_000), "1.5 s");
        let controls = HeaderControls::from_state(true, true, false);
        assert!(controls.enabled(HeaderControls::INSTRUMENT));
        assert!(controls.enabled(HeaderControls::SERIES));
        assert!(controls.enabled(HeaderControls::DOM));
        assert!(!controls.enabled(HeaderControls::HEALTH));
    }
}
