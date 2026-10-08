//! Runtime-owned cTrader catalog, per-host market sessions, and on-demand history.
//!
//! One supervisor thread owns at most one authenticated session per cTrader
//! host (demo and live). Chart, symbol and timeframe changes only adjust that
//! session's subscriptions. A host session closes once it carries no
//! subscriptions and no work for the idle-stop interval.
//!
//! The provider generation is shared by both hosts: a fault on either host
//! closes every host session and the next live demand opens a new generation,
//! so no event from a retired session can be published under a current one.
use super::{ActiveWorkerGuard, tastytrade::now_nanos};
use super::{
    Arc, AtomicBool, AtomicU64, BTreeMap, BTreeSet, BarPeriod, BarSeriesKey, COMMAND_CAPACITY,
    CatalogPublisher, Command, Coordinator, DepthSnapshot, Duration, FormingBar, HISTORY_CAPACITY,
    HistoryRequest, HistorySnapshot, InstallProviderInstrument, Instant, MAXIMUM_CONSUMERS,
    MarketBar, Mutex, Ordering, ProviderCatalogChannelSet, ProviderCatalogChannels,
    ProviderCatalogRejected, ProviderCatalogRejectionReason, ProviderCoordinatorWake,
    ProviderRealtimeChannelSet, ProviderRealtimeChannels, ProviderRuntimeLifecycle,
    ProviderRuntimeRecord, REALTIME_CAPACITY, RITHMIC_REALTIME_CONTROL_CAPACITY, Receiver,
    SearchProviderInstruments, SelectProviderInstrument, SyncSender, TopOfBookQuote, TrySendError,
    VecDeque, mpsc, thread,
};
use aeris_contracts::{
    ProviderContractMetadata, ProviderInstrumentSearchResult, ProviderInstrumentSummary,
};
use aeris_ctrader_open_api_adapter::{
    ProtoMessage,
    accounts::CtraderAccount,
    host::CtraderHost,
    hosted::{CtraderHostedAccess, load_stored_connection},
    market::{
        DepthUpdate, EventStamp, LightSymbol, MAXIMUM_STREAM_SYMBOLS, MAXIMUM_TRENDBARS_PER_PAGE,
        MarketDecodeError, MarketRequest, MarketStreams, PriceScale, SymbolStream, TrendbarPage,
        TrendbarPeriod, decode_asset_list, decode_subscription_ack, decode_symbol_by_id,
        decode_symbol_list, decode_trendbar_page,
    },
    session::{AccessToken, CtraderSession, SessionFault},
};
use aeris_observability::diagnostic;
use aeris_platform_runtime::hosted_broker::HostedBrokerConnection;
use std::sync::atomic::AtomicU64 as Counter;

pub(super) const ENTITLEMENT: &str = "ctrader-authorized";
const PROVIDER: &str = "ctrader";
const DISCONNECTED_DETAIL: &str = "Connect cTrader in Accounts to load market data";
const RETIRED_DETAIL: &str = "cTrader history request retired";
pub(super) const PRESENTATION: aeris_contracts::ProviderPresentationDescriptor =
    aeris_contracts::ProviderPresentationDescriptor {
        id: PROVIDER,
        display_name: "cTrader",
        chart_interval_labels: &["1m", "3m", "5m", "15m", "30m", "1h", "1D", "1W", "1M"],
        default_listing: "",
        search_hint: "Search cTrader symbols",
        logo_key: PROVIDER,
        catalog_symbol: aeris_contracts::ProviderCatalogSymbol::ProviderSymbol,
        selection_entitlement_id: ENTITLEMENT,
        // Restored charts re-read the symbol spec, so tick size, digits and
        // quote currency always come from the account's current catalog.
        catalog_refresh_on_startup: true,
        ready_label_suffix: "",
        depth_available: true,
        search_categories_available: false,
        market_screen_available: false,
        trades_available: false,
        connection_kind: aeris_contracts::ProviderConnectionKind::HostedBroker,
    };
pub(super) const DESCRIPTOR: super::ProviderDescriptor = super::ProviderDescriptor {
    id: PROVIDER,
    presentation: &PRESENTATION,
    account_id: ENTITLEMENT,
    capabilities: super::ProviderCapabilities {
        historical_bars: true,
        realtime_bars: true,
        streams: super::StreamRequirements::BARS
            .with(super::MarketStream::Quotes)
            .with(super::MarketStream::Depth),
    },
    reconnect_delay: Duration::from_secs(3),
    gap_policy: super::CandleGapPolicy::SessionGapsAllowed,
    history_source: super::HistorySourceKind::ProviderSession,
    connection_kind: super::ProviderConnectionKind::BrokerCapability,
    recovery_policy: super::ProviderRecoveryPolicy::WorkerReconcilesDemand,
    idle_stop_policy: super::IdleStopPolicy::WorkerManaged,
    alert_demand_update: super::AlertDemandUpdate::WorkerManaged,
    start: super::ProviderRuntimeRegistry::start_ctrader_runtime,
    flush_demand,
    prepare_search: None,
    live_model: super::LiveModel::ProviderCandles,
    supported_period,
    alert_overrides_instrument: false,
    overflow_recovery_detail: "cTrader queue overflow requires recovery",
    history_range_policy: super::HistoryRangePolicy::Bounded,
    trade_continuity: super::TradeContinuity::Sequence,
    candle_requires_connected: true,
    candle_correction_detail: "cTrader candle replacement requires covering history",
    candle_wire_interval: Some(candle_interval),
    candle_demand_policy: super::CandleDemandPolicy::SessionManaged,
    trade_demand_policy: super::TradeDemandPolicy::SessionManaged,
    instrument_missing_detail: "cTrader instrument is not installed",
    authorization_revoked_detail: "cTrader disconnected",
};

fn flush_demand(coordinator: &mut Coordinator<'_>) {
    coordinator.flush_session_managed_demand(DESCRIPTOR.id);
}

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const EVENT_POLL: Duration = Duration::from_millis(5);
const EVENTS_PER_HOST_TURN: usize = 128;
const IDLE_STOP: Duration = Duration::from_secs(30);
const RECONCILE_RETRY: Duration = Duration::from_secs(5);
const MAXIMUM_FAILURES: u8 = 5;
const HEALTHY_SESSION: Duration = Duration::from_mins(1);
const MAXIMUM_HISTORY_PAGES: usize = 8;
const MAXIMUM_SYMBOLS_PER_REQUEST: usize = 64;
const MAXIMUM_CATALOG_ACCOUNTS: usize = 16;
const MAXIMUM_SEARCH_RESULTS: usize = 100;
const CATALOG_TTL: Duration = Duration::from_mins(30);
/// Depth sizes are in cents of the base unit.
const DEPTH_QUANTITY_SCALE: u32 = 2;
/// Trendbar volume is a tick count.
const BAR_QUANTITY_SCALE: u8 = 0;
const SPOT_EVENT: u32 = 2131;
const DEPTH_EVENT: u32 = 2155;
const MINIMUM_HISTORY_SPAN_MS: i64 = 7 * 86_400_000;
const MAXIMUM_HISTORY_SPAN_MS: i64 = 20 * 365 * 86_400_000;

/// Locked or crossed provider books observed since the runtime started. They
/// are withheld from canonical state, so these counts are the only record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CtraderStreamStatistics {
    pub crossed_spots: u64,
    pub crossed_depth: u64,
}

#[derive(Default)]
pub(super) struct StreamCounters {
    crossed_spots: Counter,
    crossed_depth: Counter,
}
impl StreamCounters {
    pub(super) fn snapshot(&self) -> CtraderStreamStatistics {
        CtraderStreamStatistics {
            crossed_spots: self.crossed_spots.load(Ordering::Acquire),
            crossed_depth: self.crossed_depth.load(Ordering::Acquire),
        }
    }
}

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
    /// Instruments with live quotes; the flag requests depth as well.
    pub instruments: Vec<(InstallProviderInstrument, bool)>,
}
pub(super) enum RealtimeControl {
    Subscribe(Demand),
    Stop,
    AuthorizationChanged(bool),
}
pub(super) enum RealtimeEvent {
    Connecting(u64),
    Connected(u64),
    Recovering(u64, String),
    Failed(u64, String),
    Disconnected(u64),
    Candle(u64, String, MarketBar),
    Quote(u64, TopOfBookQuote),
    Depth(u64, DepthSnapshot),
}
impl RealtimeEvent {
    pub(super) fn generation(&self) -> u64 {
        match self {
            Self::Connecting(g)
            | Self::Connected(g)
            | Self::Recovering(g, _)
            | Self::Failed(g, _)
            | Self::Disconnected(g)
            | Self::Candle(g, ..)
            | Self::Quote(g, _)
            | Self::Depth(g, _) => *g,
        }
    }
}

/// Account-scoped routing identity of one cTrader symbol.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Route {
    live: bool,
    ctid: u64,
    symbol_id: u64,
}
impl Route {
    const fn environment(self) -> &'static str {
        if self.live { "live" } else { "demo" }
    }
    fn instrument_id(self) -> String {
        format!(
            "{PROVIDER}:{}:{}:{}",
            self.environment(),
            self.ctid,
            self.symbol_id
        )
    }
    /// Unique across accounts, because live candle events are matched by symbol.
    fn provider_symbol(self) -> String {
        format!("{}:{}:{}", self.environment(), self.ctid, self.symbol_id)
    }
    const fn venue(self) -> &'static str {
        venue(self.live)
    }
}
const fn venue(live: bool) -> &'static str {
    if live { "cTrader Live" } else { "cTrader Demo" }
}
const fn host_for(live: bool) -> CtraderHost {
    if live {
        CtraderHost::Live
    } else {
        CtraderHost::Demo
    }
}

/// The id with its trading-account segment masked, for diagnostics. Routing
/// still uses the full id; only what reaches logs changes.
pub(super) fn redacted_instrument_id(instrument_id: &str) -> Option<String> {
    let mut parts = instrument_id.splitn(4, ':');
    let (Some(PROVIDER), Some(environment), Some(account), Some(symbol)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let suffix = account
        .char_indices()
        .rev()
        .nth(1)
        .map_or("", |(index, _)| &account[index..]);
    Some(format!("{PROVIDER}:{environment}:#**{suffix}:{symbol}"))
}

/// Parses `ctrader:{demo|live}:{ctid}:{symbolId}`.
fn parse_instrument_id(instrument_id: &str) -> Result<Route, String> {
    let invalid = || "cTrader instrument id is invalid".to_string();
    let number = |value: &str| {
        (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| value.parse::<u64>().ok())
            .flatten()
            .filter(|value| *value > 0 && i64::try_from(*value).is_ok())
    };
    let mut parts = instrument_id.split(':');
    let (Some(PROVIDER), Some(environment), Some(ctid), Some(symbol), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(invalid());
    };
    let live = match environment {
        "demo" => false,
        "live" => true,
        _ => return Err(invalid()),
    };
    Ok(Route {
        live,
        ctid: number(ctid).ok_or_else(invalid)?,
        symbol_id: number(symbol).ok_or_else(invalid)?,
    })
}

/// D1 bars open at 17:00 New York, so consecutive opens are 23, 24 or 25
/// hours apart. They map only to the canonical session day; a fixed 24-hour
/// period would mislabel every bar that crosses a daylight-saving change.
fn trendbar_period(period: BarPeriod) -> Result<TrendbarPeriod, String> {
    TrendbarPeriod::for_canonical(period)
        .ok_or_else(|| "cTrader history is unavailable for this interval".to_string())
}
fn supported_period(period: BarPeriod) -> Result<(), String> {
    trendbar_period(period).map(|_| ())
}
const fn period_label(period: TrendbarPeriod) -> &'static str {
    match period {
        TrendbarPeriod::M1 => "m1",
        TrendbarPeriod::M2 => "m2",
        TrendbarPeriod::M3 => "m3",
        TrendbarPeriod::M4 => "m4",
        TrendbarPeriod::M5 => "m5",
        TrendbarPeriod::M10 => "m10",
        TrendbarPeriod::M15 => "m15",
        TrendbarPeriod::M30 => "m30",
        TrendbarPeriod::H1 => "h1",
        TrendbarPeriod::H4 => "h4",
        TrendbarPeriod::H12 => "h12",
        TrendbarPeriod::D1 => "d1",
        TrendbarPeriod::W1 => "w1",
        TrendbarPeriod::MN1 => "mn1",
    }
}
/// Upper bound of one bar's length, used only to size history windows and to
/// tell whether the newest history bar may still be open. D1, W1 and MN1 open
/// at 17:00 New York, so each can gain the hour lost at a daylight-saving
/// change; a month is bounded by 31 days.
const fn maximum_duration_ms(period: TrendbarPeriod) -> i64 {
    let minutes = match period {
        TrendbarPeriod::M1 => 1,
        TrendbarPeriod::M2 => 2,
        TrendbarPeriod::M3 => 3,
        TrendbarPeriod::M4 => 4,
        TrendbarPeriod::M5 => 5,
        TrendbarPeriod::M10 => 10,
        TrendbarPeriod::M15 => 15,
        TrendbarPeriod::M30 => 30,
        TrendbarPeriod::H1 => 60,
        TrendbarPeriod::H4 => 240,
        TrendbarPeriod::H12 => 720,
        TrendbarPeriod::D1 => 1_440 + 60,
        TrendbarPeriod::W1 => 10_080 + 60,
        TrendbarPeriod::MN1 => 44_640 + 60,
    };
    minutes * 60_000
}
pub(super) fn candle_interval(period: BarPeriod) -> Result<String, String> {
    trendbar_period(period).map(|period| period_label(period).to_string())
}
fn candle_symbol(provider_symbol: &str, period: TrendbarPeriod) -> String {
    format!("{provider_symbol}{{={}}}", period_label(period))
}

/// A failure that retires every host session and the current generation.
/// `wait` is a server-directed delay (connection limit or maintenance) that
/// replaces the local backoff and does not count toward the retry limit.
struct HostFault {
    detail: String,
    wait: Option<Duration>,
}
impl From<String> for HostFault {
    fn from(detail: String) -> Self {
        Self { detail, wait: None }
    }
}
impl From<Failure> for HostFault {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Request(detail) => Self::from(detail),
            Failure::Host(fault) => fault,
        }
    }
}

/// A request-scoped failure leaves the host session usable; a host failure
/// retires every host session and the current provider generation.
enum Failure {
    Request(String),
    Host(HostFault),
}
impl Failure {
    fn detail(&self) -> &str {
        match self {
            Self::Request(detail) => detail,
            Self::Host(fault) => &fault.detail,
        }
    }
    fn host(detail: impl Into<String>) -> Self {
        Self::Host(HostFault::from(detail.into()))
    }
}
fn classify(fault: &SessionFault) -> Failure {
    match fault {
        SessionFault::RateLimited { .. } | SessionFault::Timeout | SessionFault::Protocol => {
            Failure::Request(fault.to_string())
        }
        SessionFault::ConnectionLimit { wait, .. } | SessionFault::Maintenance { wait } => {
            Failure::Host(HostFault {
                detail: fault.to_string(),
                wait: Some(*wait),
            })
        }
        _ => Failure::host(fault.to_string()),
    }
}
fn request_error(error: &MarketDecodeError) -> Failure {
    Failure::Request(error.to_string())
}

/// One authenticated cTrader host connection. Production uses the hosted
/// capability session; deterministic tests substitute a scripted link here.
trait CtraderMarketLink: Send {
    fn accounts(&self) -> &[CtraderAccount];
    fn authorize(&mut self, account: &CtraderAccount) -> Result<(), SessionFault>;
    fn request(&mut self, request: MarketRequest) -> Result<ProtoMessage, SessionFault>;
    fn next_event(&mut self, timeout: Duration) -> Result<Option<ProtoMessage>, SessionFault>;
    fn close(&mut self);
}
type LinkOpener = Box<
    dyn FnMut(CtraderHost, &Arc<AtomicBool>) -> Result<Box<dyn CtraderMarketLink>, HostFault>
        + Send,
>;

#[derive(Default)]
struct HostedAccess {
    client: CtraderHostedAccess,
    connection: Option<HostedBrokerConnection>,
}
fn refresh_token(
    access: &Mutex<HostedAccess>,
    stop: &Arc<AtomicBool>,
) -> Result<AccessToken, SessionFault> {
    let mut guard = access.lock().map_err(|_| SessionFault::NeedsReconnect)?;
    let HostedAccess { client, connection } = &mut *guard;
    let connection = connection.as_ref().ok_or(SessionFault::NeedsReconnect)?;
    client
        .access_token(connection, stop, true)
        .map_err(|_| SessionFault::NeedsReconnect)
}
/// The stored capability is reloaded for every session, so a reconnect in
/// Accounts replaces cached credentials and a disconnect stops new sessions.
fn open_hosted(
    access: &Mutex<HostedAccess>,
    host: CtraderHost,
    stop: &Arc<AtomicBool>,
) -> Result<CtraderSession, HostFault> {
    let mut guard = access
        .lock()
        .map_err(|_| HostFault::from("cTrader hosted access failed".to_string()))?;
    let HostedAccess { client, connection } = &mut *guard;
    *connection = load_stored_connection()?;
    let connection = connection
        .as_ref()
        .ok_or_else(|| HostFault::from(DISCONNECTED_DETAIL.to_string()))?;
    let mut retried = false;
    loop {
        let token = client.access_token(connection, stop, false)?;
        let credentials = client.app_credentials(connection, stop)?;
        match CtraderSession::open(host, credentials, token, Arc::clone(stop)) {
            Err(SessionFault::ClientAuthFailure) if !retried => client.clear_credentials(),
            Err(SessionFault::NeedsReconnect) if !retried => client.clear_access_token(),
            result => return result.map_err(|fault| HostFault::from(classify(&fault))),
        }
        retried = true;
    }
}
struct HostedLink {
    session: CtraderSession,
    access: Arc<Mutex<HostedAccess>>,
    stop: Arc<AtomicBool>,
}
impl CtraderMarketLink for HostedLink {
    fn accounts(&self) -> &[CtraderAccount] {
        self.session.accounts()
    }
    fn authorize(&mut self, account: &CtraderAccount) -> Result<(), SessionFault> {
        let (access, stop) = (&self.access, &self.stop);
        self.session
            .authorize_account(account, || refresh_token(access, stop))
    }
    fn request(&mut self, request: MarketRequest) -> Result<ProtoMessage, SessionFault> {
        self.session.request(
            request.payload_type,
            request.payload,
            request.response_type,
            request.bucket,
            REQUEST_TIMEOUT,
        )
    }
    fn next_event(&mut self, timeout: Duration) -> Result<Option<ProtoMessage>, SessionFault> {
        let (access, stop) = (&self.access, &self.stop);
        self.session
            .next_event_with_refresh(timeout, || refresh_token(access, stop))
    }
    fn close(&mut self) {
        self.session.close();
    }
}

fn by_account(items: impl IntoIterator<Item = (u64, u64)>) -> BTreeMap<u64, Vec<u64>> {
    let mut grouped = BTreeMap::<u64, Vec<u64>>::new();
    for (ctid, symbol) in items {
        grouped.entry(ctid).or_default().push(symbol);
    }
    grouped
}

#[derive(Default)]
struct HostDemand {
    spots: BTreeMap<(u64, u64), SymbolStream>,
    depth: BTreeSet<(u64, u64)>,
    trendbars: BTreeSet<(u64, u64, i32)>,
}

struct HostSession {
    live: bool,
    link: Box<dyn CtraderMarketLink>,
    authorized: BTreeSet<u64>,
    streams: BTreeMap<u64, MarketStreams>,
    spots: BTreeSet<(u64, u64)>,
    depth: BTreeSet<(u64, u64)>,
    trendbars: BTreeSet<(u64, u64, i32)>,
    last_use: Instant,
}
impl HostSession {
    fn new(live: bool, link: Box<dyn CtraderMarketLink>) -> Self {
        Self {
            live,
            link,
            authorized: BTreeSet::new(),
            streams: BTreeMap::new(),
            spots: BTreeSet::new(),
            depth: BTreeSet::new(),
            trendbars: BTreeSet::new(),
            last_use: Instant::now(),
        }
    }
    fn idle(&self) -> bool {
        self.spots.is_empty() && self.depth.is_empty() && self.trendbars.is_empty()
    }
    fn authorize(&mut self, ctid: u64) -> Result<(), Failure> {
        if self.authorized.contains(&ctid) {
            return Ok(());
        }
        let account = self
            .link
            .accounts()
            .iter()
            .find(|account| account.ctid == ctid && account.is_live == self.live)
            .cloned()
            .ok_or_else(|| {
                Failure::Request("cTrader account is not available on this connection".into())
            })?;
        self.link
            .authorize(&account)
            .map_err(|fault| classify(&fault))?;
        self.authorized.insert(ctid);
        Ok(())
    }
    fn request(&mut self, request: MarketRequest) -> Result<ProtoMessage, Failure> {
        self.last_use = Instant::now();
        self.link.request(request).map_err(|fault| classify(&fault))
    }
    fn acknowledge(
        &mut self,
        request: Result<MarketRequest, MarketDecodeError>,
        ctid: u64,
    ) -> Result<(), Failure> {
        let request = request.map_err(|error| request_error(&error))?;
        let expected = request.response_type;
        let frame = self.request(request)?;
        decode_subscription_ack(&frame, expected, ctid).map_err(|error| {
            Failure::host(format!("cTrader subscription acknowledgement: {error}"))
        })
    }
    /// Removals run before additions, and spots before depth and live
    /// trendbars, because the server rejects a trendbar without its spots.
    fn apply(&mut self, desired: &HostDemand) -> Result<(), Failure> {
        self.remove_subscriptions(desired)?;
        self.add_subscriptions(desired)
    }
    fn remove_subscriptions(&mut self, desired: &HostDemand) -> Result<(), Failure> {
        let removed: Vec<_> = self
            .trendbars
            .difference(&desired.trendbars)
            .copied()
            .collect();
        for (ctid, symbol, wire) in removed {
            let period = TrendbarPeriod::from_wire(wire).map_err(|error| request_error(&error))?;
            self.acknowledge(
                MarketRequest::unsubscribe_live_trendbar(ctid, symbol, period),
                ctid,
            )?;
            self.trendbars.remove(&(ctid, symbol, wire));
        }
        let removed = self.depth.difference(&desired.depth).copied();
        for (ctid, symbols) in by_account(removed.collect::<Vec<_>>()) {
            for chunk in symbols.chunks(MAXIMUM_SYMBOLS_PER_REQUEST) {
                self.acknowledge(MarketRequest::unsubscribe_depth(ctid, chunk), ctid)?;
                for symbol in chunk {
                    self.depth.remove(&(ctid, *symbol));
                }
            }
        }
        let removed: Vec<_> = self
            .spots
            .iter()
            .filter(|key| !desired.spots.contains_key(key))
            .copied()
            .collect();
        for (ctid, symbols) in by_account(removed) {
            for chunk in symbols.chunks(MAXIMUM_SYMBOLS_PER_REQUEST) {
                self.acknowledge(MarketRequest::unsubscribe_spots(ctid, chunk), ctid)?;
                for symbol in chunk {
                    self.spots.remove(&(ctid, *symbol));
                    if let Some(streams) = self.streams.get_mut(&ctid) {
                        streams.remove(*symbol);
                    }
                }
            }
        }
        self.streams
            .retain(|ctid, _| self.spots.iter().any(|(subscribed, _)| subscribed == ctid));
        Ok(())
    }
    fn add_subscriptions(&mut self, desired: &HostDemand) -> Result<(), Failure> {
        let added: Vec<_> = desired
            .spots
            .keys()
            .filter(|key| !self.spots.contains(key))
            .copied()
            .collect();
        for (ctid, symbols) in by_account(added) {
            self.authorize(ctid)?;
            for chunk in symbols.chunks(MAXIMUM_SYMBOLS_PER_REQUEST) {
                let streams = self
                    .streams
                    .entry(ctid)
                    .or_insert_with(|| MarketStreams::new(ctid));
                for symbol in chunk {
                    if let Some(stream) = desired.spots.get(&(ctid, *symbol)) {
                        streams
                            .register(*symbol, stream.clone())
                            .map_err(|error| request_error(&error))?;
                    }
                }
                if let Err(failure) =
                    self.acknowledge(MarketRequest::subscribe_spots(ctid, chunk), ctid)
                {
                    if let Some(streams) = self.streams.get_mut(&ctid) {
                        for symbol in chunk {
                            streams.remove(*symbol);
                        }
                    }
                    return Err(failure);
                }
                for symbol in chunk {
                    self.spots.insert((ctid, *symbol));
                }
            }
        }
        let added = desired.depth.difference(&self.depth).copied();
        for (ctid, symbols) in by_account(added.collect::<Vec<_>>()) {
            for chunk in symbols.chunks(MAXIMUM_SYMBOLS_PER_REQUEST) {
                if let Some(streams) = self.streams.get_mut(&ctid) {
                    for symbol in chunk {
                        streams.reset_depth(*symbol);
                    }
                }
                self.acknowledge(MarketRequest::subscribe_depth(ctid, chunk), ctid)?;
                for symbol in chunk {
                    self.depth.insert((ctid, *symbol));
                }
            }
        }
        let added: Vec<_> = desired
            .trendbars
            .difference(&self.trendbars)
            .copied()
            .collect();
        for (ctid, symbol, wire) in added {
            let period = TrendbarPeriod::from_wire(wire).map_err(|error| request_error(&error))?;
            self.acknowledge(
                MarketRequest::subscribe_live_trendbar(ctid, symbol, period),
                ctid,
            )?;
            self.trendbars.insert((ctid, symbol, wire));
        }
        Ok(())
    }
    /// A depth delete for an unknown quote or an oversized book loses
    /// continuity; the account's depth books restart from a fresh subscription.
    fn resubscribe_depth(&mut self, ctid: u64) -> Result<(), Failure> {
        let symbols: Vec<_> = self
            .depth
            .iter()
            .filter(|(subscribed, _)| *subscribed == ctid)
            .map(|(_, symbol)| *symbol)
            .collect();
        for chunk in symbols.chunks(MAXIMUM_SYMBOLS_PER_REQUEST) {
            self.acknowledge(MarketRequest::unsubscribe_depth(ctid, chunk), ctid)?;
            for symbol in chunk {
                self.depth.remove(&(ctid, *symbol));
            }
            if let Some(streams) = self.streams.get_mut(&ctid) {
                for symbol in chunk {
                    streams.reset_depth(*symbol);
                }
            }
            self.acknowledge(MarketRequest::subscribe_depth(ctid, chunk), ctid)?;
            for symbol in chunk {
                self.depth.insert((ctid, *symbol));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryStage {
    Paging,
    /// A ranged request found nothing; one older window decides whether the
    /// provider has any earlier bars at all.
    Probe,
}
struct HistoryTask {
    request: HistoryRequest,
    route: Route,
    period: TrendbarPeriod,
    scale: PriceScale,
    stage: HistoryStage,
    floor_ms: i64,
    to_ms: i64,
    span_ms: i64,
    probe_span_ms: i64,
    pages: usize,
    bars: BTreeMap<i64, MarketBar>,
    backwards_exhausted: bool,
}
struct HistoryPlan {
    route: Route,
    period: TrendbarPeriod,
    scale: PriceScale,
    floor_ms: i64,
    to_ms: i64,
    span_ms: i64,
    probe_span_ms: i64,
}
impl HistoryPlan {
    fn apply(self, request: HistoryRequest) -> HistoryTask {
        HistoryTask {
            request,
            route: self.route,
            period: self.period,
            scale: self.scale,
            stage: HistoryStage::Paging,
            floor_ms: self.floor_ms,
            to_ms: self.to_ms,
            span_ms: self.span_ms,
            probe_span_ms: self.probe_span_ms,
            pages: 0,
            bars: BTreeMap::new(),
            backwards_exhausted: false,
        }
    }
}
impl HistoryTask {
    fn begin(
        request: HistoryRequest,
        generation: u64,
    ) -> Result<Self, Box<(HistoryRequest, String)>> {
        if request.stop.load(Ordering::Acquire) || request.provider_generation.0.get() != generation
        {
            return Err(Box::new((request, RETIRED_DETAIL.into())));
        }
        match Self::plan(&request) {
            Ok(plan) => Ok(plan.apply(request)),
            Err(error) => Err(Box::new((request, error))),
        }
    }
    fn plan(request: &HistoryRequest) -> Result<HistoryPlan, String> {
        let instrument = request
            .instrument
            .as_ref()
            .ok_or_else(|| DESCRIPTOR.instrument_missing_detail.to_string())?;
        let route = parse_instrument_id(&instrument.instrument_id)?;
        let period = trendbar_period(request.series.period)?;
        let scale = i32::try_from(instrument.price_scale)
            .ok()
            .and_then(|digits| PriceScale::new(digits).ok())
            .ok_or("cTrader instrument price scale is invalid")?;
        let duration = maximum_duration_ms(period);
        let wanted = i64::try_from(request.maximum_bars)
            .unwrap_or(i64::MAX)
            .saturating_add(2);
        let probe_span_ms = duration
            .saturating_mul(64)
            .clamp(MINIMUM_HISTORY_SPAN_MS, MAXIMUM_HISTORY_SPAN_MS);
        let (floor_ms, to_ms, span_ms) = match request.range {
            Some(range) => {
                let floor = range.start_unix_nanos.max(0).div_euclid(1_000_000);
                // The coordinator's range end is exclusive; cTrader's is inclusive.
                let to = range.end_unix_nanos.saturating_sub(1).div_euclid(1_000_000);
                (floor, to, to.saturating_sub(floor).saturating_add(1).max(1))
            }
            None => (
                0,
                now_nanos()?.div_euclid(1_000_000),
                duration
                    .saturating_mul(wanted)
                    .saturating_mul(2)
                    .clamp(MINIMUM_HISTORY_SPAN_MS, MAXIMUM_HISTORY_SPAN_MS),
            ),
        };
        Ok(HistoryPlan {
            route,
            period,
            scale,
            floor_ms,
            to_ms,
            span_ms,
            probe_span_ms,
        })
    }
    fn target(&self) -> usize {
        if self.request.range.is_some() {
            self.request.maximum_bars
        } else {
            self.request.maximum_bars.saturating_add(1)
        }
    }
    fn page(
        &self,
        host: &mut HostSession,
        from_ms: i64,
        to_ms: i64,
        count: usize,
    ) -> Result<TrendbarPage, Failure> {
        // On demo, `count = n` returned `n - 1` bars.
        let count = u32::try_from(count.saturating_add(1).clamp(2, MAXIMUM_TRENDBARS_PER_PAGE))
            .unwrap_or(u32::MAX);
        let request = MarketRequest::trendbars(
            self.route.ctid,
            self.route.symbol_id,
            self.period,
            from_ms,
            to_ms,
            Some(count),
        )
        .map_err(|error| request_error(&error))?;
        let frame = host.request(request)?;
        decode_trendbar_page(
            &frame,
            self.route.ctid,
            self.route.symbol_id,
            self.period,
            self.scale,
            1,
        )
        .map_err(|error| Failure::Request(format!("cTrader history page rejected: {error}")))
    }
    /// Performs one bounded page request. Returns whether the task is complete.
    fn advance(&mut self, host: &mut HostSession) -> Result<bool, Failure> {
        host.authorize(self.route.ctid)?;
        match self.stage {
            HistoryStage::Paging => {
                let from_ms = self
                    .to_ms
                    .saturating_sub(self.span_ms.saturating_sub(1))
                    .max(self.floor_ms);
                if from_ms > self.to_ms {
                    return Ok(self.finish_paging());
                }
                let remaining = self.target().saturating_sub(self.bars.len());
                let page = self.page(host, from_ms, self.to_ms, remaining)?;
                self.pages += 1;
                let earliest_ms = page
                    .bars
                    .first()
                    .map(|bar| bar.bar.exchange_timestamp_unix_nanos.div_euclid(1_000_000));
                self.insert(page.bars.into_iter().map(|bar| bar.bar));
                self.to_ms = match earliest_ms {
                    Some(earliest) => earliest.saturating_sub(1),
                    None if page.has_more => {
                        return Err(Failure::Request(
                            "cTrader returned an empty history page with more bars".into(),
                        ));
                    }
                    None => from_ms.saturating_sub(1),
                };
                let range_complete = self.request.range.is_some() && !page.has_more;
                if range_complete
                    || self.bars.len() >= self.target()
                    || self.pages >= MAXIMUM_HISTORY_PAGES
                    || self.to_ms < self.floor_ms
                {
                    return Ok(self.finish_paging());
                }
                Ok(false)
            }
            HistoryStage::Probe => {
                let to_ms = self.floor_ms.saturating_sub(1);
                if to_ms < 0 {
                    self.backwards_exhausted = true;
                    return Ok(true);
                }
                let from_ms = to_ms
                    .saturating_sub(self.probe_span_ms.saturating_sub(1))
                    .max(0);
                let page = self.page(host, from_ms, to_ms, 1)?;
                self.backwards_exhausted = page.bars.is_empty() && !page.has_more;
                Ok(true)
            }
        }
    }
    fn finish_paging(&mut self) -> bool {
        if self.request.range.is_some() && self.bars.is_empty() {
            self.stage = HistoryStage::Probe;
            return false;
        }
        true
    }
    fn insert(&mut self, bars: impl Iterator<Item = MarketBar>) {
        let range = self.request.range;
        for bar in bars {
            if range.is_none_or(|range| {
                bar.exchange_timestamp_unix_nanos >= range.start_unix_nanos
                    && bar.exchange_timestamp_unix_nanos < range.end_unix_nanos
            }) {
                self.bars.insert(bar.exchange_timestamp_unix_nanos, bar);
            }
        }
        while self.bars.len() > self.target() {
            self.bars.pop_first();
        }
    }
    fn snapshot(self) -> Result<HistorySnapshot, String> {
        let now = now_nanos()?;
        let mut bars: Vec<_> = self.bars.into_values().collect();
        let duration_nanos = maximum_duration_ms(self.period).saturating_mul(1_000_000);
        let forming = (self.request.range.is_none()
            && bars.last().is_some_and(|bar| {
                bar.exchange_timestamp_unix_nanos
                    .saturating_add(duration_nanos)
                    > now
            }))
        .then(|| bars.pop())
        .flatten();
        if bars.len() > self.request.maximum_bars {
            bars.drain(..bars.len() - self.request.maximum_bars);
        }
        for (index, bar) in bars.iter_mut().enumerate() {
            bar.source_sequence = index as u64 + 1;
        }
        let forming = forming.map(|mut bar| {
            bar.source_sequence = bars.len() as u64 + 1;
            FormingBar { bar, trades: None }
        });
        Ok(HistorySnapshot {
            price_scale: self.scale.digits(),
            quantity_scale: BAR_QUANTITY_SCALE,
            bars,
            forming,
            handoff_boundary_unix_nanos: Some(now),
            backwards_exhausted: self.backwards_exhausted,
        })
    }
}

#[derive(Clone)]
struct CatalogEntry {
    route: Route,
    name: String,
    description: Option<String>,
    quote_asset_id: Option<u64>,
}
impl CatalogEntry {
    fn summary(&self) -> ProviderInstrumentSummary {
        ProviderInstrumentSummary {
            symbol: self.route.provider_symbol(),
            display_symbol: self.name.clone(),
            exchange: self.route.venue().into(),
            name: self.description.clone(),
            product_code: None,
            instrument_type: None,
            expiration_date: None,
        }
    }
}
fn search_rank(entry: &CatalogEntry, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(3);
    }
    let name = entry.name.to_ascii_uppercase();
    if name == query || entry.route.provider_symbol().eq_ignore_ascii_case(query) {
        Some(0)
    } else if name.starts_with(query) {
        Some(1)
    } else if name.contains(query)
        || entry
            .description
            .as_ref()
            .is_some_and(|description| description.to_ascii_uppercase().contains(query))
    {
        Some(2)
    } else {
        None
    }
}

struct WorkerPorts {
    controls: Receiver<RealtimeControl>,
    catalog: Receiver<CatalogControl>,
    catalog_events: CatalogPublisher<CatalogEvent>,
    events: SyncSender<RealtimeEvent>,
    history: Receiver<HistoryRequest>,
    completions: SyncSender<Command>,
    generation: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    wake: ProviderCoordinatorWake,
    counters: Arc<StreamCounters>,
}
struct WorkerConfig {
    opener: LinkOpener,
    idle_stop: Duration,
    /// An epoch that stays connected this long clears the retry count.
    healthy_after: Duration,
}

/// One search in progress. Symbol lists load one account per worker turn, so
/// live events keep flowing between catalog round trips, and a failing
/// account or host is skipped instead of failing the whole search.
struct CatalogJob {
    search: SearchProviderInstruments,
    pending: VecDeque<CtraderAccount>,
    loaded: Vec<CtraderAccount>,
    failed_hosts: BTreeSet<bool>,
    host_fault: Option<HostFault>,
    first_failure: Option<String>,
}
/// Lifecycle of the current provider epoch.
#[derive(Default)]
struct EpochState {
    /// The first epoch reuses the startup generation; later epochs increment it.
    used: bool,
    /// Live demand currently holds an open generation.
    open: bool,
    /// Broker authorization is revoked; demand is held without sessions.
    paused: bool,
}
struct Worker {
    ports: WorkerPorts,
    config: WorkerConfig,
    hosts: BTreeMap<bool, HostSession>,
    demand: Demand,
    demand_dirty: bool,
    candle_symbols: BTreeMap<(bool, u64, u64, i32), BTreeSet<String>>,
    epoch_state: EpochState,
    failures: u8,
    healthy_since: Option<Instant>,
    retry_at: Instant,
    reconcile_at: Instant,
    history: Option<HistoryTask>,
    completions: VecDeque<Command>,
    ordinal: u64,
    account_list: Option<(Instant, Vec<CtraderAccount>)>,
    catalogs: BTreeMap<(bool, u64), (Instant, Vec<LightSymbol>)>,
    assets: BTreeMap<(bool, u64), (Instant, BTreeMap<u64, String>)>,
    catalog_job: Option<CatalogJob>,
    searches: BTreeMap<u64, (u64, Vec<CatalogEntry>)>,
    selection_generation: u64,
}
impl Worker {
    fn new(ports: WorkerPorts, config: WorkerConfig) -> Self {
        Self {
            ports,
            config,
            hosts: BTreeMap::new(),
            demand: Demand::default(),
            demand_dirty: false,
            candle_symbols: BTreeMap::new(),
            epoch_state: EpochState::default(),
            failures: 0,
            healthy_since: None,
            retry_at: Instant::now(),
            reconcile_at: Instant::now(),
            history: None,
            completions: VecDeque::new(),
            ordinal: 0,
            account_list: None,
            catalogs: BTreeMap::new(),
            assets: BTreeMap::new(),
            catalog_job: None,
            searches: BTreeMap::new(),
            selection_generation: 0,
        }
    }
    fn epoch(&self) -> u64 {
        self.ports.generation.load(Ordering::Acquire)
    }
    fn publish(&self, event: RealtimeEvent) -> Result<(), String> {
        let generation = event.generation();
        match self.ports.events.try_send(event) {
            Ok(()) => {
                self.ports.wake.notify();
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                self.ports.wake.report_overflow(PROVIDER, generation);
                Err("cTrader event queue overflowed; continuity requires recovery".into())
            }
            Err(TrySendError::Disconnected(_)) => Err("cTrader coordinator stopped".into()),
        }
    }
    fn demanded(&self) -> bool {
        !self.demand.series.is_empty() || !self.demand.instruments.is_empty()
    }
    fn run(mut self) {
        while !self.ports.stop.load(Ordering::Acquire) {
            self.controls();
            self.flush_completions();
            self.catalog();
            if self.epoch_state.paused {
                self.reject_history(DISCONNECTED_DETAIL);
            } else if let Err(error) = self.step() {
                self.recover(error);
            }
            self.close_idle_hosts();
            self.close_idle_epoch();
            if self.hosts.is_empty() {
                thread::sleep(Duration::from_millis(25));
            }
        }
        self.cancel_history("cTrader runtime stopped");
        self.reject_history("cTrader runtime stopped");
        self.close_hosts();
        self.flush_completions();
    }
    fn step(&mut self) -> Result<(), HostFault> {
        if Instant::now() < self.retry_at {
            return Ok(());
        }
        self.open_epoch()?;
        match self.reconcile() {
            Ok(()) => {}
            Err(Failure::Request(error)) => {
                diagnostic!("Aeris cTrader subscription change deferred: {error}");
                self.reconcile_at = Instant::now() + RECONCILE_RETRY;
            }
            Err(Failure::Host(fault)) => return Err(fault),
        }
        self.history_step()?;
        self.poll_events()?;
        if self
            .healthy_since
            .is_some_and(|since| since.elapsed() >= self.config.healthy_after)
        {
            self.failures = 0;
            self.healthy_since = None;
        }
        Ok(())
    }
    fn controls(&mut self) {
        while let Ok(control) = self.ports.controls.try_recv() {
            match control {
                RealtimeControl::Subscribe(next) => {
                    self.demand = next;
                    self.demand_dirty = true;
                    self.reconcile_at = Instant::now();
                }
                RealtimeControl::Stop => self.stop_demand(),
                RealtimeControl::AuthorizationChanged(ready) => {
                    self.close_hosts();
                    self.cancel_history("cTrader disconnected");
                    if let Some(job) = self.catalog_job.take() {
                        self.reject_catalog(
                            job.search.consumer_id,
                            job.search.search_generation,
                            false,
                            Failure::Request(DISCONNECTED_DETAIL.into()),
                        );
                    }
                    self.account_list = None;
                    self.catalogs.clear();
                    self.assets.clear();
                    self.searches.clear();
                    self.candle_symbols.clear();
                    self.epoch_state.open = false;
                    self.demand_dirty = true;
                    self.epoch_state.paused = !ready;
                    self.failures = 0;
                    self.retry_at = Instant::now();
                }
            }
        }
    }
    fn stop_demand(&mut self) {
        self.demand = Demand::default();
        self.candle_symbols.clear();
        self.cancel_history("cTrader live demand stopped");
        // At shutdown the transports are already cancelled; unsubscribing
        // would only fail and misreport the session as broken.
        if self.ports.stop.load(Ordering::Acquire) {
            self.close_hosts();
        }
        let hosts: Vec<_> = self.hosts.keys().copied().collect();
        for live in hosts {
            let Some(host) = self.hosts.get_mut(&live) else {
                continue;
            };
            if let Err(failure) = host.apply(&HostDemand::default()) {
                // A shutdown that begins mid-unsubscribe cancels the transport; the
                // session was not broken.
                if !self.ports.stop.load(Ordering::Acquire) {
                    diagnostic!(
                        "Aeris cTrader unsubscribe failed; closing the session: {}",
                        failure.detail()
                    );
                }
                if let Some(mut host) = self.hosts.remove(&live) {
                    host.link.close();
                }
            }
        }
        self.demand_dirty = false;
        if self.epoch_state.used {
            let _ = self.publish(RealtimeEvent::Disconnected(self.epoch()));
        }
        self.epoch_state.open = false;
    }
    /// The first epoch uses the initial generation. Every later epoch, after a
    /// stop, fault or authorization change, takes a new one.
    fn open_epoch(&mut self) -> Result<(), HostFault> {
        if self.epoch_state.open || !self.demanded() {
            return Ok(());
        }
        let generation = if self.epoch_state.used {
            self.ports
                .generation
                .fetch_add(1, Ordering::AcqRel)
                .checked_add(1)
                .ok_or_else(|| {
                    HostFault::from("cTrader session generation overflowed".to_string())
                })?
        } else {
            self.epoch()
        };
        self.epoch_state.used = true;
        // Subscriptions belong to a generation; a reused session starts clean.
        let stale: Vec<_> = self
            .hosts
            .iter()
            .filter(|(_, host)| !host.idle())
            .map(|(live, _)| *live)
            .collect();
        for live in stale {
            if let Some(mut host) = self.hosts.remove(&live) {
                host.link.close();
            }
        }
        self.candle_symbols.clear();
        self.publish(RealtimeEvent::Connecting(generation))?;
        self.epoch_state.open = true;
        let mut hosts = BTreeSet::new();
        for instrument in self
            .demand
            .instruments
            .iter()
            .map(|(instrument, _)| instrument)
            .chain(self.demand.series.iter().map(|(_, instrument)| instrument))
        {
            if let Ok(route) = parse_instrument_id(&instrument.instrument_id) {
                hosts.insert(route.live);
            }
        }
        for live in hosts {
            self.ensure_host(live).map_err(HostFault::from)?;
        }
        self.demand_dirty = true;
        self.reconcile_at = Instant::now();
        self.publish(RealtimeEvent::Connected(generation))?;
        self.healthy_since = Some(Instant::now());
        Ok(())
    }
    fn ensure_host(&mut self, live: bool) -> Result<&mut HostSession, Failure> {
        if !self.hosts.contains_key(&live) {
            let link =
                (self.config.opener)(host_for(live), &self.ports.stop).map_err(Failure::Host)?;
            diagnostic!("Aeris cTrader {} market session opened", venue(live));
            self.hosts.insert(live, HostSession::new(live, link));
        }
        self.hosts
            .get_mut(&live)
            .ok_or_else(|| Failure::host("cTrader host session is unavailable"))
    }
    fn desired(&mut self) -> BTreeMap<bool, HostDemand> {
        let generation = self.epoch();
        let mut desired = BTreeMap::<bool, HostDemand>::new();
        let mut candle_symbols = BTreeMap::<(bool, u64, u64, i32), BTreeSet<String>>::new();
        let stream = |instrument: &InstallProviderInstrument| {
            let route = parse_instrument_id(&instrument.instrument_id)?;
            let scale = i32::try_from(instrument.price_scale)
                .ok()
                .and_then(|digits| PriceScale::new(digits).ok())
                .ok_or("cTrader instrument price scale is invalid")?;
            Ok::<_, String>((
                route,
                SymbolStream {
                    instrument_id: instrument.instrument_id.clone(),
                    entitlement_id: instrument.entitlement_id.clone(),
                    session_generation: generation,
                    scale,
                },
            ))
        };
        let admit = |desired: &mut BTreeMap<bool, HostDemand>,
                     instrument: &InstallProviderInstrument|
         -> Option<Route> {
            let (route, stream) = match stream(instrument) {
                Ok(admitted) => admitted,
                Err(error) => {
                    diagnostic!("Aeris cTrader demand rejected: {error}");
                    return None;
                }
            };
            let host = desired.entry(route.live).or_default();
            let key = (route.ctid, route.symbol_id);
            if !host.spots.contains_key(&key) && host.spots.len() >= MAXIMUM_STREAM_SYMBOLS {
                diagnostic!(
                    "Aeris cTrader demand rejected: {} exceeds the {MAXIMUM_STREAM_SYMBOLS}-symbol stream bound",
                    super::LoggedInstrument(&instrument.instrument_id)
                );
                return None;
            }
            host.spots.entry(key).or_insert(stream);
            Some(route)
        };
        for (instrument, depth) in &self.demand.instruments {
            if let Some(route) = admit(&mut desired, instrument)
                && *depth
                && let Some(host) = desired.get_mut(&route.live)
            {
                host.depth.insert((route.ctid, route.symbol_id));
            }
        }
        for (series, instrument) in &self.demand.series {
            let Ok(period) = trendbar_period(series.period) else {
                continue;
            };
            if let Some(route) = admit(&mut desired, instrument)
                && let Some(host) = desired.get_mut(&route.live)
            {
                host.trendbars
                    .insert((route.ctid, route.symbol_id, period.wire()));
                candle_symbols
                    .entry((route.live, route.ctid, route.symbol_id, period.wire()))
                    .or_default()
                    .insert(candle_symbol(&instrument.provider_symbol, period));
            }
        }
        self.candle_symbols = candle_symbols;
        desired
    }
    fn reconcile(&mut self) -> Result<(), Failure> {
        if !self.demand_dirty || !self.epoch_state.open || Instant::now() < self.reconcile_at {
            return Ok(());
        }
        let mut desired = self.desired();
        let mut hosts: BTreeSet<bool> = desired.keys().copied().collect();
        hosts.extend(self.hosts.keys().copied());
        for live in hosts {
            let wanted = desired.remove(&live).unwrap_or_default();
            if wanted.spots.is_empty() && !self.hosts.contains_key(&live) {
                continue;
            }
            self.ensure_host(live)?.apply(&wanted)?;
        }
        self.demand_dirty = false;
        Ok(())
    }
    fn history_step(&mut self) -> Result<(), HostFault> {
        if self.history.is_none() {
            if !self.completions.is_empty() {
                return Ok(());
            }
            let Ok(request) = self.ports.history.try_recv() else {
                return Ok(());
            };
            match HistoryTask::begin(request, self.epoch()) {
                Ok(task) => self.history = Some(task),
                Err(retired) => {
                    let (request, error) = *retired;
                    self.queue_completion(request, Err(error));
                    return Ok(());
                }
            }
        }
        let Some(mut task) = self.history.take() else {
            return Ok(());
        };
        if task.request.stop.load(Ordering::Acquire)
            || task.request.provider_generation.0.get() != self.epoch()
        {
            self.queue_completion(task.request, Err(RETIRED_DETAIL.into()));
            return Ok(());
        }
        let advanced = match self.ensure_host(task.route.live) {
            Ok(host) => task.advance(host),
            Err(failure) => Err(failure),
        };
        match advanced {
            Ok(true) => {
                let request = task.request.clone();
                let result = task.snapshot();
                self.queue_completion(request, result);
            }
            Ok(false) => self.history = Some(task),
            Err(Failure::Request(error)) => self.queue_completion(task.request, Err(error)),
            Err(Failure::Host(fault)) => {
                self.queue_completion(task.request, Err(fault.detail.clone()));
                return Err(fault);
            }
        }
        Ok(())
    }
    fn poll_events(&mut self) -> Result<(), HostFault> {
        let hosts: Vec<_> = self.hosts.keys().copied().collect();
        for live in hosts {
            for _ in 0..EVENTS_PER_HOST_TURN {
                let Some(host) = self.hosts.get_mut(&live) else {
                    break;
                };
                let frame = match host.link.next_event(EVENT_POLL) {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(fault) => return Err(HostFault::from(classify(&fault))),
                };
                match frame.payload_type {
                    SPOT_EVENT => self.accept_spot(live, &frame)?,
                    DEPTH_EVENT => self.accept_depth(live, &frame)?,
                    _ => {}
                }
            }
        }
        Ok(())
    }
    fn stamp(&mut self) -> Result<EventStamp, String> {
        self.ordinal = self
            .ordinal
            .checked_add(1)
            .ok_or("cTrader delivery ordinal overflowed")?;
        Ok(EventStamp {
            source_sequence: self.ordinal,
            received_unix_nanos: now_nanos()?,
        })
    }
    fn accept_spot(&mut self, live: bool, frame: &ProtoMessage) -> Result<(), String> {
        if !self.epoch_state.open {
            return Ok(());
        }
        let stamp = self.stamp()?;
        let Some(host) = self.hosts.get_mut(&live) else {
            return Ok(());
        };
        let mut accepted = None;
        for (ctid, streams) in &mut host.streams {
            match streams.apply_spot(frame, stamp) {
                Ok(update) => {
                    accepted = Some((*ctid, update));
                    break;
                }
                Err(MarketDecodeError::AccountMismatch) => {}
                Err(MarketDecodeError::UnknownSymbol) => return Ok(()),
                Err(error) => return Err(format!("cTrader spot event rejected: {error}")),
            }
        }
        let Some((ctid, update)) = accepted else {
            return Ok(());
        };
        let generation = self.epoch();
        if update.crossed {
            self.ports
                .counters
                .crossed_spots
                .fetch_add(1, Ordering::AcqRel);
        }
        if let Some(quote) = update.quote {
            self.publish(RealtimeEvent::Quote(generation, quote))?;
        }
        for live_bar in update.live_bars {
            let key = (live, ctid, update.symbol_id, live_bar.period.wire());
            let Some(symbols) = self.candle_symbols.get(&key) else {
                continue;
            };
            for symbol in symbols {
                self.publish(RealtimeEvent::Candle(
                    generation,
                    symbol.clone(),
                    live_bar.bar,
                ))?;
            }
        }
        Ok(())
    }
    fn accept_depth(&mut self, live: bool, frame: &ProtoMessage) -> Result<(), HostFault> {
        if !self.epoch_state.open {
            return Ok(());
        }
        let stamp = self.stamp()?;
        let Some(host) = self.hosts.get_mut(&live) else {
            return Ok(());
        };
        let mut accepted = None;
        let mut resubscribe = None;
        for (ctid, streams) in &mut host.streams {
            match streams.apply_depth(frame, stamp) {
                Ok(update) => {
                    accepted = Some(update);
                    break;
                }
                Err(MarketDecodeError::AccountMismatch) => {}
                Err(MarketDecodeError::UnknownSymbol) => return Ok(()),
                Err(
                    error @ (MarketDecodeError::UnknownDepthQuote
                    | MarketDecodeError::LimitExceeded(_)),
                ) => {
                    diagnostic!("Aeris cTrader depth continuity lost: {error}");
                    resubscribe = Some(*ctid);
                    break;
                }
                Err(error) => return Err(format!("cTrader depth event rejected: {error}").into()),
            }
        }
        if let Some(ctid) = resubscribe {
            return match host.resubscribe_depth(ctid) {
                Ok(()) => Ok(()),
                Err(Failure::Request(error)) => {
                    diagnostic!("Aeris cTrader depth resubscription deferred: {error}");
                    self.demand_dirty = true;
                    self.reconcile_at = Instant::now() + RECONCILE_RETRY;
                    Ok(())
                }
                Err(Failure::Host(fault)) => Err(fault),
            };
        }
        match accepted {
            Some(DepthUpdate::Snapshot(snapshot)) => {
                Ok(self.publish(RealtimeEvent::Depth(self.epoch(), snapshot))?)
            }
            Some(DepthUpdate::Crossed) => {
                self.ports
                    .counters
                    .crossed_depth
                    .fetch_add(1, Ordering::AcqRel);
                Ok(())
            }
            None => Ok(()),
        }
    }
    fn catalog(&mut self) {
        if self.catalog_job.is_some() {
            self.advance_search();
            return;
        }
        let Ok(control) = self.ports.catalog.try_recv() else {
            return;
        };
        match control {
            CatalogControl::Search(search) => {
                let accounts = if self.epoch_state.paused {
                    Err(Failure::Request(DISCONNECTED_DETAIL.into()))
                } else {
                    self.accounts()
                };
                match accounts {
                    Ok(accounts) => {
                        self.catalog_job = Some(CatalogJob {
                            search,
                            pending: accounts
                                .into_iter()
                                .take(MAXIMUM_CATALOG_ACCOUNTS)
                                .collect(),
                            loaded: Vec::new(),
                            failed_hosts: BTreeSet::new(),
                            host_fault: None,
                            first_failure: None,
                        });
                        self.advance_search();
                    }
                    Err(failure) => self.reject_catalog(
                        search.consumer_id,
                        search.search_generation,
                        false,
                        failure,
                    ),
                }
            }
            CatalogControl::Select(select) => {
                let result = if self.epoch_state.paused {
                    Err(Failure::Request(DISCONNECTED_DETAIL.into()))
                } else {
                    self.select(&select)
                };
                match result {
                    Ok(event) => self.send_catalog(event),
                    Err(failure) => self.reject_catalog(
                        select.consumer_id,
                        select.selection_generation,
                        true,
                        failure,
                    ),
                }
            }
        }
    }
    fn send_catalog(&self, event: CatalogEvent) {
        if self.ports.catalog_events.send(event).is_err() {
            diagnostic!("Aeris cTrader catalog publication was dropped");
        }
    }
    fn reject_catalog(&mut self, consumer: u64, command: u64, selection: bool, failure: Failure) {
        diagnostic!("Aeris cTrader catalog request failed: {}", failure.detail());
        if let Failure::Host(fault) = failure {
            self.recover(fault);
        }
        self.send_catalog(CatalogEvent::Rejected {
            rejection: ProviderCatalogRejected {
                consumer_id: consumer,
                provider: PROVIDER.into(),
                provider_generation: Some(self.epoch()),
                command_generation: command,
                reason: ProviderCatalogRejectionReason::SearchRejected,
            },
            selection,
        });
    }
    /// Loads cached catalogs without a round trip and at most one uncached
    /// account per call, then publishes once every account was attempted.
    fn advance_search(&mut self) {
        let Some(mut job) = self.catalog_job.take() else {
            return;
        };
        while let Some(account) = job.pending.pop_front() {
            if job.failed_hosts.contains(&account.is_live) {
                continue;
            }
            let fetched = !self.catalog_fresh(&account);
            match self.symbols(&account) {
                Ok(_) => job.loaded.push(account),
                Err(Failure::Request(detail)) => {
                    diagnostic!("Aeris cTrader catalog skipped one account: {detail}");
                    job.first_failure.get_or_insert(detail);
                }
                Err(Failure::Host(fault)) => {
                    diagnostic!(
                        "Aeris cTrader catalog skipped {}: {}",
                        venue(account.is_live),
                        fault.detail
                    );
                    job.failed_hosts.insert(account.is_live);
                    job.first_failure
                        .get_or_insert_with(|| fault.detail.clone());
                    job.host_fault.get_or_insert(fault);
                }
            }
            if fetched && !job.pending.is_empty() {
                self.catalog_job = Some(job);
                return;
            }
        }
        let CatalogJob {
            search,
            loaded,
            host_fault,
            first_failure,
            ..
        } = job;
        if let (true, Some(detail)) = (loaded.is_empty(), first_failure) {
            let failure = host_fault.map_or(Failure::Request(detail), Failure::Host);
            self.reject_catalog(search.consumer_id, search.search_generation, false, failure);
        } else {
            let event = self.search_results(&search, &loaded);
            self.send_catalog(event);
            if let Some(fault) = host_fault {
                self.recover(fault);
            }
        }
    }
    /// The token's account list is the same on both hosts. It is cached so a
    /// search does not reopen a host, and a demo session opened only to list
    /// accounts closes at once when every account is live.
    fn accounts(&mut self) -> Result<Vec<CtraderAccount>, Failure> {
        if let Some(host) = self.hosts.values().next() {
            let accounts = host.link.accounts().to_vec();
            self.account_list = Some((Instant::now(), accounts.clone()));
            return Ok(accounts);
        }
        if let Some((loaded_at, accounts)) = &self.account_list
            && loaded_at.elapsed() < CATALOG_TTL
        {
            return Ok(accounts.clone());
        }
        let accounts = self.ensure_host(false)?.link.accounts().to_vec();
        if accounts.iter().all(|account| account.is_live)
            && let Some(mut host) = self.hosts.remove(&false)
        {
            host.link.close();
            diagnostic!(
                "Aeris cTrader {} market session closed; no demo accounts",
                venue(false)
            );
        }
        self.account_list = Some((Instant::now(), accounts.clone()));
        Ok(accounts)
    }
    fn catalog_fresh(&self, account: &CtraderAccount) -> bool {
        self.catalogs
            .get(&(account.is_live, account.ctid))
            .is_some_and(|(loaded_at, _)| loaded_at.elapsed() < CATALOG_TTL)
    }
    fn symbols(&mut self, account: &CtraderAccount) -> Result<&[LightSymbol], Failure> {
        let key = (account.is_live, account.ctid);
        if !self.catalog_fresh(account) {
            let host = self.ensure_host(account.is_live)?;
            host.authorize(account.ctid)?;
            let frame = host.request(
                MarketRequest::symbols_list(account.ctid).map_err(|error| request_error(&error))?,
            )?;
            let symbols =
                decode_symbol_list(&frame, account.ctid).map_err(|error| request_error(&error))?;
            self.catalogs.insert(key, (Instant::now(), symbols));
        }
        self.catalogs
            .get(&key)
            .map(|(_, symbols)| symbols.as_slice())
            .ok_or_else(|| Failure::Request("cTrader symbol catalog is unavailable".into()))
    }
    fn search_results(
        &mut self,
        search: &SearchProviderInstruments,
        accounts: &[CtraderAccount],
    ) -> CatalogEvent {
        let query = search.query.trim().to_ascii_uppercase();
        let mut matches = Vec::new();
        for account in accounts {
            let Some((_, symbols)) = self.catalogs.get(&(account.is_live, account.ctid)) else {
                continue;
            };
            for symbol in symbols.iter().filter(|symbol| symbol.enabled) {
                let entry = CatalogEntry {
                    route: Route {
                        live: account.is_live,
                        ctid: account.ctid,
                        symbol_id: symbol.symbol_id,
                    },
                    name: symbol.name.clone(),
                    description: symbol.description.clone(),
                    quote_asset_id: symbol.quote_asset_id,
                };
                if let Some(rank) = search_rank(&entry, &query) {
                    matches.push((rank, entry));
                }
            }
        }
        matches.sort_by(|(left_rank, left), (right_rank, right)| {
            (left_rank, &left.name, left.route).cmp(&(right_rank, &right.name, right.route))
        });
        let limit = usize::try_from(search.maximum_results)
            .unwrap_or(MAXIMUM_SEARCH_RESULTS)
            .min(MAXIMUM_SEARCH_RESULTS);
        let entries: Vec<_> = matches
            .into_iter()
            .take(limit)
            .map(|(_, entry)| entry)
            .collect();
        let instruments = entries.iter().map(CatalogEntry::summary).collect();
        if !self.searches.contains_key(&search.consumer_id)
            && self.searches.len() >= MAXIMUM_CONSUMERS
        {
            self.searches.pop_first();
        }
        self.searches
            .insert(search.consumer_id, (search.search_generation, entries));
        CatalogEvent::Search(ProviderInstrumentSearchResult {
            consumer_id: search.consumer_id,
            provider: PROVIDER.into(),
            provider_generation: self.epoch(),
            search_generation: search.search_generation,
            instruments,
        })
    }
    fn select(&mut self, selection: &SelectProviderInstrument) -> Result<CatalogEvent, Failure> {
        if selection.entitlement_id != ENTITLEMENT {
            return Err(Failure::Request("cTrader entitlement changed".into()));
        }
        let entry = self
            .searches
            .get(&selection.consumer_id)
            .filter(|(generation, _)| *generation == selection.search_generation)
            .and_then(|(_, entries)| {
                entries.iter().find(|entry| {
                    entry.route.provider_symbol() == selection.symbol
                        && entry.route.venue() == selection.exchange
                })
            })
            .cloned()
            .ok_or_else(|| {
                Failure::Request("cTrader selection is no longer in the current search".into())
            })?;
        let route = entry.route;
        let host = self.ensure_host(route.live)?;
        host.authorize(route.ctid)?;
        let frame = host.request(
            MarketRequest::symbol_by_id(route.ctid, &[route.symbol_id])
                .map_err(|error| request_error(&error))?,
        )?;
        let spec = decode_symbol_by_id(&frame, route.ctid)
            .map_err(|error| request_error(&error))?
            .into_iter()
            .find(|spec| spec.symbol_id == route.symbol_id)
            .ok_or_else(|| Failure::Request("cTrader symbol details are missing".into()))?;
        let quote_asset = entry
            .quote_asset_id
            .ok_or_else(|| Failure::Request("cTrader symbol quote asset is missing".into()))?;
        let currency = self.asset_name(route, quote_asset)?;
        self.selection_generation = self
            .selection_generation
            .checked_add(1)
            .ok_or_else(|| Failure::Request("cTrader selection generation overflowed".into()))?;
        Ok(CatalogEvent::Selection {
            consumer_id: selection.consumer_id,
            command_generation: selection.selection_generation,
            instrument: InstallProviderInstrument {
                provider: PROVIDER.into(),
                session_generation: self.epoch(),
                selection_generation: self.selection_generation,
                instrument_id: route.instrument_id(),
                provider_symbol: route.provider_symbol(),
                display_symbol: entry.name,
                venue_id: route.venue().into(),
                price_scale: u32::from(spec.price_scale.digits()),
                quantity_scale: DEPTH_QUANTITY_SCALE,
                entitlement_id: ENTITLEMENT.into(),
                price_increment: Some(spec.tick_units()),
                // Quantities are in cents of the base unit and prices are in
                // the quote asset, so one unit moving one price unit is one
                // unit of quote currency.
                contract_metadata: Some(Box::new(ProviderContractMetadata {
                    point_value: Some(1),
                    point_value_scale: Some(0),
                    currency: Some(currency),
                    order_quantity_increment: Some(spec.step_volume),
                    ..ProviderContractMetadata::default()
                })),
            },
        })
    }
    /// The account's asset names are cached like its symbol list.
    fn asset_name(&mut self, route: Route, asset_id: u64) -> Result<String, Failure> {
        let key = (route.live, route.ctid);
        let fresh = self
            .assets
            .get(&key)
            .is_some_and(|(loaded_at, _)| loaded_at.elapsed() < CATALOG_TTL);
        if !fresh {
            let host = self.ensure_host(route.live)?;
            host.authorize(route.ctid)?;
            let frame = host.request(
                MarketRequest::asset_list(route.ctid).map_err(|error| request_error(&error))?,
            )?;
            let assets = decode_asset_list(&frame, route.ctid)
                .map_err(|error| request_error(&error))?
                .into_iter()
                .map(|asset| (asset.asset_id, asset.name))
                .collect();
            self.assets.insert(key, (Instant::now(), assets));
        }
        self.assets
            .get(&key)
            .and_then(|(_, assets)| assets.get(&asset_id))
            .cloned()
            .ok_or_else(|| Failure::Request("cTrader quote asset is unknown".into()))
    }
    fn queue_completion(
        &mut self,
        request: HistoryRequest,
        result: Result<HistorySnapshot, String>,
    ) {
        // One task runs at a time and new work waits for this queue to drain.
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
    fn cancel_history(&mut self, detail: &str) {
        if let Some(task) = self.history.take() {
            self.queue_completion(task.request, Err(detail.into()));
        }
    }
    /// Bounded by the history channel capacity; completions drain each turn.
    fn reject_history(&mut self, detail: &str) {
        if !self.completions.is_empty() {
            return;
        }
        while let Ok(request) = self.ports.history.try_recv() {
            self.queue_completion(request, Err(detail.into()));
        }
    }
    fn close_hosts(&mut self) {
        for (live, mut host) in std::mem::take(&mut self.hosts) {
            host.link.close();
            diagnostic!("Aeris cTrader {} market session closed", venue(live));
        }
    }
    fn close_idle_hosts(&mut self) {
        let idle_stop = self.config.idle_stop;
        let busy = self.history.as_ref().map(|task| task.route.live);
        self.hosts.retain(|live, host| {
            let keep = !host.idle() || busy == Some(*live) || host.last_use.elapsed() < idle_stop;
            if !keep {
                host.link.close();
                diagnostic!(
                    "Aeris cTrader {} market session closed while idle",
                    venue(*live)
                );
            }
            keep
        });
    }
    /// An epoch whose demand is gone ends once its last host session has
    /// idled out, so the next demand opens a fresh generation instead of
    /// reviving retired session state. Hosts stay open for the idle-stop
    /// window first, which keeps quick symbol changes on the same session.
    fn close_idle_epoch(&mut self) {
        if !self.epoch_state.open || self.demanded() || !self.hosts.is_empty() {
            return;
        }
        self.epoch_state.open = false;
        self.candle_symbols.clear();
        let _ = self.publish(RealtimeEvent::Disconnected(self.epoch()));
    }
    /// The only reconnect policy for cTrader sessions: exponential local
    /// backoff with a retry limit, or exactly the server-directed wait.
    fn recover(&mut self, fault: HostFault) {
        let HostFault {
            detail: error,
            wait,
        } = fault;
        diagnostic!("Aeris cTrader market recovery: {error}");
        self.close_hosts();
        self.cancel_history(&error);
        self.candle_symbols.clear();
        self.demand_dirty = true;
        self.healthy_since = None;
        let delay = if let Some(wait) = wait {
            wait
        } else {
            self.failures = self.failures.saturating_add(1);
            self.epoch_state.paused = self.failures >= MAXIMUM_FAILURES;
            Duration::from_secs(3u64.saturating_mul(1u64 << self.failures.min(4)))
        };
        self.retry_at = Instant::now() + delay;
        if !self.epoch_state.open {
            return;
        }
        self.epoch_state.open = false;
        let generation = self.epoch();
        let event = if self.epoch_state.paused {
            RealtimeEvent::Failed(
                generation,
                format!("{error}. Automatic retry limit reached; connect cTrader to retry."),
            )
        } else {
            RealtimeEvent::Recovering(generation, error)
        };
        if let Err(publish_error) = self.publish(event) {
            diagnostic!("Aeris cTrader provider state publication failed: {publish_error}");
            if self.epoch_state.paused {
                self.ports.wake.report_failure(PROVIDER, generation);
            }
        }
    }
}

pub(super) fn start_record(
    descriptor: super::ProviderDescriptor,
    completions: &SyncSender<Command>,
    wake: ProviderCoordinatorWake,
    activity: &Arc<Mutex<BTreeSet<String>>>,
    counters: &Arc<StreamCounters>,
) -> Result<ProviderRuntimeRecord, String> {
    let access = Arc::new(Mutex::new(HostedAccess::default()));
    let opener: LinkOpener = Box::new(move |host, stop| {
        let session = open_hosted(&access, host, stop)?;
        Ok(Box::new(HostedLink {
            session,
            access: Arc::clone(&access),
            stop: Arc::clone(stop),
        }) as Box<dyn CtraderMarketLink>)
    });
    spawn_record(
        descriptor,
        completions,
        wake,
        activity,
        counters,
        WorkerConfig {
            opener,
            idle_stop: IDLE_STOP,
            healthy_after: HEALTHY_SESSION,
        },
    )
}

fn spawn_record(
    descriptor: super::ProviderDescriptor,
    completions: &SyncSender<Command>,
    wake: ProviderCoordinatorWake,
    activity: &Arc<Mutex<BTreeSet<String>>>,
    counters: &Arc<StreamCounters>,
    config: WorkerConfig,
) -> Result<ProviderRuntimeRecord, String> {
    let cancellation = Arc::new(AtomicBool::new(false));
    let lifecycle = Arc::new(ProviderRuntimeLifecycle::default());
    let generation = Arc::new(AtomicU64::new(1));
    let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (controls, controls_rx) = mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
    let (events_tx, events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let (catalog_controls, catalog_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
    let (catalog_tx, catalog_events) = mpsc::sync_channel(COMMAND_CAPACITY);
    let ports = WorkerPorts {
        controls: controls_rx,
        catalog: catalog_rx,
        catalog_events: CatalogPublisher::new(catalog_tx, descriptor.id, wake.clone()),
        events: events_tx,
        history: history_rx,
        completions: completions.clone(),
        generation,
        stop: Arc::clone(&cancellation),
        wake,
        counters: Arc::clone(counters),
    };
    let worker_activity = Arc::clone(activity);
    let worker = thread::Builder::new()
        .name("aeris-ctrader-market".into())
        .spawn(move || {
            let _guard = ActiveWorkerGuard::register("aeris-ctrader-market", worker_activity);
            Worker::new(ports, config).run();
        })
        .map_err(|error| error.to_string())?;
    Ok(ProviderRuntimeRecord {
        descriptor,
        history: history_tx,
        cancellation,
        lifecycle,
        realtime: ProviderRealtimeChannels {
            enabled: true,
            channels: ProviderRealtimeChannelSet::Ctrader { controls, events },
        },
        catalog: ProviderCatalogChannels {
            enabled: true,
            channels: ProviderCatalogChannelSet::Ctrader {
                controls: catalog_controls,
                events: catalog_events,
            },
        },
        workers: vec![worker],
    })
}

#[cfg(test)]
#[path = "ctrader_tests.rs"]
mod ctrader_tests;
