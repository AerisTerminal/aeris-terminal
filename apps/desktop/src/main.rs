//! Axiusflow's native GPUI terminal entry point.

mod assets;
mod chart_chrome;
mod live_market_worker;
mod market_worker;
mod readiness_conformance;
mod rithmic_history;
mod rithmic_live_chart;
mod rithmic_market_worker;
mod rithmic_shell;
mod rithmic_transition_capture;
mod windowed_benchmark;

use axiusflow_application::{ReplayProvenance, ReplayStreamUpdate};
use axiusflow_chart_integration::{
    ChartBridgeMetrics, ChartDrawingTool, ChartIndicator, ChartThemeMode, OriginChartView,
};
use axiusflow_coinbase_market_adapter::CoinbaseSpotProduct;
use axiusflow_design_system::{AxiusflowTheme, RadiusToken, ThemeColor};
use axiusflow_market_data::ChartInterval;
use axiusflow_observability::{FeedConnectionState, FeedDiagnosticsSnapshot};
use axiusflow_rithmic_protocol_adapter::{
    RithmicCatalogEvent, RithmicCatalogRejection, RithmicInstrumentSelection,
    RithmicProviderCommandError as RithmicCommandError, RithmicReadOnlySubscription,
    RithmicSymbolSearch, SearchPattern,
};
use axiusflow_terminal_ui::{DomFrame, ReadOnlyDomView};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Entity, FocusHandle, FontWeight, Hsla,
    KeyDownEvent, MouseButton, Render, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    rgb, size,
};
use gpui_component::{
    ActiveTheme, Disableable, Icon, Root, Selectable, Sizable, StyledExt, TitleBar,
    button::{Button, ButtonVariants},
    hover_card::HoverCard,
    input::{Input, InputEvent, InputState},
    resizable::{h_resizable, resizable_panel},
    scroll::ScrollableElement,
    spinner::Spinner,
    theme::{Theme as ComponentTheme, ThemeMode as ComponentThemeMode, ThemeTokens},
};
use gpui_hugeicons::{HugeiconsAssets, IconName as HugeIcon};
use gpui_platform::application;
use market_worker::{
    ChartState, DesktopMarketGeneration, MarketDataWorker, MarketWorkerBootstrap,
    MarketWorkerMessage, MarketWorkerPublication, MarketWorkerStartup, PendingUiDiagnostics,
    UiDiagnosticsFeedback,
};
use std::{path::PathBuf, rc::Rc, sync::mpsc::TrySendError, time::Duration};

const SIDE_PANEL_INITIAL_WIDTH: f32 = 320.0;
const SIDE_PANEL_MINIMUM_WIDTH: f32 = 240.0;
const SIDE_PANEL_MAXIMUM_WIDTH: f32 = 640.0;
const MAXIMUM_STATUS_CHARACTERS: usize = 160;
const TOOLTIP_OPEN_DELAY: Duration = Duration::from_millis(400);
const TOOLTIP_CLOSE_DELAY: Duration = Duration::ZERO;

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
    drawing_toolbar: DrawingToolbarVisibility,
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
    symbol_selection_pending: bool,
    series_browser: rithmic_history::RithmicSeriesBrowser,
    series_message: String,
    rithmic_autoload_started: bool,
    rithmic_reconnect: RithmicReconnectState,
    symbol_input: Option<Entity<InputState>>,
    indicator_input: Entity<InputState>,
    indicator_message: Option<String>,
    chrome_overlay: Option<ChromeOverlay>,
    chrome_selection: usize,
    chrome_focus: FocusHandle,
    provider: TerminalProvider,
    coinbase_products: Vec<CoinbaseSpotProduct>,
    coinbase_product: Option<CoinbaseSpotProduct>,
    coinbase_history_root: Option<PathBuf>,
    coinbase_diagnostics: CoinbaseDiagnosticsMode,
    coinbase_switch: CoinbaseSwitchState,
    coinbase_interval: ChartInterval,
    coinbase_catalog: CoinbaseCatalogState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalProvider {
    Coinbase,
    Rithmic,
    Fixture,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CoinbaseSwitchState {
    #[default]
    Idle,
    Pending,
}

impl CoinbaseSwitchState {
    const fn is_pending(self) -> bool {
        matches!(self, Self::Pending)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CoinbaseCatalogState {
    #[default]
    Loading,
    Ready,
}

impl CoinbaseCatalogState {
    const fn needs_fetch(self) -> bool {
        matches!(self, Self::Loading)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoinbaseDiagnosticsMode {
    Standard,
    Detailed,
}

impl CoinbaseDiagnosticsMode {
    const fn from_enabled(enabled: bool) -> Self {
        if enabled {
            Self::Detailed
        } else {
            Self::Standard
        }
    }

    const fn enabled(self) -> bool {
        matches!(self, Self::Detailed)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChromeOverlay {
    Instrument,
    Indicator,
    Timeframe,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RithmicSessionRetirement {
    None,
    Offline,
    Recovering,
    Stopped,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DrawingToolbarVisibility {
    #[default]
    Expanded,
    Collapsed,
}

impl DrawingToolbarVisibility {
    const fn is_collapsed(self) -> bool {
        matches!(self, Self::Collapsed)
    }

    fn toggle(&mut self) {
        *self = if self.is_collapsed() {
            Self::Expanded
        } else {
            Self::Collapsed
        };
    }
}

impl RithmicSessionRetirement {
    const fn from_connection(state: FeedConnectionState) -> Self {
        match state {
            FeedConnectionState::Disconnected => Self::Offline,
            FeedConnectionState::Recovering => Self::Recovering,
            FeedConnectionState::Stopped => Self::Stopped,
            FeedConnectionState::Discovering
            | FeedConnectionState::Authenticating
            | FeedConnectionState::Streaming => Self::None,
        }
    }

    const fn chart_state(self, has_market_data: bool) -> Option<ChartState> {
        match self {
            Self::Offline if has_market_data => Some(ChartState::Stale),
            Self::Recovering if has_market_data => Some(ChartState::Recovering),
            Self::Stopped => Some(ChartState::Error),
            Self::None | Self::Offline | Self::Recovering => None,
        }
    }
}

fn rithmic_production_subscription() -> Result<RithmicReadOnlySubscription, RithmicCommandError> {
    RithmicReadOnlySubscription::try_new(true, true, true)
}

const fn should_apply_rithmic_worker_stop(
    disconnected: bool,
    connection_state: Option<FeedConnectionState>,
) -> bool {
    disconnected
        && matches!(
            connection_state,
            Some(state) if !matches!(state, FeedConnectionState::Stopped)
        )
}

impl RithmicReconnectState {
    fn target(&self) -> Option<&RithmicReconnectTarget> {
        match self {
            Self::AwaitingSearch(target) | Self::SearchInFlight(target) => Some(target),
            Self::Idle => None,
        }
    }

    fn capture_retired_selection(
        &mut self,
        selection: Option<rithmic_shell::RithmicSymbolSelection>,
        series: rithmic_history::RithmicSeries,
    ) -> bool {
        if *self == Self::Idle
            && let Some(selection) = selection
        {
            *self = Self::AwaitingSearch(RithmicReconnectTarget {
                symbol: selection.instrument.symbol,
                exchange: selection.instrument.exchange,
                series,
            });
        }
        *self == Self::Idle
    }
}

fn observe_chart(chart: Option<&Entity<OriginChartView>>, cx: &mut Context<TerminalApp>) {
    if let Some(chart) = chart {
        cx.observe(chart, |_, _, cx| cx.notify()).detach();
    }
}

#[derive(Clone)]
enum InstrumentMenuSelection {
    Rithmic(usize),
    Coinbase(CoinbaseSpotProduct),
}

#[derive(Clone)]
struct InstrumentMenuEntry {
    symbol: String,
    detail: String,
    checked: bool,
    selection: InstrumentMenuSelection,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RithmicReadyAction {
    None,
    Reconnect(String),
    Autoload,
}

fn rithmic_ready_action(
    state: FeedConnectionState,
    message: &str,
    reconnect: &RithmicReconnectState,
    autoload_started: bool,
) -> RithmicReadyAction {
    if state != FeedConnectionState::Authenticating
        || !message.contains("ready for instrument search")
    {
        return RithmicReadyAction::None;
    }
    match reconnect {
        RithmicReconnectState::AwaitingSearch(target) => {
            RithmicReadyAction::Reconnect(target.symbol.clone())
        }
        RithmicReconnectState::Idle if !autoload_started => RithmicReadyAction::Autoload,
        RithmicReconnectState::Idle | RithmicReconnectState::SearchInFlight(_) => {
            RithmicReadyAction::None
        }
    }
}

struct HeaderState {
    theme: AxiusflowTheme,
    provider: TerminalProvider,
    instrument_label: String,
    series_label: String,
    instruments: Vec<InstrumentMenuEntry>,
    selected_series: Option<rithmic_history::RithmicSeries>,
    symbol_input: Option<Entity<InputState>>,
    indicator_input: Entity<InputState>,
    indicator_message: Option<String>,
    symbol_message: String,
    series_message: String,
    pending: HeaderPendingState,
    controls: HeaderControls,
    dom_visible: bool,
    health_visible: bool,
    connection_state: FeedConnectionState,
    chart_state: ChartState,
    delayed: bool,
}

#[derive(Clone, Copy, Default)]
struct HeaderPendingState {
    symbol_selection: bool,
    series: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SidePanel {
    Dom,
    Health,
}

impl SidePanel {
    const fn title(self) -> &'static str {
        match self {
            Self::Dom => "Order book",
            Self::Health => "Feed health",
        }
    }

    const fn toggle_label(self) -> &'static str {
        match self {
            Self::Dom => "DOM",
            Self::Health => "Health",
        }
    }

    const fn toggle_tooltip(self) -> &'static str {
        match self {
            Self::Dom => "Toggle read-only depth panel",
            Self::Health => "Toggle feed health panel",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChartNoticePlacement {
    Center,
    TopLeft,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChartNoticeTone {
    Muted,
    Warning,
    Loss,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ChartSurfaceNotice {
    label: &'static str,
    detail: Option<String>,
    placement: ChartNoticePlacement,
    tone: ChartNoticeTone,
}

fn chart_surface_notice(
    state: ChartState,
    has_market_data: bool,
    detail: &str,
) -> Option<ChartSurfaceNotice> {
    let placement = if has_market_data {
        ChartNoticePlacement::TopLeft
    } else {
        ChartNoticePlacement::Center
    };
    let detail = bounded_status_detail(detail, state.label());
    match state {
        ChartState::Loading => Some(ChartSurfaceNotice {
            label: state.label(),
            detail,
            placement,
            tone: ChartNoticeTone::Muted,
        }),
        ChartState::Ready => None,
        ChartState::Stale | ChartState::Recovering => Some(ChartSurfaceNotice {
            label: state.label(),
            detail,
            placement,
            tone: ChartNoticeTone::Warning,
        }),
        ChartState::Error => Some(ChartSurfaceNotice {
            label: state.label(),
            detail,
            placement,
            tone: ChartNoticeTone::Loss,
        }),
    }
}

fn bounded_status_detail(detail: &str, generic_label: &str) -> Option<String> {
    let detail = detail.trim();
    if detail.is_empty() || detail.eq_ignore_ascii_case(generic_label) {
        return None;
    }
    let mut bounded: String = detail.chars().take(MAXIMUM_STATUS_CHARACTERS).collect();
    if detail.chars().count() > MAXIMUM_STATUS_CHARACTERS {
        bounded.push('…');
    }
    Some(bounded)
}

fn chart_status_detail<'a>(
    chart_state: ChartState,
    connection_state: FeedConnectionState,
    chart_message: &'a str,
    connection_message: Option<&'a str>,
) -> &'a str {
    if chart_state != ChartState::Ready && connection_state != FeedConnectionState::Streaming {
        connection_message.unwrap_or(chart_message)
    } else {
        chart_message
    }
}

fn instrument_selector_label(selected: Option<(&str, &str)>, selection_pending: bool) -> String {
    let _ = selection_pending;
    selected.map_or_else(
        || "Contract".to_string(),
        |(symbol, exchange)| format!("{symbol} / {exchange}"),
    )
}

fn terminal_instrument_label(app: &TerminalApp) -> String {
    if app.provider == TerminalProvider::Coinbase {
        return app.coinbase_product.as_ref().map_or_else(
            || "Select market".to_string(),
            |product| product.display_symbol.clone(),
        );
    }
    instrument_selector_label(
        app.symbol_browser.selected().map(|selection| {
            (
                selection.instrument.symbol.as_str(),
                selection.instrument.exchange.as_str(),
            )
        }),
        app.symbol_selection_pending,
    )
}

fn series_selector_label(
    selected: Option<rithmic_history::RithmicSeries>,
    pending: Option<rithmic_history::RithmicSeries>,
) -> String {
    let _ = pending;
    selected
        .map_or("Series", rithmic_history::RithmicSeries::label)
        .to_string()
}

#[derive(Clone, Copy)]
struct HeaderControls(u8);

impl HeaderControls {
    const INSTRUMENT: u8 = 1;
    const SERIES: u8 = 2;
    const DOM: u8 = 4;
    const HEALTH: u8 = 8;
    const FIT: u8 = 16;
    const LATEST: u8 = 32;

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

    const fn with_chart_controls(mut self, chart_ready: bool) -> Self {
        if chart_ready {
            self.0 |= Self::FIT | Self::LATEST;
        }
        self
    }
}

struct TerminalStartupState {
    chart: Option<Entity<OriginChartView>>,
    chart_state: ChartState,
    chart_state_message: String,
    replay_label: String,
    worker_label: String,
    subscription_id: String,
    connection_state: Option<FeedConnectionState>,
    connection_message: Option<String>,
    provider: TerminalProvider,
    coinbase_product: Option<CoinbaseSpotProduct>,
    coinbase_history_root: Option<PathBuf>,
    coinbase_detailed_diagnostics: bool,
}

fn terminal_startup_state(
    startup: MarketWorkerStartup,
    cx: &mut Context<TerminalApp>,
) -> TerminalStartupState {
    match startup {
        MarketWorkerStartup::Shell(shell) => {
            let profile = shell.profile_label();
            let connection = shell.connection();
            let message = shell.message().to_string();
            TerminalStartupState {
                chart: Some(cx.new(move |_| OriginChartView::empty())),
                chart_state: ChartState::Loading,
                chart_state_message: message.clone(),
                replay_label: profile,
                worker_label: "Rithmic market worker".to_string(),
                subscription_id: "Loading chart".to_string(),
                connection_state: Some(connection),
                connection_message: Some(message),
                provider: TerminalProvider::Rithmic,
                coinbase_product: None,
                coinbase_history_root: None,
                coinbase_detailed_diagnostics: false,
            }
        }
        MarketWorkerStartup::Loading(startup) => TerminalStartupState {
            chart: None,
            chart_state: ChartState::Loading,
            chart_state_message: "waiting for a covering market snapshot".to_string(),
            replay_label: "waiting for a covering market snapshot".to_string(),
            worker_label: startup.worker_label,
            subscription_id: startup.subscription_id,
            connection_state: Some(FeedConnectionState::Discovering),
            connection_message: Some("Connecting to Coinbase public markets".to_string()),
            provider: TerminalProvider::Coinbase,
            coinbase_product: Some(startup.coinbase_product),
            coinbase_history_root: Some(startup.history_root),
            coinbase_detailed_diagnostics: startup.detailed_diagnostics,
        },
        MarketWorkerStartup::Ready(bootstrap) => {
            let replay_label = generation_status(
                &bootstrap.worker_label,
                &bootstrap.subscription_id,
                &bootstrap.generation,
            );
            let snapshot = bootstrap.snapshot;
            TerminalStartupState {
                chart: Some(cx.new(move |_| OriginChartView::with_replay(&snapshot))),
                chart_state: ChartState::Ready,
                chart_state_message: "market snapshot is current".to_string(),
                replay_label,
                worker_label: bootstrap.worker_label,
                subscription_id: bootstrap.subscription_id,
                connection_state: None,
                connection_message: None,
                provider: TerminalProvider::Fixture,
                coinbase_product: None,
                coinbase_history_root: None,
                coinbase_detailed_diagnostics: false,
            }
        }
    }
}

impl TerminalApp {
    fn new(
        cx: &mut Context<Self>,
        startup: MarketWorkerStartup,
        market_worker: MarketDataWorker,
        symbol_input: Option<Entity<InputState>>,
        indicator_input: Entity<InputState>,
    ) -> Self {
        let theme = AxiusflowTheme::dark();
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
            coinbase_product,
            coinbase_history_root,
            coinbase_detailed_diagnostics,
        } = terminal_startup_state(startup, cx);
        let bridge_label = chart.as_ref().map_or_else(
            || "bridge awaiting snapshot".to_string(),
            |chart| bridge_status(chart.read(cx).replay_bridge_metrics()),
        );
        observe_chart(chart.as_ref(), cx);
        let dom = cx.new(move |_| ReadOnlyDomView::new(theme));
        Self {
            chart,
            dom,
            side_panel: None,
            drawing_toolbar: DrawingToolbarVisibility::Expanded,
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
            symbol_message: if provider == TerminalProvider::Coinbase {
                "Loading Coinbase public spot catalog".to_string()
            } else {
                "Search for an entitled Rithmic Test symbol".to_string()
            },
            symbol_selection_pending: false,
            series_browser: rithmic_history::RithmicSeriesBrowser::default(),
            series_message: "Select a symbol before choosing a series".to_string(),
            rithmic_autoload_started: false,
            rithmic_reconnect: RithmicReconnectState::Idle,
            symbol_input,
            indicator_input,
            indicator_message: None,
            chrome_overlay: None,
            chrome_selection: 0,
            chrome_focus: cx.focus_handle().tab_stop(true),
            provider,
            coinbase_products: coinbase_product.clone().into_iter().collect(),
            coinbase_product,
            coinbase_history_root,
            coinbase_diagnostics: CoinbaseDiagnosticsMode::from_enabled(
                coinbase_detailed_diagnostics,
            ),
            coinbase_switch: CoinbaseSwitchState::Idle,
            coinbase_interval: ChartInterval::Minute1,
            coinbase_catalog: CoinbaseCatalogState::Loading,
        }
    }

    fn available_intervals(&self) -> Vec<ChartInterval> {
        ChartInterval::ALL
            .into_iter()
            .filter(|interval| {
                self.provider != TerminalProvider::Coinbase || *interval != ChartInterval::Tick100
            })
            .collect()
    }

    fn selected_interval(&self) -> ChartInterval {
        if self.provider == TerminalProvider::Coinbase {
            self.coinbase_interval
        } else {
            self.series_browser
                .selected()
                .map_or(ChartInterval::Minute1, |request| request.series.interval())
        }
    }

    fn select_interval(&mut self, interval: ChartInterval, cx: &mut Context<Self>) -> bool {
        if self.provider != TerminalProvider::Coinbase {
            self.select_rithmic_series(interval.into(), cx);
            return true;
        }
        if self.coinbase_interval == interval {
            return true;
        }
        let (Some(product), Some(history_root)) = (
            self.coinbase_product.clone(),
            self.coinbase_history_root.clone(),
        ) else {
            self.series_message = "Coinbase market selection is unavailable".to_string();
            cx.notify();
            return false;
        };
        let worker = MarketDataWorker::start_coinbase_product_interval(
            product,
            interval,
            history_root,
            std::thread::current().id(),
            self.coinbase_diagnostics.enabled(),
            self.coinbase_catalog.needs_fetch(),
        );
        let Ok((_, worker)) = worker else {
            self.series_message = format!("{} history could not start", interval.label());
            cx.notify();
            return false;
        };
        let previous = std::mem::replace(&mut self.market_worker, worker);
        previous.shutdown_detached();
        self.coinbase_interval = interval;
        self.coinbase_switch = CoinbaseSwitchState::Pending;
        self.chart_state = ChartState::Loading;
        self.chart_state_message = format!("Loading {} history", interval.label());
        self.series_message = format!("Switching to {}", interval.label());
        cx.notify();
        true
    }

    fn instrument_entries(&self, cx: &App) -> Vec<InstrumentMenuEntry> {
        if self.provider == TerminalProvider::Coinbase {
            let query = self
                .symbol_input
                .as_ref()
                .map(|input| input.read(cx).value().trim().to_ascii_uppercase())
                .unwrap_or_default();
            return self
                .coinbase_products
                .iter()
                .filter(|product| {
                    query.is_empty()
                        || product.product_id.contains(&query)
                        || product.display_symbol.contains(&query)
                        || product.base_currency.contains(&query)
                })
                .map(|product| InstrumentMenuEntry {
                    symbol: product.display_symbol.clone(),
                    detail: format!(
                        "Coinbase spot · price 1e-{} · size 1e-{}",
                        product.price_scale, product.quantity_scale
                    ),
                    checked: self
                        .coinbase_product
                        .as_ref()
                        .is_some_and(|selected| selected.product_id == product.product_id),
                    selection: InstrumentMenuSelection::Coinbase(product.clone()),
                })
                .collect();
        }
        self.symbol_browser
            .results()
            .iter()
            .enumerate()
            .map(|(index, instrument)| InstrumentMenuEntry {
                symbol: instrument.symbol.clone(),
                detail: instrument_menu_detail(instrument),
                checked: self.symbol_browser.selected().is_some_and(|selected| {
                    selected.instrument.symbol == instrument.symbol
                        && selected.instrument.exchange == instrument.exchange
                }),
                selection: InstrumentMenuSelection::Rithmic(index),
            })
            .collect()
    }

    fn select_instrument(
        &mut self,
        selection: InstrumentMenuSelection,
        cx: &mut Context<Self>,
    ) -> bool {
        match selection {
            InstrumentMenuSelection::Rithmic(index) => self.select_rithmic_symbol(index, cx),
            InstrumentMenuSelection::Coinbase(product) => {
                if self
                    .coinbase_product
                    .as_ref()
                    .is_some_and(|selected| selected.product_id == product.product_id)
                {
                    return true;
                }
                let Some(history_root) = self.coinbase_history_root.clone() else {
                    self.symbol_message = "Coinbase history root is unavailable".to_string();
                    cx.notify();
                    return false;
                };
                let worker = MarketDataWorker::start_coinbase_product_interval(
                    product.clone(),
                    self.coinbase_interval,
                    history_root,
                    std::thread::current().id(),
                    self.coinbase_diagnostics.enabled(),
                    self.coinbase_catalog.needs_fetch(),
                );
                let Ok((_, worker)) = worker else {
                    self.symbol_message = "Coinbase product worker could not start".to_string();
                    cx.notify();
                    return false;
                };
                let previous = std::mem::replace(&mut self.market_worker, worker);
                previous.shutdown_detached();
                self.coinbase_product = Some(product.clone());
                self.coinbase_switch = CoinbaseSwitchState::Pending;
                self.symbol_selection_pending = true;
                self.chart_state = ChartState::Loading;
                self.chart_state_message = format!("Loading {} market history", product.product_id);
                self.symbol_message = format!("Switching to {}", product.product_id);
                cx.notify();
                true
            }
        }
    }

    fn open_chrome_overlay(
        &mut self,
        overlay: ChromeOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.chrome_overlay = Some(overlay);
        self.chrome_selection = match overlay {
            ChromeOverlay::Timeframe => self
                .available_intervals()
                .iter()
                .position(|interval| *interval == self.selected_interval())
                .unwrap_or(0),
            ChromeOverlay::Instrument | ChromeOverlay::Indicator => 0,
        };
        match overlay {
            ChromeOverlay::Instrument => {
                if let Some(input) = &self.symbol_input {
                    input.update(cx, |input, input_cx| input.focus(window, input_cx));
                }
            }
            ChromeOverlay::Indicator => {
                self.indicator_input
                    .update(cx, |input, input_cx| input.focus(window, input_cx));
            }
            ChromeOverlay::Timeframe => self.chrome_focus.focus(window, cx),
        }
        cx.notify();
    }

    fn close_chrome_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chrome_overlay == Some(ChromeOverlay::Indicator) {
            self.indicator_input.update(cx, |input, input_cx| {
                input.set_value("", window, input_cx);
            });
        }
        self.chrome_overlay = None;
        self.chrome_focus.focus(window, cx);
        cx.notify();
    }

    fn on_chrome_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.chrome_overlay.is_none() {
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => self.close_chrome_overlay(window, cx),
            "up" => {
                self.chrome_selection = self.chrome_selection.saturating_sub(1);
                cx.notify();
            }
            "down" => {
                let count = match self.chrome_overlay {
                    Some(ChromeOverlay::Instrument) => self.instrument_entries(cx).len(),
                    Some(ChromeOverlay::Indicator) => chart_chrome::filter_indicator_specs(
                        self.indicator_input.read(cx).value().as_ref(),
                    )
                    .len(),
                    Some(ChromeOverlay::Timeframe) => self.available_intervals().len(),
                    None => 0,
                };
                self.chrome_selection = (self.chrome_selection + 1).min(count.saturating_sub(1));
                cx.notify();
            }
            "enter" if self.chrome_overlay == Some(ChromeOverlay::Timeframe) => {
                if let Some(interval) = self
                    .available_intervals()
                    .get(self.chrome_selection)
                    .copied()
                    && self.select_interval(interval, cx)
                {
                    self.close_chrome_overlay(window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
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
        if self.coinbase_switch.is_pending()
            && let axiusflow_application::ReplayStreamUpdate::Snapshot(snapshot) =
                publication.update
        {
            let chart = cx.new(move |_| OriginChartView::with_replay(&snapshot));
            observe_chart(Some(&chart), cx);
            self.chart = Some(chart);
            self.coinbase_switch = CoinbaseSwitchState::Idle;
            self.symbol_selection_pending = false;
            self.chart_state = ChartState::Ready;
            self.chart_state_message = "market snapshot is current".to_string();
            self.symbol_message = self.coinbase_product.as_ref().map_or_else(
                || "Coinbase market ready".to_string(),
                |product| format!("{} · Coinbase spot", product.product_id),
            );
            self.connection_state = Some(FeedConnectionState::Streaming);
            self.connection_message = Some("Coinbase public market stream is live".to_string());
            self.finish_ui_diagnostics(ui_diagnostics, true);
            cx.notify();
            return;
        }
        let (next_state, accepted) = match (&self.chart, publication.update) {
            (None, axiusflow_application::ReplayStreamUpdate::Snapshot(snapshot)) => {
                let chart = cx.new(move |_| OriginChartView::with_replay(&snapshot));
                observe_chart(Some(&chart), cx);
                self.chart = Some(chart);
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
            if self.provider == TerminalProvider::Coinbase {
                self.connection_state = Some(FeedConnectionState::Streaming);
                self.connection_message = Some("Coinbase public market stream is live".to_string());
            }
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
        eprintln!("market worker invalidated the stream: {message}");
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
        self.chart = Some(cx.new(move |_| OriginChartView::empty()));
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
                self.feed_diagnostics = Some(snapshot);
                cx.notify();
            }
            MarketWorkerMessage::Recovery { request_id, result } => {
                self.apply_recovery(request_id, result, cx);
            }
            MarketWorkerMessage::State { state, message } => {
                if state == ChartState::Error && self.provider == TerminalProvider::Coinbase {
                    self.coinbase_switch = CoinbaseSwitchState::Idle;
                    self.symbol_selection_pending = false;
                    self.connection_state = Some(FeedConnectionState::Disconnected);
                    self.connection_message = Some(message.clone());
                } else if self.provider == TerminalProvider::Coinbase
                    && matches!(state, ChartState::Loading | ChartState::Recovering)
                {
                    self.connection_state = Some(FeedConnectionState::Recovering);
                    self.connection_message = Some(message.clone());
                }
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
            MarketWorkerMessage::CoinbaseCatalog(result) => match result {
                Ok(products) => {
                    self.coinbase_catalog = CoinbaseCatalogState::Ready;
                    self.coinbase_products = products;
                    self.symbol_message = format!(
                        "{} active Coinbase spot markets",
                        self.coinbase_products.len()
                    );
                    cx.notify();
                }
                Err(error) => {
                    self.coinbase_catalog = CoinbaseCatalogState::Loading;
                    self.symbol_message = format!("Coinbase catalog unavailable: {error}");
                    cx.notify();
                }
            },
            MarketWorkerMessage::CoinbaseDom(frame) => {
                self.dom
                    .update(cx, |dom, dom_cx| dom.replace_frame(frame, dom_cx));
            }
        }
    }

    fn poll_market_worker(&mut self, cx: &mut Context<Self>) {
        if !self.window_active {
            return;
        }
        let (messages, disconnected) = self.market_worker.drain_messages();
        for message in messages {
            self.apply_market_worker_message(message, cx);
        }
        if self.provider == TerminalProvider::Rithmic
            && should_apply_rithmic_worker_stop(disconnected, self.connection_state)
        {
            self.apply_connection_state(
                FeedConnectionState::Stopped,
                "Rithmic market worker stopped".to_string(),
                cx,
            );
        } else if disconnected && self.chart_state != ChartState::Error {
            let message = match self.provider {
                TerminalProvider::Coinbase => "Coinbase market worker stopped",
                TerminalProvider::Rithmic => "Rithmic market worker stopped",
                TerminalProvider::Fixture => "Fixture market worker stopped",
            }
            .to_string();
            self.connection_state = Some(FeedConnectionState::Stopped);
            self.connection_message = Some(message.clone());
            self.set_chart_state(ChartState::Error, message, cx);
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
        let retirement = RithmicSessionRetirement::from_connection(state);
        let retained_market_data = self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).has_market_data());
        match retirement {
            RithmicSessionRetirement::Offline | RithmicSessionRetirement::Recovering => {
                self.begin_rithmic_reconnect(cx);
            }
            RithmicSessionRetirement::Stopped => {
                self.rithmic_reconnect = RithmicReconnectState::Idle;
                self.retire_rithmic_session(cx);
            }
            RithmicSessionRetirement::None => {}
        }
        if let Some(chart_state) = retirement.chart_state(retained_market_data) {
            self.chart_state = chart_state;
            self.chart_state_message.clone_from(&message);
        }
        self.connection_state = Some(state);
        let ready_action = rithmic_ready_action(
            state,
            &message,
            &self.rithmic_reconnect,
            self.rithmic_autoload_started,
        );
        self.connection_message = Some(message);
        match ready_action {
            RithmicReadyAction::Reconnect(symbol) => {
                if self.search_rithmic_query(&symbol, cx)
                    && let RithmicReconnectState::AwaitingSearch(target) = &self.rithmic_reconnect
                {
                    self.rithmic_reconnect = RithmicReconnectState::SearchInFlight(target.clone());
                }
            }
            RithmicReadyAction::Autoload => {
                self.rithmic_autoload_started = true;
                let _ = self.search_rithmic_query("MNQ", cx);
            }
            RithmicReadyAction::None => {}
        }
        cx.notify();
    }

    fn toggle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = self.theme.toggled();
        sync_component_theme(&theme, Some(window), cx);
        self.dom.update(cx, |dom, dom_cx| {
            dom.set_theme(theme, dom_cx);
        });
        if let Some(chart) = &self.chart {
            let chart_mode = match theme.mode {
                axiusflow_design_system::ThemeMode::Light => ChartThemeMode::Light,
                axiusflow_design_system::ThemeMode::Dark => ChartThemeMode::Dark,
            };
            chart.update(cx, |chart, chart_cx| {
                chart.set_theme_mode(chart_mode);
                chart_cx.notify();
            });
        }
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
        if self.symbol_browser.search_pending() || self.symbol_selection_pending {
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
            self.symbol_browser.reject_search(request.request_id);
            self.symbol_message = "Symbol search request is invalid".to_string();
            cx.notify();
            return false;
        };
        let dispatched = if self.market_worker.try_search_rithmic(search).is_ok() {
            self.symbol_message = "Searching Rithmic Test symbols".to_string();
            true
        } else {
            self.symbol_browser.reject_search(request.request_id);
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

    fn submit_symbol_input(&mut self, cx: &mut Context<Self>) -> bool {
        if self.provider != TerminalProvider::Coinbase {
            self.search_rithmic_input(cx);
            return true;
        }
        let Some(first) = self
            .instrument_entries(cx)
            .into_iter()
            .nth(self.chrome_selection)
        else {
            self.symbol_message = "No Coinbase spot markets match this search".to_string();
            cx.notify();
            return false;
        };
        self.select_instrument(first.selection, cx)
    }

    fn begin_rithmic_reconnect(&mut self, cx: &mut Context<Self>) {
        let series = self
            .series_browser
            .selected()
            .map_or(rithmic_history::RithmicSeries::Minute1, |request| {
                request.series
            });
        if self
            .rithmic_reconnect
            .capture_retired_selection(self.symbol_browser.selected().cloned(), series)
        {
            self.rithmic_autoload_started = false;
        }
        self.retire_rithmic_session(cx);
    }

    fn retire_rithmic_session(&mut self, cx: &mut Context<Self>) {
        self.symbol_selection_pending = false;
        self.symbol_browser.invalidate_session();
        self.series_browser.reset();
        self.feed_diagnostics = None;
        self.dom
            .update(cx, axiusflow_terminal_ui::ReadOnlyDomView::clear);
    }

    fn select_rithmic_symbol(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        if self.symbol_selection_pending {
            self.symbol_message = "A contract selection is already in progress".to_string();
            cx.notify();
            return false;
        }
        let Some(selection) = self.symbol_browser.select(index) else {
            return false;
        };
        let entitlement_id = format!(
            "rithmic-test:{}:{}",
            selection.instrument.exchange, selection.instrument.symbol
        );
        let request = rithmic_production_subscription().and_then(|subscription| {
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
            self.symbol_browser.reject_selection(selection.generation);
            self.symbol_message = "Symbol selection is invalid".to_string();
            cx.notify();
            return false;
        };
        let dispatched = if self.market_worker.try_select_rithmic(request).is_ok() {
            self.symbol_selection_pending = true;
            self.dom
                .update(cx, axiusflow_terminal_ui::ReadOnlyDomView::clear);
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
                    self.symbol_selection_pending = false;
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
                let (rejected, selection_rejected) = match catalog_rejection_domain(reason) {
                    CatalogCommandDomain::Search => {
                        (self.symbol_browser.reject_search(command_generation), false)
                    }
                    CatalogCommandDomain::Selection => (
                        self.symbol_browser.reject_selection(command_generation),
                        true,
                    ),
                };
                if rejected {
                    if selection_rejected {
                        self.symbol_selection_pending = false;
                    }
                    self.symbol_message = catalog_rejection_message(reason).to_string();
                    if let Some(target) = self.rithmic_reconnect.target().cloned() {
                        self.rithmic_reconnect = RithmicReconnectState::AwaitingSearch(target);
                        self.retire_rithmic_session(cx);
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
        if self.series_browser.pending().is_some() {
            self.series_message = "A chart series is already loading".to_string();
            cx.notify();
            return;
        }
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
        let visible_bar_count = snapshot.bars().len();
        self.chart = Some(cx.new(move |_| OriginChartView::with_replay(&snapshot)));
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

    fn close_side_panel(&mut self, cx: &mut Context<Self>) {
        if self.side_panel.take().is_some() {
            cx.notify();
        }
    }

    fn reset_chart_view(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.reset_view();
                chart_cx.notify();
            });
        }
    }

    fn scroll_chart_to_latest(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.scroll_to_latest();
                chart_cx.notify();
            });
        }
    }

    fn select_drawing_tool(&mut self, tool: ChartDrawingTool, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.set_drawing_tool(tool);
                chart_cx.notify();
            });
            cx.notify();
        }
    }

    fn remove_selected_drawing(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                if chart.remove_selected_drawing() {
                    chart.cancel_drawing();
                    chart_cx.notify();
                }
            });
            cx.notify();
        }
    }

    fn toggle_selected_drawing_lock(&mut self, cx: &mut Context<Self>) {
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

    fn toggle_all_drawings_lock(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                let all_locked = chart.drawings_lock_summary().all_locked;
                if chart.set_all_drawings_locked(!all_locked) {
                    chart_cx.notify();
                }
            });
            cx.notify();
        }
    }

    fn clear_drawings(&mut self, cx: &mut Context<Self>) {
        if let Some(chart) = &self.chart {
            chart.update(cx, |chart, chart_cx| {
                chart.clear_drawings();
                chart.cancel_drawing();
                chart_cx.notify();
            });
            cx.notify();
        }
    }

    fn toggle_drawing_toolbar(&mut self, cx: &mut Context<Self>) {
        self.drawing_toolbar.toggle();
        cx.notify();
    }

    fn add_indicator(&mut self, indicator: ChartIndicator, cx: &mut Context<Self>) -> bool {
        let Some(chart) = self.chart.clone() else {
            self.indicator_message = Some("Chart data is not available yet".to_string());
            cx.notify();
            return false;
        };
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
                true
            }
            Err(error) => {
                self.indicator_message = Some(error.to_string());
                cx.notify();
                false
            }
        }
    }

    fn drawing_toolbar_state(&self, cx: &App) -> DrawingToolbarState {
        self.chart
            .as_ref()
            .map_or_else(DrawingToolbarState::default, |chart| {
                DrawingToolbarState::from_chart(chart.read(cx))
            })
    }
}

fn chrome_overlay_layer(
    app_state: &TerminalApp,
    app: &Entity<TerminalApp>,
    theme: &AxiusflowTheme,
    cx: &App,
) -> Option<AnyElement> {
    let overlay = app_state.chrome_overlay?;
    let timeframe = overlay == ChromeOverlay::Timeframe;
    let panel = match overlay {
        ChromeOverlay::Instrument => instrument_dialog_content(
            app,
            &InstrumentSelectorState {
                label: terminal_instrument_label(app_state),
                instruments: app_state.instrument_entries(cx),
                input: app_state.symbol_input.clone(),
                message: app_state.symbol_message.clone(),
                selection_pending: app_state.symbol_selection_pending,
                enabled: true,
                provider: app_state.provider,
                keyboard_selection: app_state.chrome_selection,
            },
            theme,
        )
        .into_any_element(),
        ChromeOverlay::Indicator => indicator_dialog_content(
            app,
            &app_state.indicator_input,
            app_state.indicator_message.as_deref(),
            app_state.chrome_selection,
            theme,
            cx,
        )
        .into_any_element(),
        ChromeOverlay::Timeframe => timeframe_overlay_content(
            app,
            app_state.available_intervals(),
            app_state.selected_interval(),
            app_state.chrome_selection,
            app_state.series_browser.pending().is_some() || app_state.coinbase_switch.is_pending(),
            theme,
        )
        .into_any_element(),
    };
    let close_app = app.clone();
    Some(
        div()
            .id("chrome_overlay_scrim")
            .absolute()
            .top(px(chart_chrome::CHART_CHROME_HEIGHT))
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            .flex()
            .items_start()
            .when(timeframe, |scrim| scrim.justify_start().pl(px(320.0)))
            .when(!timeframe, |scrim| scrim.justify_center().pt_2())
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                close_app.update(cx, |app, app_cx| {
                    app.close_chrome_overlay(window, app_cx);
                });
                cx.stop_propagation();
            })
            .child(
                div()
                    .id("chrome_overlay_panel")
                    .flex_none()
                    .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                    .border_1()
                    .border_color(gpui_color(theme.colors.border))
                    .bg(gpui_color(theme.colors.popover))
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(panel),
            )
            .into_any_element(),
    )
}

fn timeframe_overlay_content(
    app: &Entity<TerminalApp>,
    intervals: Vec<ChartInterval>,
    selected: ChartInterval,
    keyboard_selection: usize,
    pending: bool,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    div()
        .w(px(192.0))
        .v_flex()
        .p_2()
        .gap_1()
        .children(
            intervals
                .into_iter()
                .enumerate()
                .map(move |(index, interval)| {
                    let row_app = app.clone();
                    div()
                        .id(("timeframe_overlay_row", index))
                        .h(px(40.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .px_3()
                        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                        .text_sm()
                        .when(selected == interval, |row| {
                            row.bg(gpui_color(colors.muted))
                                .text_color(gpui_color(colors.foreground))
                        })
                        .when(keyboard_selection == index, |row| {
                            row.bg(gpui_color(colors.muted))
                        })
                        .when(!pending, |row| {
                            row.cursor_pointer()
                                .hover(|row| row.bg(gpui_color(colors.accent)))
                                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                    row_app.update(cx, |app, app_cx| {
                                        if app.select_interval(interval, app_cx) {
                                            app.close_chrome_overlay(window, app_cx);
                                        }
                                    });
                                    cx.stop_propagation();
                                })
                        })
                        .child(interval.label())
                        .children(
                            (selected == interval)
                                .then(|| header_icon(HugeIcon::CheckmarkCircleIcon01)),
                        )
                }),
        )
}

impl Render for TerminalApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.schedule_diagnostics_frame(window);
        self.window_active = window.is_window_active();
        self.schedule_market_frame(window, cx);
        let theme = self.theme;
        let colors = theme.colors;
        let app = cx.entity();
        let chart_has_market_data = self
            .chart
            .as_ref()
            .is_some_and(|chart| chart.read(cx).has_market_data());
        let drawing_state = self.drawing_toolbar_state(cx);
        let overlay = chrome_overlay_layer(self, &app, &theme, cx);
        let header = terminal_header(
            cx,
            &app,
            HeaderState {
                theme,
                provider: self.provider,
                instrument_label: terminal_instrument_label(self),
                series_label: if self.provider == TerminalProvider::Coinbase {
                    self.coinbase_interval.label().to_string()
                } else {
                    series_selector_label(
                        self.series_browser.selected().map(|request| request.series),
                        self.series_browser.pending().map(|request| request.series),
                    )
                },
                instruments: self.instrument_entries(cx),
                selected_series: self.series_browser.selected().map(|request| request.series),
                symbol_input: self.symbol_input.clone(),
                indicator_input: self.indicator_input.clone(),
                indicator_message: self.indicator_message.clone(),
                symbol_message: self.symbol_message.clone(),
                series_message: self.series_message.clone(),
                pending: HeaderPendingState {
                    symbol_selection: self.symbol_selection_pending,
                    series: self.series_browser.pending().is_some()
                        || self.coinbase_switch.is_pending(),
                },
                controls: HeaderControls::from_state(
                    self.symbol_input.is_some()
                        || !self.symbol_browser.results().is_empty()
                        || !self.coinbase_products.is_empty(),
                    self.symbol_browser.selected().is_some() || self.coinbase_product.is_some(),
                    self.feed_diagnostics.is_some(),
                )
                .with_chart_controls(chart_has_market_data),
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

        let workspace = market_workspace(MarketWorkspaceState {
            app: app.clone(),
            chart: self.chart.as_ref(),
            chart_has_market_data,
            dom: self.dom.clone(),
            side_panel: self.side_panel,
            chart_state: self.chart_state,
            chart_status_detail: chart_status_detail(
                self.chart_state,
                self.connection_state
                    .unwrap_or(FeedConnectionState::Disconnected),
                &self.chart_state_message,
                self.connection_message.as_deref(),
            )
            .to_string(),
            diagnostics: self.feed_diagnostics.as_deref(),
            drawing_state,
            drawing_toolbar_collapsed: self.drawing_toolbar.is_collapsed(),
            theme: &theme,
        });

        div()
            .relative()
            .v_flex()
            .size_full()
            .track_focus(&self.chrome_focus)
            .on_key_down(cx.listener(Self::on_chrome_key_down))
            .bg(gpui_color(colors.background))
            .text_color(gpui_color(colors.foreground))
            .child(header)
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .bg(gpui_color(colors.background))
                    .child(workspace),
            )
            .children(overlay)
    }
}

struct MarketWorkspaceState<'a> {
    app: Entity<TerminalApp>,
    chart: Option<&'a Entity<OriginChartView>>,
    chart_has_market_data: bool,
    dom: Entity<ReadOnlyDomView>,
    side_panel: Option<SidePanel>,
    chart_state: ChartState,
    chart_status_detail: String,
    diagnostics: Option<&'a FeedDiagnosticsSnapshot>,
    drawing_state: DrawingToolbarState,
    drawing_toolbar_collapsed: bool,
    theme: &'a AxiusflowTheme,
}

fn market_workspace(state: MarketWorkspaceState<'_>) -> impl IntoElement + use<> {
    let MarketWorkspaceState {
        app,
        chart,
        chart_has_market_data,
        dom,
        side_panel,
        chart_state,
        chart_status_detail,
        diagnostics,
        drawing_state,
        drawing_toolbar_collapsed,
        theme,
    } = state;
    let colors = theme.colors;
    let notice = chart_surface_notice(chart_state, chart_has_market_data, &chart_status_detail);
    let chart_surface = div()
        .id("primary_chart")
        .relative()
        .v_flex()
        .flex_1()
        .overflow_hidden()
        .bg(gpui_color(colors.card))
        .child(div().flex_1().overflow_hidden().children(chart.cloned()))
        .children(notice.map(|notice| chart_notice(notice, theme)));
    let side_panel_content = resizable_panel()
        .visible(side_panel.is_some())
        .size(px(SIDE_PANEL_INITIAL_WIDTH))
        .size_range(px(SIDE_PANEL_MINIMUM_WIDTH)..px(SIDE_PANEL_MAXIMUM_WIDTH))
        .flex_none()
        .child(
            div()
                .size_full()
                .v_flex()
                .overflow_hidden()
                .children(side_panel.map(|panel| side_panel_header(panel, app.clone(), theme)))
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .children((side_panel == Some(SidePanel::Dom)).then_some(dom))
                        .children(
                            (side_panel == Some(SidePanel::Health))
                                .then(|| feed_health_panel(diagnostics, theme)),
                        ),
                ),
        );
    let chart_workspace = div()
        .relative()
        .flex()
        .size_full()
        .children(
            (!drawing_toolbar_collapsed)
                .then(|| drawing_toolbar(app.clone(), drawing_state, theme).into_any_element()),
        )
        .child(chart_surface.when(!drawing_toolbar_collapsed, |chart| {
            chart.ml(px(chart_chrome::CHART_CHROME_HEIGHT))
        }))
        .children(
            drawing_toolbar_collapsed
                .then(|| drawing_toolbar_expander(app.clone(), theme).into_any_element()),
        );
    div().size_full().overflow_hidden().child(
        h_resizable("market_workspace")
            .child(resizable_panel().child(chart_workspace))
            .child(side_panel_content),
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DrawingToolbarState {
    availability: DrawingToolbarAvailability,
    active_tool: ChartDrawingTool,
    drawing_count: usize,
    has_selection: bool,
    selected_locked: bool,
    all_locked: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum DrawingToolbarAvailability {
    #[default]
    Unavailable,
    Available,
}

impl DrawingToolbarState {
    fn from_chart(chart: &OriginChartView) -> Self {
        Self {
            availability: DrawingToolbarAvailability::Available,
            active_tool: chart.drawing_tool(),
            drawing_count: chart.drawing_count(),
            has_selection: chart.selected_drawing_id().is_some(),
            selected_locked: chart.selected_drawing_locked(),
            all_locked: chart.drawings_lock_summary().all_locked,
        }
    }
}

#[derive(Clone, Copy)]
enum DrawingToolIcon {
    Huge(HugeIcon),
    Asset(assets::DrawingIcon),
}

#[derive(Clone, Copy)]
struct DrawingToolSpec {
    id: &'static str,
    label: &'static str,
    tool: ChartDrawingTool,
    icon: DrawingToolIcon,
    icon_size: f32,
}

const DRAWING_TOOLS: [DrawingToolSpec; 8] = [
    DrawingToolSpec {
        id: "drawing_cursor",
        label: "Cursor",
        tool: ChartDrawingTool::Cursor,
        icon: DrawingToolIcon::Huge(HugeIcon::CursorIcon01),
        icon_size: 24.0,
    },
    DrawingToolSpec {
        id: "drawing_trend_line",
        label: "Trend line",
        tool: ChartDrawingTool::TrendLine,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::TrendLine),
        icon_size: 28.0,
    },
    DrawingToolSpec {
        id: "drawing_horizontal_line",
        label: "Horizontal line",
        tool: ChartDrawingTool::HorizontalLine,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::HorizontalLine),
        icon_size: 28.0,
    },
    DrawingToolSpec {
        id: "drawing_vertical_line",
        label: "Vertical line",
        tool: ChartDrawingTool::VerticalLine,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::VerticalLine),
        icon_size: 28.0,
    },
    DrawingToolSpec {
        id: "drawing_ray",
        label: "Ray",
        tool: ChartDrawingTool::Ray,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::Ray),
        icon_size: 28.0,
    },
    DrawingToolSpec {
        id: "drawing_rectangle",
        label: "Rectangle",
        tool: ChartDrawingTool::Rectangle,
        icon: DrawingToolIcon::Asset(assets::DrawingIcon::Rectangle),
        icon_size: 28.0,
    },
    DrawingToolSpec {
        id: "drawing_brush",
        label: "Brush",
        tool: ChartDrawingTool::Brush,
        icon: DrawingToolIcon::Huge(HugeIcon::Brush),
        icon_size: 24.0,
    },
    DrawingToolSpec {
        id: "drawing_text",
        label: "Text",
        tool: ChartDrawingTool::Text,
        icon: DrawingToolIcon::Huge(HugeIcon::Text),
        icon_size: 24.0,
    },
];

fn drawing_toolbar(
    app: Entity<TerminalApp>,
    state: DrawingToolbarState,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let tool_app = app.clone();
    let tools = DRAWING_TOOLS.into_iter().map(move |spec| {
        let app = tool_app.clone();
        let enabled = state.availability == DrawingToolbarAvailability::Available;
        let button = drawing_toolbar_action(
            drawing_toolbar_button(
                spec.id,
                spec.icon,
                spec.label,
                spec.icon_size,
                theme,
                state.active_tool == spec.tool,
            ),
            enabled,
        );
        chrome_tooltip(
            spec.id,
            spec.label,
            button_activation(button, enabled, move |_, cx| {
                app.update(cx, |app, app_cx| {
                    app.select_drawing_tool(spec.tool, app_cx);
                });
            }),
        )
    });
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .w(px(chart_chrome::CHART_CHROME_HEIGHT))
        .v_flex()
        .items_center()
        .overflow_hidden()
        .border_r_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.card))
        .child(
            div()
                .v_flex()
                .items_center()
                .gap_1()
                .py_2()
                .size_full()
                .min_h(px(0.0))
                .overflow_y_scrollbar()
                .children(tools)
                .child(drawing_toolbar_actions(app, state, theme)),
        )
}

fn drawing_toolbar_actions(
    app: Entity<TerminalApp>,
    state: DrawingToolbarState,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    div()
        .v_flex()
        .items_center()
        .gap_1()
        .py_2()
        .w_full()
        .border_t_1()
        .border_color(gpui_color(theme.colors.border))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_delete_selected",
                "Delete selected drawing",
                HugeIcon::DeleteIcon02,
                24.0,
                false,
                state.has_selection,
                TerminalApp::remove_selected_drawing,
            ),
            app.clone(),
            theme,
        ))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_lock_selected",
                "Lock or unlock selected drawing",
                HugeIcon::Lock,
                20.0,
                state.selected_locked,
                state.has_selection,
                TerminalApp::toggle_selected_drawing_lock,
            ),
            app.clone(),
            theme,
        ))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_lock_all",
                "Lock or unlock all drawings",
                HugeIcon::AiLock,
                20.0,
                state.all_locked,
                state.drawing_count > 0,
                TerminalApp::toggle_all_drawings_lock,
            ),
            app.clone(),
            theme,
        ))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_clear_all",
                "Clear all drawings",
                HugeIcon::AiEraser,
                24.0,
                false,
                state.drawing_count > 0,
                TerminalApp::clear_drawings,
            ),
            app.clone(),
            theme,
        ))
        .child(drawing_action_control(
            DrawingActionSpec::new(
                "drawing_toolbar_collapse",
                "Collapse drawing toolbar",
                HugeIcon::ArrowLeftIcon01,
                24.0,
                false,
                true,
                TerminalApp::toggle_drawing_toolbar,
            ),
            app,
            theme,
        ))
}

#[derive(Clone, Copy)]
struct DrawingActionSpec {
    id: &'static str,
    tooltip: &'static str,
    icon: HugeIcon,
    icon_size: f32,
    selected: bool,
    enabled: bool,
    action: fn(&mut TerminalApp, &mut Context<TerminalApp>),
}

impl DrawingActionSpec {
    const fn new(
        id: &'static str,
        tooltip: &'static str,
        icon: HugeIcon,
        icon_size: f32,
        selected: bool,
        enabled: bool,
        action: fn(&mut TerminalApp, &mut Context<TerminalApp>),
    ) -> Self {
        Self {
            id,
            tooltip,
            icon,
            icon_size,
            selected,
            enabled,
            action,
        }
    }
}

fn drawing_action_control(
    spec: DrawingActionSpec,
    app: Entity<TerminalApp>,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let button = drawing_toolbar_button(
        spec.id,
        DrawingToolIcon::Huge(spec.icon),
        spec.tooltip,
        spec.icon_size,
        theme,
        spec.selected,
    );
    let button = drawing_toolbar_action(button, spec.enabled);
    let button = button_activation(button, spec.enabled, move |_, cx| {
        app.update(cx, spec.action);
    });
    chrome_tooltip(spec.id, spec.tooltip, button)
}

fn drawing_toolbar_expander(
    app: Entity<TerminalApp>,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    div()
        .absolute()
        .left_0()
        .bottom_0()
        .border_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.card))
        .child(chrome_tooltip(
            "drawing_toolbar_expand",
            "Expand drawing toolbar",
            button_activation(
                drawing_toolbar_button(
                    "drawing_toolbar_expand",
                    DrawingToolIcon::Huge(HugeIcon::ArrowRightIcon01),
                    "Expand drawing toolbar",
                    14.0,
                    theme,
                    false,
                )
                .w(px(24.0))
                .h(px(28.0))
                .cursor_pointer(),
                true,
                move |_, cx| {
                    app.update(cx, TerminalApp::toggle_drawing_toolbar);
                },
            ),
        ))
}

fn drawing_toolbar_button(
    id: &'static str,
    icon: DrawingToolIcon,
    _tooltip: &'static str,
    icon_size: f32,
    theme: &AxiusflowTheme,
    selected: bool,
) -> Button {
    let icon = match icon {
        DrawingToolIcon::Huge(icon) => header_icon(icon),
        DrawingToolIcon::Asset(icon) => Icon::default().path(icon.path()),
    };
    let button = Button::new(id)
        .icon(icon)
        .ghost()
        .compact()
        .with_size(px(icon_size / 0.75))
        .w(px(32.0))
        .h(px(32.0))
        .rounded(px(f32::from(
            chart_chrome::CHART_CONTROL_RADIUS.logical_pixels(),
        )));
    chrome_button_style(button, theme, selected, true, true)
}

fn drawing_toolbar_action(button: Button, enabled: bool) -> Button {
    button
        .disabled(!enabled)
        .when(enabled, Button::cursor_pointer)
        .when(!enabled, Button::cursor_not_allowed)
}

fn side_panel_header(
    panel: SidePanel,
    app: Entity<TerminalApp>,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    div()
        .h(px(30.0))
        .flex_none()
        .flex()
        .items_center()
        .px_2()
        .border_l_1()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .text_color(gpui_color(colors.muted_foreground))
        .child(div().flex_1().child(panel.title().to_uppercase()))
        .child(chrome_tooltip(
            "close_side_panel",
            "Close side panel",
            button_activation(
                Button::new("close_side_panel")
                    .icon(header_icon(HugeIcon::CancelIcon01))
                    .ghost()
                    .compact()
                    .cursor_pointer(),
                true,
                move |_, cx| {
                    app.update(cx, TerminalApp::close_side_panel);
                },
            ),
        ))
}

fn chart_notice(notice: ChartSurfaceNotice, theme: &AxiusflowTheme) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let tone = match notice.tone {
        ChartNoticeTone::Muted => colors.muted_foreground,
        ChartNoticeTone::Warning => colors.warning,
        ChartNoticeTone::Loss => colors.loss,
    };
    let loading = notice.label == ChartState::Loading.label();
    let label = div()
        .v_flex()
        .gap_1()
        .px_2()
        .py_1()
        .border_1()
        .rounded(px(f32::from(
            chart_chrome::CHART_SURFACE_RADIUS.logical_pixels(),
        )))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.background.with_alpha(0.94)))
        .text_xs()
        .text_color(gpui_color(tone))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .children(loading.then(|| Spinner::new().small().color(gpui_color(tone))))
                .child(notice.label),
        )
        .children((!loading).then_some(notice.detail).flatten().map(|detail| {
            div()
                .text_color(gpui_color(colors.muted_foreground))
                .child(detail)
        }));
    match notice.placement {
        ChartNoticePlacement::Center => div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(label),
        ChartNoticePlacement::TopLeft => div().absolute().top_2().left_2().child(label),
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
        RithmicCatalogRejection::SearchDispatchUnavailable => {
            "The Rithmic search could not be scheduled"
        }
        RithmicCatalogRejection::SelectionDispatchUnavailable => {
            "The Rithmic selection could not be scheduled"
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CatalogCommandDomain {
    Search,
    Selection,
}

const fn catalog_rejection_domain(reason: RithmicCatalogRejection) -> CatalogCommandDomain {
    match reason {
        RithmicCatalogRejection::SearchRejected
        | RithmicCatalogRejection::SupersededSearch
        | RithmicCatalogRejection::SearchDispatchUnavailable => CatalogCommandDomain::Search,
        RithmicCatalogRejection::InstrumentUnavailable
        | RithmicCatalogRejection::SubscriptionRejected
        | RithmicCatalogRejection::SelectionDispatchUnavailable => CatalogCommandDomain::Selection,
    }
}

fn terminal_header(
    cx: &mut Context<TerminalApp>,
    app: &Entity<TerminalApp>,
    state: HeaderState,
) -> impl IntoElement + use<> {
    div()
        .relative()
        .h(px(state.theme.dimensions.app_header_height.logical_pixels))
        .flex_none()
        .child(TitleBar::new().h(px(state.theme.dimensions.app_header_height.logical_pixels)))
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .right(px(102.0))
                .occlude()
                .flex()
                .items_center()
                .gap_2()
                .px_4()
                .child(
                    div()
                        .flex_none()
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .child("Axiusflow"),
                )
                .child(header_controls(cx, app, state)),
        )
}

fn header_controls(
    cx: &mut Context<TerminalApp>,
    app: &Entity<TerminalApp>,
    state: HeaderState,
) -> impl IntoElement + use<> {
    let dom_toggle = side_panel_toggle(
        app.clone(),
        &state.theme,
        SidePanel::Dom,
        state.controls.enabled(HeaderControls::DOM),
        state.dom_visible,
    );
    let health_toggle = side_panel_toggle(
        app.clone(),
        &state.theme,
        SidePanel::Health,
        state.controls.enabled(HeaderControls::HEALTH),
        state.health_visible,
    );
    let (connection_label, connection_color) = connection_presentation(
        state.provider,
        state.connection_state,
        state.chart_state,
        state.delayed,
    );
    div()
        .h_full()
        .flex()
        .items_center()
        .gap_2()
        .child(connection_status_indicator(
            connection_label,
            connection_color(&state.theme),
        ))
        .child(instrument_selector(
            cx,
            app.clone(),
            &InstrumentSelectorState {
                label: state.instrument_label,
                instruments: state.instruments,
                input: state.symbol_input,
                message: state.symbol_message,
                selection_pending: state.pending.symbol_selection,
                enabled: state.controls.enabled(HeaderControls::INSTRUMENT),
                provider: state.provider,
                keyboard_selection: 0,
            },
            &state.theme,
        ))
        .child(series_selector(
            app.clone(),
            state.series_label,
            state.selected_series,
            state.series_message,
            state.pending.series,
            &state.theme,
            state.controls.enabled(HeaderControls::SERIES),
        ))
        .child(panel_toggle(
            PanelToggleState {
                id: "latest_chart",
                label: "Latest",
                icon: HugeIcon::ArrowRightDouble,
                enabled: state.controls.enabled(HeaderControls::LATEST),
                selected: false,
                tooltip: "Return to the latest bar (End)",
                toggle: TerminalApp::scroll_chart_to_latest,
            },
            &state.theme,
            app.clone(),
        ))
        .child(panel_toggle(
            PanelToggleState {
                id: "fit_chart",
                label: "Fit",
                icon: HugeIcon::FitToScreen,
                enabled: state.controls.enabled(HeaderControls::FIT),
                selected: false,
                tooltip: "Fit chart and reset price scales (Home)",
                toggle: TerminalApp::reset_chart_view,
            },
            &state.theme,
            app.clone(),
        ))
        .child(indicator_selector(
            cx,
            app.clone(),
            state.indicator_input,
            state.indicator_message,
            state.controls.enabled(HeaderControls::FIT),
            &state.theme,
        ))
        .child(dom_toggle)
        .child(health_toggle)
        .child(theme_toggle(app.clone(), &state.theme))
}

fn side_panel_toggle(
    app: Entity<TerminalApp>,
    theme: &AxiusflowTheme,
    panel: SidePanel,
    enabled: bool,
    selected: bool,
) -> AnyElement {
    let (id, icon, toggle) = match panel {
        SidePanel::Dom => (
            "dom_toggle",
            HugeIcon::SidebarRightIcon01,
            TerminalApp::toggle_dom as fn(&mut TerminalApp, &mut Context<TerminalApp>),
        ),
        SidePanel::Health => (
            "health_toggle",
            HugeIcon::ActivityIcon01,
            TerminalApp::toggle_health as fn(&mut TerminalApp, &mut Context<TerminalApp>),
        ),
    };
    panel_toggle(
        PanelToggleState {
            id,
            label: panel.toggle_label(),
            icon,
            enabled,
            selected,
            tooltip: panel.toggle_tooltip(),
            toggle,
        },
        theme,
        app,
    )
    .into_any_element()
}

fn instrument_selector(
    _cx: &mut Context<TerminalApp>,
    app: Entity<TerminalApp>,
    state: &InstrumentSelectorState,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let trigger = Button::new("instrument_selector")
        .icon(header_icon(HugeIcon::ExchangeIcon01))
        .label(state.label.clone())
        .dropdown_caret(true)
        .ghost()
        .border_1()
        .border_color(gpui_color(theme.colors.border))
        .bg(gpui_color(theme.colors.muted))
        .text_color(gpui_color(theme.colors.foreground))
        .hover(|style| {
            style
                .bg(gpui_color(theme.colors.accent))
                .text_color(gpui_color(theme.colors.foreground))
        })
        .h(px(28.0))
        .px_3()
        .rounded(px(f32::from(
            chart_chrome::SYMBOL_TRIGGER_RADIUS.logical_pixels(),
        )))
        .disabled(!state.enabled)
        .when(state.enabled, Button::cursor_pointer)
        .when(!state.enabled, Button::cursor_not_allowed);
    let trigger = trigger.when(!state.enabled, |trigger| {
        trigger.text_color(gpui_color(theme.colors.muted_foreground))
    });
    chrome_tooltip(
        "instrument_selector",
        format!(
            "Search or select a {} market",
            match state.provider {
                TerminalProvider::Coinbase => "Coinbase spot",
                TerminalProvider::Rithmic => "Rithmic",
                TerminalProvider::Fixture => "fixture",
            }
        ),
        button_activation(
            trigger.loading(state.selection_pending),
            state.enabled,
            move |window, cx| {
                app.update(cx, |app, app_cx| {
                    app.open_chrome_overlay(ChromeOverlay::Instrument, window, app_cx);
                });
            },
        ),
    )
}

fn connection_status_indicator(label: String, color: ThemeColor) -> impl IntoElement {
    chrome_tooltip(
        "connection_status",
        label,
        div()
            .id("connection_status_dot")
            .size(px(7.0))
            .flex_none()
            .rounded_full()
            .bg(gpui_color(color)),
    )
}

fn indicator_selector(
    _cx: &mut Context<TerminalApp>,
    app: Entity<TerminalApp>,
    _input: Entity<InputState>,
    _message: Option<String>,
    enabled: bool,
    _theme: &AxiusflowTheme,
) -> impl IntoElement {
    let trigger = Button::new("indicator_selector")
        .icon(header_icon(HugeIcon::ChartLineDataIcon02))
        .ghost()
        .w(px(chart_chrome::CHART_CONTROL_SIZE))
        .h(px(chart_chrome::CHART_CONTROL_SIZE))
        .rounded(px(f32::from(
            chart_chrome::CHART_CONTROL_RADIUS.logical_pixels(),
        )))
        .disabled(!enabled)
        .when(enabled, Button::cursor_pointer)
        .when(!enabled, Button::cursor_not_allowed);
    chrome_tooltip(
        "indicator_selector",
        "Indicators",
        button_activation(trigger, enabled, move |window, cx| {
            app.update(cx, |app, app_cx| {
                app.open_chrome_overlay(ChromeOverlay::Indicator, window, app_cx);
            });
        }),
    )
}

fn indicator_dialog_content(
    app: &Entity<TerminalApp>,
    input: &Entity<InputState>,
    message: Option<&str>,
    keyboard_selection: usize,
    theme: &AxiusflowTheme,
    cx: &App,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let indicator_specs = chart_chrome::filter_indicator_specs(input.read(cx).value().as_ref());
    let result_count = indicator_specs.len();
    let (status, status_color) = indicator_status(message, &colors);
    let rows = indicator_specs
        .into_iter()
        .enumerate()
        .map(|(index, spec)| {
            let row_app = app.clone();
            let add_app = app.clone();
            let row_input = input.clone();
            let add_input = input.clone();
            let indicator = native_indicator(spec.kind);
            div()
                .id(("indicator_dialog_row", index))
                .min_h(px(52.0))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .rounded(px(f32::from(
                    chart_chrome::CHART_CONTROL_RADIUS.logical_pixels(),
                )))
                .cursor_pointer()
                .when(keyboard_selection == index, |row| {
                    row.bg(gpui_color(colors.muted))
                })
                .hover(|row| row.bg(gpui_color(colors.accent)))
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    let added = row_app.update(cx, |app, cx| app.add_indicator(indicator, cx));
                    if added {
                        let _ = &row_input;
                        row_app.update(cx, |app, app_cx| {
                            app.close_chrome_overlay(window, app_cx);
                        });
                    }
                    cx.stop_propagation();
                })
                .child(
                    div()
                        .v_flex()
                        .gap_0p5()
                        .flex_1()
                        .child(div().text_sm().child(spec.label))
                        .child(
                            div()
                                .text_xs()
                                .text_color(gpui_color(colors.muted_foreground))
                                .child(format!(
                                    "{}  ·  {}  ·  {}",
                                    spec.kind.identifier().to_ascii_uppercase(),
                                    spec.parameter_description,
                                    spec.location_description()
                                )),
                        ),
                )
                .child(button_activation(
                    Button::new(("add_indicator", index))
                        .icon(header_icon(HugeIcon::AddIcon01))
                        .outline()
                        .compact()
                        .cursor_pointer()
                        .tab_stop(false),
                    true,
                    move |window, cx| {
                        let added = add_app.update(cx, |app, cx| app.add_indicator(indicator, cx));
                        if added {
                            let _ = &add_input;
                            add_app.update(cx, |app, app_cx| {
                                app.close_chrome_overlay(window, app_cx);
                            });
                        }
                    },
                ))
        });
    div()
        .w(px(720.0))
        .bg(gpui_color(colors.card))
        .text_color(gpui_color(colors.card_foreground))
        .child(indicator_dialog_header(input, &colors))
        .child(indicator_status_bar(
            result_count,
            status,
            status_color,
            &colors,
        ))
        .child(
            div()
                .v_flex()
                .gap_1()
                .p_2()
                .max_h(px(480.0))
                .overflow_y_scrollbar()
                .children(rows),
        )
        .child(indicator_dialog_footer(&colors))
}

fn indicator_status(
    message: Option<&str>,
    colors: &axiusflow_design_system::ThemeColors,
) -> (String, ThemeColor) {
    message.map_or_else(
        || ("OHLC-compatible".to_string(), colors.muted_foreground),
        |message| (message.to_string(), colors.loss),
    )
}

fn indicator_status_bar(
    result_count: usize,
    status: String,
    status_color: ThemeColor,
    colors: &axiusflow_design_system::ThemeColors,
) -> impl IntoElement + use<> {
    div()
        .h(px(34.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .text_color(gpui_color(colors.muted_foreground))
        .child(format!("{result_count} native indicators"))
        .child(div().text_color(gpui_color(status_color)).child(status))
}

fn indicator_dialog_footer(
    colors: &axiusflow_design_system::ThemeColors,
) -> impl IntoElement + use<> {
    div()
        .h(px(40.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .border_t_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .text_color(gpui_color(colors.muted_foreground))
        .child("Enter Add  ·  Esc Close")
        .child("Publisher: Native")
}

fn indicator_dialog_header(
    input: &Entity<InputState>,
    colors: &axiusflow_design_system::ThemeColors,
) -> impl IntoElement + use<> {
    div()
        .h(px(chart_chrome::CHART_CHROME_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .child(header_icon(HugeIcon::SearchIcon01))
        .child(
            Input::new(input)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                .flex_1(),
        )
}

const fn native_indicator(kind: chart_chrome::IndicatorKind) -> ChartIndicator {
    match kind {
        chart_chrome::IndicatorKind::Sma => ChartIndicator::Sma,
        chart_chrome::IndicatorKind::Ema => ChartIndicator::Ema,
        chart_chrome::IndicatorKind::Wma => ChartIndicator::Wma,
        chart_chrome::IndicatorKind::BollingerBands => ChartIndicator::Bollinger,
        chart_chrome::IndicatorKind::Vwap => ChartIndicator::Vwap,
        chart_chrome::IndicatorKind::Volume => ChartIndicator::Volume,
        chart_chrome::IndicatorKind::Rsi => ChartIndicator::Rsi,
        chart_chrome::IndicatorKind::Macd => ChartIndicator::Macd,
        chart_chrome::IndicatorKind::Stochastic => ChartIndicator::Stochastic,
        chart_chrome::IndicatorKind::Atr => ChartIndicator::Atr,
    }
}

struct InstrumentSelectorState {
    label: String,
    instruments: Vec<InstrumentMenuEntry>,
    input: Option<Entity<InputState>>,
    message: String,
    selection_pending: bool,
    enabled: bool,
    provider: TerminalProvider,
    keyboard_selection: usize,
}

fn instrument_dialog_content(
    app: &Entity<TerminalApp>,
    state: &InstrumentSelectorState,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let header = instrument_dialog_header(state, theme);
    let rows = state
        .instruments
        .iter()
        .enumerate()
        .map(|(index, instrument)| {
            let checked = instrument.checked;
            let app = app.clone();
            let symbol = instrument.symbol.clone();
            let detail = instrument.detail.clone();
            let selection = instrument.selection.clone();
            div()
                .id(("instrument_dialog_row", index))
                .min_h(px(48.0))
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .text_sm()
                .when(state.keyboard_selection == index, |row| {
                    row.bg(gpui_color(colors.muted))
                })
                .when(checked, |row| row.bg(gpui_color(colors.muted)))
                .when(!state.selection_pending, |row| {
                    row.cursor_pointer()
                        .hover(|row| row.bg(gpui_color(colors.accent)))
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            let dispatched = app
                                .update(cx, |app, cx| app.select_instrument(selection.clone(), cx));
                            if dispatched {
                                app.update(cx, |app, app_cx| {
                                    app.close_chrome_overlay(window, app_cx);
                                });
                            }
                            cx.stop_propagation();
                        })
                })
                .when(state.selection_pending, gpui::Styled::cursor_not_allowed)
                .child(
                    div()
                        .size(px(24.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(gpui_color(colors.muted))
                        .child(header_icon(HugeIcon::ExchangeIcon01)),
                )
                .child(
                    div()
                        .v_flex()
                        .gap_0p5()
                        .flex_1()
                        .child(div().text_sm().child(symbol))
                        .child(
                            div()
                                .text_xs()
                                .text_color(gpui_color(colors.muted_foreground))
                                .child(detail),
                        ),
                )
                .children(
                    checked
                        .then(|| header_icon(HugeIcon::CheckmarkCircleIcon01).into_any_element()),
                )
        });
    div()
        .w(px(720.0))
        .bg(gpui_color(colors.card))
        .text_color(gpui_color(colors.card_foreground))
        .child(header)
        .child(
            div()
                .h(px(34.0))
                .flex_none()
                .flex()
                .items_center()
                .px_3()
                .border_b_1()
                .border_color(gpui_color(colors.border))
                .text_xs()
                .text_color(gpui_color(colors.muted_foreground))
                .child(div().flex_1().child(state.message.clone()))
                .child(format!("{} results", state.instruments.len())),
        )
        .child(
            div()
                .v_flex()
                .gap_1()
                .p_2()
                .max_h(px(480.0))
                .overflow_y_scrollbar()
                .children(rows),
        )
        .child(instrument_dialog_footer(&colors, state.provider))
}

fn instrument_dialog_footer(
    colors: &axiusflow_design_system::ThemeColors,
    provider: TerminalProvider,
) -> impl IntoElement + use<> {
    div()
        .h(px(40.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .border_t_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .text_color(gpui_color(colors.muted_foreground))
        .child("Enter Search  ·  Esc Close")
        .child(match provider {
            TerminalProvider::Coinbase => "Coinbase public spot catalog",
            TerminalProvider::Rithmic => "Rithmic Test catalog",
            TerminalProvider::Fixture => "Fixture catalog",
        })
}

fn instrument_dialog_header(state: &InstrumentSelectorState, theme: &AxiusflowTheme) -> AnyElement {
    let Some(input) = state.input.as_ref() else {
        return div().into_any_element();
    };
    div()
        .h(px(chart_chrome::CHART_CHROME_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .border_b_1()
        .border_color(gpui_color(theme.colors.border))
        .child(header_icon(HugeIcon::SearchIcon01))
        .child(
            Input::new(input)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                .flex_1(),
        )
        .into_any_element()
}

fn instrument_menu_detail(
    instrument: &axiusflow_rithmic_protocol_adapter::SymbolSearchResult,
) -> String {
    let mut details = vec![instrument.exchange.as_str()];
    if let Some(instrument_type) = instrument.instrument_type.as_deref() {
        details.push(instrument_type);
    }
    if let Some(expiration) = instrument.expiration_date.as_deref() {
        details.push(expiration);
    }
    details.join(" · ")
}

#[derive(Clone, Copy)]
struct PanelToggleState {
    id: &'static str,
    label: &'static str,
    icon: HugeIcon,
    enabled: bool,
    selected: bool,
    tooltip: &'static str,
    toggle: fn(&mut TerminalApp, &mut Context<TerminalApp>),
}

fn panel_toggle(
    state: PanelToggleState,
    theme: &AxiusflowTheme,
    app: Entity<TerminalApp>,
) -> impl IntoElement {
    let button = Button::new(state.id)
        .icon(header_icon(state.icon))
        .label(state.label)
        .disabled(!state.enabled)
        .when(state.enabled, Button::cursor_pointer)
        .when(!state.enabled, Button::cursor_not_allowed);
    let button = button_activation(button, state.enabled, move |_, cx| {
        app.update(cx, state.toggle);
    });
    chrome_tooltip(
        state.id,
        state.tooltip,
        chrome_button_style(button, theme, state.selected, false, state.enabled),
    )
}

fn theme_toggle(app: Entity<TerminalApp>, theme: &AxiusflowTheme) -> impl IntoElement + use<> {
    let next = theme.mode.toggled();
    let icon = match next {
        axiusflow_design_system::ThemeMode::Light => HugeIcon::SunIcon03,
        axiusflow_design_system::ThemeMode::Dark => HugeIcon::MoonIcon02,
    };
    let button = Button::new("theme_toggle")
        .icon(header_icon(icon))
        .cursor_pointer();
    let button = button_activation(button, true, move |window, cx| {
        app.update(cx, |app, cx| app.toggle_theme(window, cx));
    });
    chrome_tooltip(
        "theme_toggle",
        format!("Switch to {} theme", next.label()),
        chrome_button_style(button, theme, false, false, true),
    )
}

fn header_icon(name: HugeIcon) -> Icon {
    Icon::default().path(name.path())
}

fn chrome_tooltip(
    id: &'static str,
    label: impl Into<gpui::SharedString>,
    trigger: impl IntoElement + 'static,
) -> HoverCard {
    let label = label.into();
    HoverCard::new((id, usize::MAX))
        .trigger(trigger)
        .open_delay(TOOLTIP_OPEN_DELAY)
        .close_delay(TOOLTIP_CLOSE_DELAY)
        .appearance(false)
        .content(move |_, _, cx| {
            div()
                .px_2()
                .py_1()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .text_xs()
                .text_color(cx.theme().popover_foreground)
                .child(label.clone())
        })
}

fn series_selector(
    app: Entity<TerminalApp>,
    label: String,
    _selected: Option<rithmic_history::RithmicSeries>,
    _message: String,
    pending: bool,
    theme: &AxiusflowTheme,
    enabled: bool,
) -> impl IntoElement {
    let button = Button::new("series_selector")
        .label(label)
        .dropdown_caret(true)
        .disabled(!enabled)
        .loading(pending)
        .when(enabled, Button::cursor_pointer)
        .when(!enabled, Button::cursor_not_allowed);
    chrome_tooltip(
        "series_selector",
        "Select chart timeframe",
        button_activation(
            chrome_button_style(button, theme, false, false, enabled),
            enabled && !pending,
            move |window, cx| {
                app.update(cx, |app, app_cx| {
                    app.open_chrome_overlay(ChromeOverlay::Timeframe, window, app_cx);
                });
            },
        ),
    )
}

fn chrome_button_style(
    button: Button,
    theme: &AxiusflowTheme,
    selected: bool,
    muted_when_idle: bool,
    enabled: bool,
) -> Button {
    let colors = theme.colors;
    let idle = if muted_when_idle || !enabled {
        colors.muted_foreground
    } else {
        colors.foreground
    };
    button
        .selected(selected)
        .ghost()
        .text_color(gpui_color(idle))
        .when(selected, |button| {
            button
                .bg(gpui_color(colors.muted))
                .text_color(gpui_color(colors.foreground))
        })
        .hover(|style| {
            style
                .bg(gpui_color(colors.accent))
                .text_color(gpui_color(colors.foreground))
        })
}

fn button_activation(
    button: Button,
    enabled: bool,
    handler: impl Fn(&mut Window, &mut App) + 'static,
) -> Button {
    let handler = Rc::new(handler);
    let mouse_handler = handler.clone();
    button
        .when(enabled, |button| {
            button.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                mouse_handler(window, cx);
                cx.stop_propagation();
            })
        })
        .on_click(move |event, window, cx| {
            if matches!(event, ClickEvent::Keyboard(_)) {
                handler(window, cx);
            }
        })
}

type ConnectionColor = fn(&AxiusflowTheme) -> ThemeColor;

fn connection_presentation(
    provider: TerminalProvider,
    state: FeedConnectionState,
    chart_state: ChartState,
    delayed: bool,
) -> (String, ConnectionColor) {
    let provider = match provider {
        TerminalProvider::Coinbase => "Coinbase",
        TerminalProvider::Rithmic => "Test",
        TerminalProvider::Fixture => "Fixture",
    };
    if chart_state == ChartState::Stale {
        return (format!("{provider} · Stale"), |theme| theme.colors.warning);
    }
    if chart_state == ChartState::Recovering {
        return (format!("{provider} · Reconnecting"), |theme| {
            theme.colors.warning
        });
    }
    if chart_state == ChartState::Error && state == FeedConnectionState::Streaming {
        return (format!("{provider} · Data error"), |theme| {
            theme.colors.loss
        });
    }
    if state == FeedConnectionState::Streaming && delayed {
        return (format!("{provider} · Delayed"), |theme| {
            theme.colors.warning
        });
    }
    match state {
        FeedConnectionState::Disconnected => ("Offline".to_string(), |theme| theme.colors.loss),
        FeedConnectionState::Discovering => (format!("{provider} · Discovering"), |theme| {
            theme.colors.info
        }),
        FeedConnectionState::Authenticating => (format!("{provider} · Authenticating"), |theme| {
            theme.colors.info
        }),
        FeedConnectionState::Streaming => {
            (format!("{provider} · Live"), |theme| theme.colors.profit)
        }
        FeedConnectionState::Recovering => (format!("{provider} · Reconnecting"), |theme| {
            theme.colors.warning
        }),
        FeedConnectionState::Stopped => ("Stopped".to_string(), |theme| theme.colors.loss),
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
        .size_full()
        .flex_none()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.card))
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
    component.button_primary = gpui_color(colors.primary);
    component.button_primary_foreground = gpui_color(colors.primary_foreground);
    component.button_primary_hover = gpui_color(colors.primary);
    component.button_primary_active = gpui_color(colors.primary);
    component.secondary = gpui_color(colors.secondary);
    component.secondary_foreground = gpui_color(colors.secondary_foreground);
    component.secondary_hover = gpui_color(colors.accent);
    component.secondary_active = gpui_color(colors.muted);
    component.button_secondary = gpui_color(colors.secondary);
    component.button_secondary_foreground = gpui_color(colors.secondary_foreground);
    component.button_secondary_hover = gpui_color(colors.accent);
    component.button_secondary_active = gpui_color(colors.muted);

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

    component.sidebar = gpui_color(colors.card);
    component.sidebar_foreground = gpui_color(colors.card_foreground);
    component.sidebar_border = gpui_color(colors.border);
    component.table = gpui_color(colors.card);
    component.table_head = gpui_color(colors.muted);
    component.table_row_border = gpui_color(colors.border);
    component.title_bar = gpui_color(colors.card);
    component.title_bar_border = gpui_color(colors.border);
    component.status_bar = gpui_color(colors.card);
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
        true,
    )?;
    if !matches!(startup, MarketWorkerStartup::Loading(_)) {
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

#[derive(Debug, Eq, PartialEq)]
struct RithmicTestArguments {
    history_root: std::path::PathBuf,
    detailed_diagnostics: bool,
    native_transition_report: Option<std::path::PathBuf>,
}

fn parse_rithmic_test_arguments(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<RithmicTestArguments, String> {
    let usage = "usage: axiusflow_desktop --rithmic-test <history-root> [--detailed-diagnostics] [--capture-native-transitions <report-path>]";
    let history_root = arguments.next().ok_or_else(|| usage.to_string())?;
    let mut detailed_diagnostics = false;
    let mut native_transition_report = None;
    while let Some(flag) = arguments.next() {
        if flag == "--detailed-diagnostics" && !detailed_diagnostics {
            detailed_diagnostics = true;
        } else if flag == "--capture-native-transitions" && native_transition_report.is_none() {
            let report = arguments.next().ok_or_else(|| usage.to_string())?;
            if report.is_empty() {
                return Err(usage.to_string());
            }
            native_transition_report = Some(std::path::PathBuf::from(report));
        } else {
            return Err(usage.to_string());
        }
    }
    Ok(RithmicTestArguments {
        history_root: std::path::PathBuf::from(history_root),
        detailed_diagnostics,
        native_transition_report,
    })
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
    match startup {
        MarketWorkerStartup::Shell(_) => {
            Some(cx.new(|cx| InputState::new(window, cx).placeholder("Search Rithmic symbols")))
        }
        MarketWorkerStartup::Loading(_) => Some(
            cx.new(|cx| InputState::new(window, cx).placeholder("Search Coinbase spot markets")),
        ),
        MarketWorkerStartup::Ready(_) => None,
    }
}

fn desktop_window_options(cx: &mut App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitleBar::title_bar_options()),
        ..Default::default()
    }
}

fn subscribe_symbol_input(
    input: Option<Entity<InputState>>,
    terminal: &Entity<TerminalApp>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(input) = input else {
        return;
    };
    let terminal = terminal.clone();
    window
        .subscribe(&input, cx, move |_, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let selected = terminal.update(cx, TerminalApp::submit_symbol_input);
                if selected {
                    terminal.update(cx, |app, app_cx| {
                        app.close_chrome_overlay(window, app_cx);
                    });
                }
            } else {
                terminal.update(cx, |app, cx| {
                    app.chrome_selection = 0;
                    cx.notify();
                });
            }
        })
        .detach();
}

fn subscribe_indicator_input(
    input: &Entity<InputState>,
    terminal: &Entity<TerminalApp>,
    window: &mut Window,
    cx: &mut App,
) {
    let terminal = terminal.clone();
    let input = input.clone();
    window
        .subscribe(
            &input.clone(),
            cx,
            move |_, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let query = input.read(cx).value().to_string();
                    let indicator = terminal.update(cx, |app, _| {
                        chart_chrome::filter_indicator_specs(&query)
                            .get(app.chrome_selection)
                            .map(|spec| native_indicator(spec.kind))
                    });
                    if let Some(indicator) = indicator {
                        let added = terminal.update(cx, |app, cx| app.add_indicator(indicator, cx));
                        if added {
                            terminal.update(cx, |app, app_cx| {
                                app.close_chrome_overlay(window, app_cx);
                            });
                        }
                    }
                } else {
                    terminal.update(cx, |app, cx| {
                        app.chrome_selection = 0;
                        cx.notify();
                    });
                }
            },
        )
        .detach();
}

fn terminal_root(
    bootstrap: MarketWorkerStartup,
    market_worker: MarketDataWorker,
    window: &mut Window,
    cx: &mut App,
) -> Entity<Root> {
    let symbol_input = symbol_input_for_startup(&bootstrap, window, cx);
    let search_input = symbol_input.clone();
    let indicator_input =
        cx.new(|cx| InputState::new(window, cx).placeholder("Search native indicators"));
    let indicator_search_input = indicator_input.clone();
    let terminal = cx.new(move |cx| {
        TerminalApp::new(cx, bootstrap, market_worker, symbol_input, indicator_input)
    });
    subscribe_symbol_input(search_input, &terminal, window, cx);
    subscribe_indicator_input(&indicator_search_input, &terminal, window, cx);
    cx.new(|cx| Root::new(terminal, window, cx))
}

fn default_coinbase_history_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map_or_else(
        || PathBuf::from("local-data").join("coinbase-history"),
        |root| {
            PathBuf::from(root)
                .join("Axiusflow")
                .join("market-history")
                .join("coinbase")
        },
    )
}

fn coinbase_cli_worker(
    mut arguments: impl Iterator<Item = std::ffi::OsString>,
) -> (MarketWorkerStartup, MarketDataWorker) {
    let usage = "usage: axiusflow_desktop --coinbase-live <PRODUCT-ID> <history-root> [--detailed-diagnostics]";
    let product = arguments.next().unwrap_or_else(|| {
        eprintln!("{usage}");
        std::process::exit(2);
    });
    let history_root = arguments.next().unwrap_or_else(|| {
        eprintln!("{usage}");
        std::process::exit(2);
    });
    let detailed_diagnostics = match arguments.next() {
        Some(flag) if flag == "--detailed-diagnostics" => true,
        Some(_) => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
        None => false,
    };
    if arguments.next().is_some() {
        eprintln!("{usage}");
        std::process::exit(2);
    }
    MarketDataWorker::start_coinbase(
        product.to_string_lossy().into_owned(),
        PathBuf::from(history_root),
        std::thread::current().id(),
        detailed_diagnostics,
        true,
    )
    .expect("the bounded direct Coinbase worker starts")
}

fn configured_market_worker() -> Option<(MarketWorkerStartup, MarketDataWorker)> {
    let mut arguments = std::env::args_os().skip(1);
    let worker = if let Some(argument) = arguments.next() {
        if argument == "--windowed-benchmark" {
            let report_path = arguments
                .next()
                .expect("usage: axiusflow_desktop --windowed-benchmark <report-path>");
            windowed_benchmark::run(std::path::Path::new(&report_path))
                .expect("the windowed benchmark completes");
            return None;
        }
        if argument == "--desktop-readiness" {
            run_desktop_readiness_command(arguments).expect("desktop readiness conformance passes");
            return None;
        }
        if argument == "--desktop-endurance" {
            run_desktop_endurance_command(arguments).expect("desktop endurance conformance passes");
            return None;
        }
        if argument == "--coinbase-live-smoke" {
            run_coinbase_live_smoke_command(arguments)
                .expect("the Coinbase shipping live smoke passes");
            return None;
        }
        if argument == "--rithmic-test" {
            let parsed = parse_rithmic_test_arguments(arguments).unwrap_or_else(|usage| {
                eprintln!("{usage}");
                std::process::exit(2);
            });
            MarketDataWorker::start_rithmic(
                parsed.history_root,
                std::thread::current().id(),
                parsed.detailed_diagnostics,
                parsed.native_transition_report,
            )
            .unwrap_or_else(|error| {
                eprintln!("Rithmic Test shell could not start: {error}");
                std::process::exit(1);
            })
        } else if argument == "--coinbase-live" {
            coinbase_cli_worker(arguments)
        } else if argument == "--fixture" {
            if arguments.next().is_some() {
                eprintln!("usage: axiusflow_desktop --fixture");
                std::process::exit(2);
            }
            MarketDataWorker::start().expect("the bounded binary fixture worker bootstraps")
        } else {
            eprintln!("unsupported argument: {}", argument.to_string_lossy());
            std::process::exit(2);
        }
    } else {
        MarketDataWorker::start_coinbase(
            "BTC-USD".to_string(),
            default_coinbase_history_root(),
            std::thread::current().id(),
            false,
            true,
        )
        .expect("the default Coinbase public market worker starts")
    };
    Some(worker)
}

fn main() {
    let Some((bootstrap, market_worker)) = configured_market_worker() else {
        return;
    };
    application()
        .with_assets(HugeiconsAssets::with_fallback(assets::DesktopAssets))
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            sync_component_theme(&AxiusflowTheme::dark(), None, cx);
            let options = desktop_window_options(cx);

            cx.open_window(options, move |window, cx| {
                terminal_root(bootstrap, market_worker, window, cx)
            })
            .expect("the Axiusflow terminal window opens");
            cx.activate(true);
        });
}

#[cfg(test)]
mod tests {
    use super::{
        CatalogCommandDomain, ChartNoticePlacement, ChartNoticeTone, ChartState, HeaderControls,
        RithmicReadyAction, RithmicReconnectState, RithmicReconnectTarget,
        RithmicSessionRetirement, SidePanel, TerminalProvider, bounded_status_detail,
        catalog_rejection_domain, chart_status_detail, chart_surface_notice,
        connection_presentation, default_rithmic_contract_index, duration_label, gpui_color,
        instrument_menu_detail, instrument_selector_label, milli_rate,
        parse_rithmic_test_arguments, publication_chart_state, reconciled_bridge_state,
        reconnect_contract_index, rithmic_production_subscription, rithmic_ready_action,
        series_selector_label, should_apply_rithmic_worker_stop,
    };
    use axiusflow_design_system::ThemeColor;
    use axiusflow_observability::FeedConnectionState;
    use axiusflow_rithmic_protocol_adapter::{
        RithmicCatalogRejection, RithmicReadOnlySubscription, SymbolSearchResult,
    };
    use std::ffi::OsString;

    #[test]
    fn publication_is_ready_only_after_bridge_acceptance_without_recovery() {
        assert_eq!(publication_chart_state(true, false), ChartState::Ready);
        assert_eq!(publication_chart_state(false, true), ChartState::Recovering);
        assert_eq!(publication_chart_state(true, true), ChartState::Recovering);
    }

    #[test]
    fn catalog_rejections_preserve_search_and_selection_generation_domains() {
        for reason in [
            RithmicCatalogRejection::SearchRejected,
            RithmicCatalogRejection::SupersededSearch,
            RithmicCatalogRejection::SearchDispatchUnavailable,
        ] {
            assert_eq!(
                catalog_rejection_domain(reason),
                CatalogCommandDomain::Search
            );
        }
        for reason in [
            RithmicCatalogRejection::InstrumentUnavailable,
            RithmicCatalogRejection::SubscriptionRejected,
            RithmicCatalogRejection::SelectionDispatchUnavailable,
        ] {
            assert_eq!(
                catalog_rejection_domain(reason),
                CatalogCommandDomain::Selection
            );
        }
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
    fn disconnected_session_retires_surfaces_and_keeps_a_stale_chart_notice() {
        let retirement =
            RithmicSessionRetirement::from_connection(FeedConnectionState::Disconnected);
        assert_eq!(retirement, RithmicSessionRetirement::Offline);
        assert_eq!(retirement.chart_state(true), Some(ChartState::Stale));
        assert_eq!(retirement.chart_state(false), None);
        let mut retained_chart_state = retirement
            .chart_state(true)
            .expect("offline retained chart becomes stale");
        assert_eq!(retained_chart_state, ChartState::Stale);
        retained_chart_state =
            RithmicSessionRetirement::from_connection(FeedConnectionState::Recovering)
                .chart_state(true)
                .expect("the same retained chart advances to reconnecting");
        assert_eq!(retained_chart_state, ChartState::Recovering);
        assert_eq!(
            RithmicSessionRetirement::from_connection(FeedConnectionState::Recovering)
                .chart_state(true),
            Some(ChartState::Recovering)
        );
        assert_eq!(
            RithmicSessionRetirement::from_connection(FeedConnectionState::Streaming),
            RithmicSessionRetirement::None
        );
    }

    #[test]
    fn terminal_session_stop_is_a_truthful_chart_error_with_or_without_data() {
        let stopped = RithmicSessionRetirement::from_connection(FeedConnectionState::Stopped);
        assert_eq!(stopped.chart_state(true), Some(ChartState::Error));
        assert_eq!(stopped.chart_state(false), Some(ChartState::Error));
        assert_eq!(
            chart_surface_notice(
                stopped.chart_state(true).expect("stopped chart state"),
                true,
                "Rithmic market worker stopped",
            )
            .expect("retained chart error notice")
            .placement,
            ChartNoticePlacement::TopLeft
        );
        assert_eq!(
            chart_surface_notice(
                stopped.chart_state(false).expect("stopped chart state"),
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
    fn production_rithmic_selection_requests_every_read_only_market_class() {
        assert_eq!(
            rithmic_production_subscription(),
            RithmicReadOnlySubscription::try_new(true, true, true)
        );
        assert_ne!(
            rithmic_production_subscription(),
            RithmicReadOnlySubscription::try_new(true, false, true)
        );
    }

    #[test]
    fn authentication_ready_reselects_the_retired_contract() {
        let reconnect = RithmicReconnectState::AwaitingSearch(RithmicReconnectTarget {
            symbol: "MNQU6".to_string(),
            exchange: "CME".to_string(),
            series: crate::rithmic_history::RithmicSeries::Minute5,
        });
        assert_eq!(
            rithmic_ready_action(
                FeedConnectionState::Authenticating,
                "Rithmic Test session is ready for instrument search",
                &reconnect,
                true,
            ),
            RithmicReadyAction::Reconnect("MNQU6".to_string())
        );
        assert_eq!(
            rithmic_ready_action(
                FeedConnectionState::Discovering,
                "discovering Rithmic Test systems",
                &reconnect,
                true,
            ),
            RithmicReadyAction::None
        );
    }

    #[test]
    fn interrupted_initial_autoload_restarts_after_authentication() {
        let mut reconnect = RithmicReconnectState::Idle;
        assert!(
            reconnect
                .capture_retired_selection(None, crate::rithmic_history::RithmicSeries::Minute1,)
        );
        assert_eq!(
            rithmic_ready_action(
                FeedConnectionState::Authenticating,
                "Rithmic Test session is ready for instrument search",
                &reconnect,
                false,
            ),
            RithmicReadyAction::Autoload
        );
        assert_eq!(
            rithmic_ready_action(
                FeedConnectionState::Authenticating,
                "Rithmic Test session is ready for instrument search",
                &RithmicReconnectState::Idle,
                true,
            ),
            RithmicReadyAction::None
        );
    }

    #[test]
    fn gpui_theme_attachment_preserves_alpha() {
        let attached = gpui_color(ThemeColor::from_rgb8(240, 240, 240).with_alpha(19.0 / 255.0));
        assert!((attached.a - 19.0 / 255.0).abs() < f32::EPSILON);
    }

    #[test]
    fn rithmic_test_cli_bounds_diagnostics_and_native_transition_capture() {
        let parsed = parse_rithmic_test_arguments(
            [
                "cache",
                "--capture-native-transitions",
                "evidence/transitions.json",
                "--detailed-diagnostics",
            ]
            .into_iter()
            .map(OsString::from),
        )
        .expect("valid Rithmic Test arguments parse");
        assert_eq!(parsed.history_root, std::path::PathBuf::from("cache"));
        assert!(parsed.detailed_diagnostics);
        assert_eq!(
            parsed.native_transition_report,
            Some(std::path::PathBuf::from("evidence/transitions.json"))
        );
        assert!(parse_rithmic_test_arguments(std::iter::empty()).is_err());
        assert!(
            parse_rithmic_test_arguments(
                [OsString::from("cache"), OsString::from("--unknown")].into_iter()
            )
            .is_err()
        );
        assert!(
            parse_rithmic_test_arguments(
                [
                    "cache",
                    "--capture-native-transitions",
                    "first.json",
                    "--capture-native-transitions",
                    "second.json",
                ]
                .into_iter()
                .map(OsString::from)
            )
            .is_err()
        );
    }

    #[test]
    fn header_lifecycle_and_health_values_are_truthfully_labeled() {
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Disconnected,
                ChartState::Loading,
                false,
            )
            .0,
            "Offline"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Recovering,
                ChartState::Loading,
                false,
            )
            .0,
            "Test · Reconnecting"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Streaming,
                ChartState::Ready,
                false,
            )
            .0,
            "Test · Live"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Streaming,
                ChartState::Stale,
                false,
            )
            .0,
            "Test · Stale"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Rithmic,
                FeedConnectionState::Streaming,
                ChartState::Ready,
                true,
            )
            .0,
            "Test · Delayed"
        );
        assert_eq!(
            connection_presentation(
                TerminalProvider::Coinbase,
                FeedConnectionState::Streaming,
                ChartState::Ready,
                false,
            )
            .0,
            "Coinbase · Live"
        );
        assert_eq!(milli_rate(12_345), "12.345");
        assert_eq!(duration_label(850_000), "850 µs");
        assert_eq!(duration_label(42_000_000), "42 ms");
        assert_eq!(duration_label(1_500_000_000), "1.5 s");
        let controls = HeaderControls::from_state(true, true, false).with_chart_controls(true);
        assert!(controls.enabled(HeaderControls::INSTRUMENT));
        assert!(controls.enabled(HeaderControls::SERIES));
        assert!(controls.enabled(HeaderControls::DOM));
        assert!(!controls.enabled(HeaderControls::HEALTH));
        assert!(controls.enabled(HeaderControls::FIT));
        assert!(controls.enabled(HeaderControls::LATEST));
    }

    #[test]
    fn instrument_menu_detail_keeps_exchange_type_and_expiration_distinct() {
        let instrument = SymbolSearchResult {
            symbol: "MNQU6".to_string(),
            exchange: "CME".to_string(),
            name: Some("Micro E-mini Nasdaq-100".to_string()),
            product_code: Some("MNQ".to_string()),
            instrument_type: Some("Future".to_string()),
            expiration_date: Some("2026-09".to_string()),
        };

        assert_eq!(
            instrument_menu_detail(&instrument),
            "CME · Future · 2026-09"
        );
    }

    #[test]
    fn chart_controls_follow_retained_data_instead_of_transient_chart_state() {
        let retained_chart_controls =
            HeaderControls::from_state(true, false, false).with_chart_controls(true);
        assert!(retained_chart_controls.enabled(HeaderControls::FIT));
        assert!(retained_chart_controls.enabled(HeaderControls::LATEST));

        let empty_chart_controls =
            HeaderControls::from_state(true, false, false).with_chart_controls(false);
        assert!(!empty_chart_controls.enabled(HeaderControls::FIT));
        assert!(!empty_chart_controls.enabled(HeaderControls::LATEST));
    }

    #[test]
    fn side_panel_controls_keep_stable_labels_and_explicit_destinations() {
        assert_eq!(SidePanel::Dom.toggle_label(), "DOM");
        assert_eq!(SidePanel::Dom.title(), "Order book");
        assert_eq!(SidePanel::Health.toggle_label(), "Health");
        assert_eq!(SidePanel::Health.title(), "Feed health");
        assert_ne!(
            SidePanel::Dom.toggle_tooltip(),
            SidePanel::Health.toggle_tooltip()
        );
    }

    #[test]
    fn chart_notice_distinguishes_empty_loading_from_retained_recovery() {
        let loading = chart_surface_notice(
            ChartState::Loading,
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
            "Rithmic Test session will retry",
        )
        .expect("recovery notice");
        assert_eq!(recovery.label, "Reconnecting chart");
        assert_eq!(recovery.placement, ChartNoticePlacement::TopLeft);
        assert_eq!(recovery.tone, ChartNoticeTone::Warning);
        assert!(chart_surface_notice(ChartState::Ready, true, "current").is_none());
    }

    #[test]
    fn chart_error_and_stale_notices_use_truthful_severity() {
        let stale = chart_surface_notice(ChartState::Stale, true, "trade stream is silent")
            .expect("stale notice");
        assert_eq!(stale.label, "Chart stale");
        assert_eq!(stale.tone, ChartNoticeTone::Warning);

        let error = chart_surface_notice(
            ChartState::Error,
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
        assert_eq!(series_selector_label(None, None), "Series");
        assert_eq!(
            series_selector_label(Some(crate::rithmic_history::RithmicSeries::Minute1), None),
            "1m"
        );
        assert_eq!(
            series_selector_label(
                Some(crate::rithmic_history::RithmicSeries::Minute1),
                Some(crate::rithmic_history::RithmicSeries::Minute5),
            ),
            "1m"
        );
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
}
