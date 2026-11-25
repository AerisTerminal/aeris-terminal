//! Shared runtime plumbing for public cryptocurrency exchange providers
//! (Binance, Bybit, OKX, and the Aggregated multi-venue feed).

use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use aeris_contracts::{InstallProviderInstrument, ProviderContractMetadata};
use aeris_crypto_common::{
    CryptoError, CryptoResult, NORMALIZED_PRICE_SCALE, NORMALIZED_QUANTITY_SCALE,
};
use aeris_market_data::{BarPeriod, DepthSnapshot, MarketBar, MarketTrade, TopOfBookQuote};
use aeris_market_engine::{
    MarketStream,
    fence::{Epoch, Fenced, ProviderTag},
};
use aeris_market_runtime::{
    ProviderCatalogRejected, ProviderCatalogRejectionReason, ProviderInstrumentSearchResult,
    SearchProviderInstruments, SelectProviderInstrument,
    plugin::{
        ActiveWorkerGuard, COMMAND_CAPACITY, CatalogPublisher, HistoryRequest, HistorySnapshot,
        HistorySource, ProviderCandleSymbol, ProviderCatalogChannelImpl, ProviderCatalogChannels,
        ProviderCatalogCommand, ProviderCatalogEvent, ProviderCoordinatorWake, ProviderDemand,
        ProviderDescriptor, ProviderDisconnect, ProviderEvent, ProviderEventKind,
        ProviderEventSink, ProviderFault, ProviderFaultKind, ProviderRealtimeChannels,
        ProviderRuntimeRecord, ProviderRuntimeSpec, ProviderSession, ProviderSessionError,
        ProviderStartHandle, ProviderTradeBatch, REALTIME_CAPACITY, WireProviderEvent,
        history_instrument_from_install, try_control, try_send_catalog_control,
    },
};

pub(crate) const REALTIME_CONTROL_CAPACITY: usize = 2;
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
pub(crate) const READ_TIMEOUT: Duration = Duration::from_millis(5);
pub(crate) const LIFECYCLE_EMIT_BACKPRESSURE: Duration = Duration::from_secs(2);
pub(crate) const LIFECYCLE_EMIT_POLL: Duration = Duration::from_millis(20);
pub(crate) const CATALOG_REFRESH_INTERVAL: Duration = Duration::from_mins(5);
pub(crate) const CATALOG_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Instrument routing identity carried in a provider demand update.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CryptoInstrumentDemand {
    pub provider_symbol: String,
    pub instrument_id: String,
    pub entitlement_id: String,
    pub price_scale: u8,
    pub quantity_scale: u8,
}

/// One demanded candle feed with its native provider interval string.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CryptoCandleDemand {
    pub instrument: CryptoInstrumentDemand,
    pub interval: String,
}

/// Complete desired subscription set for one crypto provider.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CryptoDemand {
    pub candles: Vec<CryptoCandleDemand>,
    pub trades: Vec<CryptoInstrumentDemand>,
    pub quotes: Vec<CryptoInstrumentDemand>,
    pub books: Vec<CryptoInstrumentDemand>,
}

/// Control messages delivered to a crypto provider's realtime worker.
pub(crate) enum CryptoRealtimeControl {
    Connect(ProviderEventSink),
    Subscribe(CryptoDemand),
    Stop,
}

/// Control messages delivered to a crypto provider's catalog worker.
pub(crate) enum CryptoCatalogControl {
    Search(SearchProviderInstruments),
    Select(SelectProviderInstrument),
}

/// Events published by a crypto provider's catalog worker.
pub(crate) enum CryptoCatalogEvent {
    SearchCompleted(ProviderInstrumentSearchResult),
    SelectionResolved {
        consumer_id: u64,
        command_generation: u64,
        instrument: InstallProviderInstrument,
    },
    Rejected {
        rejection: ProviderCatalogRejected,
        selection: bool,
    },
    RefreshFailed {
        detail: String,
    },
}

impl From<CryptoCatalogEvent> for ProviderCatalogEvent {
    fn from(event: CryptoCatalogEvent) -> Self {
        match event {
            CryptoCatalogEvent::SearchCompleted(result) => Self::SearchCompleted(result),
            CryptoCatalogEvent::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            } => Self::SelectionResolved {
                consumer_id,
                command_generation,
                instrument,
            },
            CryptoCatalogEvent::Rejected {
                rejection,
                selection,
            } => Self::Rejected {
                rejection,
                selection,
            },
            CryptoCatalogEvent::RefreshFailed { detail } => Self::RefreshFailed(detail),
        }
    }
}

/// Events published by a crypto provider's realtime worker.
pub(crate) enum CryptoRealtimeEvent {
    Connecting(u64),
    Connected(u64),
    Candle(u64, String, String, MarketBar),
    Trades(u64, Vec<MarketTrade>),
    Quote(u64, TopOfBookQuote),
    Depth(u64, DepthSnapshot),
    Heartbeat(u64, Option<u64>),
    Recovering(u64),
    Disconnected(u64),
}

/// Projects a [`ProviderDemand`] into a [`CryptoDemand`] using the provider's interval mapper.
pub(crate) fn crypto_demand_from_provider(
    demand: &ProviderDemand,
    interval_for_period: fn(BarPeriod) -> CryptoResult<&'static str>,
) -> CryptoDemand {
    let mut candles = BTreeSet::new();
    let mut trades = BTreeSet::new();
    let mut quotes = BTreeSet::new();
    let mut books = BTreeSet::new();

    for (series, streams, instrument) in &demand.candle_series {
        let (Ok(price_scale), Ok(quantity_scale), Ok(interval)) = (
            u8::try_from(instrument.price_scale),
            u8::try_from(instrument.quantity_scale),
            interval_for_period(series.period),
        ) else {
            continue;
        };
        let mapping = CryptoInstrumentDemand {
            provider_symbol: instrument.provider_symbol.clone(),
            instrument_id: series.instrument_id.clone(),
            entitlement_id: series.entitlement_id.clone(),
            price_scale,
            quantity_scale,
        };
        if streams.contains(MarketStream::Bars) {
            candles.insert(CryptoCandleDemand {
                instrument: mapping.clone(),
                interval: interval.to_string(),
            });
        }
        if streams.contains(MarketStream::Trades) {
            trades.insert(mapping.clone());
        }
        if streams.contains(MarketStream::Quotes) || streams.contains(MarketStream::Depth) {
            quotes.insert(mapping.clone());
        }
        if streams.contains(MarketStream::Depth) {
            books.insert(mapping);
        }
    }

    for requested in &demand.instruments {
        let instrument = &requested.instrument;
        let (Ok(price_scale), Ok(quantity_scale)) = (
            u8::try_from(instrument.price_scale),
            u8::try_from(instrument.quantity_scale),
        ) else {
            continue;
        };
        let mapping = CryptoInstrumentDemand {
            provider_symbol: instrument.provider_symbol.clone(),
            instrument_id: instrument.instrument_id.clone(),
            entitlement_id: instrument.entitlement_id.clone(),
            price_scale,
            quantity_scale,
        };
        if requested.streams.contains(MarketStream::Trades) {
            trades.insert(mapping.clone());
        }
        if requested.streams.contains(MarketStream::Quotes)
            || requested.streams.contains(MarketStream::Depth)
        {
            quotes.insert(mapping.clone());
        }
        if requested.streams.contains(MarketStream::Depth) {
            books.insert(mapping);
        }
    }

    CryptoDemand {
        candles: candles.into_iter().collect(),
        trades: trades.into_iter().collect(),
        quotes: quotes.into_iter().collect(),
        books: books.into_iter().collect(),
    }
}

/// Converts a [`CryptoRealtimeEvent`] into a provider-neutral [`WireProviderEvent`].
pub(crate) fn into_wire_provider_event(
    provider_id: &'static str,
    recovering_detail: &'static str,
    event: CryptoRealtimeEvent,
) -> WireProviderEvent {
    let generation = match &event {
        CryptoRealtimeEvent::Connecting(g)
        | CryptoRealtimeEvent::Connected(g)
        | CryptoRealtimeEvent::Candle(g, ..)
        | CryptoRealtimeEvent::Trades(g, _)
        | CryptoRealtimeEvent::Quote(g, _)
        | CryptoRealtimeEvent::Depth(g, _)
        | CryptoRealtimeEvent::Heartbeat(g, _)
        | CryptoRealtimeEvent::Recovering(g)
        | CryptoRealtimeEvent::Disconnected(g) => *g,
    };
    let transient = ProviderFault {
        kind: ProviderFaultKind::Transient,
        auto_recovers: false,
        detail: recovering_detail,
    };
    let kind = match event {
        CryptoRealtimeEvent::Connecting(_) => ProviderEventKind::Connecting,
        CryptoRealtimeEvent::Connected(_) => ProviderEventKind::Connected,
        CryptoRealtimeEvent::Heartbeat(_, rtt) => ProviderEventKind::Heartbeat(rtt),
        CryptoRealtimeEvent::Recovering(_) => ProviderEventKind::recovering_from_fault(transient),
        CryptoRealtimeEvent::Disconnected(_) => {
            ProviderEventKind::Disconnected(ProviderDisconnect::from_fault(transient))
        }
        CryptoRealtimeEvent::Candle(_, coin, interval, bar) => ProviderEventKind::Candle {
            symbol: ProviderCandleSymbol { coin, interval },
            bar,
        },
        CryptoRealtimeEvent::Trades(_, trades) => {
            ProviderEventKind::Trades(ProviderTradeBatch::Many(trades))
        }
        CryptoRealtimeEvent::Quote(_, quote) => ProviderEventKind::Quote(quote),
        CryptoRealtimeEvent::Depth(_, snapshot) => ProviderEventKind::Depth(snapshot),
    };
    WireProviderEvent {
        provider: provider_id,
        generation,
        kind,
    }
}

/// Shared [`ProviderSession`] implementation for public crypto providers.
pub(crate) struct CryptoSession {
    pub controls: SyncSender<CryptoRealtimeControl>,
    pub history: SyncSender<HistoryRequest>,
    pub interval_for_period: fn(BarPeriod) -> CryptoResult<&'static str>,
}

impl ProviderSession for CryptoSession {
    fn connect(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        debug_assert_eq!(sink.epoch, epoch);
        try_control(&self.controls, CryptoRealtimeControl::Connect(sink))
    }

    fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError> {
        let projected = crypto_demand_from_provider(demand, self.interval_for_period);
        try_control(&self.controls, CryptoRealtimeControl::Subscribe(projected))
    }

    fn request_history(&self, request: HistoryRequest) -> Result<bool, ProviderSessionError> {
        try_control(&self.history, request)
    }

    fn release_idle(&self) -> Result<bool, ProviderSessionError> {
        try_control(&self.controls, CryptoRealtimeControl::Stop)
    }

    fn shutdown(&self) {
        if self.release_idle().is_err() {
            // Worker already stopped.
        }
    }
}

/// Shared [`HistorySource`] dispatching to Binance, Bybit, OKX, or Aggregated history.
pub(crate) enum CryptoHistorySource {
    Binance(aeris_binance_market_adapter::BinanceHttpClient),
    Bybit(aeris_bybit_market_adapter::BybitHttpClient),
    Okx(aeris_okx_market_adapter::OkxHttpClient),
    Aggregated {
        catalog: crate::aggregated_crypto::SharedAggregatedCatalog,
        forming_seeds: crate::aggregated_crypto::SharedFormingSeeds,
        binance: aeris_binance_market_adapter::BinanceHttpClient,
        bybit: aeris_bybit_market_adapter::BybitHttpClient,
        okx: aeris_okx_market_adapter::OkxHttpClient,
        hyperliquid: aeris_hyperliquid_market_adapter::HyperliquidHttpClient,
    },
}

impl HistorySource for CryptoHistorySource {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        let installed = request
            .instrument
            .as_ref()
            .ok_or_else(|| "Provider instrument is not installed".to_string())?;
        let instrument = history_instrument_from_install(installed);
        let provider_gen = request.provider_generation.0.get();
        match self {
            Self::Binance(client) => aeris_binance_market_adapter::history::fetch(
                client,
                &request.series,
                provider_gen,
                &instrument,
                request.req_window(),
                &request.stop,
            )
            .map_err(CryptoError::into_message),
            Self::Bybit(client) => aeris_bybit_market_adapter::history::fetch(
                client,
                &request.series,
                provider_gen,
                &instrument,
                request.req_window(),
                &request.stop,
            )
            .map_err(CryptoError::into_message),
            Self::Okx(client) => aeris_okx_market_adapter::history::fetch(
                client,
                &request.series,
                provider_gen,
                &instrument,
                request.req_window(),
                &request.stop,
            )
            .map_err(CryptoError::into_message),
            Self::Aggregated {
                catalog,
                forming_seeds,
                binance,
                bybit,
                okx,
                hyperliquid,
            } => crate::aggregated_crypto::fetch_aggregated_history(
                catalog,
                forming_seeds,
                binance,
                bybit,
                okx,
                hyperliquid,
                request,
                &instrument,
                provider_gen,
            )
            .map_err(CryptoError::into_message),
        }
    }
}

/// Builds a [`ProviderRuntimeSpec`] for a public crypto provider.
#[allow(clippy::too_many_arguments)]
pub(crate) fn crypto_provider_spec<FCat, FRt>(
    descriptor: ProviderDescriptor,
    history: CryptoHistorySource,
    realtime_enabled: bool,
    catalog_worker_name: &'static str,
    realtime_worker_name: &'static str,
    recovering_detail: &'static str,
    interval_for_period: fn(BarPeriod) -> CryptoResult<&'static str>,
    run_catalog: FCat,
    run_realtime: FRt,
) -> ProviderRuntimeSpec
where
    FCat: FnOnce(
            &Receiver<CryptoCatalogControl>,
            &CatalogPublisher<CryptoCatalogEvent>,
            &Arc<AtomicU64>,
            &Arc<AtomicBool>,
        ) + Send
        + 'static,
    FRt: FnOnce(
            &Receiver<CryptoRealtimeControl>,
            &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
            &Arc<AtomicBool>,
            &Arc<AtomicU64>,
            &ProviderCoordinatorWake,
        ) + Send
        + 'static,
{
    ProviderRuntimeSpec::new(
        descriptor,
        Some(Box::new(history)),
        realtime_enabled,
        Box::new(move |spec, handle| {
            start_crypto_provider_record(
                spec,
                handle,
                catalog_worker_name,
                realtime_worker_name,
                recovering_detail,
                interval_for_period,
                run_catalog,
                run_realtime,
            )
        }),
    )
}

/// Builds a [`ProviderRuntimeRecord`] and spawns the provider's catalog and realtime threads.
///
/// # Errors
/// Returns an error if either worker thread fails to spawn.
#[allow(clippy::too_many_arguments)]
fn start_crypto_provider_record<FCat, FRt>(
    spec: ProviderRuntimeSpec,
    handle: &ProviderStartHandle<'_>,
    catalog_worker_name: &'static str,
    realtime_worker_name: &'static str,
    recovering_detail: &'static str,
    interval_for_period: fn(BarPeriod) -> CryptoResult<&'static str>,
    run_catalog: FCat,
    run_realtime: FRt,
) -> Result<ProviderRuntimeRecord, String>
where
    FCat: FnOnce(
            &Receiver<CryptoCatalogControl>,
            &CatalogPublisher<CryptoCatalogEvent>,
            &Arc<AtomicU64>,
            &Arc<AtomicBool>,
        ) + Send
        + 'static,
    FRt: FnOnce(
            &Receiver<CryptoRealtimeControl>,
            &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
            &Arc<AtomicBool>,
            &Arc<AtomicU64>,
            &ProviderCoordinatorWake,
        ) + Send
        + 'static,
{
    let enabled = spec.realtime_enabled;
    let descriptor = spec.descriptor;
    let mut started = handle.start_history_runtime(spec)?;
    let wake = handle.wake();
    let active_workers = handle.active_workers();
    let (catalog_events_tx, catalog_events) = mpsc::sync_channel(COMMAND_CAPACITY);
    let catalog_events_tx = CatalogPublisher::new(catalog_events_tx, descriptor.id, wake.clone());
    let (catalog_controls, catalog_controls_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (realtime_events_tx, realtime_events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let (realtime_controls, realtime_controls_rx) = mpsc::sync_channel(REALTIME_CONTROL_CAPACITY);
    let ws_generation = Arc::new(AtomicU64::new(1));

    if enabled {
        let cat_cancel = Arc::clone(&started.cancellation);
        let cat_lifecycle = Arc::clone(&started.lifecycle);
        let cat_activity = Arc::clone(&active_workers);
        let cat_gen = Arc::clone(&ws_generation);
        let cat_worker = thread::Builder::new()
            .name(catalog_worker_name.to_string())
            .spawn(move || {
                let _guard = ActiveWorkerGuard::register(catalog_worker_name, cat_activity);
                run_catalog(
                    &catalog_controls_rx,
                    &catalog_events_tx,
                    &cat_gen,
                    &cat_cancel,
                );
                if !cat_cancel.load(Ordering::Acquire) {
                    cat_lifecycle
                        .mark_terminal_failure("Provider catalog runtime stopped unexpectedly");
                }
            })
            .map_err(|error| error.to_string())?;
        started.workers.push(cat_worker);

        let rt_cancel = Arc::clone(&started.cancellation);
        let rt_lifecycle = Arc::clone(&started.lifecycle);
        let rt_activity = Arc::clone(&active_workers);
        let rt_wake = wake.clone();
        let rt_worker = thread::Builder::new()
            .name(realtime_worker_name.to_string())
            .spawn(move || {
                let _guard = ActiveWorkerGuard::register(realtime_worker_name, rt_activity);
                run_realtime(
                    &realtime_controls_rx,
                    &realtime_events_tx,
                    &rt_cancel,
                    &ws_generation,
                    &rt_wake,
                );
                if !rt_cancel.load(Ordering::Acquire) {
                    rt_lifecycle.mark_terminal_failure("Provider runtime stopped unexpectedly");
                }
            })
            .map_err(|error| error.to_string())?;
        started.workers.push(rt_worker);
    }

    let provider_id = descriptor.id;
    let realtime = enabled.then(|| {
        ProviderRealtimeChannels::new(
            move |history| {
                Box::new(CryptoSession {
                    controls: realtime_controls.clone(),
                    history: history.clone(),
                    interval_for_period,
                })
            },
            move || {
                let bound = realtime_events.try_recv().ok()?;
                let epoch = bound.epoch;
                let wire = into_wire_provider_event(provider_id, recovering_detail, bound.value);
                (wire.generation == epoch.get().get()).then_some(ProviderEvent::bound(
                    wire.provider,
                    epoch,
                    wire.generation,
                    wire.kind,
                ))
            },
        )
    });

    Ok(ProviderRuntimeRecord {
        descriptor,
        history: started.history,
        cancellation: started.cancellation,
        lifecycle: started.lifecycle,
        realtime,
        display_depth: None,
        catalog: ProviderCatalogChannels {
            enabled,
            channels: Box::new(ProviderCatalogChannelImpl::new(
                catalog_controls,
                catalog_events,
                dispatch_crypto_catalog,
            )),
        },
        workers: started.workers,
    })
}

fn dispatch_crypto_catalog(
    controls: &SyncSender<CryptoCatalogControl>,
    provider: &str,
    command: ProviderCatalogCommand,
) -> Result<(), String> {
    match command {
        ProviderCatalogCommand::Search(search) => {
            try_send_catalog_control(controls, CryptoCatalogControl::Search(search), provider)
        }
        ProviderCatalogCommand::Select(select) => {
            try_send_catalog_control(controls, CryptoCatalogControl::Select(select), provider)
        }
        ProviderCatalogCommand::Screen(_) | ProviderCatalogCommand::Scan(_) => Err(format!(
            "{provider} does not support market screening or scanning"
        )),
    }
}

/// Normalizes USD-pegged stablecoin quotes (`USDT`, `USDC`, etc.) to `"USD"` for practice-account
/// contract currency compatibility, matching `hyperliquid_realtime`.
fn normalized_contract_currency(quote_asset: &str) -> String {
    match quote_asset.to_ascii_uppercase().as_str() {
        "USD" | "USDT" | "USDC" | "FDUSD" | "USDE" | "DAI" => "USD".to_string(),
        _ => quote_asset.to_string(),
    }
}

/// Builds standard [`ProviderContractMetadata`] for a fixed-tick linear crypto perpetual or spot instrument.
///
/// Fixed-tick venues leave `price_significant_figures` and `price_max_decimals` unset so
/// `instrument_terms_from_install` resolves `PriceGrid::Fixed` from `InstallProviderInstrument::price_increment`.
#[must_use]
pub(crate) fn crypto_contract_metadata(
    is_spot: bool,
    quote_asset: &str,
    quantity_decimals: u8,
) -> ProviderContractMetadata {
    let order_quantity_increment = NORMALIZED_QUANTITY_SCALE
        .checked_sub(u32::from(quantity_decimals))
        .and_then(|digits| 10_i64.checked_pow(digits));
    ProviderContractMetadata {
        point_value: Some(1),
        point_value_scale: Some(0),
        currency: Some(normalized_contract_currency(quote_asset)),
        order_quantity_increment,
        instrument_class: Some(if is_spot {
            aeris_contracts::ProviderInstrumentClass::CryptoSpot
        } else {
            aeris_contracts::ProviderInstrumentClass::CryptoPerpetual
        } as i32),
        ..Default::default()
    }
}

/// Builds an [`InstallProviderInstrument`] for a selected crypto instrument.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub(crate) fn build_install_instrument(
    provider: &str,
    entitlement_id: &str,
    session_generation: u64,
    selection_generation: u64,
    instrument_id: String,
    provider_symbol: String,
    display_symbol: String,
    venue_id: String,
    quantity_decimals: u8,
    tick_size_fixed: i64,
    is_spot: bool,
    quote_asset: &str,
) -> InstallProviderInstrument {
    let terms = crypto_contract_metadata(is_spot, quote_asset, quantity_decimals);
    InstallProviderInstrument {
        provider: provider.to_string(),
        session_generation: session_generation.max(1),
        selection_generation: selection_generation.max(1),
        instrument_id,
        provider_symbol,
        display_symbol,
        venue_id,
        price_scale: NORMALIZED_PRICE_SCALE,
        quantity_scale: NORMALIZED_QUANTITY_SCALE,
        entitlement_id: entitlement_id.to_string(),
        price_increment: Some(tick_size_fixed.max(1)),
        contract_metadata: Some(Box::new(terms)),
    }
}

/// Publishes one catalog event, returning `true` when the worker should stop.
pub(crate) fn publish_catalog_event(
    events: &CatalogPublisher<CryptoCatalogEvent>,
    event: CryptoCatalogEvent,
    stop: &Arc<AtomicBool>,
) -> bool {
    if stop.load(Ordering::Acquire) {
        return true;
    }
    events.send(event).is_err()
}

/// Publishes one rejected catalog search/selection result.
pub(crate) fn reject_catalog(
    events: &CatalogPublisher<CryptoCatalogEvent>,
    consumer_id: u64,
    provider: &str,
    command_generation: u64,
    reason: ProviderCatalogRejectionReason,
    selection: bool,
    stop: &Arc<AtomicBool>,
) -> bool {
    publish_catalog_event(
        events,
        CryptoCatalogEvent::Rejected {
            rejection: ProviderCatalogRejected {
                consumer_id,
                provider: provider.to_string(),
                provider_generation: None,
                command_generation,
                reason,
            },
            selection,
        },
        stop,
    )
}

/// Emits a lifecycle event with bounded backpressure retry; returns `false` if disconnected/stopped.
pub(crate) fn emit_lifecycle(
    sink: ProviderEventSink,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    event: CryptoRealtimeEvent,
    wake: &ProviderCoordinatorWake,
    stop: &Arc<AtomicBool>,
) -> bool {
    let deadline = Instant::now() + LIFECYCLE_EMIT_BACKPRESSURE;
    let mut pending = Fenced {
        epoch: sink.epoch,
        value: event,
    };
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        match events.try_send(pending) {
            Ok(()) => {
                wake.notify();
                return true;
            }
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(returned)) => {
                if Instant::now() >= deadline {
                    return true;
                }
                pending = returned;
                thread::sleep(LIFECYCLE_EMIT_POLL);
            }
        }
    }
}

/// Emits a best-effort market-data event (drops if the bounded queue is full).
pub(crate) fn emit_data(
    sink: ProviderEventSink,
    events: &SyncSender<Fenced<ProviderTag, CryptoRealtimeEvent>>,
    event: CryptoRealtimeEvent,
    wake: &ProviderCoordinatorWake,
) {
    if events
        .try_send(Fenced {
            epoch: sink.epoch,
            value: event,
        })
        .is_ok()
    {
        wake.notify();
    }
}

/// Current monotonic-ish wall-clock Unix nanoseconds for `received_unix_nanos`.
#[must_use]
pub(crate) fn now_unix_nanos() -> i64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos());
    i64::try_from(nanos).unwrap_or(i64::MAX).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_instruments::{InstrumentClass, InstrumentDecimal, PriceGrid};
    use aeris_market_runtime::market_service::instrument_terms_from_install;

    #[test]
    fn fixed_tick_crypto_providers_produce_valid_practice_trading_terms() {
        let cases = [
            (
                "crypto_agg",
                "instrument:crypto_agg:BTCUSDT.P",
                "PERP:BTC:USDT",
                "BTCUSDT.P",
                "AGG-PERP",
                3_u8,
                10_000_000_i64,
                false,
                InstrumentClass::CryptoPerpetual,
            ),
            (
                "crypto_agg",
                "instrument:crypto_agg:BTCUSDT",
                "SPOT:BTC:USDT",
                "BTCUSDT",
                "AGG-SPOT",
                5_u8,
                1_000_000_i64,
                true,
                InstrumentClass::CryptoSpot,
            ),
            (
                "binance",
                "instrument:binance:usdm:BTCUSDT",
                "usdm:BTCUSDT",
                "BTCUSDT.P",
                "BINANCE-FUTURES",
                3_u8,
                10_000_000_i64,
                false,
                InstrumentClass::CryptoPerpetual,
            ),
            (
                "bybit",
                "instrument:bybit:linear:BTCUSDT",
                "linear:BTCUSDT",
                "BTCUSDT.P",
                "BYBIT-LINEAR",
                3_u8,
                10_000_000_i64,
                false,
                InstrumentClass::CryptoPerpetual,
            ),
            (
                "okx",
                "instrument:okx:swap:BTC-USDT-SWAP",
                "swap:BTC-USDT-SWAP",
                "BTCUSDT.P",
                "OKX-SWAP",
                2_u8,
                10_000_000_i64,
                false,
                InstrumentClass::CryptoPerpetual,
            ),
        ];

        for (
            provider,
            instrument_id,
            provider_symbol,
            display_symbol,
            venue_id,
            qty_decimals,
            tick_size_fixed,
            is_spot,
            expected_class,
        ) in cases
        {
            let install = build_install_instrument(
                provider,
                "public",
                1,
                1,
                instrument_id.to_string(),
                provider_symbol.to_string(),
                display_symbol.to_string(),
                venue_id.to_string(),
                qty_decimals,
                tick_size_fixed,
                is_spot,
                "USDT",
            );
            let terms = instrument_terms_from_install(&install).unwrap_or_else(|err| {
                panic!("{provider} ({display_symbol}) must produce valid trading terms: {err}")
            });
            assert_eq!(terms.instrument_class, expected_class);
            assert_eq!(
                terms.price_grid,
                PriceGrid::Fixed(
                    InstrumentDecimal::try_new(tick_size_fixed, 8).expect("fixed tick")
                )
            );
            assert_eq!(terms.currency, "USD");
            assert_eq!(terms.route.provider_id, provider);
        }
    }
}
