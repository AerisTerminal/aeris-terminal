//! Engine-owned public Hyperliquid market-data lifecycle.
//!
//! One multiplexed WebSocket carries every demanded market feed; HTTP only
//! serves catalog refreshes on a second worker thread so slow metadata never
//! stalls live prices. No credentials exist: the public feed needs none.
//!
//! The realtime thread owns the socket and one generation counter. Each
//! connection bumps the generation, resubscribes the last demanded set, and
//! announces itself; the coordinator fences every event against the engine
//! session, so a reconnect can never rewrite a retired series. Symbol,
//! timeframe, tab, and layout changes only resend the desired subscription
//! set over the same connection.

use aeris_observability::diagnostic;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    ProviderCatalogRejected, ProviderCatalogRejectionReason, ProviderInstrumentSearchResult,
    ProviderMarketScreen, ScreenProviderMarkets, SearchProviderInstruments,
    SelectProviderInstrument,
};
use aeris_contracts::{InstallProviderInstrument, ProviderContractMetadata};
use aeris_hyperliquid_market_adapter::{
    DeliveredTradeIds, HYPERLIQUID_WS_URL, HyperliquidCatalog, HyperliquidHttpConfig,
    HyperliquidLiveCandle, WsClientEvent, decode_book_snapshot, decode_live_candle,
    decode_trades_batch, fetch_meta_bundle, parse_ws_frame,
};
use aeris_market_data::{DepthSnapshot, MarketTrade, TopOfBookQuote};
use aeris_market_engine::fence::{Fenced, ProviderTag};
use aeris_platform_runtime::{MarketSocket, MarketSocketEvent};

use crate::hyperliquid_market_screen::{RetainedMarketStatistics, instrument_summary};
use crate::market_service::provider_session::ProviderEventSink;
use crate::market_service::{CatalogPublisher, ProviderCoordinatorWake};

pub(crate) const CATALOG_WORKER_NAME: &str = "aeris-hyperliquid-catalog";
pub(crate) const PROVIDER_WORKER_NAME: &str = "aeris-hyperliquid-provider";

/// Public account and entitlement identity for credential-free market data.
pub(crate) const HYPERLIQUID_PUBLIC_ACCOUNT_ID: &str = "hyperliquid-public";
/// Entitlement revision pinned to the unauthenticated public feed.
pub(crate) const HYPERLIQUID_PUBLIC_ENTITLEMENT_ID: &str = "hyperliquid-public";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Quiet-socket control responsiveness bound. A demanded symbol change is
/// reconciled between reads, so keeping this short prevents a healthy but
/// temporarily quiet socket from adding human-visible subscription latency.
const READ_TIMEOUT: Duration = Duration::from_millis(2);
const PING_INTERVAL: Duration = Duration::from_secs(20);
const MESSAGE_SILENCE_TIMEOUT: Duration = Duration::from_secs(45);
/// Retry bound for an epoch-lifecycle emission against a full event queue.
/// The coordinator drains continuously, so a delivery still missing this
/// far past due means the worker cannot help the coordinator observe it.
const LIFECYCLE_EMIT_BACKPRESSURE: Duration = Duration::from_secs(2);
/// Poll interval between epoch-lifecycle emission retries.
const LIFECYCLE_EMIT_POLL: Duration = Duration::from_millis(20);
const CATALOG_REFRESH_INTERVAL: Duration = Duration::from_mins(5);
const CATALOG_POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAXIMUM_DECODE_FAILURES_PER_CONNECTION: u32 = 100;
const MAXIMUM_SUBSCRIPTION_FRAMES_PER_DEMAND: usize = 512;

/// One demanded instrument with the mapping the worker needs to decode it.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct HyperliquidInstrumentDemand {
    /// Exact wire `coin` for info/WebSocket calls.
    pub wire_coin: String,
    /// Stable engine instrument identity.
    pub instrument_id: String,
    /// Catalog entitlement revision.
    pub entitlement_id: String,
    /// Fixed-point scales from the install.
    pub price_scale: u8,
    /// Fixed-point scales from the install.
    pub quantity_scale: u8,
}

/// One demanded candle feed with its native interval.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct HyperliquidCandleDemand {
    /// Instrument mapping for this feed.
    pub instrument: HyperliquidInstrumentDemand,
    /// Native Hyperliquid interval (`1m`, `1h`, ...).
    pub interval: String,
}

/// The complete desired subscription set. Newest wins: the coordinator sends
/// the whole set whenever demand changes and the worker diffs it against the
/// live subscriptions without reconnecting.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HyperliquidDemand {
    /// Candle feeds, one per demanded series.
    pub candles: Vec<HyperliquidCandleDemand>,
    /// Trade feeds, one per demanded instrument.
    pub trades: Vec<HyperliquidInstrumentDemand>,
    /// BBO feeds, one per quote-demanded instrument.
    pub quotes: Vec<HyperliquidInstrumentDemand>,
    /// Book feeds, one per depth-demanded instrument.
    pub books: Vec<HyperliquidInstrumentDemand>,
}

pub(crate) enum HyperliquidRealtimeControl {
    Connect(ProviderEventSink),
    Subscribe(HyperliquidDemand),
    Stop,
    #[cfg(feature = "probe")]
    FailWebSocket {
        subsequent_connect_failures: u8,
    },
}

pub(crate) enum HyperliquidCatalogControl {
    Search(SearchProviderInstruments),
    Select(SelectProviderInstrument),
    Screen(ScreenProviderMarkets),
}

pub(crate) enum HyperliquidCatalogEvent {
    SearchCompleted(ProviderInstrumentSearchResult),
    ScreenCompleted(ProviderMarketScreen),
    ScreenRejected(ProviderCatalogRejected),
    SelectionResolved {
        consumer_id: u64,
        command_generation: u64,
        instrument: InstallProviderInstrument,
    },
    Rejected {
        rejection: ProviderCatalogRejected,
        selection: bool,
    },
    /// A background refresh failed; the retained catalog keeps serving.
    /// The coordinator downgrades provider state so the failure surfaces
    /// instead of aging silently.
    RefreshFailed {
        detail: String,
    },
}

pub(crate) enum HyperliquidRealtimeEvent {
    Connecting(u64),
    Connected(u64),
    Candle(u64, String, String, HyperliquidLiveCandle),
    Trades(u64, Vec<MarketTrade>),
    Quote(u64, TopOfBookQuote),
    Depth(u64, DepthSnapshot),
    Heartbeat(u64, Option<u64>),
    Recovering(u64),
    Disconnected(u64),
}

/// Desired subscription identity for diffing without reconnecting.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum SubscriptionKey {
    Candle { coin: String, interval: String },
    Trades { coin: String },
    Bbo { coin: String },
    Book { coin: String },
}

/// Runs the catalog worker: retained-catalog search/select over HTTP.
pub(crate) fn run_catalog(
    controls: &Receiver<HyperliquidCatalogControl>,
    events: &CatalogPublisher<HyperliquidCatalogEvent>,
    ws_generation: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
    http_config: HyperliquidHttpConfig,
) {
    let mut catalog = CatalogState::default();
    let mut next_refresh = Instant::now();
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let timeout = catalog_wait(&catalog, next_refresh, Instant::now());
        match controls.recv_timeout(timeout) {
            Ok(control) => {
                if handle_catalog_control(
                    control,
                    &mut catalog,
                    events,
                    ws_generation,
                    stop,
                    http_config,
                ) {
                    return;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        // A search/select may have just populated the catalog. Do not fetch
        // the same bundle again immediately after that successful request.
        if let Some(fetched_at) = catalog.fetched_at {
            next_refresh = next_refresh.max(fetched_at + CATALOG_REFRESH_INTERVAL);
        }
        if catalog.present().is_some() && Instant::now() >= next_refresh {
            match fetch_meta_bundle(http_config) {
                Ok(replacement) => {
                    catalog.replace(replacement);
                    next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                }
                Err(error) => {
                    next_refresh = Instant::now() + CATALOG_REFRESH_INTERVAL;
                    if publish_catalog(
                        events,
                        HyperliquidCatalogEvent::RefreshFailed { detail: error },
                        stop,
                    ) {
                        return;
                    }
                }
            }
        }
    }
}

fn catalog_wait(catalog: &CatalogState, next_refresh: Instant, now: Instant) -> Duration {
    if catalog.present().is_none() {
        CATALOG_POLL_INTERVAL
    } else {
        next_refresh
            .saturating_duration_since(now)
            .min(CATALOG_POLL_INTERVAL)
    }
}

#[derive(Default)]
struct CatalogState {
    catalog: Option<HyperliquidCatalog>,
    fetched_at: Option<Instant>,
    epoch: u64,
    selection_generation: u64,
    statistics: RetainedMarketStatistics,
}

impl CatalogState {
    fn present(&self) -> Option<&HyperliquidCatalog> {
        self.catalog.as_ref()
    }

    fn replace(&mut self, catalog: HyperliquidCatalog) {
        self.catalog = Some(catalog);
        self.fetched_at = Some(Instant::now());
        self.epoch = self.epoch.saturating_add(1).max(1);
    }
}

/// Handles one catalog control. Returns true when the worker must exit.
fn handle_catalog_control(
    control: HyperliquidCatalogControl,
    catalog: &mut CatalogState,
    events: &CatalogPublisher<HyperliquidCatalogEvent>,
    ws_generation: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
    http_config: HyperliquidHttpConfig,
) -> bool {
    match control {
        HyperliquidCatalogControl::Search(search) => {
            handle_catalog_search(search, catalog, events, ws_generation, stop, http_config)
        }
        HyperliquidCatalogControl::Select(selection) => {
            handle_catalog_select(selection, catalog, events, ws_generation, stop, http_config)
        }
        HyperliquidCatalogControl::Screen(screen) => {
            handle_catalog_screen(&screen, catalog, events, stop, http_config)
        }
    }
}

/// Answers one market screen from the retained catalog and recent statistics.
/// Returns true when the worker must exit.
fn handle_catalog_screen(
    screen: &ScreenProviderMarkets,
    catalog: &mut CatalogState,
    events: &CatalogPublisher<HyperliquidCatalogEvent>,
    stop: &Arc<AtomicBool>,
    http_config: HyperliquidHttpConfig,
) -> bool {
    let refreshed = if ensure_catalog(catalog, http_config) {
        catalog.statistics.refresh(http_config, Instant::now())
    } else {
        Err("hyperliquid catalog is unavailable".to_string())
    };
    if let Err(error) = &refreshed {
        diagnostic!("Aeris Hyperliquid market screen is unavailable: {error}");
    }
    if stop.load(Ordering::Acquire) {
        return true;
    }
    let maximum_results = usize::try_from(screen.maximum_results).unwrap_or(usize::MAX);
    let answered = refreshed.ok().and_then(|()| {
        let current = catalog.present()?;
        catalog.statistics.screen(current, maximum_results)
    });
    let event = match answered {
        Some((captured_at_unix_millis, markets)) => {
            HyperliquidCatalogEvent::ScreenCompleted(ProviderMarketScreen {
                consumer_id: screen.consumer_id,
                provider: "hyperliquid".to_string(),
                provider_generation: catalog.epoch.max(1),
                screen_generation: screen.screen_generation,
                captured_at_unix_millis,
                markets,
            })
        }
        None => HyperliquidCatalogEvent::ScreenRejected(ProviderCatalogRejected {
            consumer_id: screen.consumer_id,
            provider: "hyperliquid".to_string(),
            provider_generation: None,
            command_generation: screen.screen_generation,
            reason: ProviderCatalogRejectionReason::DispatchUnavailable,
        }),
    };
    publish_catalog(events, event, stop)
}

/// Fetches the catalog when missing so a cold start still answers.
/// Returns whether a catalog is now present.
fn ensure_catalog(catalog: &mut CatalogState, http_config: HyperliquidHttpConfig) -> bool {
    if catalog.present().is_some() {
        return true;
    }
    if let Ok(replacement) = fetch_meta_bundle(http_config) {
        catalog.replace(replacement);
    }
    catalog.present().is_some()
}

fn reject_catalog_command(
    events: &CatalogPublisher<HyperliquidCatalogEvent>,
    ws_generation: &Arc<AtomicU64>,
    consumer_id: u64,
    command_generation: u64,
    selection: bool,
    stop: &AtomicBool,
) {
    let _ = publish_catalog(
        events,
        HyperliquidCatalogEvent::Rejected {
            rejection: ProviderCatalogRejected {
                consumer_id,
                provider: "hyperliquid".to_string(),
                provider_generation: Some(ws_generation.load(Ordering::Acquire).max(1)),
                command_generation,
                reason: ProviderCatalogRejectionReason::DispatchUnavailable,
            },
            selection,
        },
        stop,
    );
}

fn handle_catalog_search(
    search: SearchProviderInstruments,
    catalog: &mut CatalogState,
    events: &CatalogPublisher<HyperliquidCatalogEvent>,
    ws_generation: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
    http_config: HyperliquidHttpConfig,
) -> bool {
    // No catalog yet: fetch on demand so a cold start with no background
    // refresh still answers the first search.
    if catalog.present().is_none() {
        if ensure_catalog(catalog, http_config) {
            return handle_catalog_search(
                search,
                catalog,
                events,
                ws_generation,
                stop,
                http_config,
            );
        }
        if stop.load(Ordering::Acquire) {
            return true;
        }
        reject_catalog_command(
            events,
            ws_generation,
            search.consumer_id,
            search.search_generation,
            false,
            stop,
        );
        return false;
    }
    let Some(current) = catalog.present() else {
        return false;
    };
    let instruments = current
        .search(
            &search.query,
            search.exchange.as_deref(),
            usize::try_from(search.maximum_results).unwrap_or(16).max(1),
        )
        .iter()
        .map(instrument_summary)
        .collect();
    let _ = publish_catalog(
        events,
        HyperliquidCatalogEvent::SearchCompleted(ProviderInstrumentSearchResult {
            consumer_id: search.consumer_id,
            provider: "hyperliquid".to_string(),
            provider_generation: catalog.epoch.max(1),
            search_generation: search.search_generation,
            instruments,
        }),
        stop,
    );
    false
}

fn handle_catalog_select(
    selection: SelectProviderInstrument,
    catalog: &mut CatalogState,
    events: &CatalogPublisher<HyperliquidCatalogEvent>,
    ws_generation: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
    http_config: HyperliquidHttpConfig,
) -> bool {
    let command_generation = selection.selection_generation;
    // Resolve-on-demand when the catalog is missing entirely so a cold
    // start without a background refresh still answers.
    if catalog.present().is_none() {
        if ensure_catalog(catalog, http_config) {
            return handle_catalog_select(
                selection,
                catalog,
                events,
                ws_generation,
                stop,
                http_config,
            );
        }
        if stop.load(Ordering::Acquire) {
            return true;
        }
        reject_catalog_command(
            events,
            ws_generation,
            selection.consumer_id,
            selection.selection_generation,
            true,
            stop,
        );
        return false;
    }
    let resolved = catalog.present().and_then(|current| {
        current
            .instruments
            .values()
            .find(|instrument| {
                instrument.wire_coin == selection.symbol
                    && instrument.venue == selection.exchange
                    && instrument_matches_entitlement(instrument, &selection.entitlement_id)
            })
            .map(|instrument| {
                (
                    instrument.clone(),
                    current.price_increment(&instrument.instrument_id),
                )
            })
    });
    let Some((resolved, price_increment)) = resolved else {
        let _ = publish_catalog(
            events,
            HyperliquidCatalogEvent::Rejected {
                rejection: ProviderCatalogRejected {
                    consumer_id: selection.consumer_id,
                    provider: "hyperliquid".to_string(),
                    provider_generation: Some(ws_generation.load(Ordering::Acquire).max(1)),
                    command_generation: selection.selection_generation,
                    reason: ProviderCatalogRejectionReason::InstrumentUnavailable,
                },
                selection: true,
            },
            stop,
        );
        return false;
    };
    catalog.selection_generation = catalog.selection_generation.saturating_add(1).max(1);
    let terms = hyperliquid_terms(&resolved);
    let _ = publish_catalog(
        events,
        HyperliquidCatalogEvent::SelectionResolved {
            consumer_id: selection.consumer_id,
            command_generation,
            instrument: InstallProviderInstrument {
                provider: "hyperliquid".to_string(),
                session_generation: ws_generation.load(Ordering::Acquire).max(1),
                selection_generation: catalog.selection_generation,
                instrument_id: resolved.instrument_id,
                provider_symbol: resolved.wire_coin,
                display_symbol: resolved.display,
                venue_id: resolved.venue,
                price_scale: u32::from(resolved.price_scale),
                quantity_scale: u32::from(resolved.quantity_scale),
                entitlement_id: HYPERLIQUID_PUBLIC_ENTITLEMENT_ID.to_string(),
                // Hyperliquid ticks follow significant figures, so this is the
                // valid increment near the catalog mark price at selection.
                price_increment,
                contract_metadata: Some(Box::new(terms)),
            },
        },
        stop,
    );
    false
}

fn hyperliquid_terms(
    resolved: &aeris_hyperliquid_market_adapter::HyperliquidInstrument,
) -> ProviderContractMetadata {
    let spot = matches!(
        resolved.kind,
        aeris_hyperliquid_market_adapter::HyperliquidMarketKind::Spot { .. }
    );
    ProviderContractMetadata {
        // Hyperliquid sizes are base-asset units and linear PnL is
        // (exit - entry) * size, so one size unit has point value one.
        point_value: Some(1),
        point_value_scale: Some(0),
        currency: Some("USD".to_string()),
        price_significant_figures: Some(5),
        // Exchange tick/lot docs: at most 6 - szDecimals price decimals
        // for perps, 8 - szDecimals for spot. The normalized quantity
        // scale is not the native szDecimals.
        price_max_decimals: (if spot { 8_u32 } else { 6_u32 }).checked_sub(resolved.sz_decimals),
        instrument_class: Some(if spot {
            aeris_contracts::ProviderInstrumentClass::CryptoSpot
        } else {
            aeris_contracts::ProviderInstrumentClass::CryptoPerpetual
        } as i32),
        ..Default::default()
    }
}

fn instrument_matches_entitlement(
    instrument: &aeris_hyperliquid_market_adapter::HyperliquidInstrument,
    entitlement_id: &str,
) -> bool {
    // The public feed has exactly one entitlement; anything else is stale.
    let _ = instrument;
    entitlement_id == HYPERLIQUID_PUBLIC_ENTITLEMENT_ID
}

/// Runs the realtime worker: one multiplexed socket for every demand.
pub(crate) fn run(
    controls: &Receiver<HyperliquidRealtimeControl>,
    events: &SyncSender<Fenced<ProviderTag, HyperliquidRealtimeEvent>>,
    stop: &Arc<AtomicBool>,
    ws_generation: &Arc<AtomicU64>,
    wake: &ProviderCoordinatorWake,
) {
    let mut demand = HyperliquidDemand::default();
    let mut pending_epoch = None;
    #[cfg(feature = "probe")]
    let mut forced_failures = 0;
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        if pending_epoch.is_none() {
            match controls.recv_timeout(CATALOG_POLL_INTERVAL) {
                Ok(HyperliquidRealtimeControl::Connect(sink)) => pending_epoch = Some(sink),
                Ok(HyperliquidRealtimeControl::Subscribe(next)) => demand = next,
                Ok(HyperliquidRealtimeControl::Stop)
                | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                #[cfg(feature = "probe")]
                Ok(HyperliquidRealtimeControl::FailWebSocket { .. }) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
            continue;
        }
        drain_controls(controls, &mut demand, &mut pending_epoch);
        let Some(sink) = pending_epoch.take() else {
            continue;
        };
        let generation = sink.epoch.get().get();
        ws_generation.store(generation, Ordering::Release);
        if emit_lifecycle(
            events,
            HyperliquidRealtimeEvent::Connecting(generation),
            stop,
            wake,
            sink,
        ) {
            return;
        }
        let connection = connect_market_socket(
            stop,
            #[cfg(feature = "probe")]
            &mut forced_failures,
        );
        match connection {
            Ok((mut socket, _shutdown)) => {
                diagnostic!("Aeris Hyperliquid market session opened epoch={generation}");
                match run_session(
                    &mut socket,
                    generation,
                    &mut demand,
                    controls,
                    (events, sink),
                    stop,
                    wake,
                ) {
                    SessionExit::Closed if !wake.overflowed("hyperliquid", generation) => return,
                    SessionExit::Closed => {}
                    SessionExit::Parked => {
                        diagnostic!("Aeris Hyperliquid realtime parked by demand owner");
                    }
                    SessionExit::Reconnect(reason) => {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        diagnostic!("Aeris Hyperliquid realtime reconnecting: {reason}");
                    }
                    #[cfg(feature = "probe")]
                    SessionExit::ProbeFailure(count) => {
                        forced_failures = count;
                    }
                }
            }
            Err(error) => {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                diagnostic!("Aeris Hyperliquid connection failed: {error}");
                if emit(
                    events,
                    HyperliquidRealtimeEvent::Recovering(generation),
                    stop,
                    wake,
                    sink,
                ) {
                    return;
                }
            }
        }
        if emit_lifecycle(
            events,
            HyperliquidRealtimeEvent::Disconnected(generation),
            stop,
            wake,
            sink,
        ) {
            return;
        }
    }
}

fn connect_market_socket(
    stop: &Arc<AtomicBool>,
    #[cfg(feature = "probe")] forced_failures: &mut u8,
) -> Result<(MarketSocket, aeris_platform_runtime::MarketSocketShutdown), String> {
    #[cfg(feature = "probe")]
    if *forced_failures > 0 {
        *forced_failures -= 1;
        return Err("probe WebSocket-only connection failure".into());
    }
    MarketSocket::connect(HYPERLIQUID_WS_URL, CONNECT_TIMEOUT, stop)
        .map_err(|error| error.to_string())
}

fn drain_controls(
    controls: &Receiver<HyperliquidRealtimeControl>,
    demand: &mut HyperliquidDemand,
    pending_epoch: &mut Option<ProviderEventSink>,
) {
    // Newest demand wins; stopping parks rather than closing the worker.
    while let Ok(control) = controls.try_recv() {
        match control {
            HyperliquidRealtimeControl::Subscribe(next) => {
                *demand = next;
            }
            HyperliquidRealtimeControl::Connect(sink) => *pending_epoch = Some(sink),
            HyperliquidRealtimeControl::Stop => *pending_epoch = None,
            #[cfg(feature = "probe")]
            HyperliquidRealtimeControl::FailWebSocket { .. } => {}
        }
    }
}

#[derive(PartialEq, Eq)]
enum SessionExit {
    Reconnect(String),
    Parked,
    Closed,
    #[cfg(feature = "probe")]
    ProbeFailure(u8),
}

/// Publishes without blocking; overflow rejects the pending catalog requests.
fn publish_catalog<T>(events: &CatalogPublisher<T>, event: T, stop: &AtomicBool) -> bool {
    if stop.load(Ordering::Acquire) {
        return true;
    }
    // Overflow is reported by the publisher; the coordinator rejects pending
    // requests. The worker continues serving instead of becoming terminal.
    let _ = events.send(event);
    false
}

/// Sends one realtime event; returns true when cancelled or the coordinator is gone.
fn emit(
    events: &SyncSender<Fenced<ProviderTag, HyperliquidRealtimeEvent>>,
    event: HyperliquidRealtimeEvent,
    stop: &AtomicBool,
    wake: &ProviderCoordinatorWake,
    sink: ProviderEventSink,
) -> bool {
    if stop.load(Ordering::Acquire) {
        return true;
    }
    let generation = event.generation();
    match events.try_send(sink.stamp(event)) {
        Ok(()) => {
            wake.notify();
            false
        }
        Err(TrySendError::Full(_)) => {
            wake.report_overflow("hyperliquid", generation);
            false
        }
        Err(TrySendError::Disconnected(_)) => true,
    }
}

/// Emits one epoch-lifecycle event that the coordinator must observe.
///
/// `Connecting` opens a reserved epoch and `Disconnected` advances its
/// retry ladder; dropping either would strand the reserved epoch forever,
/// because the coordinator never re-sends the `Connect` control for it.
/// A full event queue retries under bounded backpressure until the
/// coordinator drains it. Cancellation, a coordinator-less channel, or a
/// queue still full past the bound gives up on the one event; the worker
/// itself continues serving.
fn emit_lifecycle(
    events: &SyncSender<Fenced<ProviderTag, HyperliquidRealtimeEvent>>,
    event: HyperliquidRealtimeEvent,
    stop: &AtomicBool,
    wake: &ProviderCoordinatorWake,
    sink: ProviderEventSink,
) -> bool {
    let generation = event.generation();
    let deadline = Instant::now() + LIFECYCLE_EMIT_BACKPRESSURE;
    let mut pending = event;
    loop {
        if stop.load(Ordering::Acquire) {
            return true;
        }
        match events.try_send(sink.stamp(pending)) {
            Ok(()) => {
                wake.notify();
                return false;
            }
            Err(TrySendError::Full(undelivered)) => {
                wake.report_overflow("hyperliquid", generation);
                if Instant::now() >= deadline {
                    return false;
                }
                pending = undelivered.value;
                thread::sleep(LIFECYCLE_EMIT_POLL);
            }
            Err(TrySendError::Disconnected(_)) => return true,
        }
    }
}

impl HyperliquidRealtimeEvent {
    pub(crate) fn generation(&self) -> u64 {
        match self {
            HyperliquidRealtimeEvent::Connecting(g)
            | HyperliquidRealtimeEvent::Connected(g)
            | HyperliquidRealtimeEvent::Recovering(g)
            | HyperliquidRealtimeEvent::Disconnected(g)
            | HyperliquidRealtimeEvent::Heartbeat(g, _)
            | HyperliquidRealtimeEvent::Candle(g, ..)
            | HyperliquidRealtimeEvent::Trades(g, _)
            | HyperliquidRealtimeEvent::Quote(g, _)
            | HyperliquidRealtimeEvent::Depth(g, _) => *g,
        }
    }
}

struct RealtimeEventSink<'a> {
    events: RealtimeEventChannel<'a>,
    stop: &'a AtomicBool,
    wake: &'a ProviderCoordinatorWake,
}

enum RealtimeEventChannel<'a> {
    Bound {
        events: &'a SyncSender<Fenced<ProviderTag, HyperliquidRealtimeEvent>>,
        sink: ProviderEventSink,
    },
    #[cfg(test)]
    Raw(&'a SyncSender<HyperliquidRealtimeEvent>),
}

impl RealtimeEventSink<'_> {
    fn send(&self, event: HyperliquidRealtimeEvent) -> bool {
        if self.stop.load(Ordering::Acquire) {
            return true;
        }
        let generation = event.generation();
        if self.wake.overflowed("hyperliquid", generation) {
            return true;
        }
        let result = match &self.events {
            RealtimeEventChannel::Bound { events, sink } => {
                events.try_send(sink.stamp(event)).map_err(|_| ())
            }
            #[cfg(test)]
            RealtimeEventChannel::Raw(events) => events.try_send(event).map_err(|_| ()),
        };
        if result.is_ok() {
            self.wake.notify();
            false
        } else {
            self.wake.report_overflow("hyperliquid", generation);
            true
        }
    }
}

struct SessionState {
    pending_subscriptions: BTreeMap<SubscriptionKey, Instant>,
    connected: bool,
    active: BTreeMap<SubscriptionKey, String>,
    feed: FeedState,
    last_inbound: Instant,
    last_ping: Instant,
    pending_ping: Option<Instant>,
}

/// What one connection knows when decoding inbound market frames.
struct FeedState {
    instruments: BTreeMap<String, HyperliquidInstrumentDemand>,
    /// Feeds unsubscribed on this connection, with the instant each was
    /// unsubscribed. The provider keeps sending frames already in flight until
    /// it acknowledges the unsubscribe, so those are expected, not malformed.
    retiring: BTreeMap<SubscriptionKey, Instant>,
    next_trade_sequence: u64,
    book_sequences: BTreeMap<String, u64>,
    /// Kept across unsubscribe for every coin still demanded on any channel,
    /// since its canonical tape outlives a `trades` resubscription.
    delivered_trades: BTreeMap<String, DeliveredTradeIds>,
    decode_failures: u32,
}

impl FeedState {
    fn new() -> Self {
        Self {
            instruments: BTreeMap::new(),
            retiring: BTreeMap::new(),
            next_trade_sequence: 1,
            book_sequences: BTreeMap::new(),
            delivered_trades: BTreeMap::new(),
            decode_failures: 0,
        }
    }

    /// Installs the current demand mapping. A coin no longer demanded on any
    /// channel has no canonical tape left to protect, so a later
    /// subscription's snapshot is published as new.
    fn replace_instruments(&mut self, instruments: BTreeMap<String, HyperliquidInstrumentDemand>) {
        self.delivered_trades
            .retain(|coin, _| instruments.contains_key(coin));
        self.instruments = instruments;
    }

    /// An unacknowledged unsubscribe stops excusing its frames after the same
    /// bound a subscription acknowledgement gets.
    fn expire_retiring(&mut self, now: Instant) {
        self.retiring
            .retain(|_, retired| now.saturating_duration_since(*retired) < CONNECT_TIMEOUT);
    }
}

fn run_session(
    socket: &mut MarketSocket,
    generation: u64,
    demand: &mut HyperliquidDemand,
    controls: &Receiver<HyperliquidRealtimeControl>,
    bound_events: (
        &SyncSender<Fenced<ProviderTag, HyperliquidRealtimeEvent>>,
        ProviderEventSink,
    ),
    stop: &Arc<AtomicBool>,
    wake: &ProviderCoordinatorWake,
) -> SessionExit {
    let sink = RealtimeEventSink {
        events: RealtimeEventChannel::Bound {
            events: bound_events.0,
            sink: bound_events.1,
        },
        stop,
        wake,
    };
    let started = Instant::now();
    let mut state = SessionState {
        pending_subscriptions: BTreeMap::new(),
        connected: false,
        active: BTreeMap::new(),
        feed: FeedState::new(),
        last_inbound: started,
        // Make the first qualified RTT probe due immediately. Subsequent
        // probes retain the normal interval from the actual send instant.
        last_ping: started.checked_sub(PING_INTERVAL).unwrap_or(started),
        pending_ping: None,
    };
    if reconcile_subscriptions(socket, demand, &mut state).is_err() {
        return SessionExit::Reconnect("subscription write failed".to_string());
    }
    loop {
        if stop.load(Ordering::Acquire) {
            socket.close();
            return SessionExit::Closed;
        }
        // Newest demand wins; Stop parks the socket until demand returns.
        #[cfg(feature = "probe")]
        let mut probe_failure = None;
        let (changed, parked) = drain_session_controls(
            controls,
            demand,
            #[cfg(feature = "probe")]
            &mut probe_failure,
        );
        #[cfg(feature = "probe")]
        if let Some(count) = probe_failure {
            socket.close();
            return SessionExit::ProbeFailure(count);
        }
        if parked {
            socket.close();
            return SessionExit::Parked;
        }
        if changed && reconcile_subscriptions(socket, demand, &mut state).is_err() {
            return SessionExit::Reconnect("subscription update write failed".to_string());
        }
        let now = Instant::now();
        if let Some(reason) = heartbeat(socket, &mut state, now) {
            return SessionExit::Reconnect(reason.to_string());
        }
        match socket.read_event(now + READ_TIMEOUT) {
            Ok(MarketSocketEvent::Text(text)) => {
                let received_at = Instant::now();
                state.last_inbound = received_at;
                match handle_frame(&text, generation, &mut state.feed, &sink) {
                    Ok(progress) => {
                        if let FrameProgress::Subscribed(key) = &progress {
                            state.pending_subscriptions.remove(key);
                            if !state.connected && state.pending_subscriptions.is_empty() {
                                if sink.send(HyperliquidRealtimeEvent::Connected(generation)) {
                                    return SessionExit::Closed;
                                }
                                state.connected = true;
                            }
                        }
                        if matches!(progress, FrameProgress::Pong) && {
                            let rtt = application_ping_rtt_nanos(&mut state, received_at);
                            state.connected
                                && sink.send(HyperliquidRealtimeEvent::Heartbeat(generation, rtt))
                        } {
                            return SessionExit::Closed;
                        }
                    }
                    Err(FrameOutcome::Closed) => {
                        return if wake.overflowed("hyperliquid", generation) {
                            SessionExit::Reconnect("local event queue overflow".into())
                        } else {
                            SessionExit::Closed
                        };
                    }
                    Err(FrameOutcome::Reconnect) => {
                        return SessionExit::Reconnect(
                            "frame validation or sequence failed".to_string(),
                        );
                    }
                }
            }
            Ok(MarketSocketEvent::Pong) => {
                state.last_inbound = Instant::now();
                if state.connected
                    && sink.send(HyperliquidRealtimeEvent::Heartbeat(generation, None))
                {
                    return SessionExit::Closed;
                }
            }
            Err(error) if error.is_read_timeout() => {}
            Err(error) => return SessionExit::Reconnect(error.to_string()),
        }
    }
}

fn drain_session_controls(
    controls: &Receiver<HyperliquidRealtimeControl>,
    demand: &mut HyperliquidDemand,
    #[cfg(feature = "probe")] probe_failure: &mut Option<u8>,
) -> (bool, bool) {
    let mut changed = false;
    let mut parked = false;
    while let Ok(control) = controls.try_recv() {
        match control {
            HyperliquidRealtimeControl::Subscribe(next) => {
                changed |= *demand != next;
                *demand = next;
                parked = false;
            }
            HyperliquidRealtimeControl::Connect(_) => {}
            HyperliquidRealtimeControl::Stop => parked = true,
            #[cfg(feature = "probe")]
            HyperliquidRealtimeControl::FailWebSocket {
                subsequent_connect_failures,
            } => *probe_failure = Some(subsequent_connect_failures),
        }
    }
    (changed, parked)
}

#[derive(Debug, PartialEq, Eq)]
enum FrameOutcome {
    Closed,
    Reconnect,
}

/// Sends a heartbeat ping when the feed has been quiet, and reports a dead
/// socket after sustained silence. Returns true when the session must end.
fn heartbeat(
    socket: &mut MarketSocket,
    state: &mut SessionState,
    now: Instant,
) -> Option<&'static str> {
    if let Some(reason) = session_deadline(state, now) {
        return Some(reason);
    }
    state.feed.expire_retiring(now);
    if now.saturating_duration_since(state.last_inbound) >= MESSAGE_SILENCE_TIMEOUT {
        return Some("inbound silence exceeded 45 seconds");
    }
    if application_ping_due(state, now) {
        if socket
            .send_text(&aeris_hyperliquid_market_adapter::build_ping())
            .is_err()
        {
            return Some("heartbeat write failed");
        }
        state.last_ping = now;
        state.pending_ping = Some(now);
    }
    None
}

fn session_deadline(state: &SessionState, now: Instant) -> Option<&'static str> {
    if state
        .pending_ping
        .is_some_and(|sent| now.saturating_duration_since(sent) >= PING_INTERVAL)
    {
        return Some("application heartbeat response timed out");
    }
    if state
        .pending_subscriptions
        .values()
        .any(|sent| now.saturating_duration_since(*sent) >= CONNECT_TIMEOUT)
    {
        return Some("subscription acknowledgement timed out");
    }
    None
}

fn application_ping_due(state: &SessionState, now: Instant) -> bool {
    now.saturating_duration_since(state.last_ping) >= PING_INTERVAL && state.pending_ping.is_none()
}

fn application_ping_rtt_nanos(state: &mut SessionState, received_at: Instant) -> Option<u64> {
    let sent_at = state.pending_ping.take()?;
    let nanos = received_at.saturating_duration_since(sent_at).as_nanos();
    Some(u64::try_from(nanos).unwrap_or(u64::MAX).max(1))
}

/// Records one desired subscription frame, keeping the first coin mapping.
/// A frame that fails to build (blank coin) is dropped: the coordinator only
/// sends validated demand, so this is defense in depth, not a silent skip.
fn insert_subscription(
    desired: &mut BTreeMap<SubscriptionKey, String>,
    key: SubscriptionKey,
    frame: Result<String, String>,
) {
    if let std::collections::btree_map::Entry::Vacant(entry) = desired.entry(key)
        && let Ok(frame) = frame
    {
        entry.insert(frame);
    }
}

/// Diffs the demanded set against live subscriptions without reconnecting.
fn reconcile_subscriptions(
    socket: &mut MarketSocket,
    demand: &HyperliquidDemand,
    state: &mut SessionState,
) -> Result<(), String> {
    let mut desired = BTreeMap::new();
    let mut instruments = BTreeMap::new();
    for candle in demand
        .candles
        .iter()
        .take(MAXIMUM_SUBSCRIPTION_FRAMES_PER_DEMAND)
    {
        insert_subscription(
            &mut desired,
            SubscriptionKey::Candle {
                coin: candle.instrument.wire_coin.clone(),
                interval: candle.interval.clone(),
            },
            aeris_hyperliquid_market_adapter::build_candle_subscription(
                &candle.instrument.wire_coin,
                &candle.interval,
            ),
        );
        instruments.insert(
            candle.instrument.wire_coin.clone(),
            candle.instrument.clone(),
        );
    }
    for trade in demand
        .trades
        .iter()
        .take(MAXIMUM_SUBSCRIPTION_FRAMES_PER_DEMAND)
    {
        insert_subscription(
            &mut desired,
            SubscriptionKey::Trades {
                coin: trade.wire_coin.clone(),
            },
            aeris_hyperliquid_market_adapter::build_trades_subscription(&trade.wire_coin),
        );
        instruments.insert(trade.wire_coin.clone(), trade.clone());
    }
    for quote in demand
        .quotes
        .iter()
        .take(MAXIMUM_SUBSCRIPTION_FRAMES_PER_DEMAND)
    {
        insert_subscription(
            &mut desired,
            SubscriptionKey::Bbo {
                coin: quote.wire_coin.clone(),
            },
            aeris_hyperliquid_market_adapter::build_bbo_subscription(&quote.wire_coin),
        );
        instruments.insert(quote.wire_coin.clone(), quote.clone());
    }
    for book in demand
        .books
        .iter()
        .take(MAXIMUM_SUBSCRIPTION_FRAMES_PER_DEMAND)
    {
        insert_subscription(
            &mut desired,
            SubscriptionKey::Book {
                coin: book.wire_coin.clone(),
            },
            aeris_hyperliquid_market_adapter::build_l2_subscription(&book.wire_coin),
        );
        instruments.insert(book.wire_coin.clone(), book.clone());
    }
    remove_retired_subscriptions(socket, &desired, state)?;
    retain_book_sequences(&mut state.feed.book_sequences, &desired);
    for (key, frame) in &desired {
        if !state.active.contains_key(key) {
            socket.send_text(frame).map_err(|error| error.to_string())?;
            state.active.insert(key.clone(), frame.clone());
            state.feed.retiring.remove(key);
            state
                .pending_subscriptions
                .insert(key.clone(), Instant::now());
        }
    }
    state.feed.replace_instruments(instruments);
    Ok(())
}

fn retain_book_sequences(
    sequences: &mut BTreeMap<String, u64>,
    desired: &BTreeMap<SubscriptionKey, String>,
) {
    sequences.retain(|coin, _| {
        desired.keys().any(|key| match key {
            SubscriptionKey::Bbo { coin: active } | SubscriptionKey::Book { coin: active } => {
                active == coin
            }
            _ => false,
        })
    });
}

fn remove_retired_subscriptions(
    socket: &mut MarketSocket,
    desired: &BTreeMap<SubscriptionKey, String>,
    state: &mut SessionState,
) -> Result<(), String> {
    // Unsubscribe first so a replaced feed never double-delivers.
    let removed = state
        .active
        .keys()
        .filter(|key| !desired.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    for key in removed {
        state.pending_subscriptions.remove(&key);
        if let Some(frame) = state.active.remove(&key)
            && let Ok(raw) = serde_json::from_str::<serde_json::Value>(&frame)
            && let Some(subscription) = raw.get("subscription")
        {
            socket
                .send_text(&aeris_hyperliquid_market_adapter::build_unsubscribe(
                    subscription,
                ))
                .map_err(|error| error.to_string())?;
            state.feed.retiring.insert(key, Instant::now());
        }
    }
    Ok(())
}

enum FrameProgress {
    Pong,
    Subscribed(SubscriptionKey),
    Other,
}

enum Acknowledgement {
    Subscribed(SubscriptionKey),
    Unsubscribed(SubscriptionKey),
}

fn acknowledged_subscription(raw: &str) -> Result<Acknowledgement, String> {
    let data: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| "invalid subscription response".to_string())?;
    let subscribed = match data.get("method").and_then(serde_json::Value::as_str) {
        Some("subscribe") => true,
        Some("unsubscribe") => false,
        _ => return Err("subscription response omitted method".into()),
    };
    let subscription = data
        .get("subscription")
        .ok_or("subscription response omitted identity")?;
    let coin = subscription
        .get("coin")
        .and_then(serde_json::Value::as_str)
        .filter(|coin| !coin.is_empty())
        .ok_or("subscription response omitted coin")?
        .to_string();
    let key = match subscription.get("type").and_then(serde_json::Value::as_str) {
        Some("candle") => SubscriptionKey::Candle {
            coin,
            interval: subscription
                .get("interval")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or("subscription response omitted interval")?
                .to_string(),
        },
        Some("trades") => SubscriptionKey::Trades { coin },
        Some("bbo") => SubscriptionKey::Bbo { coin },
        Some("l2Book") => SubscriptionKey::Book { coin },
        _ => return Err("unexpected subscription response type".into()),
    };
    Ok(if subscribed {
        Acknowledgement::Subscribed(key)
    } else {
        Acknowledgement::Unsubscribed(key)
    })
}

fn handle_frame(
    text: &str,
    generation: u64,
    feed: &mut FeedState,
    sink: &RealtimeEventSink<'_>,
) -> Result<FrameProgress, FrameOutcome> {
    let event = match parse_ws_frame(text) {
        Ok(event) => event,
        Err(reason) => {
            return record_frame_failure(&mut feed.decode_failures, &reason)
                .map(|()| FrameProgress::Other);
        }
    };
    if matches!(&event, WsClientEvent::Pong) {
        return Ok(FrameProgress::Pong);
    }
    if let WsClientEvent::Subscribed { channel } = &event {
        return match acknowledged_subscription(channel) {
            Ok(Acknowledgement::Subscribed(key)) => Ok(FrameProgress::Subscribed(key)),
            Ok(Acknowledgement::Unsubscribed(key)) => {
                feed.retiring.remove(&key);
                Ok(FrameProgress::Other)
            }
            Err(reason) => record_frame_failure(&mut feed.decode_failures, &reason)
                .map(|()| FrameProgress::Other),
        };
    }
    let mut frame = FrameDecoder {
        generation,
        feed: &mut *feed,
        sink,
    };
    let outcome = match event {
        WsClientEvent::Candle {
            coin,
            interval,
            candles,
        } => frame.candle(&coin, &interval, &candles),
        WsClientEvent::Trades { coin, trades } => frame.trades(&coin, &trades),
        WsClientEvent::Book { coin, book } => frame.book(&coin, &book),
        WsClientEvent::Bbo { coin, bbo } => frame.bbo(&coin, &bbo),
        // Control acknowledgements, context, and mids have no demanding
        // consumer in this milestone and are dropped rather than misrouted.
        WsClientEvent::Subscribed { .. }
        | WsClientEvent::Context { .. }
        | WsClientEvent::AllMids { .. } => Ok(()),
        WsClientEvent::Pong => unreachable!("application pong handled above"),
    };
    match outcome {
        // The failure budget is per connection, never per streak: every
        // malformed frame counts whether or not healthy frames interleave,
        // or a partially wedged feed (one corrupt channel beside healthy
        // ones) would never trip the reconnect threshold. Benign control
        // traffic neither counts nor forgives; only a new connection resets.
        Ok(()) => Ok(FrameProgress::Other),
        Err(FrameError::Abort(outcome)) => Err(outcome),
        Err(FrameError::Malformed(reason)) => {
            record_frame_failure(&mut feed.decode_failures, &reason).map(|()| FrameProgress::Other)
        }
    }
}

/// Counts one malformed frame; too many in a row means the connection itself
/// is suspect, so the session reconnects instead of spamming the log.
fn record_frame_failure(decode_failures: &mut u32, reason: &str) -> Result<(), FrameOutcome> {
    let reason = if reason.starts_with("hyperliquid channel is unsupported:") {
        "hyperliquid channel is unsupported"
    } else {
        reason
    };
    note_decode_failure(decode_failures, reason);
    if *decode_failures >= MAXIMUM_DECODE_FAILURES_PER_CONNECTION {
        return Err(FrameOutcome::Reconnect);
    }
    Ok(())
}

struct FrameDecoder<'a> {
    generation: u64,
    feed: &'a mut FeedState,
    sink: &'a RealtimeEventSink<'a>,
}

/// How one decoded frame finished.
///
/// Malformed remote payloads count toward the reconnect threshold; local
/// terminal conditions (`Closed`, sequence-overflow `Reconnect`) pass through
/// without counting, since the connection itself is not suspect.
enum FrameError {
    Malformed(String),
    Abort(FrameOutcome),
}

impl FrameDecoder<'_> {
    /// Drops data for a coin with no demand mapping. Frames still in flight
    /// for a feed this connection is retiring are expected and drop quietly;
    /// anything else is counted. Neither may enter another instrument's state.
    fn undemanded(&self, key: &SubscriptionKey, reason: &str) -> Result<(), FrameError> {
        if self.feed.retiring.contains_key(key) {
            Ok(())
        } else {
            Err(FrameError::Malformed(reason.to_string()))
        }
    }

    fn bbo(&mut self, coin: &str, bbo: &serde_json::value::RawValue) -> Result<(), FrameError> {
        let Some(mapping) = self.feed.instruments.get(coin) else {
            return self.undemanded(
                &SubscriptionKey::Bbo {
                    coin: coin.to_string(),
                },
                "bbo coin is not demanded",
            );
        };
        let sequence = self.feed.book_sequences.get(coin).copied().unwrap_or(1);
        let quote = aeris_hyperliquid_market_adapter::decode_bbo_quote(
            bbo,
            coin,
            &mapping.instrument_id,
            &mapping.entitlement_id,
            self.generation,
            sequence,
            unix_nanos_now(),
        )
        .map_err(|reason| FrameError::Malformed(format!("bbo: {reason}")))?;
        let next = sequence
            .checked_add(1)
            .ok_or(FrameError::Abort(FrameOutcome::Reconnect))?;
        self.feed.book_sequences.insert(coin.to_string(), next);
        if self
            .sink
            .send(HyperliquidRealtimeEvent::Quote(self.generation, quote))
        {
            return Err(FrameError::Abort(FrameOutcome::Closed));
        }
        Ok(())
    }

    fn candle(
        &mut self,
        coin: &str,
        interval: &str,
        candles: &serde_json::value::RawValue,
    ) -> Result<(), FrameError> {
        // The live channel emits one `Candle` object per frame; the decoder
        // rejects anything else without touching the socket.
        let candle = decode_live_candle(
            candles,
            price_scale_for(coin, &self.feed.instruments),
            quantity_scale_for(coin, &self.feed.instruments),
        )
        .map_err(|reason| FrameError::Malformed(format!("candle: {reason}")))?;
        if self.sink.send(HyperliquidRealtimeEvent::Candle(
            self.generation,
            coin.to_string(),
            interval.to_string(),
            candle,
        )) {
            return Err(FrameError::Abort(FrameOutcome::Closed));
        }
        Ok(())
    }

    fn trades(
        &mut self,
        coin: &str,
        trades: &serde_json::value::RawValue,
    ) -> Result<(), FrameError> {
        let Some(mapping) = self.feed.instruments.get(coin) else {
            return self.undemanded(
                &SubscriptionKey::Trades {
                    coin: coin.to_string(),
                },
                "trade coin is not demanded",
            );
        };
        let first = self.feed.next_trade_sequence;
        let mut batch = decode_trades_batch(
            trades,
            coin,
            &mapping.instrument_id,
            &mapping.entitlement_id,
            self.generation,
            unix_nanos_now(),
            first,
        )
        .map_err(|reason| FrameError::Malformed(format!("trades: {reason}")))?;
        let advance = u64::try_from(batch.trades.len()).unwrap_or(u64::MAX);
        match first.checked_add(advance) {
            Some(next) => self.feed.next_trade_sequence = next.max(1),
            None => return Err(FrameError::Abort(FrameOutcome::Reconnect)),
        }
        // A resubscription snapshot replays trades this connection already
        // published; the replays keep the local sequences they were decoded
        // with, so published sequences only ever increase.
        self.feed
            .delivered_trades
            .entry(coin.to_string())
            .or_default()
            .retain_undelivered(&mut batch.trades);
        if !batch.trades.is_empty()
            && self.sink.send(HyperliquidRealtimeEvent::Trades(
                self.generation,
                batch.trades,
            ))
        {
            return Err(FrameError::Abort(FrameOutcome::Closed));
        }
        Ok(())
    }

    fn book(&mut self, coin: &str, book: &serde_json::value::RawValue) -> Result<(), FrameError> {
        let Some(mapping) = self.feed.instruments.get(coin) else {
            return self.undemanded(
                &SubscriptionKey::Book {
                    coin: coin.to_string(),
                },
                "book coin is not demanded",
            );
        };
        let sequence = self.feed.book_sequences.get(coin).copied().unwrap_or(1);
        let decoded = decode_book_snapshot(
            book,
            coin,
            &mapping.instrument_id,
            &mapping.entitlement_id,
            self.generation,
            sequence,
            unix_nanos_now(),
        )
        .map_err(|reason| FrameError::Malformed(format!("book: {reason}")))?;
        let next = sequence
            .checked_add(1)
            .ok_or(FrameError::Abort(FrameOutcome::Reconnect))?;
        self.feed.book_sequences.insert(coin.to_string(), next);
        if self.sink.send(HyperliquidRealtimeEvent::Depth(
            self.generation,
            decoded.snapshot,
        )) {
            return Err(FrameError::Abort(FrameOutcome::Closed));
        }
        Ok(())
    }
}

fn price_scale_for(coin: &str, instruments: &BTreeMap<String, HyperliquidInstrumentDemand>) -> u32 {
    instruments
        .get(coin)
        .map_or(8, |mapping| u32::from(mapping.price_scale))
}

fn quantity_scale_for(
    coin: &str,
    instruments: &BTreeMap<String, HyperliquidInstrumentDemand>,
) -> u32 {
    instruments
        .get(coin)
        .map_or(8, |mapping| u32::from(mapping.quantity_scale))
}

fn note_decode_failure(decode_failures: &mut u32, reason: &str) {
    *decode_failures = decode_failures.saturating_add(1);
    if *decode_failures == 1 || (*decode_failures).is_multiple_of(10) {
        diagnostic!(
            "Aeris engine Hyperliquid feed dropped malformed data: {reason} ({} this connection)",
            *decode_failures,
        );
    }
}

fn unix_nanos_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_platform_runtime::{LiveMarketGateOutcome, LiveMarketGateRecorder};

    fn demand() -> BTreeMap<String, HyperliquidInstrumentDemand> {
        BTreeMap::from([(
            "BTC".to_string(),
            HyperliquidInstrumentDemand {
                wire_coin: "BTC".to_string(),
                instrument_id: "instrument:hyperliquid:BTC".to_string(),
                entitlement_id: "hyperliquid-public".to_string(),
                price_scale: 8,
                quantity_scale: 8,
            },
        )])
    }

    /// Feed state of a fresh connection demanding BTC.
    fn feed() -> FeedState {
        let mut feed = FeedState::new();
        feed.instruments = demand();
        feed
    }

    /// One healthy live candle for the demanded coin.
    fn good_candle() -> String {
        r#"{"channel":"candle","data":{"t":60000,"T":120000,"s":"BTC","i":"1m","o":"10.0","h":"11.0","l":"9.0","c":"10.5","v":"7.0","n":3}}"#.to_string()
    }

    /// Valid envelope carrying data for a coin with no demand mapping: the
    /// parse succeeds, so only payload-level accounting catches it.
    fn unmapped_trades() -> String {
        r#"{"channel":"trades","data":[{"coin":"ETH"}]}"#.to_string()
    }

    /// Benign control traffic: healthy for the connection, but evidence of
    /// nothing about payload decoders.
    fn subscribed_ack() -> String {
        r#"{"channel":"subscriptionResponse","data":{"method":"subscribe","subscription":{"type":"candle","coin":"BTC","interval":"1m"}}}"#.to_string()
    }

    fn handle(
        text: &str,
        feed: &mut FeedState,
        events: &SyncSender<HyperliquidRealtimeEvent>,
    ) -> Result<(), FrameOutcome> {
        let stop = AtomicBool::new(false);
        let wake = ProviderCoordinatorWake::for_tests();
        let sink = RealtimeEventSink {
            events: RealtimeEventChannel::Raw(events),
            stop: &stop,
            wake: &wake,
        };
        handle_frame(text, 1, feed, &sink).map(|_| ())
    }

    fn published_trade_ids(received: &Receiver<HyperliquidRealtimeEvent>) -> Vec<String> {
        received
            .try_iter()
            .filter_map(|event| match event {
                HyperliquidRealtimeEvent::Trades(_, trades) => Some(trades),
                _ => None,
            })
            .flatten()
            .map(|trade| trade.trade_id)
            .collect()
    }

    fn one_trade(coin: &str, time: i64, tid: u64) -> String {
        format!(
            r#"{{"channel":"trades","data":[{{"coin":"{coin}","px":"10","sz":"1","side":"B","time":{time},"tid":{tid},"hash":"0xabc","users":["0xbuyer","0xseller"]}}]}}"#
        )
    }

    #[test]
    fn lifecycle_emission_retries_until_the_coordinator_drains() {
        let (events, received) = std::sync::mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let wake = ProviderCoordinatorWake::for_tests();
        let sink = ProviderEventSink {
            epoch: aeris_market_engine::fence::EpochSequence::<ProviderTag>::default().next(),
        };
        // Fill the queue so the lifecycle emission must retry under
        // backpressure instead of dropping the epoch-opening event.
        events
            .send(sink.stamp(HyperliquidRealtimeEvent::Connected(7)))
            .expect("filler is queued");
        let (delivered, delivered_received) = std::sync::mpsc::sync_channel(1);
        let emitter_stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let emitted = !emit_lifecycle(
                &events,
                HyperliquidRealtimeEvent::Connecting(9),
                &emitter_stop,
                &wake,
                sink,
            );
            delivered.send(emitted).expect("emission result reported");
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            delivered_received.try_recv().is_err(),
            "emission must wait for queue space"
        );
        received
            .try_recv()
            .expect("filler is still queued while backpressured");
        assert!(
            delivered_received
                .recv_timeout(Duration::from_secs(2))
                .expect("bounded retry delivers once the queue drains"),
            "connecting event is delivered, not dropped"
        );
        match received.try_recv() {
            Ok(fenced) => assert!(matches!(
                fenced.value,
                HyperliquidRealtimeEvent::Connecting(9)
            )),
            Err(error) => panic!("connecting event missing after delivery: {error:?}"),
        }
    }

    #[test]
    fn lifecycle_emission_ends_when_the_coordinator_channel_is_gone() {
        let (events, received) = std::sync::mpsc::sync_channel(1);
        drop(received);
        let stop = Arc::new(AtomicBool::new(false));
        let wake = ProviderCoordinatorWake::for_tests();
        let sink = ProviderEventSink {
            epoch: aeris_market_engine::fence::EpochSequence::<ProviderTag>::default().next(),
        };
        assert!(emit_lifecycle(
            &events,
            HyperliquidRealtimeEvent::Connecting(1),
            &stop,
            &wake,
            sink,
        ));
    }

    #[test]
    fn full_live_queue_reports_local_overflow_without_waiting() {
        let (events, _received) = std::sync::mpsc::sync_channel(1);
        events.send(HyperliquidRealtimeEvent::Connected(1)).unwrap();
        let stop = AtomicBool::new(false);
        let wake = ProviderCoordinatorWake::for_tests();
        let sink = RealtimeEventSink {
            events: RealtimeEventChannel::Raw(&events),
            stop: &stop,
            wake: &wake,
        };
        assert!(sink.send(HyperliquidRealtimeEvent::Heartbeat(1, None)));
        assert!(wake.overflowed("hyperliquid", 1));
        assert!(
            !wake.overflowed("hyperliquid", 2),
            "new generation can recover"
        );
    }

    #[test]
    fn subscription_acknowledgement_matches_exact_stream_identity() {
        assert!(
            matches!(acknowledged_subscription(r#"{"method":"subscribe","subscription":{"type":"candle","coin":"BTC","interval":"1m"}}"#),
            Ok(Acknowledgement::Subscribed(SubscriptionKey::Candle { coin, interval })) if coin == "BTC" && interval == "1m")
        );
        assert!(acknowledged_subscription(r#"{"method":"subscribe"}"#).is_err());
        // Observed shape: the unsubscribe acknowledgement echoes the request.
        assert!(
            matches!(acknowledged_subscription(r#"{"method":"unsubscribe","subscription":{"type":"l2Book","coin":"BTC","nSigFigs":null,"mantissa":null,"fast":false}}"#),
            Ok(Acknowledgement::Unsubscribed(SubscriptionKey::Book { coin })) if coin == "BTC")
        );
        assert!(acknowledged_subscription(r#"{"method":"unsubscribe"}"#).is_err());
    }

    #[test]
    fn healthy_other_traffic_does_not_hide_pending_pong_or_subscription_deadlines() {
        let now = Instant::now();
        let mut state = SessionState {
            active: BTreeMap::new(),
            pending_subscriptions: BTreeMap::new(),
            connected: true,
            feed: FeedState::new(),
            last_inbound: now,
            last_ping: now,
            pending_ping: Some(now.checked_sub(PING_INTERVAL).unwrap()),
        };
        assert_eq!(
            session_deadline(&state, now),
            Some("application heartbeat response timed out")
        );
        state.pending_ping = None;
        state.pending_subscriptions.insert(
            SubscriptionKey::Trades { coin: "BTC".into() },
            now.checked_sub(CONNECT_TIMEOUT).unwrap(),
        );
        assert_eq!(
            session_deadline(&state, now),
            Some("subscription acknowledgement timed out")
        );
        state.pending_subscriptions.clear();
        assert_eq!(
            session_deadline(&state, now),
            None,
            "acknowledged quiet market is not a broken subscription"
        );
    }

    #[test]
    fn bbo_frames_enter_the_canonical_quote_path() {
        let mut feed = feed();
        let (events, received) = std::sync::mpsc::sync_channel(8);
        handle(
            r#"{"channel":"bbo","data":{"coin":"BTC","time":1700000000000,"bbo":[{"px":"10.0","sz":"2.0","n":1},{"px":"10.5","sz":"3.0","n":2}]}}"#,
            &mut feed,
            &events,
        )
        .expect("bbo frame accepted");

        let quote = received
            .try_iter()
            .find_map(|event| match event {
                HyperliquidRealtimeEvent::Quote(_, quote) => Some(quote),
                _ => None,
            })
            .expect("bbo publishes a quote event");
        assert_eq!(quote.metadata.provider_id, "hyperliquid");
        assert_eq!(quote.metadata.instrument_id, "instrument:hyperliquid:BTC");
        assert_eq!(
            quote.metadata.timestamps.exchange_unix_nanos,
            Some(1_700_000_000_000_000_000)
        );
        assert_eq!(quote.bid.map(|level| level.price), Some(1_000_000_000));
        assert_eq!(quote.ask.map(|level| level.price), Some(1_050_000_000));
    }

    #[test]
    fn bbo_sequence_survives_reconciliation_without_full_depth() {
        let mut sequences = BTreeMap::from([("BTC".to_string(), 101), ("ETH".to_string(), 15)]);
        let desired = BTreeMap::from([
            (SubscriptionKey::Bbo { coin: "BTC".into() }, String::new()),
            (
                SubscriptionKey::Candle {
                    coin: "BTC".into(),
                    interval: "1m".into(),
                },
                String::new(),
            ),
            (
                SubscriptionKey::Trades { coin: "ETH".into() },
                String::new(),
            ),
        ]);
        retain_book_sequences(&mut sequences, &desired);
        assert_eq!(sequences, BTreeMap::from([("BTC".to_string(), 101)]));

        let mut feed = feed();
        feed.book_sequences = sequences;
        let (events, received) = std::sync::mpsc::sync_channel(8);
        handle(
            r#"{"channel":"bbo","data":{"coin":"BTC","time":1700000000000,"bbo":[{"px":"10.0","sz":"2.0","n":1},{"px":"10.5","sz":"3.0","n":2}]}}"#,
            &mut feed,
            &events,
        )
        .expect("bbo remains valid after reconciliation");
        let HyperliquidRealtimeEvent::Quote(_, quote) = received.try_recv().expect("quote event")
        else {
            panic!("expected quote event");
        };
        assert_eq!(quote.metadata.source_sequence, 101);
        assert_eq!(feed.book_sequences.get("BTC"), Some(&102));
    }

    #[test]
    fn book_ingestion_sequence_exhaustion_reconnects_without_publishing_duplicates() {
        let mut feed = feed();
        let (events, received) = std::sync::mpsc::sync_channel(8);
        for frame in [
            r#"{"channel":"bbo","data":{"coin":"BTC","time":1,"bbo":[null,null]}}"#,
            r#"{"channel":"l2Book","data":{"coin":"BTC","time":1,"levels":[[],[]]}}"#,
        ] {
            feed.book_sequences.insert("BTC".to_string(), u64::MAX);
            assert!(matches!(
                handle(frame, &mut feed, &events),
                Err(FrameOutcome::Reconnect)
            ));
            assert_eq!(
                feed.decode_failures, 0,
                "local exhaustion is not malformed provider data"
            );
            assert!(received.try_recv().is_err());
        }
    }

    #[test]
    fn payload_failures_count_across_healthy_frames() {
        let mut feed = feed();
        let (events, received) = std::sync::mpsc::sync_channel(1_024);
        // Just under the threshold of payload failures keeps the session up,
        // even with a healthy candle between every two failures: interleaved
        // success must not forgive sustained payload corruption.
        for _ in 0..MAXIMUM_DECODE_FAILURES_PER_CONNECTION - 1 {
            handle(&unmapped_trades(), &mut feed, &events).expect("under threshold stays up");
            handle(&good_candle(), &mut feed, &events).expect("healthy frame handled");
        }
        // The next payload failure trips the threshold despite the healthy
        // traffic around it: the budget is per connection, not per streak.
        assert!(matches!(
            handle(&unmapped_trades(), &mut feed, &events),
            Err(FrameOutcome::Reconnect)
        ));
        assert_eq!(
            received.try_iter().count(),
            (MAXIMUM_DECODE_FAILURES_PER_CONNECTION - 1) as usize
        );
    }

    #[test]
    fn catalog_search_answers_only_the_requested_exchange() {
        let mut catalog = CatalogState::default();
        catalog.replace(
            aeris_hyperliquid_market_adapter::decode_catalog(
                &aeris_hyperliquid_market_adapter::RawMetaBundle {
                    core_perp_meta: serde_json::json!({"universe": [
                        {"name": "BTC", "szDecimals": 5},
                    ]}),
                    spot_meta: serde_json::json!({
                        "universe": [],
                        "tokens": [{"name": "USDC", "szDecimals": 8, "index": 0}],
                    }),
                    perp_dexs: serde_json::json!(["xyz", null]),
                    builder_metas: vec![(
                        "xyz".to_string(),
                        serde_json::json!({"universe": [{"name": "BTC", "szDecimals": 5}]}),
                    )],
                },
            )
            .expect("catalog"),
        );
        let (sender, published) = std::sync::mpsc::sync_channel(2);
        let events =
            CatalogPublisher::new(sender, "hyperliquid", ProviderCoordinatorWake::for_tests());
        let stop = Arc::new(AtomicBool::new(false));
        let search = |exchange: Option<&str>| SearchProviderInstruments {
            consumer_id: 41,
            search_generation: 1,
            startup_resolve: false,
            provider: "hyperliquid".into(),
            query: "BTC".into(),
            maximum_results: 16,
            categories: crate::InstrumentSearchCategories::ALL,
            exchange: exchange.map(str::to_string),
        };
        let exchanges = |event| match event {
            HyperliquidCatalogEvent::SearchCompleted(result) => result
                .instruments
                .into_iter()
                .map(|instrument| instrument.exchange)
                .collect::<Vec<_>>(),
            _ => panic!("expected a search result"),
        };

        for exchange in [None, Some("xyz")] {
            assert!(!handle_catalog_search(
                search(exchange),
                &mut catalog,
                &events,
                &Arc::new(AtomicU64::new(1)),
                &stop,
                HyperliquidHttpConfig::default(),
            ));
        }

        let mut every = exchanges(published.recv().expect("unfiltered result"));
        every.sort();
        assert_eq!(every, ["Hyperliquid", "xyz"]);
        assert_eq!(
            exchanges(published.recv().expect("filtered result")),
            ["xyz"]
        );
    }

    #[test]
    fn cold_catalog_waits_instead_of_spinning() {
        let now = Instant::now();
        let mut catalog = CatalogState::default();
        assert_eq!(catalog_wait(&catalog, now, now), CATALOG_POLL_INTERVAL);
        catalog.replace(HyperliquidCatalog::default());
        assert_eq!(catalog_wait(&catalog, now, now), Duration::ZERO);
        assert_eq!(
            catalog_wait(&catalog, now + CATALOG_REFRESH_INTERVAL, now),
            CATALOG_POLL_INTERVAL
        );
    }

    #[test]
    fn application_ping_rtt_is_monotonic_one_shot_and_not_feed_age() {
        let started = Instant::now();
        let mut state = SessionState {
            pending_subscriptions: BTreeMap::new(),
            connected: false,
            active: BTreeMap::new(),
            feed: FeedState::new(),
            last_inbound: started + Duration::from_secs(19),
            last_ping: started,
            pending_ping: None,
        };
        let due = started + PING_INTERVAL;
        assert!(application_ping_due(&state, due));
        state.pending_ping = Some(due);
        assert!(!application_ping_due(&state, due + PING_INTERVAL));
        assert_eq!(
            application_ping_rtt_nanos(&mut state, due + Duration::from_millis(12)),
            Some(12_000_000)
        );
        assert_eq!(application_ping_rtt_nanos(&mut state, due), None);
    }

    #[test]
    fn first_application_ping_is_due_immediately_for_live_rtt() {
        let started = Instant::now();
        let state = SessionState {
            pending_subscriptions: BTreeMap::new(),
            connected: false,
            active: BTreeMap::new(),
            feed: FeedState::new(),
            last_inbound: started,
            last_ping: started.checked_sub(PING_INTERVAL).unwrap_or(started),
            pending_ping: None,
        };

        assert!(application_ping_due(&state, started));
    }

    #[test]
    fn trade_source_ordinal_does_not_regress_across_symbol_switch_back() {
        let instrument = |coin: &str| {
            BTreeMap::from([(
                coin.to_string(),
                HyperliquidInstrumentDemand {
                    wire_coin: coin.to_string(),
                    instrument_id: format!("instrument:hyperliquid:{coin}"),
                    entitlement_id: "hyperliquid-public".to_string(),
                    price_scale: 8,
                    quantity_scale: 8,
                },
            )])
        };
        let (events, received) = std::sync::mpsc::sync_channel(8);
        let mut feed = FeedState::new();

        for (coin, time, tid) in [
            ("BTC", 1_700_000_000_000_i64, 1_u64),
            ("ETH", 1_700_000_000_001_i64, 2_u64),
            ("BTC", 1_700_000_000_002_i64, 3_u64),
        ] {
            feed.instruments = instrument(coin);
            handle(&one_trade(coin, time, tid), &mut feed, &events).expect("trade frame accepted");
        }

        let sequences = received
            .try_iter()
            .filter_map(|event| match event {
                HyperliquidRealtimeEvent::Trades(_, trades) => Some(trades),
                _ => None,
            })
            .flatten()
            .map(|trade| trade.metadata.source_sequence)
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![1, 2, 3]);
        assert_eq!(feed.next_trade_sequence, 4);
    }

    /// The observed subscription response: acknowledgement, then a snapshot
    /// of recent trades as an ordinary `trades` frame.
    fn trades_frame(trades: &[(i64, u64)]) -> String {
        let rows = trades
            .iter()
            .map(|(time, tid)| {
                format!(
                    r#"{{"coin":"BTC","px":"10","sz":"1","side":"B","time":{time},"tid":{tid},"hash":"0xabc","users":["0xbuyer","0xseller"]}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(r#"{{"channel":"trades","data":[{rows}]}}"#)
    }

    #[test]
    fn resubscription_snapshot_does_not_republish_delivered_trades() {
        let mut feed = feed();
        let (events, received) = std::sync::mpsc::sync_channel(16);
        handle(
            &trades_frame(&[(1_000, 11), (1_000, 12), (2_000, 13)]),
            &mut feed,
            &events,
        )
        .expect("first subscription snapshot");
        handle(&trades_frame(&[(3_000, 14)]), &mut feed, &events).expect("live trade");
        assert_eq!(published_trade_ids(&received).len(), 4);

        // Unsubscribed and subscribed again on the same connection while BTC
        // stayed demanded on another channel: the new snapshot repeats
        // delivered trades beside ones not yet delivered.
        feed.replace_instruments(demand());
        handle(
            &trades_frame(&[(1_000, 12), (2_000, 13), (3_000, 14), (4_000, 15)]),
            &mut feed,
            &events,
        )
        .expect("resubscription snapshot");
        let replay = received
            .try_iter()
            .filter_map(|event| match event {
                HyperliquidRealtimeEvent::Trades(_, trades) => Some(trades),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(
            replay
                .iter()
                .map(|trade| trade.trade_id.as_str())
                .collect::<Vec<_>>(),
            vec!["hl:BTC:4000:15"],
            "only the undelivered trade is published"
        );
        assert!(
            replay[0].metadata.source_sequence > 4,
            "ingestion order keeps increasing past the dropped replays"
        );
        assert_eq!(feed.decode_failures, 0, "a replay is not malformed data");
    }

    #[test]
    fn delivered_trades_reset_when_the_coin_leaves_demand_or_the_connection() {
        let mut feed = feed();
        let (events, received) = std::sync::mpsc::sync_channel(16);
        handle(&trades_frame(&[(1_000, 11)]), &mut feed, &events).expect("trade");
        assert_eq!(published_trade_ids(&received).len(), 1);

        // Nothing demands BTC any more, so its canonical tape is gone and a
        // later subscription's snapshot starts a fresh one.
        feed.replace_instruments(BTreeMap::new());
        feed.replace_instruments(demand());
        handle(&trades_frame(&[(1_000, 11)]), &mut feed, &events).expect("snapshot");
        assert_eq!(published_trade_ids(&received), vec!["hl:BTC:1000:11"]);

        let mut reconnected = self::feed();
        handle(&trades_frame(&[(1_000, 11)]), &mut reconnected, &events).expect("snapshot");
        assert_eq!(
            published_trade_ids(&received),
            vec!["hl:BTC:1000:11"],
            "a new connection publishes into a new session generation"
        );
    }

    #[test]
    fn in_flight_frames_for_a_retiring_feed_are_dropped_without_counting() {
        let mut feed = feed();
        feed.instruments.clear();
        let retired_at = Instant::now();
        for key in [
            SubscriptionKey::Bbo { coin: "BTC".into() },
            SubscriptionKey::Trades { coin: "BTC".into() },
            SubscriptionKey::Book { coin: "BTC".into() },
        ] {
            feed.retiring.insert(key, retired_at);
        }
        let (events, received) = std::sync::mpsc::sync_channel(16);
        for frame in [
            r#"{"channel":"bbo","data":{"coin":"BTC","time":1,"bbo":[null,null]}}"#.to_string(),
            one_trade("BTC", 1_000, 11),
            r#"{"channel":"l2Book","data":{"coin":"BTC","time":1,"levels":[[],[]]}}"#.to_string(),
        ] {
            handle(&frame, &mut feed, &events).expect("in-flight frame handled");
        }
        assert_eq!(
            feed.decode_failures, 0,
            "in-flight frames are not malformed"
        );
        assert!(received.try_recv().is_err(), "retired data is never routed");

        // The acknowledgement ends the in-flight window for that feed only.
        handle(
            r#"{"channel":"subscriptionResponse","data":{"method":"unsubscribe","subscription":{"type":"bbo","coin":"BTC"}}}"#,
            &mut feed,
            &events,
        )
        .expect("unsubscribe acknowledgement");
        handle(
            r#"{"channel":"bbo","data":{"coin":"BTC","time":2,"bbo":[null,null]}}"#,
            &mut feed,
            &events,
        )
        .expect("counted, under threshold");
        assert_eq!(
            feed.decode_failures, 1,
            "data after the acknowledgement counts"
        );
        handle(&unmapped_trades(), &mut feed, &events).expect("counted, under threshold");
        assert_eq!(
            feed.decode_failures, 2,
            "a never-subscribed coin still counts"
        );
        assert!(received.try_recv().is_err());

        // Without an acknowledgement the excuse lapses after a bounded wait.
        feed.expire_retiring(retired_at + CONNECT_TIMEOUT.saturating_sub(Duration::from_millis(1)));
        assert_eq!(feed.retiring.len(), 2);
        feed.expire_retiring(retired_at + CONNECT_TIMEOUT);
        assert!(feed.retiring.is_empty());
        handle(&one_trade("BTC", 2_000, 12), &mut feed, &events).expect("counted");
        assert_eq!(feed.decode_failures, 3);
    }

    #[test]
    fn subscriptions_only_change_on_new_demand_and_last_control_wins() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(4);
        let mut demand = HyperliquidDemand::default();
        #[cfg(feature = "probe")]
        let mut probe_failure = None;
        assert_eq!(
            drain_session_controls(
                &receiver,
                &mut demand,
                #[cfg(feature = "probe")]
                &mut probe_failure
            ),
            (false, false)
        );
        sender
            .send(HyperliquidRealtimeControl::Subscribe(demand.clone()))
            .unwrap();
        assert_eq!(
            drain_session_controls(
                &receiver,
                &mut demand,
                #[cfg(feature = "probe")]
                &mut probe_failure
            ),
            (false, false)
        );
        let next = HyperliquidDemand {
            books: vec![HyperliquidInstrumentDemand::default()],
            ..Default::default()
        };
        sender.send(HyperliquidRealtimeControl::Stop).unwrap();
        sender
            .send(HyperliquidRealtimeControl::Subscribe(next.clone()))
            .unwrap();
        assert_eq!(
            drain_session_controls(
                &receiver,
                &mut demand,
                #[cfg(feature = "probe")]
                &mut probe_failure
            ),
            (true, false)
        );
        assert_eq!(demand, next);
        sender.send(HyperliquidRealtimeControl::Stop).unwrap();
        assert_eq!(
            drain_session_controls(
                &receiver,
                &mut demand,
                #[cfg(feature = "probe")]
                &mut probe_failure
            ),
            (false, true)
        );
    }

    #[cfg(feature = "probe")]
    #[test]
    fn probe_failure_is_websocket_only_and_does_not_clear_demand() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(2);
        let mut demand = HyperliquidDemand {
            quotes: vec![HyperliquidInstrumentDemand::default()],
            ..Default::default()
        };
        sender
            .send(HyperliquidRealtimeControl::FailWebSocket {
                subsequent_connect_failures: 2,
            })
            .expect("probe control");
        let mut failures = None;
        assert_eq!(
            drain_session_controls(&receiver, &mut demand, &mut failures),
            (false, false)
        );
        assert_eq!(failures, Some(2));
        assert_eq!(demand.quotes.len(), 1);
        let mut remaining = 1;
        let stop = Arc::new(AtomicBool::new(false));
        assert!(connect_market_socket(&stop, &mut remaining).is_err());
        assert_eq!(remaining, 0);
    }

    #[test]
    fn full_event_queue_observes_cancellation() {
        let (events, _received) = std::sync::mpsc::sync_channel(1);
        events.send(1).expect("fill queue");
        let events =
            CatalogPublisher::new(events, "hyperliquid", ProviderCoordinatorWake::for_tests());
        let stop = AtomicBool::new(true);
        assert!(publish_catalog(&events, 2, &stop));
    }

    #[test]
    #[ignore = "drives the live Hyperliquid public feed"]
    fn live_worker_cancellation_is_prompt() {
        let recorder =
            LiveMarketGateRecorder::start("hyperliquid").expect("live gate evidence starts");
        let (controls, control_rx) = std::sync::mpsc::sync_channel(2);
        let (events, event_rx) = std::sync::mpsc::sync_channel(32);
        let stop = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(1));
        let worker_stop = Arc::clone(&stop);
        let worker_generation = Arc::clone(&generation);
        let worker = std::thread::spawn(move || {
            let wake = ProviderCoordinatorWake::for_tests();
            run(
                &control_rx,
                &events,
                &worker_stop,
                &worker_generation,
                &wake,
            );
        });
        let instrument = HyperliquidInstrumentDemand {
            wire_coin: "BTC".to_string(),
            instrument_id: "hyperliquid:perp:BTC".to_string(),
            entitlement_id: HYPERLIQUID_PUBLIC_ENTITLEMENT_ID.to_string(),
            price_scale: 8,
            quantity_scale: 8,
        };
        controls
            .send(HyperliquidRealtimeControl::Subscribe(HyperliquidDemand {
                candles: vec![HyperliquidCandleDemand {
                    instrument,
                    interval: "1m".to_string(),
                }],
                ..Default::default()
            }))
            .expect("demand");
        controls
            .send(HyperliquidRealtimeControl::Connect(ProviderEventSink {
                epoch: aeris_market_engine::fence::EpochSequence::<ProviderTag>::default().next(),
            }))
            .expect("connect");
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut connected = false;
        let mut candle = false;
        while Instant::now() < deadline && !(connected && candle) {
            match event_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Fenced {
                    value: HyperliquidRealtimeEvent::Connected(_),
                    ..
                }) => connected = true,
                Ok(Fenced {
                    value: HyperliquidRealtimeEvent::Candle(_, _, _, _),
                    ..
                }) => candle = true,
                Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        stop.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !worker.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(worker.is_finished(), "live worker ignored cancellation");
        worker.join().expect("worker");
        assert!(connected, "live Hyperliquid worker never connected");
        assert!(candle, "live Hyperliquid worker never published a candle");
        recorder
            .finish(
                LiveMarketGateOutcome::Passed,
                "connected, published a live candle, and cancelled promptly",
            )
            .expect("live gate evidence completes");
    }

    #[test]
    fn benign_control_traffic_neither_counts_nor_forgives() {
        let mut feed = feed();
        let (events, _received) = std::sync::mpsc::sync_channel(1_024);
        for _ in 0..MAXIMUM_DECODE_FAILURES_PER_CONNECTION - 1 {
            handle(&unmapped_trades(), &mut feed, &events).expect("under threshold stays up");
        }
        // Acknowledgements are healthy traffic but prove nothing about the
        // payload decoders: they must not consume budget, and must not reset
        // it either.
        for _ in 0..16 {
            handle(&subscribed_ack(), &mut feed, &events).expect("control traffic handled");
        }
        assert!(matches!(
            handle(&unmapped_trades(), &mut feed, &events),
            Err(FrameOutcome::Reconnect)
        ));
    }
}
