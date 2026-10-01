//! Runtime-owned tastytrade catalog, multiplexed stream, and on-demand history.
use super::{
    ActiveWorkerGuard, Arc, AtomicBool, AtomicU64, BTreeMap, BTreeSet, BarPeriod, BarSeriesKey,
    COMMAND_CAPACITY, CatalogPublisher, Command, Coordinator, Duration, FailureStage, FormingBar,
    HISTORY_CAPACITY, HistoryRequest, HistorySnapshot, InstallProviderInstrument, Instant,
    LIVE_BUFFER_CAPACITY, MAXIMUM_CONSUMERS, MarketBar, MarketTrade, Mutex, Ordering,
    ProviderCatalogChannelSet, ProviderCatalogChannels, ProviderCatalogRejected,
    ProviderCatalogRejectionReason, ProviderCoordinatorWake, ProviderGeneration,
    ProviderRealtimeChannelSet, ProviderRealtimeChannels, ProviderRealtimeDispatch,
    ProviderRuntimeLifecycle, ProviderRuntimeRecord, REALTIME_CAPACITY,
    RITHMIC_REALTIME_CONTROL_CAPACITY, Receiver, RecvTimeoutError, SearchProviderInstruments,
    SelectProviderInstrument, SyncSender, TopOfBookQuote, TrySendError, VecDeque,
    broker_authorization, id, mpsc, thread,
};
use aeris_contracts::{ProviderInstrumentSearchResult, ProviderInstrumentSummary};
use aeris_market_data::{DepthLevel, EventMetadata, QualifiedTimestamp};
use aeris_tastytrade_market_adapter::{
    ConnectionCapability, DATA_SCALE, DxlinkSession, FeedEvent, QuoteToken, SearchInstrument,
    Subscription, TastytradeBrokerClient, TradePrint,
};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) const ENTITLEMENT: &str = "tastytrade-authorized";
const SNAPSHOT_BEGIN: u32 = 4;
const SNAPSHOT_END: u32 = 8;
const SNAPSHOT_SNIP: u32 = 16;
const REMOVE_EVENT: u32 = 2;
const TX_PENDING: u32 = 1;
const MAXIMUM_TICK_HISTORY: usize = 65_536;

pub(super) enum CatalogControl {
    Search(SearchProviderInstruments),
    Select(SelectProviderInstrument),
}
pub(super) enum CatalogEvent {
    Search(ProviderInstrumentSearchResult),
    Selection {
        consumer_id: u64,
        command_generation: u64,
        instrument: InstallProviderInstrument,
    },
    Rejected {
        rejection: ProviderCatalogRejected,
        selection: bool,
    },
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Demand {
    pub series: Vec<(BarSeriesKey, InstallProviderInstrument)>,
    pub instruments: Vec<InstallProviderInstrument>,
}
pub(super) enum RealtimeControl {
    Subscribe(Demand),
    Stop,
    AuthorizationChanged(bool),
    Recover(u64),
}
pub(super) struct IndexedTradeMutation {
    index: String,
    trade: Option<MarketTrade>,
}
pub(super) enum RealtimeEvent {
    Connecting(u64),
    Connected(u64),
    Recovering(u64, String),
    Disconnected(u64),
    Candle(u64, String, MarketBar),
    CandleRecovery(u64, String),
    Quote(u64, TopOfBookQuote),
    Trades(u64, InstallProviderInstrument, Vec<IndexedTradeMutation>),
    Tape(u64, InstallProviderInstrument, Vec<MarketTrade>, bool),
}
impl RealtimeEvent {
    pub(super) fn generation(&self) -> u64 {
        match self {
            Self::Connecting(g)
            | Self::Connected(g)
            | Self::Recovering(g, _)
            | Self::Disconnected(g)
            | Self::Candle(g, ..)
            | Self::CandleRecovery(g, ..)
            | Self::Quote(g, _)
            | Self::Trades(g, ..)
            | Self::Tape(g, ..) => *g,
        }
    }
}

/// One REST lane keeps catalog and streaming-token refresh below the hosted rate limit.
#[derive(Default)]
pub(super) struct BrokerApi {
    state: Mutex<(TastytradeBrokerClient, Option<Instant>)>,
}
impl BrokerApi {
    fn call<T>(
        &self,
        stop: &Arc<AtomicBool>,
        operation: impl FnOnce(&mut TastytradeBrokerClient, &ConnectionCapability) -> Result<T, String>,
    ) -> Result<T, String> {
        self.with_client(stop, |client| {
            let capability = broker_authorization::load_connection()?;
            operation(client, &capability)
        })
    }
    pub(super) fn with_client<T>(
        &self,
        stop: &Arc<AtomicBool>,
        operation: impl FnOnce(&mut TastytradeBrokerClient) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Tastytrade request lane failed")?;
        while state.1.is_some_and(|time| Instant::now() < time) {
            if stop.load(Ordering::Acquire) {
                return Err("Tastytrade request cancelled".into());
            }
            thread::sleep(Duration::from_millis(25));
        }
        if stop.load(Ordering::Acquire) {
            return Err("Tastytrade request cancelled".into());
        }
        let result = operation(&mut state.0);
        state.1 = Some(Instant::now() + Duration::from_secs(3));
        result
    }
}

pub(super) fn run_catalog(
    controls: &Receiver<CatalogControl>,
    events: &CatalogPublisher<CatalogEvent>,
    generation: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
    api: &Arc<BrokerApi>,
) {
    let mut searches = BTreeMap::<u64, (u64, Vec<SearchInstrument>)>::new();
    let mut selection_generation = 0u64;
    while !stop.load(Ordering::Acquire) {
        let control = match controls.recv_timeout(Duration::from_millis(100)) {
            Ok(control) => control,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let (consumer, command, selection) = match &control {
            CatalogControl::Search(s) => (s.consumer_id, s.search_generation, false),
            CatalogControl::Select(s) => (s.consumer_id, s.selection_generation, true),
        };
        let result: Result<CatalogEvent, String> = (|| match control {
            CatalogControl::Search(search) => {
                let items =
                    api.call(stop, |client, cap| client.search(cap, &search.query, stop))?;
                let summaries = items
                    .iter()
                    .take(search.maximum_results.min(100) as usize)
                    .map(instrument_summary)
                    .collect();
                if !searches.contains_key(&consumer) && searches.len() >= MAXIMUM_CONSUMERS {
                    searches.pop_first();
                }
                searches.insert(consumer, (command, items));
                Ok(CatalogEvent::Search(ProviderInstrumentSearchResult {
                    consumer_id: consumer,
                    provider: "tastytrade".into(),
                    provider_generation: generation.load(Ordering::Acquire),
                    search_generation: command,
                    instruments: summaries,
                }))
            }
            CatalogControl::Select(selection) => {
                if selection.entitlement_id != ENTITLEMENT {
                    return Err("Tastytrade entitlement changed".into());
                }
                let item = searches
                    .get(&consumer)
                    .filter(|(epoch, _)| *epoch == selection.search_generation)
                    .and_then(|(_, items)| {
                        items.iter().find(|item| {
                            item.symbol == selection.symbol
                                && catalog_venue(item) == selection.exchange
                        })
                    })
                    .ok_or("Tastytrade selection is no longer in the current search")?;
                let resolved = api.call(stop, |client, cap| client.instrument(cap, item, stop))?;
                selection_generation = selection_generation
                    .checked_add(1)
                    .ok_or("Tastytrade selection generation overflowed")?;
                Ok(CatalogEvent::Selection {
                    consumer_id: consumer,
                    command_generation: command,
                    instrument: InstallProviderInstrument {
                        provider: "tastytrade".into(),
                        session_generation: generation.load(Ordering::Acquire),
                        selection_generation,
                        instrument_id: format!(
                            "tastytrade:{}:{}",
                            resolved.instrument_type, resolved.symbol
                        ),
                        provider_symbol: resolved.streamer_symbol,
                        display_symbol: resolved.symbol,
                        venue_id: resolved.venue,
                        price_scale: DATA_SCALE,
                        quantity_scale: DATA_SCALE,
                        entitlement_id: ENTITLEMENT.into(),
                        price_increment: resolved.tick_size,
                        contract_metadata: None,
                    },
                })
            }
        })();
        let event = result.unwrap_or_else(|error| {
            eprintln!("Aeris tastytrade catalog request failed: {error}");
            CatalogEvent::Rejected {
                rejection: ProviderCatalogRejected {
                    consumer_id: consumer,
                    provider: "tastytrade".into(),
                    provider_generation: Some(generation.load(Ordering::Acquire)),
                    command_generation: command,
                    reason: ProviderCatalogRejectionReason::DispatchUnavailable,
                },
                selection,
            }
        });
        if events.send(event).is_err() && stop.load(Ordering::Acquire) {
            return;
        }
    }
}
fn instrument_summary(item: &SearchInstrument) -> ProviderInstrumentSummary {
    ProviderInstrumentSummary {
        symbol: item.symbol.clone(),
        display_symbol: item.symbol.clone(),
        exchange: catalog_venue(item),
        name: item.description.clone(),
        product_code: None,
        instrument_type: Some(item.instrument_type.clone()),
        expiration_date: None,
    }
}
fn catalog_venue(item: &SearchInstrument) -> String {
    // Search class is a grouping label only. Selection resolves the authoritative venue.
    item.exchange
        .clone()
        .unwrap_or_else(|| item.instrument_type.clone())
}

pub(super) fn candle_period(period: BarPeriod) -> Result<String, String> {
    period.validate().map_err(|e| e.to_string())?;
    let (count, unit) = match period {
        BarPeriod::Time { seconds } if seconds.is_multiple_of(3600) => (seconds / 3600, "h"),
        BarPeriod::Time { seconds } => (seconds / 60, "m"),
        BarPeriod::Session { days } => (days, "d"),
        BarPeriod::Week { weeks } => (weeks, "w"),
        BarPeriod::Month { months } => (months, "mo"),
        BarPeriod::Tick { .. } => {
            return Err("Tick candles require distinct same-time bucket identities".into());
        }
    };
    Ok(if count == 1 {
        unit.into()
    } else {
        format!("{count}{unit}")
    })
}
fn candle_symbol(
    series: &BarSeriesKey,
    instrument: &InstallProviderInstrument,
) -> Result<String, String> {
    Ok(format!(
        "{}{{={}}}",
        instrument.provider_symbol,
        candle_period(series.period)?
    ))
}
fn now_nanos() -> Result<i64, String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "Machine clock is invalid")?;
    i64::try_from(duration.as_nanos()).map_err(|_| "Machine clock exceeds timestamp range".into())
}
fn metadata(
    instrument: &InstallProviderInstrument,
    generation: u64,
    ordinal: u64,
    provider_time: Option<i64>,
) -> Result<EventMetadata, String> {
    Ok(EventMetadata {
        provider_id: "tastytrade".into(),
        instrument_id: instrument.instrument_id.clone(),
        entitlement_id: instrument.entitlement_id.clone(),
        source_sequence: ordinal,
        session_generation: generation,
        timestamps: QualifiedTimestamp {
            exchange_unix_nanos: provider_time,
            provider_unix_nanos: None,
            received_unix_nanos: now_nanos()?,
        },
    })
}
fn market_trade(
    instrument: &InstallProviderInstrument,
    generation: u64,
    ordinal: u64,
    index: String,
    print: &TradePrint,
) -> Result<Option<MarketTrade>, String> {
    if print.spread_leg {
        return Ok(None);
    }
    let trade = MarketTrade {
        metadata: metadata(instrument, generation, ordinal, Some(print.time_nanos))?,
        trade_id: index,
        price: print.price,
        quantity: print.quantity,
        aggressor: print.aggressor,
    };
    trade.validate().map_err(|e| e.to_string())?;
    Ok(Some(trade))
}
fn publish(
    events: &SyncSender<RealtimeEvent>,
    wake: &ProviderCoordinatorWake,
    event: RealtimeEvent,
) -> Result<(), String> {
    let generation = event.generation();
    match events.try_send(event) {
        Ok(()) => {
            wake.notify();
            Ok(())
        }
        Err(TrySendError::Full(_)) => {
            wake.report_overflow(2, generation);
            Err("Tastytrade event queue overflowed; continuity requires recovery".into())
        }
        Err(TrySendError::Disconnected(_)) => Err("Tastytrade coordinator stopped".into()),
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CandleHistoryState {
    Collecting,
    Ending,
    Ready,
    Published,
}
struct HistoryTask {
    request: HistoryRequest,
    symbol: String,
    deadline: Instant,
    candles: BTreeMap<String, MarketBar>,
    candle_state: CandleHistoryState,
    trades: BTreeMap<String, MarketTrade>,
    tape_complete: bool,
    tape_end_seen: bool,
    tape_truncated: bool,
}
impl HistoryTask {
    fn begin(
        request: &HistoryRequest,
        channel: u64,
        session: &mut DxlinkSession,
    ) -> Result<Self, String> {
        let instrument = request
            .instrument
            .as_ref()
            .ok_or("Tastytrade instrument is not installed")?;
        let symbol = candle_symbol(&request.series, instrument)?;
        let now = now_nanos()?;
        let duration = request
            .series
            .period
            .duration_nanos()
            .unwrap_or(86_400_000_000_000);
        let from = request.range.map_or_else(
            || {
                now.saturating_sub(
                    duration
                        .saturating_mul(i64::try_from(request.maximum_bars).unwrap_or(i64::MAX))
                        .saturating_mul(2),
                )
                .max(0)
            },
            |range| range.start_unix_nanos.max(0),
        );
        session.open(
            channel,
            "AUTO",
            vec![
                Subscription {
                    kind: "Candle",
                    symbol: symbol.clone(),
                    from_time_ms: Some(from / 1_000_000),
                },
                Subscription {
                    kind: "TimeAndSale",
                    symbol: instrument.provider_symbol.clone(),
                    from_time_ms: Some(from / 1_000_000),
                },
            ],
        )?;
        Ok(Self {
            request: request.clone(),
            symbol,
            deadline: Instant::now() + Duration::from_secs(45),
            candles: BTreeMap::new(),
            candle_state: CandleHistoryState::Collecting,
            trades: BTreeMap::new(),
            tape_complete: false,
            tape_end_seen: false,
            tape_truncated: false,
        })
    }
    fn accept(
        &mut self,
        event: FeedEvent,
        generation: u64,
        ordinal: &mut u64,
    ) -> Result<(), String> {
        match event {
            FeedEvent::Candle {
                symbol,
                flags,
                index,
                bar,
                ..
            } if symbol == self.symbol => {
                if self.candle_state == CandleHistoryState::Published {
                    return Ok(());
                }
                if flags & SNAPSHOT_BEGIN != 0 {
                    self.candles.clear();
                    self.candle_state = CandleHistoryState::Collecting;
                }
                if flags & REMOVE_EVENT != 0 {
                    self.candles.remove(&index);
                } else if let Some(bar) = bar {
                    bar.validate().map_err(|e| e.to_string())?;
                    let in_range = self.request.range.is_none_or(|range| {
                        bar.exchange_timestamp_unix_nanos >= range.start_unix_nanos
                            && bar.exchange_timestamp_unix_nanos < range.end_unix_nanos
                    });
                    if in_range {
                        if !self.candles.contains_key(&index) && self.candles.len() >= 16_384 {
                            return Err("Tastytrade candle snapshot exceeded its bound".into());
                        }
                        self.candles.insert(index, bar);
                    }
                }
                if flags & (SNAPSHOT_END | SNAPSHOT_SNIP) != 0 {
                    self.candle_state = CandleHistoryState::Ending;
                }
                if self.candle_state == CandleHistoryState::Ending && flags & TX_PENDING == 0 {
                    self.candle_state = CandleHistoryState::Ready;
                }
            }
            FeedEvent::Trade {
                symbol,
                flags,
                index,
                kind,
                trade,
                ..
            } => {
                let instrument = self
                    .request
                    .instrument
                    .as_ref()
                    .ok_or("Tastytrade history instrument missing")?;
                if symbol != instrument.provider_symbol {
                    return Ok(());
                }
                if flags & SNAPSHOT_BEGIN != 0 {
                    self.trades.clear();
                    self.tape_complete = false;
                    self.tape_end_seen = false;
                }
                if flags & REMOVE_EVENT != 0 || kind == "CANCEL" {
                    self.trades.remove(&index);
                } else if let Some(print) = trade {
                    *ordinal = ordinal
                        .checked_add(1)
                        .ok_or("Tastytrade ingestion ordinal overflowed")?;
                    if let Some(trade) =
                        market_trade(instrument, generation, *ordinal, index.clone(), &print)?
                    {
                        let in_range = self.request.range.is_none_or(|range| {
                            trade
                                .metadata
                                .timestamps
                                .exchange_unix_nanos
                                .is_some_and(|time| {
                                    time >= range.start_unix_nanos && time < range.end_unix_nanos
                                })
                        });
                        if in_range {
                            if self.trades.len() >= MAXIMUM_TICK_HISTORY
                                && !self.trades.contains_key(&index)
                            {
                                self.tape_truncated = true;
                                self.tape_complete = true;
                            } else {
                                self.trades.insert(index, trade);
                            }
                        }
                    }
                }
                if flags & (SNAPSHOT_END | SNAPSHOT_SNIP) != 0 {
                    self.tape_end_seen = true;
                    self.tape_truncated |= flags & SNAPSHOT_SNIP != 0;
                }
                if self.tape_end_seen && flags & TX_PENDING == 0 {
                    self.tape_complete = true;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn snapshot(&self) -> Result<HistorySnapshot, String> {
        let mut bars: Vec<_> = self
            .candles
            .values()
            .copied()
            .filter(|bar| {
                self.request.range.is_none_or(|range| {
                    bar.exchange_timestamp_unix_nanos >= range.start_unix_nanos
                        && bar.exchange_timestamp_unix_nanos < range.end_unix_nanos
                })
            })
            .collect();
        bars.sort_unstable_by_key(|bar| bar.exchange_timestamp_unix_nanos);
        bars.dedup_by_key(|bar| bar.exchange_timestamp_unix_nanos);
        let mut forming = None;
        if self.request.range.is_none() && !bars.is_empty() {
            // The last provider bucket remains live; it is never persisted as completed history.
            forming = bars.pop().map(|bar| FormingBar { bar, trades: None });
        }
        if bars.len() > self.request.maximum_bars {
            bars.drain(..bars.len() - self.request.maximum_bars);
        }
        for (index, bar) in bars.iter_mut().enumerate() {
            bar.source_sequence = index as u64 + 1;
        }
        if let Some(open) = &mut forming {
            open.bar.source_sequence = bars.len() as u64 + 1;
        }
        Ok(HistorySnapshot {
            price_scale: u8::try_from(DATA_SCALE)
                .map_err(|_| "Tastytrade price scale exceeds storage")?,
            quantity_scale: u8::try_from(DATA_SCALE)
                .map_err(|_| "Tastytrade quantity scale exceeds storage")?,
            bars,
            forming,
            handoff_boundary_unix_nanos: None,
        })
    }
}

struct TokenRequest {
    identity: u64,
    generation: u64,
}
struct TokenReply {
    identity: u64,
    generation: u64,
    result: Result<QuoteToken, String>,
}
fn run_tokens(
    requests: &Receiver<TokenRequest>,
    replies: &SyncSender<TokenReply>,
    api: &BrokerApi,
    stop: &Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        let request = match requests.recv_timeout(Duration::from_millis(100)) {
            Ok(request) => request,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let result = api.call(stop, |client, cap| client.quote_token(cap, stop));
        let mut reply = TokenReply {
            identity: request.identity,
            generation: request.generation,
            result,
        };
        loop {
            match replies.try_send(reply) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => return,
                Err(TrySendError::Full(value)) => reply = value,
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }
}
struct WorkerPorts {
    controls: Receiver<RealtimeControl>,
    events: SyncSender<RealtimeEvent>,
    history: Receiver<HistoryRequest>,
    completions: SyncSender<Command>,
    generation: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    wake: ProviderCoordinatorWake,
    token_requests: SyncSender<TokenRequest>,
    token_replies: Receiver<TokenReply>,
}
struct Worker {
    ports: WorkerPorts,
    demand: Demand,
    socket: Option<DxlinkSession>,
    retry_at: Instant,
    refresh_at: Instant,
    connected_at: Instant,
    socket_url: String,
    pending_token: Option<(u64, u64)>,
    token_identity: u64,
    histories: BTreeMap<u64, HistoryTask>,
    completions: VecDeque<Command>,
    channel: u64,
    ordinal: u64,
    demand_dirty: bool,
    live_from_ms: i64,
    failures: u8,
    paused: bool,
    trade_batches: BTreeMap<String, (bool, Vec<IndexedTradeMutation>)>,
    candle_batches: BTreeMap<String, (bool, Vec<MarketBar>)>,
}
impl Worker {
    fn new(ports: WorkerPorts) -> Self {
        Self {
            ports,
            demand: Demand::default(),
            socket: None,
            retry_at: Instant::now(),
            refresh_at: Instant::now(),
            connected_at: Instant::now(),
            socket_url: String::new(),
            pending_token: None,
            token_identity: 0,
            histories: BTreeMap::new(),
            completions: VecDeque::new(),
            channel: 5,
            ordinal: 0,
            demand_dirty: true,
            live_from_ms: 0,
            failures: 0,
            paused: false,
            trade_batches: BTreeMap::new(),
            candle_batches: BTreeMap::new(),
        }
    }
    fn epoch(&self) -> u64 {
        self.ports.generation.load(Ordering::Acquire)
    }
    fn publish(&self, event: RealtimeEvent) -> Result<(), String> {
        publish(&self.ports.events, &self.ports.wake, event)
    }
    fn run(mut self) {
        while !self.ports.stop.load(Ordering::Acquire) {
            self.controls();
            self.flush_completions();
            let work = !self.demand.series.is_empty() || !self.demand.instruments.is_empty();
            if self.paused || (!work && self.socket.is_none()) {
                self.cancel_histories("Tastytrade history demand retired");
                thread::sleep(Duration::from_millis(25));
                continue;
            }
            let result = (|| {
                self.connect()?;
                if self.socket.is_some() && self.connected_at.elapsed() >= Duration::from_secs(60) {
                    self.failures = 0;
                }
                self.reconcile()?;
                self.start_history()?;
                self.finish_history()?;
                for _ in 0..128 {
                    let Some(socket) = &mut self.socket else {
                        break;
                    };
                    let Some(event) = socket.poll(Instant::now() + Duration::from_millis(2))?
                    else {
                        break;
                    };
                    self.accept(event)?;
                }
                Ok::<(), String>(())
            })();
            if let Err(error) = result {
                self.recover(error);
            }
            if self.socket.is_none() {
                thread::sleep(Duration::from_millis(25));
            }
        }
        self.cancel_histories("Tastytrade runtime stopped");
        self.flush_completions();
    }
    fn controls(&mut self) {
        while let Ok(control) = self.ports.controls.try_recv() {
            match control {
                RealtimeControl::Subscribe(next) => {
                    self.demand = next;
                    self.demand_dirty = true;
                }
                RealtimeControl::Stop => {
                    self.demand = Demand::default();
                    self.pending_token = None;
                    self.socket = None;
                    let _ = self.publish(RealtimeEvent::Disconnected(self.epoch()));
                }
                RealtimeControl::AuthorizationChanged(ready) => {
                    self.pending_token = None;
                    self.paused = !ready;
                    self.failures = 0;
                    self.retry_at = Instant::now();
                    if !ready {
                        self.socket = None;
                        self.cancel_histories("Tastytrade disconnected");
                    }
                }
                RealtimeControl::Recover(generation) if generation == self.epoch() => {
                    self.recover("Tick state requires covering recovery".into());
                }
                RealtimeControl::Recover(_) => {}
            }
        }
    }
    fn connect(&mut self) -> Result<(), String> {
        while let Ok(reply) = self.ports.token_replies.try_recv() {
            if self.pending_token != Some((reply.identity, reply.generation)) {
                continue;
            }
            self.pending_token = None;
            self.install_token(&reply.result?)?;
        }
        if self.pending_token.is_some() || Instant::now() < self.retry_at {
            return Ok(());
        }
        if self.socket.is_some() && Instant::now() < self.refresh_at {
            return Ok(());
        }
        if self.socket.is_none() {
            let epoch = if self.ordinal == 0 {
                self.epoch()
            } else {
                self.ports
                    .generation
                    .fetch_add(1, Ordering::AcqRel)
                    .checked_add(1)
                    .ok_or("Tastytrade session generation overflowed")?
            };
            self.ordinal = self.ordinal.max(1);
            self.publish(RealtimeEvent::Connecting(epoch))?;
        }
        self.token_identity = self
            .token_identity
            .checked_add(1)
            .ok_or("Tastytrade token request identity overflowed")?;
        let request = TokenRequest {
            identity: self.token_identity,
            generation: self.epoch(),
        };
        let identity = (request.identity, request.generation);
        match self.ports.token_requests.try_send(request) {
            Ok(()) => self.pending_token = Some(identity),
            Err(TrySendError::Full(_)) => {
                self.retry_at = Instant::now() + Duration::from_millis(100);
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err("Tastytrade token worker stopped".into());
            }
        }
        Ok(())
    }
    fn install_token(&mut self, token: &QuoteToken) -> Result<(), String> {
        let expires = chrono::DateTime::parse_from_rfc3339(&token.expires_at)
            .map_err(|_| "Tastytrade token expiry invalid")?
            .timestamp();
        let seconds = expires
            .saturating_sub(now_nanos()? / 1_000_000_000)
            .saturating_sub(60);
        if seconds <= 0 {
            return Err("Tastytrade streaming token is expired".into());
        }
        self.refresh_at = Instant::now()
            + Duration::from_secs(
                u64::try_from(seconds).map_err(|_| "Tastytrade expiry duration invalid")?,
            );
        if let Some(socket) = &mut self.socket {
            if self.socket_url != token.dxlink_url {
                return Err("Tastytrade streaming endpoint changed; reseeding market state".into());
            }
            return socket.reauthorize(token);
        }
        let mut socket = DxlinkSession::connect(token, &self.ports.stop)?;
        socket.open(1, "AUTO", Vec::new())?;
        socket.open(3, "AUTO", Vec::new())?;
        self.socket_url.clone_from(&token.dxlink_url);
        self.connected_at = Instant::now();
        self.live_from_ms = now_nanos()?.saturating_sub(8 * 60 * 1_000_000_000) / 1_000_000;
        self.demand_dirty = true;
        self.socket = Some(socket);
        self.publish(RealtimeEvent::Connected(self.epoch()))
    }
    fn reconcile(&mut self) -> Result<(), String> {
        if !self.demand_dirty {
            return Ok(());
        }
        let Some(socket) = &mut self.socket else {
            return Ok(());
        };
        let streams = self
            .demand
            .instruments
            .iter()
            .flat_map(|instrument| {
                [
                    Subscription {
                        kind: "Quote",
                        symbol: instrument.provider_symbol.clone(),
                        from_time_ms: None,
                    },
                    Subscription {
                        kind: "TimeAndSale",
                        symbol: instrument.provider_symbol.clone(),
                        from_time_ms: Some(self.live_from_ms),
                    },
                ]
            })
            .collect();
        socket.replace(1, streams)?;
        let candles = self
            .demand
            .series
            .iter()
            .map(|(series, instrument)| {
                Ok(Subscription {
                    kind: "Candle",
                    symbol: candle_symbol(series, instrument)?,
                    from_time_ms: Some(self.live_from_ms),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        socket.replace(3, candles)?;
        self.trade_batches.retain(|symbol, _| {
            self.demand
                .instruments
                .iter()
                .any(|instrument| instrument.provider_symbol == *symbol)
        });
        self.candle_batches.retain(|symbol, _| {
            self.demand.series.iter().any(|(series, instrument)| {
                candle_symbol(series, instrument).is_ok_and(|expected| expected == *symbol)
            })
        });
        self.demand_dirty = false;
        Ok(())
    }
    fn start_history(&mut self) -> Result<(), String> {
        if !self.completions.is_empty() || self.histories.len() >= 2 {
            return Ok(());
        }
        let epoch = self.epoch();
        let Some(socket) = &mut self.socket else {
            return Ok(());
        };
        let Ok(request) = self.ports.history.try_recv() else {
            return Ok(());
        };
        if request.stop.load(Ordering::Acquire) || request.provider_generation.0.get() != epoch {
            self.queue_completion(request, Err("Tastytrade history request retired".into()));
            return Ok(());
        }
        self.channel = self
            .channel
            .checked_add(2)
            .ok_or("Tastytrade channel identity overflowed")?;
        match HistoryTask::begin(&request, self.channel, socket) {
            Ok(task) => {
                self.histories.insert(self.channel, task);
            }
            Err(error) => {
                self.queue_completion(request, Err(error.clone()));
                return Err(error);
            }
        }
        Ok(())
    }
    fn finish_history(&mut self) -> Result<(), String> {
        let channels: Vec<_> = self.histories.keys().copied().collect();
        for channel in channels {
            let Some(task) = self.histories.get_mut(&channel) else {
                continue;
            };
            let cancelled = task.request.stop.load(Ordering::Acquire)
                || !self
                    .demand
                    .series
                    .iter()
                    .any(|(series, _)| *series == task.request.series);
            if task.candle_state == CandleHistoryState::Ready && !cancelled {
                let result = task.snapshot();
                task.candle_state = CandleHistoryState::Published;
                let request = task.request.clone();
                self.queue_completion(request, result);
            }
            let Some(task) = self.histories.get(&channel) else {
                continue;
            };
            if !cancelled
                && Instant::now() < task.deadline
                && !(task.candle_state != CandleHistoryState::Collecting && task.tape_complete)
            {
                continue;
            }
            let Some(mut task) = self.histories.remove(&channel) else {
                continue;
            };
            if let Some(socket) = &mut self.socket {
                socket.close_channel(channel)?;
            }
            if task.candle_state != CandleHistoryState::Published {
                self.queue_completion(
                    task.request.clone(),
                    Err("Tastytrade candle history is unavailable or cancelled".into()),
                );
            }
            if !cancelled {
                let instrument = task
                    .request
                    .instrument
                    .take()
                    .ok_or("Tastytrade history instrument missing")?;
                let mut trades: Vec<_> = task.trades.into_values().collect();
                trades.sort_unstable_by(|a, b| {
                    (a.metadata.timestamps.exchange_unix_nanos, &a.trade_id)
                        .cmp(&(b.metadata.timestamps.exchange_unix_nanos, &b.trade_id))
                });
                self.publish(RealtimeEvent::Tape(
                    self.epoch(),
                    instrument,
                    trades,
                    task.tape_truncated || !task.tape_complete,
                ))?;
            }
        }
        Ok(())
    }
    fn accept(&mut self, event: FeedEvent) -> Result<(), String> {
        self.ordinal = self
            .ordinal
            .checked_add(1)
            .ok_or("Tastytrade delivery ordinal overflowed")?;
        let epoch = self.epoch();
        if let Some(task) = self.histories.get_mut(&event.channel()) {
            return task.accept(event, epoch, &mut self.ordinal);
        }
        match event {
            FeedEvent::Quote {
                channel: 1,
                symbol,
                bid,
                ask,
                time_nanos,
            } => {
                let Some(instrument) = self
                    .demand
                    .instruments
                    .iter()
                    .find(|instrument| instrument.provider_symbol == symbol)
                else {
                    return Ok(());
                };
                let level = |(price, quantity)| DepthLevel {
                    price,
                    quantity,
                    order_count: None,
                };
                let quote = TopOfBookQuote {
                    metadata: metadata(instrument, epoch, self.ordinal, time_nanos)?,
                    bid: bid.map(level),
                    ask: ask.map(level),
                };
                quote.validate().map_err(|e| e.to_string())?;
                self.publish(RealtimeEvent::Quote(epoch, quote))?;
            }
            FeedEvent::Trade {
                channel: 1,
                symbol,
                index,
                kind,
                flags,
                trade,
                ..
            } => self.accept_trade(symbol, index, &kind, flags, trade)?,
            FeedEvent::Candle {
                channel: 3,
                symbol,
                flags,
                bar,
                ..
            } => self.accept_candle(&symbol, flags, bar)?,
            _ => {}
        }
        Ok(())
    }
    fn accept_trade(
        &mut self,
        symbol: String,
        index: String,
        kind: &str,
        flags: u32,
        print: Option<TradePrint>,
    ) -> Result<(), String> {
        let Some(instrument) = self
            .demand
            .instruments
            .iter()
            .find(|instrument| instrument.provider_symbol == symbol)
            .cloned()
        else {
            return Ok(());
        };
        let trade = if kind == "CANCEL" || flags & REMOVE_EVENT != 0 {
            None
        } else {
            print
                .map(|print| {
                    market_trade(
                        &instrument,
                        self.epoch(),
                        self.ordinal,
                        index.clone(),
                        &print,
                    )
                })
                .transpose()?
                .flatten()
        };
        let batch = self.trade_batches.entry(symbol).or_default();
        if flags & SNAPSHOT_BEGIN != 0 {
            batch.0 = true;
            batch.1.clear();
        }
        if batch.1.len() >= MAXIMUM_TICK_HISTORY {
            return Err("Tastytrade live transaction exceeded its bound".into());
        }
        batch.1.push(IndexedTradeMutation { index, trade });
        if flags & (SNAPSHOT_END | SNAPSHOT_SNIP) != 0 {
            batch.0 = false;
        }
        if !batch.0 && flags & TX_PENDING == 0 {
            let changes = std::mem::take(&mut batch.1);
            self.publish(RealtimeEvent::Trades(self.epoch(), instrument, changes))?;
        }
        Ok(())
    }
    fn accept_candle(
        &mut self,
        symbol: &str,
        flags: u32,
        bar: Option<MarketBar>,
    ) -> Result<(), String> {
        if !self.demand.series.iter().any(|(series, instrument)| {
            candle_symbol(series, instrument).is_ok_and(|expected| expected == symbol)
        }) {
            return Ok(());
        }
        let batch = self.candle_batches.entry(symbol.to_string()).or_default();
        if flags & SNAPSHOT_BEGIN != 0 {
            batch.0 = true;
            batch.1.clear();
        }
        if flags & REMOVE_EVENT != 0 && bar.is_some() {
            batch.1.clear();
            batch.0 = false;
            return self.publish(RealtimeEvent::CandleRecovery(
                self.epoch(),
                symbol.to_string(),
            ));
        }
        if let Some(bar) = bar {
            if batch.1.len() >= LIVE_BUFFER_CAPACITY {
                return Err("Tastytrade candle transaction exceeded its bound".into());
            }
            batch.1.push(bar);
        }
        if flags & (SNAPSHOT_END | SNAPSHOT_SNIP) != 0 {
            batch.0 = false;
        }
        if !batch.0 && flags & TX_PENDING == 0 {
            let mut bars = std::mem::take(&mut batch.1);
            bars.sort_unstable_by_key(|bar| bar.exchange_timestamp_unix_nanos);
            for bar in bars {
                self.publish(RealtimeEvent::Candle(self.epoch(), symbol.to_string(), bar))?;
            }
        }
        Ok(())
    }
    fn queue_completion(
        &mut self,
        request: HistoryRequest,
        result: Result<HistorySnapshot, String>,
    ) {
        // No new history is accepted while completions wait. At most two tasks can finish.
        self.completions.push_back(Command::HistoryCompleted(
            request.series,
            request.provider_generation,
            request.range,
            result,
        ));
    }
    fn flush_completions(&mut self) {
        for _ in 0..HISTORY_CAPACITY {
            let Some(command) = self.completions.pop_front() else {
                break;
            };
            match self.ports.completions.try_send(command) {
                Ok(()) => {}
                Err(TrySendError::Full(command)) => {
                    self.completions.push_front(command);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.ports.stop.store(true, Ordering::Release);
                    break;
                }
            }
        }
    }
    fn cancel_histories(&mut self, detail: &str) {
        for (_, task) in std::mem::take(&mut self.histories) {
            if task.candle_state != CandleHistoryState::Published {
                self.queue_completion(task.request, Err(detail.into()));
            }
        }
    }
    fn recover(&mut self, error: String) {
        self.socket = None;
        self.pending_token = None;
        self.trade_batches.clear();
        self.candle_batches.clear();
        self.cancel_histories(&error);
        self.failures = self.failures.saturating_add(1);
        self.paused = self.failures >= 5;
        self.retry_at =
            Instant::now() + Duration::from_secs(3u64.saturating_mul(1u64 << self.failures.min(4)));
        let detail = if self.paused {
            format!("{error}. Automatic retry limit reached; connect tastytrade to retry.")
        } else {
            error
        };
        let _ = self.publish(RealtimeEvent::Recovering(self.epoch(), detail));
    }
}

pub(super) fn start_record(
    completions: &SyncSender<Command>,
    wake: ProviderCoordinatorWake,
    activity: &Arc<Mutex<BTreeSet<String>>>,
    api: &Arc<BrokerApi>,
) -> Result<ProviderRuntimeRecord, String> {
    let cancellation = Arc::new(AtomicBool::new(false));
    let lifecycle = Arc::new(ProviderRuntimeLifecycle::default());
    let generation = Arc::new(AtomicU64::new(1));
    let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (controls, controls_rx) = mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
    let (events_tx, events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let (catalog_controls, catalog_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (catalog_tx, catalog_events) = mpsc::sync_channel(COMMAND_CAPACITY);
    let catalog = CatalogPublisher::new(catalog_tx, 2, wake.clone());
    let worker_stop = Arc::clone(&cancellation);
    let worker_generation = Arc::clone(&generation);
    let worker_api = Arc::clone(api);
    let worker_activity = Arc::clone(activity);
    let catalog_worker = thread::Builder::new()
        .name("aeris-tastytrade-catalog".into())
        .spawn(move || {
            let _guard = ActiveWorkerGuard::register("aeris-tastytrade-catalog", worker_activity);
            run_catalog(
                &catalog_rx,
                &catalog,
                &worker_generation,
                &worker_stop,
                &worker_api,
            );
        })
        .map_err(|error| error.to_string())?;
    let (token_requests, requests) = mpsc::sync_channel(1);
    let (replies, token_replies) = mpsc::sync_channel(1);
    let token_stop = Arc::clone(&cancellation);
    let token_api = Arc::clone(api);
    let token_activity = Arc::clone(activity);
    let token_worker = match thread::Builder::new()
        .name("aeris-tastytrade-tokens".into())
        .spawn(move || {
            let _guard = ActiveWorkerGuard::register("aeris-tastytrade-tokens", token_activity);
            run_tokens(&requests, &replies, &token_api, &token_stop);
        }) {
        Ok(worker) => worker,
        Err(error) => {
            cancellation.store(true, Ordering::Release);
            let _ = catalog_worker.join();
            return Err(error.to_string());
        }
    };
    let worker_stop = Arc::clone(&cancellation);
    let worker_activity = Arc::clone(activity);
    let completions = completions.clone();
    let provider_worker = match thread::Builder::new()
        .name("aeris-tastytrade-provider".into())
        .spawn(move || {
            let _guard = ActiveWorkerGuard::register("aeris-tastytrade-provider", worker_activity);
            Worker::new(WorkerPorts {
                controls: controls_rx,
                events: events_tx,
                history: history_rx,
                completions,
                generation,
                stop: worker_stop,
                wake,
                token_requests,
                token_replies,
            })
            .run();
        }) {
        Ok(worker) => worker,
        Err(error) => {
            cancellation.store(true, Ordering::Release);
            let _ = catalog_worker.join();
            let _ = token_worker.join();
            return Err(error.to_string());
        }
    };
    Ok(ProviderRuntimeRecord {
        history: history_tx,
        cancellation,
        lifecycle,
        realtime: ProviderRealtimeChannels {
            enabled: true,
            channels: ProviderRealtimeChannelSet::Tastytrade { controls, events },
        },
        catalog: ProviderCatalogChannels {
            enabled: true,
            channels: ProviderCatalogChannelSet::Tastytrade {
                controls: catalog_controls,
                events: catalog_events,
            },
        },
        workers: vec![catalog_worker, token_worker, provider_worker],
    })
}

impl Coordinator<'_> {
    pub(super) fn handle_tastytrade_catalog(&mut self, event: CatalogEvent) {
        if self.tastytrade_suspended {
            return;
        }
        match event {
            CatalogEvent::Search(search) => self.handle_catalog_search(search),
            CatalogEvent::Selection {
                consumer_id,
                command_generation,
                instrument,
            } => self.handle_catalog_selection(consumer_id, command_generation, instrument),
            CatalogEvent::Rejected {
                rejection,
                selection,
            } => self.handle_catalog_rejection(rejection, selection),
        }
    }
    pub(super) fn flush_tastytrade_demand(&mut self) {
        if let Some(generation) = self.tastytrade_recovery {
            if let Some(record) = self.providers.records.get("tastytrade")
                && let ProviderRealtimeDispatch::Tastytrade { controls, .. } = &record.realtime
                && controls
                    .try_send(RealtimeControl::Recover(generation))
                    .is_ok()
            {
                self.tastytrade_recovery = None;
            }
            return;
        }
        if let Some(ready) = self.tastytrade_authorization {
            if let Some(record) = self.providers.records.get("tastytrade")
                && let ProviderRealtimeDispatch::Tastytrade { controls, .. } = &record.realtime
                && controls
                    .try_send(RealtimeControl::AuthorizationChanged(ready))
                    .is_ok()
            {
                self.tastytrade_authorization = None;
            }
            return;
        }
        if self.tastytrade_suspended {
            return;
        }
        let mut demand = Demand::default();
        let mut instruments = BTreeMap::new();
        for series in self
            .candle_live
            .keys()
            .filter(|series| series.provider_id == "tastytrade")
        {
            if !self.engine.has_subscription(series) {
                continue;
            }
            let Ok(instrument) = self.candle_instrument(series) else {
                continue;
            };
            demand.series.push((series.clone(), instrument.clone()));
            instruments.insert(instrument.instrument_id.clone(), instrument.clone());
        }
        for instrument in self.price_alerts.active_instruments("tastytrade") {
            instruments.insert(instrument.instrument_id.clone(), instrument);
        }
        demand.instruments = instruments.into_values().collect();
        if self.tastytrade_demand.as_ref() == Some(&demand) {
            return;
        }
        let Some(record) = self.providers.records.get("tastytrade") else {
            return;
        };
        let ProviderRealtimeDispatch::Tastytrade { controls, .. } = &record.realtime else {
            return;
        };
        if controls
            .try_send(RealtimeControl::Subscribe(demand.clone()))
            .is_ok()
        {
            self.tastytrade_demand = Some(demand);
        }
    }
    pub(super) fn handle_tastytrade_realtime(&mut self, event: RealtimeEvent) {
        if self.tastytrade_suspended || event.generation() < self.tastytrade_generation_floor {
            return;
        }
        let generation = event.generation();
        if self
            .providers
            .wake
            .is_some_and(|wake| wake.overflowed(2, generation))
        {
            return;
        }
        match event {
            RealtimeEvent::Connecting(generation) => {
                self.candle_provider_connecting("tastytrade", generation);
            }
            RealtimeEvent::Connected(generation) => {
                self.candle_provider_online("tastytrade", generation);
            }
            RealtimeEvent::Recovering(generation, detail) => {
                self.candle_provider_recovering(
                    "tastytrade",
                    generation,
                    "Tastytrade feed requires recovery",
                );
                self.broadcast_provider_for("tastytrade", Some(&detail));
            }
            RealtimeEvent::Disconnected(generation) => {
                if let Ok(generation) = id(generation).map(ProviderGeneration) {
                    let _ = self.engine.end_provider_session("tastytrade", generation);
                }
                self.broadcast_provider_for("tastytrade", None);
            }
            RealtimeEvent::Quote(generation, quote) => {
                self.provider_quote("tastytrade", generation, &quote);
            }
            RealtimeEvent::CandleRecovery(generation, symbol) => {
                self.recover_tastytrade_candle(generation, &symbol);
            }
            RealtimeEvent::Candle(generation, symbol, bar) => {
                let Ok(generation) = id(generation).map(ProviderGeneration) else {
                    return;
                };
                if self
                    .engine
                    .provider_status("tastytrade")
                    .and_then(|status| status.generation)
                    != Some(generation)
                {
                    return;
                }
                let failed = self
                    .candle_live
                    .iter_mut()
                    .filter(|(series, live)| {
                        series.provider_id == "tastytrade"
                            && live.generation == generation
                            && format!("{}{{={}}}", live.wire_coin, live.interval) == symbol
                    })
                    .filter_map(|(series, live)| {
                        live.accept_bar(bar).is_err().then(|| series.clone())
                    })
                    .collect::<Vec<_>>();
                for series in failed {
                    self.candle_series_recovering(
                        &series,
                        generation,
                        FailureStage::Aggregation,
                        "Tastytrade candle correction requires covering history",
                    );
                }
            }
            RealtimeEvent::Trades(generation, instrument, changes) => {
                self.accept_tastytrade_trades(generation, &instrument, &changes);
            }
            RealtimeEvent::Tape(generation, instrument, trades, truncated) => {
                self.accept_tastytrade_history(generation, &instrument, &trades, truncated);
            }
        }
    }
    fn accept_tastytrade_history(
        &mut self,
        generation: u64,
        instrument: &InstallProviderInstrument,
        trades: &[MarketTrade],
        truncated: bool,
    ) {
        if self
            .engine
            .provider_status("tastytrade")
            .and_then(|status| status.generation)
            .map(|g| g.0.get())
            != Some(generation)
        {
            return;
        }
        if let Some(book) = self
            .order_books
            .get_mut(&("tastytrade".into(), instrument.instrument_id.clone()))
        {
            for trade in trades {
                if book
                    .replace_indexed_trade(&trade.trade_id, Some(trade), true)
                    .is_err()
                {
                    self.request_tastytrade_recovery(generation);
                    return;
                }
            }
        }
        if let Some(observed) = trades
            .iter()
            .filter_map(|trade| trade.metadata.timestamps.exchange_unix_nanos)
            .max()
        {
            self.publish_non_bar_study_change(
                "tastytrade",
                &instrument.instrument_id,
                &instrument.entitlement_id,
                super::MarketStream::Trades,
                observed,
            );
        }
        self.broadcast_order_book("tastytrade", &instrument.instrument_id);
        if truncated {
            self.broadcast_provider_for("tastytrade", Some("Available tick history is partial"));
        }
    }
    fn recover_tastytrade_candle(&mut self, generation: u64, symbol: &str) {
        let Ok(generation) = id(generation).map(ProviderGeneration) else {
            return;
        };
        let series: Vec<_> = self
            .candle_live
            .iter()
            .filter(|(series, live)| {
                series.provider_id == "tastytrade"
                    && live.generation == generation
                    && format!("{}{{={}}}", live.wire_coin, live.interval) == symbol
            })
            .map(|(series, _)| series.clone())
            .collect();
        for series in series {
            self.candle_series_recovering(
                &series,
                generation,
                FailureStage::Aggregation,
                "Candle correction is reloading the available history",
            );
        }
    }
    fn accept_tastytrade_trades(
        &mut self,
        generation: u64,
        instrument: &InstallProviderInstrument,
        changes: &[IndexedTradeMutation],
    ) {
        if self
            .engine
            .provider_status("tastytrade")
            .and_then(|status| status.generation)
            .map(|g| g.0.get())
            != Some(generation)
        {
            return;
        }
        let mut failed = false;
        if let Some(book) = self
            .order_books
            .get_mut(&("tastytrade".into(), instrument.instrument_id.clone()))
        {
            for change in changes {
                if book
                    .replace_indexed_trade(&change.index, change.trade.as_ref(), false)
                    .is_err()
                {
                    failed = true;
                    break;
                }
            }
        }
        if failed {
            self.request_tastytrade_recovery(generation);
            return;
        }
        for change in changes {
            if let Some(trade) = &change.trade {
                self.evaluate_price_alert_trade(trade);
            }
        }
        if let Some(observed) = changes
            .iter()
            .filter_map(|change| change.trade.as_ref())
            .filter_map(|trade| trade.metadata.timestamps.exchange_unix_nanos)
            .max()
        {
            self.publish_non_bar_study_change(
                "tastytrade",
                &instrument.instrument_id,
                &instrument.entitlement_id,
                super::MarketStream::Trades,
                observed,
            );
        }
        self.broadcast_order_book("tastytrade", &instrument.instrument_id);
    }
    fn request_tastytrade_recovery(&mut self, generation: u64) {
        self.tastytrade_recovery = Some(generation);
        self.candle_provider_recovering(
            "tastytrade",
            generation,
            "Tick state is reloading available history",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn instrument() -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: "tastytrade".into(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: "tastytrade:Future:/ESZ6".into(),
            provider_symbol: "/ESZ26:XCME".into(),
            display_symbol: "/ESZ6".into(),
            venue_id: "CME".into(),
            price_scale: 8,
            quantity_scale: 8,
            entitlement_id: ENTITLEMENT.into(),
            price_increment: Some(25_000_000),
            contract_metadata: None,
        }
    }
    fn request() -> HistoryRequest {
        let instrument = instrument();
        HistoryRequest {
            series: BarSeriesKey {
                provider_id: instrument.provider.clone(),
                instrument_id: instrument.instrument_id.clone(),
                entitlement_id: ENTITLEMENT.into(),
                period: BarPeriod::time(60).unwrap(),
                definition_version: 1,
            },
            provider_generation: ProviderGeneration(id(1).unwrap()),
            instrument: Some(instrument),
            maximum_bars: 1600,
            range: None,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
    fn bar() -> MarketBar {
        MarketBar {
            source_sequence: 1,
            exchange_timestamp_seconds: 1_790_800_020,
            exchange_timestamp_unix_nanos: 1_790_800_020_000_000_000,
            open: 500_000_000_000,
            high: 500_000_000_000,
            low: 500_000_000_000,
            close: 500_000_000_000,
            volume: 100_000_000,
        }
    }
    #[test]
    fn candle_history_waits_for_atomic_transaction_and_cannot_complete_twice() {
        let request = request();
        let symbol = candle_symbol(&request.series, request.instrument.as_ref().unwrap()).unwrap();
        let mut task = HistoryTask {
            request,
            symbol: symbol.clone(),
            deadline: Instant::now() + Duration::from_secs(45),
            candles: BTreeMap::new(),
            candle_state: CandleHistoryState::Collecting,
            trades: BTreeMap::new(),
            tape_complete: false,
            tape_end_seen: false,
            tape_truncated: false,
        };
        let event = |flags, bar| FeedEvent::Candle {
            channel: 7,
            symbol: symbol.clone(),
            flags,
            index: "9007199254740993".into(),
            bar,
        };
        task.accept(event(SNAPSHOT_BEGIN, Some(bar())), 1, &mut 1)
            .unwrap();
        task.accept(event(SNAPSHOT_END | TX_PENDING, None), 1, &mut 1)
            .unwrap();
        assert!(task.candle_state == CandleHistoryState::Ending);
        task.accept(event(0, Some(bar())), 1, &mut 1).unwrap();
        assert!(task.candle_state == CandleHistoryState::Ready);
        assert_eq!(task.snapshot().unwrap().forming.unwrap().bar, bar());
        task.candle_state = CandleHistoryState::Published;
        task.accept(event(SNAPSHOT_BEGIN, None), 1, &mut 1).unwrap();
        assert!(task.candle_state == CandleHistoryState::Published);
        assert_eq!(task.candles.len(), 1);
    }
    #[test]
    fn live_snapshot_end_marker_publishes_the_batch_and_retired_symbols_are_ignored() {
        let (_, controls) = mpsc::sync_channel(1);
        let (events, output) = mpsc::sync_channel(4);
        let (_, history) = mpsc::sync_channel(1);
        let (completions, _) = mpsc::sync_channel(1);
        let (token_requests, _) = mpsc::sync_channel(1);
        let (_, token_replies) = mpsc::sync_channel(1);
        let wake = ProviderCoordinatorWake::new(completions.clone());
        let mut worker = Worker::new(WorkerPorts {
            controls,
            events,
            history,
            completions,
            generation: Arc::new(AtomicU64::new(1)),
            stop: Arc::new(AtomicBool::new(false)),
            wake,
            token_requests,
            token_replies,
        });
        let request = request();
        let instrument = request.instrument.unwrap();
        let symbol = candle_symbol(&request.series, &instrument).unwrap();
        worker.demand.series.push((request.series, instrument));
        worker
            .accept_candle(&symbol, SNAPSHOT_BEGIN, Some(bar()))
            .unwrap();
        assert!(output.try_recv().is_err());
        worker
            .accept_candle(&symbol, SNAPSHOT_END | REMOVE_EVENT, None)
            .unwrap();
        assert!(matches!(
            output.try_recv().unwrap(),
            RealtimeEvent::Candle(1, _, _)
        ));
        worker.accept_candle("retired{=m}", 0, Some(bar())).unwrap();
        assert!(output.try_recv().is_err());
        assert!(worker.socket.is_none());
    }
}
