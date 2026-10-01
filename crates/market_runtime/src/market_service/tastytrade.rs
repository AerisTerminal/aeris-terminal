//! Runtime-owned tastytrade catalog, multiplexed stream, and on-demand history.
use super::{
    ActiveWorkerGuard, Arc, AtomicBool, AtomicU64, BTreeMap, BTreeSet, BarPeriod, BarSeriesKey,
    COMMAND_CAPACITY, CatalogPublisher, Command, Coordinator, Duration, FormingBar,
    HISTORY_CAPACITY, HistoryRequest, HistorySnapshot, IndexedTradeKind, IndexedTradeMutation,
    InstallProviderInstrument, Instant, LIVE_BUFFER_CAPACITY, MAXIMUM_CONSUMERS, MarketBar,
    MarketTrade, Mutex, Ordering, ProviderCatalogChannelSet, ProviderCatalogChannels,
    ProviderCatalogRejected, ProviderCatalogRejectionReason, ProviderCoordinatorWake,
    ProviderRealtimeChannelSet, ProviderRealtimeChannels, ProviderRuntimeLifecycle,
    ProviderRuntimeRecord, REALTIME_CAPACITY, RITHMIC_REALTIME_CONTROL_CAPACITY, Receiver,
    RecvTimeoutError, SearchProviderInstruments, SelectProviderInstrument, SyncSender,
    TopOfBookQuote, TrySendError, VecDeque, broker_authorization, mpsc, thread,
};
#[cfg(test)]
use super::{ProviderGeneration, id};
use aeris_contracts::{
    ProviderContractMetadata, ProviderInstrumentSearchResult, ProviderInstrumentSummary,
};
use aeris_market_data::{DepthLevel, EventMetadata, QualifiedTimestamp};
use aeris_tastytrade_market_adapter::{
    ConnectionCapability, DATA_SCALE, DxlinkSession, FeedEvent, FutureInstrument, MarketCollection,
    MarketSession, QuoteToken, ResolvedInstrument, SearchInstrument, Subscription,
    SubscriptionChangeBudget, TastytradeBrokerClient, TradePrint,
};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) const ENTITLEMENT: &str = "tastytrade-authorized";
pub(super) const PRESENTATION: aeris_contracts::ProviderPresentationDescriptor =
    aeris_contracts::ProviderPresentationDescriptor {
        id: "tastytrade",
        display_name: "tastytrade",
        chart_interval_labels: &[
            "1m", "3m", "5m", "15m", "30m", "1h", "2h", "4h", "8h", "12h", "1D", "1W", "1M",
        ],
        default_listing: "/ES",
        search_hint: "Search futures or equities",
        logo_key: "tastytrade",
        catalog_symbol: aeris_contracts::ProviderCatalogSymbol::DisplaySymbol,
        selection_entitlement_id: "tastytrade-authorized",
        catalog_refresh_on_startup: true,
        depth_available: false,
        connection_kind: aeris_contracts::ProviderConnectionKind::HostedBroker,
    };
pub(super) const DESCRIPTOR: super::ProviderDescriptor = super::ProviderDescriptor {
    id: "tastytrade",
    presentation: &PRESENTATION,
    account_id: ENTITLEMENT,
    capabilities: super::ProviderCapabilities {
        historical_bars: true,
        realtime_bars: true,
        streams: super::StreamRequirements::BARS
            .with(super::MarketStream::Trades)
            .with(super::MarketStream::Quotes),
    },
    reconnect_delay: Duration::from_secs(3),
    gap_policy: super::CandleGapPolicy::SessionGapsAllowed,
    history_source: super::HistorySourceKind::ProviderSession,
    connection_kind: super::ProviderConnectionKind::BrokerCapability,
    recovery_policy: super::ProviderRecoveryPolicy::WorkerReconcilesDemand,
    idle_stop_policy: super::IdleStopPolicy::WorkerManaged,
    alert_demand_update: super::AlertDemandUpdate::WorkerManaged,
    start: super::ProviderRuntimeRegistry::start_tastytrade_runtime,
    flush_demand,
    prepare_search: Some(prepare_search),
    live_model: super::LiveModel::TradeBuilt,
    supported_period: super::tastytrade_supported_period,
    alert_overrides_instrument: true,
    overflow_recovery_detail: "Tastytrade queue overflow requires recovery",
    history_range_policy: super::HistoryRangePolicy::FromTimeOnly,
    trade_continuity: super::TradeContinuity::Indexed,
    candle_requires_connected: false,
    candle_correction_detail: "Tastytrade candle correction requires covering history",
    candle_wire_interval: Some(candle_period),
    candle_demand_policy: super::CandleDemandPolicy::SessionManaged,
    trade_demand_policy: super::TradeDemandPolicy::SessionManaged,
    instrument_missing_detail: "Tastytrade instrument is not installed",
};

fn flush_demand(coordinator: &mut Coordinator<'_>) {
    coordinator.flush_session_managed_demand(DESCRIPTOR.id);
}

fn prepare_search(api: &BrokerApi, consumer: u64, generation: u64) -> Result<(), String> {
    api.register_search(consumer, generation)
}

const SNAPSHOT_BEGIN: u32 = 4;
const SNAPSHOT_END: u32 = 8;
const SNAPSHOT_SNIP: u32 = 16;
const REMOVE_EVENT: u32 = 2;
const TX_PENDING: u32 = 1;
const MAXIMUM_TICK_HISTORY: usize = 65_536;
const MAXIMUM_CONCURRENT_CANDLE_HISTORY: usize = 4;

pub(super) enum CatalogControl {
    Search(SearchProviderInstruments),
    Select(SelectProviderInstrument),
}
pub(super) enum CatalogEvent {
    Search(ProviderInstrumentSearchResult),
    SearchPreliminary(ProviderInstrumentSearchResult),
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
struct RemoteSearchRequest {
    search: SearchProviderInstruments,
    authorization_epoch: u64,
}
struct RemoteSearchReply {
    request: RemoteSearchRequest,
    result: Result<Vec<SearchInstrument>, String>,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Demand {
    pub series: Vec<(BarSeriesKey, InstallProviderInstrument)>,
    pub instruments: Vec<InstallProviderInstrument>,
    pub tape_instruments: Vec<InstallProviderInstrument>,
}
pub(super) enum RealtimeControl {
    Subscribe(Demand),
    Stop,
    AuthorizationChanged(bool),
    Recover(u64),
}
pub(super) enum RealtimeEvent {
    Connecting(u64),
    Connected(u64),
    Recovering(u64, String),
    Disconnected(u64),
    Candle(u64, String, MarketBar, u64, u64),
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

/// One REST lane shares provider credentials and request limits across workers.
#[derive(Default)]
pub(super) struct BrokerApi {
    state: Mutex<BrokerApiState>,
    futures_sessions: Mutex<Option<(Instant, [MarketSession; 2])>>,
    equity_session: Mutex<Option<(Instant, MarketSession)>>,
    search_control: Mutex<BTreeMap<u64, (u64, Arc<AtomicBool>)>>,
    authorization_epoch: AtomicU64,
}
#[derive(Default)]
struct BrokerApiState {
    client: TastytradeBrokerClient,
    capability: Option<ConnectionCapability>,
    futures: Option<(Instant, Vec<FutureInstrument>)>,
    searches: BTreeMap<String, (Instant, Vec<SearchInstrument>)>,
    search_order: VecDeque<String>,
    instruments: BTreeMap<(String, String), (Instant, Result<ResolvedInstrument, String>)>,
    instrument_order: VecDeque<(String, String)>,
}
impl BrokerApi {
    pub(super) fn register_search(&self, consumer: u64, generation: u64) -> Result<(), String> {
        let mut searches = self
            .search_control
            .lock()
            .map_err(|_| "Tastytrade search control failed")?;
        if searches
            .get(&consumer)
            .is_some_and(|(current, _)| *current >= generation)
        {
            return Ok(());
        }
        if let Some((_, cancellation)) = searches.remove(&consumer) {
            cancellation.store(true, Ordering::Release);
        }
        if searches.len() >= MAXIMUM_CONSUMERS
            && let Some((_, (_, cancellation))) = searches.pop_first()
        {
            cancellation.store(true, Ordering::Release);
        }
        searches.insert(consumer, (generation, Arc::new(AtomicBool::new(false))));
        Ok(())
    }
    pub(super) fn cancel_searches(&self) {
        if let Ok(mut searches) = self.search_control.lock() {
            for (_, (_, cancellation)) in std::mem::take(&mut *searches) {
                cancellation.store(true, Ordering::Release);
            }
        }
    }
    fn call<T>(
        &self,
        stop: &Arc<AtomicBool>,
        operation: impl FnOnce(&mut TastytradeBrokerClient, &ConnectionCapability) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Tastytrade request lane failed")?;
        if stop.load(Ordering::Acquire) {
            return Err("Tastytrade request cancelled".into());
        }
        if state.capability.is_none() {
            state.capability = Some(broker_authorization::load_connection()?);
        }
        let BrokerApiState {
            client, capability, ..
        } = &mut *state;
        operation(
            client,
            capability.as_ref().ok_or("Tastytrade connection missing")?,
        )
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
        if stop.load(Ordering::Acquire) {
            return Err("Tastytrade request cancelled".into());
        }
        operation(&mut state.client)
    }
    pub(super) fn clear(&self) -> Result<(), String> {
        self.cancel_searches();
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Tastytrade request lane failed")?;
        state.client.clear_access_token();
        state.capability = None;
        state.futures = None;
        state.searches.clear();
        state.search_order.clear();
        state.instruments.clear();
        state.instrument_order.clear();
        let mut sessions = self
            .futures_sessions
            .lock()
            .map_err(|_| "Tastytrade session cache failed")?;
        self.authorization_epoch.fetch_add(1, Ordering::AcqRel);
        *sessions = None;
        *self
            .equity_session
            .lock()
            .map_err(|_| "Tastytrade session cache failed")? = None;
        Ok(())
    }
    fn search(
        &self,
        consumer: u64,
        generation: u64,
        query: &str,
        stop: &Arc<AtomicBool>,
    ) -> Result<Vec<SearchInstrument>, String> {
        let cancellation = {
            let mut searches = self
                .search_control
                .lock()
                .map_err(|_| "Tastytrade search control failed")?;
            match searches.get(&consumer) {
                Some((current, cancellation)) if *current == generation => Arc::clone(cancellation),
                Some((current, _)) if *current > generation => {
                    return Err("Tastytrade search superseded".into());
                }
                _ => {
                    if let Some((_, prior)) = searches.remove(&consumer) {
                        prior.store(true, Ordering::Release);
                    }
                    if searches.len() >= MAXIMUM_CONSUMERS
                        && let Some((_, (_, prior))) = searches.pop_first()
                    {
                        prior.store(true, Ordering::Release);
                    }
                    let cancellation = Arc::new(AtomicBool::new(false));
                    searches.insert(consumer, (generation, Arc::clone(&cancellation)));
                    cancellation
                }
            }
        };
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Tastytrade request lane failed")?;
        if stop.load(Ordering::Acquire) || cancellation.load(Ordering::Acquire) {
            return Err("Tastytrade request cancelled".into());
        }
        if let Some((fetched, items)) = state.searches.get(query)
            && fetched.elapsed() < Duration::from_secs(30)
        {
            let items = items.clone();
            state.search_order.retain(|entry| entry != query);
            state.search_order.push_back(query.to_string());
            return Ok(items);
        }
        if state.capability.is_none() {
            state.capability = Some(broker_authorization::load_connection()?);
        }
        let BrokerApiState {
            client, capability, ..
        } = &mut *state;
        let items = client.search(
            capability.as_ref().ok_or("Tastytrade connection missing")?,
            query,
            &cancellation,
        )?;
        if stop.load(Ordering::Acquire) || cancellation.load(Ordering::Acquire) {
            return Err("Tastytrade search superseded".into());
        }
        state.search_order.retain(|entry| entry != query);
        if state.search_order.len() >= 32
            && let Some(oldest) = state.search_order.pop_front()
        {
            state.searches.remove(&oldest);
        }
        state.search_order.push_back(query.to_string());
        state
            .searches
            .insert(query.to_string(), (Instant::now(), items.clone()));
        Ok(items)
    }
    pub(super) fn futures(&self, stop: &Arc<AtomicBool>) -> Result<Vec<FutureInstrument>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Tastytrade request lane failed")?;
        if let Some((fetched, futures)) = &state.futures
            && fetched.elapsed() < Duration::from_mins(30)
        {
            return Ok(futures.clone());
        }
        if state.capability.is_none() {
            state.capability = Some(broker_authorization::load_connection()?);
        }
        let BrokerApiState {
            client, capability, ..
        } = &mut *state;
        let futures = client.active_futures(
            capability.as_ref().ok_or("Tastytrade connection missing")?,
            stop,
        )?;
        state.futures = Some((Instant::now(), futures.clone()));
        Ok(futures)
    }
    fn instrument(
        &self,
        instrument: &SearchInstrument,
        stop: &Arc<AtomicBool>,
    ) -> Result<ResolvedInstrument, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Tastytrade request lane failed")?;
        if stop.load(Ordering::Acquire) {
            return Err("Tastytrade request cancelled".into());
        }
        let key = (
            instrument.instrument_type.clone(),
            instrument.symbol.clone(),
        );
        if let Some((fetched, resolved)) = state.instruments.get(&key)
            && fetched.elapsed()
                < if resolved.is_ok() {
                    Duration::from_mins(30)
                } else {
                    Duration::from_secs(1)
                }
        {
            let resolved = resolved.clone();
            state.instrument_order.retain(|entry| entry != &key);
            state.instrument_order.push_back(key);
            return resolved;
        }
        if state.capability.is_none() {
            state.capability = Some(broker_authorization::load_connection()?);
        }
        let BrokerApiState {
            client, capability, ..
        } = &mut *state;
        let resolved = client.instrument(
            capability.as_ref().ok_or("Tastytrade connection missing")?,
            instrument,
            stop,
        );
        if stop.load(Ordering::Acquire) {
            return Err("Tastytrade request cancelled".into());
        }
        state.instrument_order.retain(|entry| entry != &key);
        if state.instrument_order.len() >= 64
            && let Some(oldest) = state.instrument_order.pop_front()
        {
            state.instruments.remove(&oldest);
        }
        state.instrument_order.push_back(key.clone());
        state
            .instruments
            .insert(key, (Instant::now(), resolved.clone()));
        resolved
    }
    fn futures_catalog_ready(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.futures.is_some())
    }
    fn connection_ready(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.capability.is_some())
    }
    fn refresh_futures_sessions(&self, stop: &Arc<AtomicBool>) -> Result<(), String> {
        let epoch = self.authorization_epoch.load(Ordering::Acquire);
        let sessions = self.call(stop, |client, capability| {
            client.current_futures_sessions(capability, stop)
        })?;
        let mut cache = self
            .futures_sessions
            .lock()
            .map_err(|_| "Tastytrade session cache failed")?;
        if self.authorization_epoch.load(Ordering::Acquire) != epoch {
            return Err("Tastytrade futures calendar request retired".into());
        }
        *cache = Some((Instant::now(), sessions));
        Ok(())
    }
    fn refresh_equity_session(&self, stop: &Arc<AtomicBool>) -> Result<(), String> {
        let epoch = self.authorization_epoch.load(Ordering::Acquire);
        let session = self.call(stop, |client, capability| {
            client.current_equity_session(capability, stop)
        })?;
        let mut cache = self
            .equity_session
            .lock()
            .map_err(|_| "Tastytrade equity session cache failed")?;
        if self.authorization_epoch.load(Ordering::Acquire) != epoch {
            return Err("Tastytrade equity calendar request retired".into());
        }
        *cache = Some((Instant::now(), session));
        Ok(())
    }
    fn market_session(&self, instrument: &InstallProviderInstrument) -> Option<MarketSession> {
        let collection = market_collection(instrument)?;
        if collection == MarketCollection::Equity {
            return self.equity_session.lock().ok().and_then(|session| {
                session
                    .as_ref()
                    .filter(|(fetched, _)| fetched.elapsed() < Duration::from_mins(6))
                    .map(|(_, session)| *session)
            });
        }
        self.futures_sessions.lock().ok().and_then(|sessions| {
            sessions
                .as_ref()
                .filter(|(fetched, _)| fetched.elapsed() < Duration::from_mins(6))
                .and_then(|(_, sessions)| {
                    sessions
                        .iter()
                        .find(|session| session.collection == collection)
                        .copied()
                })
        })
    }
}

fn market_collection(instrument: &InstallProviderInstrument) -> Option<MarketCollection> {
    match instrument.venue_id.as_str() {
        "CME" => Some(MarketCollection::Cme),
        "CFE" => Some(MarketCollection::Cfe),
        _ if instrument.instrument_id.starts_with("tastytrade:Equity:")
            || instrument.instrument_id.starts_with("tastytrade:Index:") =>
        {
            Some(MarketCollection::Equity)
        }
        _ => None,
    }
}

pub(super) fn run_catalog(
    controls: &Receiver<CatalogControl>,
    events: &CatalogPublisher<CatalogEvent>,
    generation: &Arc<AtomicU64>,
    stop: &Arc<AtomicBool>,
    api: &Arc<BrokerApi>,
) {
    let (remote_tx, remote_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (reply_tx, replies) = mpsc::sync_channel(COMMAND_CAPACITY);
    let remote_api = Arc::clone(api);
    let remote_stop = Arc::clone(stop);
    let remote_worker = thread::Builder::new()
        .name("aeris-tastytrade-equity-search".into())
        .spawn(move || run_remote_search(&remote_rx, &reply_tx, &remote_api, &remote_stop));
    let Ok(remote_worker) = remote_worker else {
        eprintln!("Aeris tastytrade equity search worker could not start");
        return;
    };
    let mut searches = BTreeMap::<u64, (u64, Vec<SearchInstrument>)>::new();
    let mut futures = api.futures(stop).unwrap_or_default();
    let mut authorization_epoch = api.authorization_epoch.load(Ordering::Acquire);
    let mut refresh_at = Instant::now() + Duration::from_mins(30);
    let mut selection_generation = 0u64;
    while !stop.load(Ordering::Acquire) {
        let current_epoch = api.authorization_epoch.load(Ordering::Acquire);
        if current_epoch != authorization_epoch {
            authorization_epoch = current_epoch;
            futures.clear();
            searches.clear();
            refresh_at = Instant::now() + Duration::from_mins(30);
        }
        refresh_catalog(&mut futures, &mut refresh_at, api, stop);
        while let Ok(reply) = replies.try_recv() {
            if let Some(event) = remote_search_event(reply, &mut searches, generation, api)
                && events.send(event).is_err()
            {
                break;
            }
        }
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
                let needs_remote = !search.query.is_empty() && !search.query.starts_with('/');
                let result =
                    search_catalog(&search, &mut futures, &mut searches, api, stop, generation);
                if result.is_ok() && needs_remote {
                    remote_tx
                        .try_send(RemoteSearchRequest {
                            search,
                            authorization_epoch,
                        })
                        .map_err(|_| "Tastytrade equity search queue is full")?;
                }
                result.map(|event| match event {
                    CatalogEvent::Search(result) if needs_remote => {
                        CatalogEvent::SearchPreliminary(result)
                    }
                    other => other,
                })
            }
            CatalogControl::Select(selection) => resolve_selection(
                &selection,
                &futures,
                &searches,
                api,
                stop,
                generation,
                &mut selection_generation,
            ),
        })();
        if api.authorization_epoch.load(Ordering::Acquire) != authorization_epoch {
            continue;
        }
        if matches!(&result, Ok(CatalogEvent::SearchPreliminary(search)) if search.instruments.is_empty())
        {
            continue;
        }
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
    drop(remote_tx);
    let _ = remote_worker.join();
}
fn refresh_catalog(
    futures: &mut Vec<FutureInstrument>,
    refresh_at: &mut Instant,
    api: &BrokerApi,
    stop: &Arc<AtomicBool>,
) {
    if Instant::now() >= *refresh_at {
        *refresh_at = match api.futures(stop) {
            Ok(updated) => {
                *futures = updated;
                Instant::now() + Duration::from_mins(30)
            }
            Err(_) => Instant::now() + Duration::from_mins(1),
        };
    }
}
fn remote_search_event(
    reply: RemoteSearchReply,
    searches: &mut BTreeMap<u64, (u64, Vec<SearchInstrument>)>,
    generation: &AtomicU64,
    api: &BrokerApi,
) -> Option<CatalogEvent> {
    if reply.request.authorization_epoch != api.authorization_epoch.load(Ordering::Acquire) {
        return None;
    }
    let search = &reply.request.search;
    let (search_generation, items) = searches.get_mut(&search.consumer_id)?;
    if *search_generation != search.search_generation {
        return None;
    }
    let final_result = match reply.result {
        Ok(remote) => {
            for item in remote {
                if items.len() >= 100 {
                    break;
                }
                if !items.iter().any(|existing| {
                    existing.symbol == item.symbol
                        && existing.instrument_type == item.instrument_type
                }) {
                    items.push(item);
                }
            }
            true
        }
        Err(error) => {
            eprintln!("Aeris tastytrade equity search unavailable: {error}");
            !items.is_empty()
        }
    };
    Some(if final_result {
        CatalogEvent::Search(ProviderInstrumentSearchResult {
            consumer_id: search.consumer_id,
            provider: "tastytrade".into(),
            provider_generation: generation.load(Ordering::Acquire),
            search_generation: search.search_generation,
            instruments: items
                .iter()
                .take(search.maximum_results.min(100) as usize)
                .map(instrument_summary)
                .collect(),
        })
    } else {
        CatalogEvent::Rejected {
            rejection: ProviderCatalogRejected {
                consumer_id: search.consumer_id,
                provider: "tastytrade".into(),
                provider_generation: Some(generation.load(Ordering::Acquire)),
                command_generation: search.search_generation,
                reason: ProviderCatalogRejectionReason::DispatchUnavailable,
            },
            selection: false,
        }
    })
}
fn resolve_selection(
    selection: &SelectProviderInstrument,
    futures: &[FutureInstrument],
    searches: &BTreeMap<u64, (u64, Vec<SearchInstrument>)>,
    api: &BrokerApi,
    stop: &Arc<AtomicBool>,
    generation: &AtomicU64,
    selection_generation: &mut u64,
) -> Result<CatalogEvent, String> {
    if selection.entitlement_id != ENTITLEMENT {
        return Err("Tastytrade entitlement changed".into());
    }
    let item = searches
        .get(&selection.consumer_id)
        .filter(|(epoch, _)| *epoch == selection.search_generation)
        .and_then(|(_, items)| {
            items.iter().find(|item| {
                item.symbol == selection.symbol && catalog_venue(item) == selection.exchange
            })
        })
        .ok_or("Tastytrade selection is no longer in the current search")?;
    let resolved = if item.instrument_type == "Future" {
        futures
            .iter()
            .find(|future| future.symbol == item.symbol)
            .map(ResolvedInstrument::from_future)
            .transpose()?
    } else {
        None
    };
    let resolved = match resolved {
        Some(resolved) => resolved,
        None => api.instrument(item, stop)?,
    };
    *selection_generation = selection_generation
        .checked_add(1)
        .ok_or("Tastytrade selection generation overflowed")?;
    Ok(CatalogEvent::Selection {
        consumer_id: selection.consumer_id,
        command_generation: selection.selection_generation,
        instrument: install_resolved(
            resolved,
            generation.load(Ordering::Acquire),
            *selection_generation,
        ),
    })
}
fn run_remote_search(
    requests: &Receiver<RemoteSearchRequest>,
    replies: &SyncSender<RemoteSearchReply>,
    api: &BrokerApi,
    stop: &Arc<AtomicBool>,
) {
    let mut session_refresh_at = Instant::now();
    let mut equity_refresh_at = Instant::now();
    while !stop.load(Ordering::Acquire) {
        if Instant::now() >= session_refresh_at && api.futures_catalog_ready() {
            session_refresh_at = match api.refresh_futures_sessions(stop) {
                Ok(()) => Instant::now() + Duration::from_mins(5),
                Err(error) => {
                    eprintln!("Aeris tastytrade futures calendar unavailable: {error}");
                    Instant::now() + Duration::from_secs(30)
                }
            };
        }
        if Instant::now() >= equity_refresh_at && api.connection_ready() {
            equity_refresh_at = match api.refresh_equity_session(stop) {
                Ok(()) => Instant::now() + Duration::from_mins(5),
                Err(error) => {
                    eprintln!("Aeris tastytrade equity calendar unavailable: {error}");
                    Instant::now() + Duration::from_secs(30)
                }
            };
        }
        let first = match requests.recv_timeout(Duration::from_millis(100)) {
            Ok(request) => request,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut pending = BTreeMap::new();
        pending.insert(first.search.consumer_id, first);
        let deadline = Instant::now() + Duration::from_millis(120);
        while let Ok(request) =
            requests.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            pending.insert(request.search.consumer_id, request);
        }
        for (_, request) in pending {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let result = api.search(
                request.search.consumer_id,
                request.search.search_generation,
                &request.search.query,
                stop,
            );
            let mut reply = RemoteSearchReply { request, result };
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
}
fn install_resolved(
    resolved: ResolvedInstrument,
    session_generation: u64,
    selection_generation: u64,
) -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "tastytrade".into(),
        session_generation,
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
        contract_metadata: (resolved.instrument_type == "Future").then(|| {
            Box::new(ProviderContractMetadata {
                point_value: resolved.point_value,
                point_value_scale: resolved.point_value.map(|_| DATA_SCALE),
                currency: resolved.currency,
                contract_expiry: resolved.expiration_date,
                first_notice_date: resolved.first_notice_date,
                last_trade_date: resolved.last_trade_date,
                session_hours: Vec::new(),
            })
        }),
    }
}
fn search_catalog(
    search: &SearchProviderInstruments,
    futures: &mut Vec<FutureInstrument>,
    searches: &mut BTreeMap<u64, (u64, Vec<SearchInstrument>)>,
    api: &BrokerApi,
    stop: &Arc<AtomicBool>,
    generation: &Arc<AtomicU64>,
) -> Result<CatalogEvent, String> {
    if futures.is_empty() {
        *futures = api.futures(stop)?;
    }
    let query = search.query.to_ascii_uppercase();
    let mut matching: Vec<_> = futures
        .iter()
        .filter(|future| {
            future.active
                && (query.is_empty()
                    || future.symbol.to_ascii_uppercase().contains(&query)
                    || future.product_code.to_ascii_uppercase().contains(&query))
        })
        .collect();
    matching.sort_unstable_by(|left, right| {
        (!left.active_month, &left.expiration_date, &left.symbol).cmp(&(
            !right.active_month,
            &right.expiration_date,
            &right.symbol,
        ))
    });
    let items: Vec<SearchInstrument> = matching
        .into_iter()
        .take(100)
        .map(|future| SearchInstrument {
            symbol: future.symbol.clone(),
            instrument_type: "Future".into(),
            exchange: Some(future.exchange.clone()),
            description: Some(future.product_code.clone()),
        })
        .collect();
    let summaries = items
        .iter()
        .take(search.maximum_results.min(100) as usize)
        .map(instrument_summary)
        .collect();
    if !searches.contains_key(&search.consumer_id) && searches.len() >= MAXIMUM_CONSUMERS {
        searches.pop_first();
    }
    searches.insert(search.consumer_id, (search.search_generation, items));
    Ok(CatalogEvent::Search(ProviderInstrumentSearchResult {
        consumer_id: search.consumer_id,
        provider: "tastytrade".into(),
        provider_generation: generation.load(Ordering::Acquire),
        search_generation: search.search_generation,
        instruments: summaries,
    }))
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
            wake.report_overflow("tastytrade", generation);
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
    candles: BTreeMap<String, (MarketBar, u64)>,
    saw_newer_candle: bool,
    candle_state: CandleHistoryState,
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
            vec![Subscription {
                kind: "Candle",
                symbol: symbol.clone(),
                from_time_ms: Some(from / 1_000_000),
            }],
        )?;
        Ok(Self {
            request: request.clone(),
            symbol,
            deadline: Instant::now() + Duration::from_secs(45),
            candles: BTreeMap::new(),
            saw_newer_candle: false,
            candle_state: CandleHistoryState::Collecting,
        })
    }
    fn accept(
        &mut self,
        event: FeedEvent,
        _generation: u64,
        _ordinal: &mut u64,
    ) -> Result<(), String> {
        match event {
            FeedEvent::Candle {
                symbol,
                flags,
                index,
                bar,
                count,
                ..
            } if symbol == self.symbol => {
                if self.candle_state == CandleHistoryState::Published {
                    return Ok(());
                }
                if flags & SNAPSHOT_BEGIN != 0 {
                    self.candles.clear();
                    self.saw_newer_candle = false;
                    self.candle_state = CandleHistoryState::Collecting;
                }
                if flags & REMOVE_EVENT != 0 {
                    self.candles.remove(&index);
                } else if let Some(bar) = bar {
                    bar.validate().map_err(|e| e.to_string())?;
                    if self.request.range.is_some_and(|range| {
                        bar.exchange_timestamp_unix_nanos >= range.end_unix_nanos
                    }) {
                        self.saw_newer_candle = true;
                    }
                    let in_range = self.request.range.is_none_or(|range| {
                        bar.exchange_timestamp_unix_nanos >= range.start_unix_nanos
                            && bar.exchange_timestamp_unix_nanos < range.end_unix_nanos
                    });
                    if in_range {
                        if !self.candles.contains_key(&index) && self.candles.len() >= 16_384 {
                            return Err("Tastytrade candle snapshot exceeded its bound".into());
                        }
                        self.candles.insert(index, (bar, count));
                    }
                }
                if flags & (SNAPSHOT_END | SNAPSHOT_SNIP) != 0 {
                    self.candle_state = CandleHistoryState::Ending;
                }
                if self.candle_state == CandleHistoryState::Ending && flags & TX_PENDING == 0 {
                    self.candle_state = CandleHistoryState::Ready;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn snapshot(&self, session: Option<MarketSession>) -> Result<HistorySnapshot, String> {
        self.snapshot_at(now_nanos()?, session)
    }
    fn snapshot_at(
        &self,
        now: i64,
        session: Option<MarketSession>,
    ) -> Result<HistorySnapshot, String> {
        let mut candles: Vec<_> = self
            .candles
            .values()
            .copied()
            .filter(|(bar, _)| {
                self.request.range.is_none_or(|range| {
                    bar.exchange_timestamp_unix_nanos >= range.start_unix_nanos
                        && bar.exchange_timestamp_unix_nanos < range.end_unix_nanos
                })
            })
            .collect();
        candles.sort_unstable_by_key(|(bar, _)| bar.exchange_timestamp_unix_nanos);
        candles.dedup_by_key(|(bar, _)| bar.exchange_timestamp_unix_nanos);
        let backwards_exhausted = candles.is_empty() && self.saw_newer_candle;
        let mut forming = None;
        if self.request.range.is_none()
            && self
                .request
                .series
                .period
                .duration_nanos()
                .is_some_and(|duration| {
                    candles.last().is_some_and(|(bar, _)| {
                        bar.exchange_timestamp_unix_nanos.saturating_add(duration) > now
                    })
                })
        {
            if let Some(instrument) = &self.request.instrument
                && market_collection(instrument).is_some()
                && session.is_none()
            {
                return Err("Tastytrade market session calendar is unavailable".into());
            }
            if session.is_none_or(|session| session.contains(now))
                && let Some((bar, count)) = candles.pop()
            {
                forming = Some(FormingBar {
                    bar,
                    trades: Some(
                        u32::try_from(count)
                            .map_err(|_| "Tastytrade candle trade count exceeds storage")?,
                    ),
                });
            }
        }
        let mut bars = candles.into_iter().map(|(bar, _)| bar).collect::<Vec<_>>();
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
            // Live events accepted before this snapshot was taken fall on or
            // before this time fence. The shared trade handoff drops that
            // buffered prefix so the forming provider candle and TimeAndSale
            // stream cannot count the same print twice.
            handoff_boundary_unix_nanos: Some(now),
            backwards_exhausted,
        })
    }
}

struct TapeTask {
    instrument: InstallProviderInstrument,
    channel: u64,
    deadline: Instant,
    from_nanos: i64,
    until_nanos: i64,
    trades: BTreeMap<String, MarketTrade>,
    end_seen: bool,
    complete: bool,
    truncated: bool,
}
impl TapeTask {
    fn begin(
        instrument: InstallProviderInstrument,
        channel: u64,
        socket: &mut DxlinkSession,
        from_nanos: i64,
    ) -> Result<Self, String> {
        let until_nanos = now_nanos()?;
        socket.open(
            channel,
            "HISTORY",
            vec![Subscription {
                kind: "TimeAndSale",
                symbol: instrument.provider_symbol.clone(),
                from_time_ms: Some(from_nanos / 1_000_000),
            }],
        )?;
        Ok(Self {
            instrument,
            channel,
            deadline: Instant::now() + Duration::from_secs(15),
            from_nanos,
            until_nanos,
            trades: BTreeMap::new(),
            end_seen: false,
            complete: false,
            truncated: false,
        })
    }
    fn accept(
        &mut self,
        event: FeedEvent,
        generation: u64,
        ordinal: &mut u64,
    ) -> Result<(), String> {
        let FeedEvent::Trade {
            symbol,
            flags,
            index,
            kind,
            trade,
            ..
        } = event
        else {
            return Ok(());
        };
        if symbol != self.instrument.provider_symbol {
            return Ok(());
        }
        if flags & SNAPSHOT_BEGIN != 0 {
            self.trades.clear();
            self.end_seen = false;
            self.complete = false;
        }
        if flags & REMOVE_EVENT != 0 || kind == "CANCEL" {
            self.trades.remove(&index);
        } else if let Some(print) = trade
            && (self.from_nanos..self.until_nanos).contains(&print.time_nanos)
        {
            *ordinal = ordinal
                .checked_add(1)
                .ok_or("Tastytrade ingestion ordinal overflowed")?;
            if let Some(trade) = market_trade(
                &self.instrument,
                generation,
                *ordinal,
                index.clone(),
                &print,
            )? {
                if self.trades.len() >= MAXIMUM_TICK_HISTORY && !self.trades.contains_key(&index) {
                    self.truncated = true;
                    self.complete = true;
                } else {
                    self.trades.insert(index, trade);
                }
            }
        }
        if flags & (SNAPSHOT_END | SNAPSHOT_SNIP) != 0 {
            self.end_seen = true;
            self.truncated |= flags & SNAPSHOT_SNIP != 0;
        }
        if self.end_seen && flags & TX_PENDING == 0 {
            self.complete = true;
        }
        Ok(())
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
    api: Arc<BrokerApi>,
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
    subscription_budget: Arc<Mutex<SubscriptionChangeBudget>>,
    retry_at: Instant,
    refresh_at: Instant,
    connected_at: Instant,
    socket_url: String,
    pending_token: Option<(u64, u64)>,
    cached_token: Option<QuoteToken>,
    token_identity: u64,
    histories: BTreeMap<u64, HistoryTask>,
    tape: Option<TapeTask>,
    loaded_tapes: BTreeSet<String>,
    completions: VecDeque<Command>,
    channel: u64,
    ordinal: u64,
    demand_dirty: bool,
    live_from_ms: i64,
    failures: u8,
    paused: bool,
    trade_batches: BTreeMap<String, (bool, Vec<IndexedTradeMutation>)>,
    candle_batches: BTreeMap<String, (bool, Vec<(MarketBar, u64)>)>,
    quotes: BTreeMap<String, TopOfBookQuote>,
}
impl Worker {
    fn new(ports: WorkerPorts) -> Self {
        Self {
            ports,
            demand: Demand::default(),
            socket: None,
            subscription_budget: Arc::new(Mutex::new(SubscriptionChangeBudget::default())),
            retry_at: Instant::now(),
            refresh_at: Instant::now(),
            connected_at: Instant::now(),
            socket_url: String::new(),
            pending_token: None,
            cached_token: None,
            token_identity: 0,
            histories: BTreeMap::new(),
            tape: None,
            loaded_tapes: BTreeSet::new(),
            completions: VecDeque::new(),
            channel: 5,
            ordinal: 0,
            demand_dirty: true,
            live_from_ms: 0,
            failures: 0,
            paused: false,
            trade_batches: BTreeMap::new(),
            candle_batches: BTreeMap::new(),
            quotes: BTreeMap::new(),
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
                self.finish_tape()?;
                self.start_tape()?;
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
                    self.loaded_tapes.retain(|id| {
                        self.demand
                            .tape_instruments
                            .iter()
                            .any(|instrument| instrument.instrument_id == *id)
                    });
                    self.demand_dirty = true;
                }
                RealtimeControl::Stop => {
                    self.demand = Demand::default();
                    self.pending_token = None;
                    self.socket = None;
                    self.tape = None;
                    self.loaded_tapes.clear();
                    let _ = self.publish(RealtimeEvent::Disconnected(self.epoch()));
                }
                RealtimeControl::AuthorizationChanged(ready) => {
                    self.pending_token = None;
                    self.cached_token = None;
                    self.paused = !ready;
                    self.failures = 0;
                    self.retry_at = Instant::now();
                    if !ready {
                        self.socket = None;
                        self.tape = None;
                        self.loaded_tapes.clear();
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
            let token = reply.result?;
            self.install_token(&token)?;
            self.cached_token = Some(token);
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
            if Instant::now() < self.refresh_at
                && let Some(token) = self.cached_token.take()
            {
                let result = self.install_token(&token);
                self.cached_token = Some(token);
                return result;
            }
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
        let mut socket = DxlinkSession::connect(
            token,
            &self.ports.stop,
            Arc::clone(&self.subscription_budget),
        )?;
        socket.open(1, "AUTO", Vec::new())?;
        socket.open(3, "AUTO", Vec::new())?;
        socket.open(5, "STREAM", Vec::new())?;
        self.socket_url.clone_from(&token.dxlink_url);
        self.connected_at = Instant::now();
        self.live_from_ms = now_nanos()? / 1_000_000;
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
        let quotes: Vec<Subscription> = self
            .demand
            .instruments
            .iter()
            .map(|instrument| Subscription {
                kind: "Quote",
                symbol: instrument.provider_symbol.clone(),
                from_time_ms: None,
            })
            .collect();
        let trades: Vec<Subscription> = self
            .demand
            .instruments
            .iter()
            .map(|instrument| Subscription {
                kind: "TimeAndSale",
                symbol: instrument.provider_symbol.clone(),
                from_time_ms: None,
            })
            .collect();
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
        let changes = socket.subscription_changes_for_replace(1, &quotes)?
            + socket.subscription_changes_for_replace(5, &trades)?
            + socket.subscription_changes_for_replace(3, &candles)?;
        if !socket.can_change_subscriptions(changes.saturating_add(1_000))? {
            return Ok(());
        }
        socket.replace(1, quotes)?;
        socket.replace(5, trades)?;
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
        if !self.completions.is_empty() || self.histories.len() >= MAXIMUM_CONCURRENT_CANDLE_HISTORY
        {
            return Ok(());
        }
        let epoch = self.epoch();
        let Some(socket) = &mut self.socket else {
            return Ok(());
        };
        if !socket.can_change_subscriptions(2)? {
            return Ok(());
        }
        let Ok(request) = self.ports.history.try_recv() else {
            return Ok(());
        };
        if let Some(tape) = self.tape.take() {
            socket.close_channel(tape.channel)?;
        }
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
                let session = task
                    .request
                    .instrument
                    .as_ref()
                    .and_then(|instrument| self.ports.api.market_session(instrument));
                let result = task.snapshot(session);
                task.candle_state = CandleHistoryState::Published;
                let request = task.request.clone();
                self.queue_completion(request, result);
            }
            let Some(task) = self.histories.get(&channel) else {
                continue;
            };
            if !cancelled
                && Instant::now() < task.deadline
                && task.candle_state != CandleHistoryState::Published
            {
                continue;
            }
            let Some(task) = self.histories.remove(&channel) else {
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
        }
        Ok(())
    }
    fn start_tape(&mut self) -> Result<(), String> {
        if self.tape.is_some() || !self.histories.is_empty() || !self.completions.is_empty() {
            return Ok(());
        }
        let Some(instrument) = self
            .demand
            .tape_instruments
            .iter()
            .find(|instrument| !self.loaded_tapes.contains(&instrument.instrument_id))
            .cloned()
        else {
            return Ok(());
        };
        let Some(socket) = &mut self.socket else {
            return Ok(());
        };
        if !socket.can_change_subscriptions(2)? {
            return Ok(());
        }
        let now = now_nanos()?;
        let Some(session) = self.ports.api.market_session(&instrument) else {
            return Ok(());
        };
        let Some(from) = session.replay_start(now) else {
            return Ok(());
        };
        self.channel = self
            .channel
            .checked_add(2)
            .ok_or("Tastytrade channel identity overflowed")?;
        self.tape = Some(TapeTask::begin(instrument, self.channel, socket, from)?);
        Ok(())
    }
    fn finish_tape(&mut self) -> Result<(), String> {
        let Some(task) = &self.tape else {
            return Ok(());
        };
        let demanded = self
            .demand
            .tape_instruments
            .iter()
            .any(|instrument| instrument.instrument_id == task.instrument.instrument_id);
        if demanded && !task.complete && Instant::now() < task.deadline {
            return Ok(());
        }
        let Some(task) = self.tape.take() else {
            return Ok(());
        };
        if let Some(socket) = &mut self.socket {
            socket.close_channel(task.channel)?;
        }
        if demanded {
            let mut trades: Vec<_> = task.trades.into_values().collect();
            trades.sort_unstable_by(|a, b| {
                (a.metadata.timestamps.exchange_unix_nanos, &a.trade_id)
                    .cmp(&(b.metadata.timestamps.exchange_unix_nanos, &b.trade_id))
            });
            self.loaded_tapes
                .insert(task.instrument.instrument_id.clone());
            self.publish(RealtimeEvent::Tape(
                self.epoch(),
                task.instrument,
                trades,
                task.truncated || !task.complete,
            ))?;
        }
        Ok(())
    }
    fn accept(&mut self, event: FeedEvent) -> Result<(), String> {
        if let FeedEvent::ChannelFailure { channel } = event {
            if let Some(task) = self.histories.remove(&channel) {
                self.queue_completion(
                    task.request,
                    Err("Tastytrade candle history channel failed".into()),
                );
            }
            if self
                .tape
                .as_ref()
                .is_some_and(|task| task.channel == channel)
                && let Some(mut task) = self.tape.take()
            {
                task.truncated = true;
                task.complete = true;
                self.tape = Some(task);
            }
            return Ok(());
        }
        self.ordinal = self
            .ordinal
            .checked_add(1)
            .ok_or("Tastytrade delivery ordinal overflowed")?;
        let epoch = self.epoch();
        if let Some(task) = self.histories.get_mut(&event.channel()) {
            return task.accept(event, epoch, &mut self.ordinal);
        }
        if let Some(task) = &mut self.tape
            && task.channel == event.channel()
        {
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
                let mut quote_metadata = metadata(instrument, epoch, self.ordinal, None)?;
                // dxFeed bidTime/askTime describe side updates, not a single exchange event.
                quote_metadata.timestamps.provider_unix_nanos = time_nanos;
                let quote = TopOfBookQuote {
                    metadata: quote_metadata,
                    bid: bid.map(level),
                    ask: ask.map(level),
                };
                quote.validate().map_err(|e| e.to_string())?;
                self.quotes.insert(symbol, quote.clone());
                self.publish(RealtimeEvent::Quote(epoch, quote))?;
            }
            FeedEvent::Trade {
                channel: 5,
                symbol,
                index,
                kind,
                flags,
                trade,
                ..
            } => self.accept_trade(symbol, index, &kind, flags, trade.as_ref())?,
            FeedEvent::Candle {
                channel: 3,
                symbol,
                flags,
                bar,
                count,
                ..
            } => self.accept_candle(&symbol, flags, bar, count)?,
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
        print: Option<&TradePrint>,
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
        let mutation_kind = if kind == "CANCEL" || flags & REMOVE_EVENT != 0 {
            IndexedTradeKind::Cancel
        } else if kind == "CORRECTION" {
            IndexedTradeKind::Correction
        } else {
            IndexedTradeKind::New
        };
        let trade = match mutation_kind {
            IndexedTradeKind::Cancel => None,
            IndexedTradeKind::New | IndexedTradeKind::Correction => print
                .map(|print| {
                    market_trade(
                        &instrument,
                        self.epoch(),
                        self.ordinal,
                        index.clone(),
                        print,
                    )
                })
                .transpose()?
                .flatten(),
        };
        if mutation_kind != IndexedTradeKind::Cancel && trade.is_none() {
            return Ok(());
        }
        if let Some(print) = print.as_ref()
            && trade.is_some()
        {
            self.update_quote_from_trade(&symbol, print)?;
        }
        let batch = self.trade_batches.entry(symbol).or_default();
        if flags & SNAPSHOT_BEGIN != 0 {
            batch.0 = true;
            batch.1.clear();
        }
        if batch.1.len() >= MAXIMUM_TICK_HISTORY {
            return Err("Tastytrade live transaction exceeded its bound".into());
        }
        batch.1.push(IndexedTradeMutation {
            index,
            kind: mutation_kind,
            source_sequence: self.ordinal,
            trade,
        });
        if flags & (SNAPSHOT_END | SNAPSHOT_SNIP) != 0 {
            batch.0 = false;
        }
        if !batch.0 && flags & TX_PENDING == 0 {
            let changes = std::mem::take(&mut batch.1);
            self.publish(RealtimeEvent::Trades(self.epoch(), instrument, changes))?;
        }
        Ok(())
    }

    /// Trades carry the freshest top prices, while quote events remain the
    /// authority for displayed sizes. Keep the quote timestamp unchanged so
    /// retained sizes cannot be presented as freshly observed at trade time.
    fn update_quote_from_trade(&mut self, symbol: &str, print: &TradePrint) -> Result<(), String> {
        let Some(previous) = self.quotes.get(symbol).cloned() else {
            return Ok(());
        };
        let mut quote = previous.clone();
        if let Some(price) = print.bid_price {
            quote.bid = quote.bid.map(|level| DepthLevel { price, ..level });
        }
        if let Some(price) = print.ask_price {
            quote.ask = quote.ask.map(|level| DepthLevel { price, ..level });
        }
        if quote == previous {
            return Ok(());
        }
        quote.metadata.source_sequence = self.ordinal;
        if quote.validate().is_err() {
            return Ok(());
        }
        self.quotes.insert(symbol.to_string(), quote.clone());
        self.publish(RealtimeEvent::Quote(self.epoch(), quote))?;
        Ok(())
    }
    fn accept_candle(
        &mut self,
        symbol: &str,
        flags: u32,
        bar: Option<MarketBar>,
        count: u64,
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
            batch.1.push((bar, count));
        }
        if flags & (SNAPSHOT_END | SNAPSHOT_SNIP) != 0 {
            batch.0 = false;
        }
        if !batch.0 && flags & TX_PENDING == 0 {
            let mut bars = std::mem::take(&mut batch.1);
            bars.sort_unstable_by_key(|(bar, _)| bar.exchange_timestamp_unix_nanos);
            for (bar, count) in bars {
                self.publish(RealtimeEvent::Candle(
                    self.epoch(),
                    symbol.to_string(),
                    bar,
                    count,
                    self.ordinal,
                ))?;
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
        self.tape = None;
        self.loaded_tapes.clear();
        self.pending_token = None;
        self.trade_batches.clear();
        self.candle_batches.clear();
        self.quotes.clear();
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
    descriptor: super::ProviderDescriptor,
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
    let catalog = CatalogPublisher::new(catalog_tx, descriptor.id, wake.clone());
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
    let provider_api = Arc::clone(api);
    let completions = completions.clone();
    let provider_worker = match thread::Builder::new()
        .name("aeris-tastytrade-provider".into())
        .spawn(move || {
            let _guard = ActiveWorkerGuard::register("aeris-tastytrade-provider", worker_activity);
            Worker::new(WorkerPorts {
                api: provider_api,
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
        descriptor,
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

#[cfg(test)]
#[path = "tastytrade_tests.rs"]
mod tests;
