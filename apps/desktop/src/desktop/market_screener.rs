//! Screener page state: one runtime market-screen consumer, the latest listed-market
//! statistics it delivered, and the presentation filters over them. Statistics refresh on a
//! bounded cadence only while the page is shown; leaving the page stops all screen traffic.

use super::*;
use aeris_contracts::{
    ProviderConnectionKind, ProviderMarketKind, ProviderMarketStatistics,
    ProviderPresentationDescriptor, ScreenProviderMarkets,
};
use gpui::{ScrollStrategy, UniformListScrollHandle};
use std::cmp::Ordering as CmpOrdering;

/// Upper bound on the rows one screen returns: above every market a provider lists today, so
/// the page shows the full listing, and within the runtime's catalog bound.
const SCREENER_MAXIMUM_RESULTS: u32 = 2_048;
const SCREENER_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// How often the visible page checks whether a refresh is due.
pub(super) const SCREENER_REFRESH_TICK: Duration = Duration::from_secs(1);
/// An unanswered screen older than this is presumed lost; the next refresh supersedes it.
const SCREENER_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const SCREENER_MESSAGES_PER_POLL: usize = 16;
const UNAVAILABLE_VALUE: &str = "—";

/// The application page shown under the title bar.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum AppView {
    #[default]
    Terminal,
    Screener,
}

impl AppView {
    pub(super) const ALL: [Self; 2] = [Self::Terminal, Self::Screener];

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Screener => "Screener",
        }
    }

    pub(super) const fn icon(self) -> HugeIcon {
        match self {
            Self::Terminal => HugeIcon::Chart,
            Self::Screener => HugeIcon::DataPanel,
        }
    }
}

const MARKET_KINDS: [ProviderMarketKind; 5] = [
    ProviderMarketKind::Perpetual,
    ProviderMarketKind::Spot,
    ProviderMarketKind::Equity,
    ProviderMarketKind::Index,
    ProviderMarketKind::Future,
];

/// The short badge shown beside a market's symbol.
pub(super) const fn market_kind_label(kind: ProviderMarketKind) -> &'static str {
    match kind {
        ProviderMarketKind::Perpetual => "Perp",
        ProviderMarketKind::Spot => "Spot",
        ProviderMarketKind::Equity => "Equity",
        ProviderMarketKind::Index => "Index",
        ProviderMarketKind::Future => "Future",
    }
}

const fn market_kind_position(kind: ProviderMarketKind) -> usize {
    match kind {
        ProviderMarketKind::Perpetual => 0,
        ProviderMarketKind::Spot => 1,
        ProviderMarketKind::Equity => 2,
        ProviderMarketKind::Index => 3,
        ProviderMarketKind::Future => 4,
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ScreenerCategory {
    #[default]
    All,
    Kind(ProviderMarketKind),
}

impl ScreenerCategory {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::All => "All markets",
            Self::Kind(ProviderMarketKind::Perpetual) => "Perpetuals",
            Self::Kind(ProviderMarketKind::Spot) => "Spot",
            Self::Kind(ProviderMarketKind::Equity) => "Stocks & ETFs",
            Self::Kind(ProviderMarketKind::Index) => "Indices",
            Self::Kind(ProviderMarketKind::Future) => "Futures",
        }
    }

    fn includes(self, kind: Option<ProviderMarketKind>) -> bool {
        match self {
            Self::All => true,
            Self::Kind(category) => kind == Some(category),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScreenerColumn {
    Market,
    Price,
    Change,
    Volume,
    OpenInterest,
    Funding,
}

impl ScreenerColumn {
    const fn default_descending(self) -> bool {
        !matches!(self, Self::Market)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ScreenerSort {
    pub(super) column: ScreenerColumn,
    pub(super) descending: bool,
}

impl Default for ScreenerSort {
    fn default() -> Self {
        Self {
            column: ScreenerColumn::Volume,
            descending: true,
        }
    }
}

impl ScreenerSort {
    /// Clicking the sorted column flips its direction; another column starts in its natural one.
    pub(super) fn toggled(self, column: ScreenerColumn) -> Self {
        if self.column == column {
            Self {
                column,
                descending: !self.descending,
            }
        } else {
            Self {
                column,
                descending: column.default_descending(),
            }
        }
    }
}

/// One presented screener row; every value is formatted once when the rows are rebuilt.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ScreenerRow {
    pub(super) symbol: String,
    pub(super) exchange: String,
    pub(super) display_symbol: String,
    pub(super) kind: Option<ProviderMarketKind>,
    pub(super) price: String,
    pub(super) change: String,
    pub(super) change_direction: Option<CmpOrdering>,
    pub(super) volume: String,
    pub(super) open_interest: String,
    pub(super) funding: String,
    pub(super) funding_direction: Option<CmpOrdering>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ScreenerCategoryCounts {
    pub(super) all: usize,
    by_kind: [usize; MARKET_KINDS.len()],
}

impl ScreenerCategoryCounts {
    pub(super) const fn get(self, category: ScreenerCategory) -> usize {
        match category {
            ScreenerCategory::All => self.all,
            ScreenerCategory::Kind(kind) => self.by_kind[market_kind_position(kind)],
        }
    }

    /// "All markets" followed by every category the current screen lists.
    pub(super) fn categories(self) -> Vec<ScreenerCategory> {
        std::iter::once(ScreenerCategory::All)
            .chain(
                MARKET_KINDS
                    .into_iter()
                    .map(ScreenerCategory::Kind)
                    .filter(|category| self.get(*category) > 0),
            )
            .collect()
    }
}

/// Optional columns, shown only when some market in the screen reports them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ScreenerReportedColumns {
    open_interest: bool,
    funding: bool,
}

impl ScreenerReportedColumns {
    fn of(markets: &[ProviderMarketStatistics]) -> Self {
        Self {
            open_interest: markets
                .iter()
                .any(|market| market.open_interest_notional.is_some()),
            funding: markets.iter().any(|market| market.funding_rate.is_some()),
        }
    }

    pub(super) const fn shows(self, column: ScreenerColumn) -> bool {
        match column {
            ScreenerColumn::OpenInterest => self.open_interest,
            ScreenerColumn::Funding => self.funding,
            ScreenerColumn::Market
            | ScreenerColumn::Price
            | ScreenerColumn::Change
            | ScreenerColumn::Volume => true,
        }
    }
}

/// Sort keys derived once per statistics row, in display units.
struct ScreenerEntry<'a> {
    statistics: &'a ProviderMarketStatistics,
    kind: Option<ProviderMarketKind>,
    price: Option<f64>,
    change_ratio: Option<f64>,
    volume: Option<f64>,
    open_interest: Option<f64>,
    hourly_funding: Option<f64>,
}

impl<'a> ScreenerEntry<'a> {
    fn new(statistics: &'a ProviderMarketStatistics) -> Self {
        Self {
            statistics,
            kind: statistics.kind,
            price: statistics
                .mark_price
                .map(|value| fixed_to_f64(value, statistics.price_scale)),
            change_ratio: change_ratio(statistics),
            volume: statistics
                .day_notional_volume
                .map(|value| fixed_to_f64(value, statistics.notional_scale))
                .or_else(|| {
                    statistics
                        .day_volume
                        .map(|value| fixed_to_f64(value, statistics.volume_scale))
                }),
            open_interest: statistics
                .open_interest_notional
                .map(|value| fixed_to_f64(value, statistics.notional_scale)),
            hourly_funding: hourly_funding(statistics),
        }
    }

    fn key(&self, column: ScreenerColumn) -> Option<f64> {
        match column {
            ScreenerColumn::Market => None,
            ScreenerColumn::Price => self.price,
            ScreenerColumn::Change => self.change_ratio,
            ScreenerColumn::Volume => self.volume,
            ScreenerColumn::OpenInterest => self.open_interest,
            ScreenerColumn::Funding => self.hourly_funding,
        }
    }

    fn display_symbol(&self) -> &str {
        &self.statistics.instrument.display_symbol
    }

    fn matches(&self, query: &str) -> bool {
        query.is_empty()
            || [
                self.statistics.instrument.display_symbol.as_str(),
                self.statistics.instrument.symbol.as_str(),
            ]
            .into_iter()
            .any(|value| value.to_lowercase().contains(query))
    }
}

fn fixed_to_f64(value: i64, scale: u32) -> f64 {
    let exponent = i32::try_from(scale.min(18)).unwrap_or(18);
    value.to_f64().unwrap_or(0.0) / 10_f64.powi(exponent)
}

fn change_ratio(statistics: &ProviderMarketStatistics) -> Option<f64> {
    let (mark, previous) = statistics.mark_price.zip(statistics.previous_day_price)?;
    (previous != 0)
        .then(|| (mark - previous).to_f64().unwrap_or(0.0) / previous.to_f64().unwrap_or(1.0))
}

/// Funding normalized to one hour, so rows reported on different intervals compare directly.
fn hourly_funding(statistics: &ProviderMarketStatistics) -> Option<f64> {
    let rate = statistics.funding_rate?;
    let interval = statistics
        .funding_interval_seconds
        .filter(|seconds| *seconds > 0)?;
    Some(fixed_to_f64(rate, statistics.funding_rate_scale) * 3_600.0 / f64::from(interval))
}

/// Present values in the requested direction; unreported values always sort last.
fn compare_keys(left: Option<f64>, right: Option<f64>, descending: bool) -> CmpOrdering {
    match (left, right) {
        (Some(left), Some(right)) => {
            let ordering = left.total_cmp(&right);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        }
        (Some(_), None) => CmpOrdering::Less,
        (None, Some(_)) => CmpOrdering::Greater,
        (None, None) => CmpOrdering::Equal,
    }
}

fn compare_entries(
    left: &ScreenerEntry<'_>,
    right: &ScreenerEntry<'_>,
    sort: ScreenerSort,
) -> CmpOrdering {
    let primary = if sort.column == ScreenerColumn::Market {
        let ordering = left
            .display_symbol()
            .to_lowercase()
            .cmp(&right.display_symbol().to_lowercase());
        if sort.descending {
            ordering.reverse()
        } else {
            ordering
        }
    } else {
        compare_keys(
            left.key(sort.column),
            right.key(sort.column),
            sort.descending,
        )
    };
    primary
        .then_with(|| compare_keys(left.volume, right.volume, true))
        .then_with(|| left.display_symbol().cmp(right.display_symbol()))
}

/// Filters, orders and formats the latest statistics for presentation.
pub(super) fn screener_rows(
    markets: &[ProviderMarketStatistics],
    category: ScreenerCategory,
    query: &str,
    sort: ScreenerSort,
) -> Vec<ScreenerRow> {
    let query = query.trim().to_lowercase();
    let mut entries = markets
        .iter()
        .map(ScreenerEntry::new)
        .filter(|entry| category.includes(entry.kind) && entry.matches(&query))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| compare_entries(left, right, sort));
    entries.iter().map(screener_row).collect()
}

pub(super) fn screener_category_counts(
    markets: &[ProviderMarketStatistics],
) -> ScreenerCategoryCounts {
    markets.iter().fold(
        ScreenerCategoryCounts::default(),
        |mut counts, statistics| {
            counts.all += 1;
            if let Some(kind) = statistics.kind {
                counts.by_kind[market_kind_position(kind)] += 1;
            }
            counts
        },
    )
}

fn screener_row(entry: &ScreenerEntry<'_>) -> ScreenerRow {
    let statistics = entry.statistics;
    let instrument = &statistics.instrument;
    ScreenerRow {
        symbol: instrument.symbol.clone(),
        exchange: instrument.exchange.clone(),
        display_symbol: instrument.display_symbol.clone(),
        kind: entry.kind,
        price: statistics.mark_price.map_or_else(
            || UNAVAILABLE_VALUE.to_string(),
            |value| grouped_decimal(&market_price_text(value, statistics.price_scale)),
        ),
        change: entry.change_ratio.map_or_else(
            || UNAVAILABLE_VALUE.to_string(),
            |ratio| format!("{:+.2}%", ratio * 100.0),
        ),
        change_direction: statistics
            .mark_price
            .zip(statistics.previous_day_price)
            .map(|(mark, previous)| mark.cmp(&previous)),
        volume: if statistics.day_notional_volume.is_some() {
            compact_notional(statistics.day_notional_volume, statistics.notional_scale)
        } else {
            compact_quantity(statistics.day_volume, statistics.volume_scale)
        },
        open_interest: compact_notional(
            statistics.open_interest_notional,
            statistics.notional_scale,
        ),
        funding: entry.hourly_funding.map_or_else(
            || UNAVAILABLE_VALUE.to_string(),
            |rate| format!("{:+.4}%", rate * 100.0),
        ),
        funding_direction: statistics.funding_rate.map(|rate| rate.cmp(&0)),
    }
}

/// Inserts thousands separators into the whole part of a plain decimal string.
pub(super) fn grouped_decimal(text: &str) -> String {
    let (sign, unsigned) = text
        .strip_prefix('-')
        .map_or(("", text), |unsigned| ("-", unsigned));
    let (whole, fraction) = unsigned
        .split_once('.')
        .map_or((unsigned, None), |(whole, fraction)| {
            (whole, Some(fraction))
        });
    let mut grouped = String::with_capacity(text.len() + whole.len() / 3);
    for (index, digit) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    match fraction {
        Some(fraction) => format!("{sign}{grouped}.{fraction}"),
        None => format!("{sign}{grouped}"),
    }
}

pub(super) fn compact_notional(value: Option<i64>, scale: u32) -> String {
    compact_amount(value, scale, "$")
}

/// Shares or contracts, compacted like a notional but without a currency mark.
pub(super) fn compact_quantity(value: Option<i64>, scale: u32) -> String {
    compact_amount(value, scale, "")
}

fn compact_amount(value: Option<i64>, scale: u32, currency: &str) -> String {
    let Some(value) = value else {
        return UNAVAILABLE_VALUE.to_string();
    };
    let amount = fixed_to_f64(value, scale);
    let sign = if amount < 0.0 { "-" } else { "" };
    let magnitude = amount.abs();
    for (threshold, suffix) in [
        (1_000_000_000_000.0, "T"),
        (1_000_000_000.0, "B"),
        (1_000_000.0, "M"),
        (1_000.0, "K"),
    ] {
        if magnitude >= threshold {
            return format!("{sign}{currency}{:.2}{suffix}", magnitude / threshold);
        }
    }
    format!("{sign}{currency}{magnitude:.2}")
}

/// Wall-clock capture time as `HH:MM:SS UTC`.
pub(super) fn capture_time_label(captured_at_unix_millis: i64) -> String {
    let seconds = captured_at_unix_millis.div_euclid(1_000).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02} UTC",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScreenerStatus {
    /// No screen has been answered yet.
    Loading,
    /// Rows reflect the latest answered screen.
    Ready,
}

struct PendingScreenerSelection {
    generation: u64,
    display_symbol: String,
}

/// What one poll of the screen consumer changed.
#[derive(Default)]
pub(super) struct ScreenerPoll {
    pub(super) changed: bool,
    /// A screener row resolved to an installed instrument that the terminal should open.
    pub(super) opened: Option<InstallProviderInstrument>,
}

pub(super) struct MarketScreener {
    /// Every provider that answers a market screen, in registration order.
    providers: Vec<&'static ProviderPresentationDescriptor>,
    provider: Option<&'static ProviderPresentationDescriptor>,
    worker: Option<MarketDataWorker>,
    markets: Vec<ProviderMarketStatistics>,
    captured_at_unix_millis: Option<i64>,
    displayed_generation: u64,
    next_generation: u64,
    pending_screen: Option<(u64, Instant)>,
    next_refresh_at: Option<Instant>,
    pending_selection: Option<PendingScreenerSelection>,
    message: Option<String>,
    sort: ScreenerSort,
    category: ScreenerCategory,
    query: String,
    rows: Rc<Vec<ScreenerRow>>,
    counts: ScreenerCategoryCounts,
    reported_columns: ScreenerReportedColumns,
    search_input: Option<Entity<InputState>>,
    pub(super) scroll: UniformListScrollHandle,
    /// Refresh cadence task; held only while the page is shown, and dropping it cancels it.
    refresh_task: Option<Task<()>>,
}

impl Default for MarketScreener {
    fn default() -> Self {
        let providers = aeris_market_runtime::built_in_provider_presentations()
            .iter()
            .filter(|descriptor| descriptor.market_screen_available)
            .collect::<Vec<_>>();
        Self {
            provider: providers.first().copied(),
            providers,
            worker: None,
            markets: Vec::new(),
            captured_at_unix_millis: None,
            displayed_generation: 0,
            next_generation: 1,
            pending_screen: None,
            next_refresh_at: None,
            pending_selection: None,
            message: None,
            sort: ScreenerSort::default(),
            category: ScreenerCategory::default(),
            query: String::new(),
            rows: Rc::new(Vec::new()),
            counts: ScreenerCategoryCounts::default(),
            reported_columns: ScreenerReportedColumns::default(),
            search_input: None,
            scroll: UniformListScrollHandle::new(),
            refresh_task: None,
        }
    }
}

impl MarketScreener {
    pub(super) fn providers(&self) -> &[&'static ProviderPresentationDescriptor] {
        &self.providers
    }

    pub(super) fn provider_id(&self) -> Option<&'static str> {
        self.provider.map(|descriptor| descriptor.id)
    }

    pub(super) fn provider_name(&self) -> Option<&'static str> {
        self.provider.map(|descriptor| descriptor.display_name)
    }

    /// Switches the screen to another provider. Rows, filters that may not apply to its
    /// markets, and pending requests are dropped; the returned retirement belongs to the
    /// previous provider's consumer, and the page starts a new one when it is activated.
    pub(super) fn select_provider(&mut self, id: &str) -> Option<MarketWorkerRetirement> {
        let provider = self
            .providers
            .iter()
            .copied()
            .find(|descriptor| descriptor.id == id)?;
        if self.provider.is_some_and(|current| current.id == id) {
            return None;
        }
        self.provider = Some(provider);
        self.markets.clear();
        self.captured_at_unix_millis = None;
        self.next_refresh_at = None;
        self.message = None;
        self.sort = ScreenerSort::default();
        self.category = ScreenerCategory::All;
        self.counts = ScreenerCategoryCounts::default();
        self.reported_columns = ScreenerReportedColumns::default();
        self.rebuild_rows();
        self.begin_retirement()
    }

    pub(super) const fn reported_columns(&self) -> ScreenerReportedColumns {
        self.reported_columns
    }

    /// Every screened market is listed by the screening provider, so its logo marks each row.
    pub(super) fn exchange_logo(&self) -> Option<assets::ExchangeLogo> {
        self.provider
            .and_then(|descriptor| assets::ExchangeLogo::for_logo_key(descriptor.logo_key))
    }

    pub(super) fn rows(&self) -> Rc<Vec<ScreenerRow>> {
        Rc::clone(&self.rows)
    }

    pub(super) const fn counts(&self) -> ScreenerCategoryCounts {
        self.counts
    }

    pub(super) const fn sort(&self) -> ScreenerSort {
        self.sort
    }

    pub(super) const fn category(&self) -> ScreenerCategory {
        self.category
    }

    pub(super) fn status(&self) -> ScreenerStatus {
        if self.captured_at_unix_millis.is_some() {
            ScreenerStatus::Ready
        } else {
            ScreenerStatus::Loading
        }
    }

    pub(super) const fn captured_at_unix_millis(&self) -> Option<i64> {
        self.captured_at_unix_millis
    }

    pub(super) fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub(super) fn opening_symbol(&self) -> Option<&str> {
        self.pending_selection
            .as_ref()
            .map(|selection| selection.display_symbol.as_str())
    }

    pub(super) fn search_input(&self) -> Option<&Entity<InputState>> {
        self.search_input.as_ref()
    }

    pub(super) fn set_search_input(&mut self, input: Entity<InputState>) {
        self.search_input = Some(input);
    }

    /// Starts the screen consumer the first time the page opens. It holds no series demand,
    /// so it is retained while the page is hidden instead of being re-registered each visit.
    pub(super) fn ensure_worker(
        &mut self,
        factory: Option<&engine_market_worker::WorkspaceMarketFactory>,
        workspace_id: u64,
        wake: &UiWake,
    ) {
        if self.worker.is_some() {
            return;
        }
        let Some(provider) = self.provider else {
            self.message = Some("No connected provider offers a market screen".to_string());
            return;
        };
        let Some(factory) = factory else {
            self.message = Some("Market data is unavailable in this window".to_string());
            return;
        };
        match factory.create_market_screen_worker(workspace_id, provider.id) {
            Ok(worker) => {
                worker.set_message_wake(wake.callback());
                self.worker = Some(worker);
                self.message = None;
            }
            Err(error) => {
                diagnostic!("Aeris market screener could not start: {error}");
                self.message = Some(format!("Screener unavailable: {error}"));
            }
        }
    }

    /// Makes the next refresh check send a screen immediately.
    pub(super) fn refresh_now(&mut self) {
        self.next_refresh_at = None;
    }

    pub(super) fn keep_refreshing(&mut self, task: Task<()>) {
        self.refresh_task = Some(task);
    }

    pub(super) fn stop_refreshing(&mut self) {
        self.refresh_task = None;
    }

    /// Sends one screen when the cadence allows it and no recent request is unanswered.
    /// Returns whether the presentation changed.
    pub(super) fn refresh_if_due(&mut self, now: Instant) -> bool {
        let (Some(worker), Some(provider)) = (&self.worker, self.provider) else {
            return false;
        };
        if self.pending_screen.is_some_and(|(_, sent)| {
            now.saturating_duration_since(sent) < SCREENER_RESPONSE_TIMEOUT
        }) || self.next_refresh_at.is_some_and(|due| now < due)
        {
            return false;
        }
        let generation = self.next_generation;
        self.next_generation = generation.checked_add(1).unwrap_or(1);
        self.next_refresh_at = Some(now + SCREENER_REFRESH_INTERVAL);
        let request = ScreenProviderMarkets {
            consumer_id: 0,
            screen_generation: generation,
            provider: provider.id.to_string(),
            maximum_results: SCREENER_MAXIMUM_RESULTS,
        };
        if worker.try_screen_provider(request).is_ok() {
            self.pending_screen = Some((generation, now));
            false
        } else {
            self.message = Some("Market statistics are busy; retrying shortly".to_string());
            true
        }
    }

    /// Requests the instrument behind a presented row; the terminal opens it once resolved.
    pub(super) fn open_row(&mut self, row: &ScreenerRow) {
        let (Some(worker), Some(provider)) = (&self.worker, self.provider) else {
            return;
        };
        let generation = self.next_generation;
        self.next_generation = generation.checked_add(1).unwrap_or(1);
        let request = SelectProviderInstrument {
            startup_resolve: false,
            consumer_id: 0,
            selection_generation: generation,
            search_generation: self.displayed_generation.max(1),
            provider: provider.id.to_string(),
            symbol: row.symbol.clone(),
            exchange: row.exchange.clone(),
            entitlement_id: provider.selection_entitlement_id.to_string(),
        };
        if worker.try_select_provider(request).is_err() {
            self.message = Some(format!(
                "{} could not be opened; try again",
                row.display_symbol
            ));
            return;
        }
        self.pending_selection = Some(PendingScreenerSelection {
            generation,
            display_symbol: row.display_symbol.clone(),
        });
        self.message = None;
    }

    pub(super) fn scroll_to_top(&self) {
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    /// Hands the screen consumer to shutdown; the page restarts it if opened again.
    pub(super) fn begin_retirement(&mut self) -> Option<MarketWorkerRetirement> {
        self.refresh_task = None;
        self.pending_screen = None;
        self.pending_selection = None;
        self.worker.take()?.begin_retirement()
    }

    pub(super) fn set_sort_column(&mut self, column: ScreenerColumn) {
        self.sort = self.sort.toggled(column);
        self.rebuild_rows();
    }

    pub(super) fn set_category(&mut self, category: ScreenerCategory) {
        if self.category != category {
            self.category = category;
            self.rebuild_rows();
        }
    }

    pub(super) fn set_query(&mut self, query: &str) {
        if self.query != query {
            query.clone_into(&mut self.query);
            self.rebuild_rows();
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows = Rc::new(screener_rows(
            &self.markets,
            self.category,
            &self.query,
            self.sort,
        ));
    }

    pub(super) fn poll(&mut self) -> ScreenerPoll {
        let mut outcome = ScreenerPoll::default();
        let Some(worker) = &mut self.worker else {
            return outcome;
        };
        let (messages, disconnected) = worker.drain_messages_up_to(SCREENER_MESSAGES_PER_POLL);
        for message in messages {
            match message {
                MarketWorkerMessage::ProviderCatalog(event) => {
                    outcome.changed |= self.apply_catalog_event(event, &mut outcome.opened);
                }
                MarketWorkerMessage::CatalogFailure { message } => {
                    self.pending_screen = None;
                    self.pending_selection = None;
                    self.message = Some(message);
                    outcome.changed = true;
                }
                _ => {}
            }
        }
        if disconnected {
            self.worker = None;
            self.pending_screen = None;
            self.pending_selection = None;
            self.message = Some("Market statistics are unavailable".to_string());
            outcome.changed = true;
        }
        outcome
    }

    /// A provider behind a user connection most often fails to screen because it is not
    /// connected, so its message points to Accounts.
    fn unavailable_message(&self) -> String {
        match self.provider {
            Some(provider)
                if matches!(
                    provider.connection_kind,
                    ProviderConnectionKind::HostedBroker | ProviderConnectionKind::Credentials
                ) =>
            {
                format!(
                    "{} market statistics are unavailable. Check its connection in Accounts; retrying",
                    provider.display_name
                )
            }
            _ => "Market statistics are unavailable; retrying".to_string(),
        }
    }

    fn apply_catalog_event(
        &mut self,
        event: ProviderCatalogEvent,
        opened: &mut Option<InstallProviderInstrument>,
    ) -> bool {
        match event {
            ProviderCatalogEvent::ScreenCompleted(screen)
                if self
                    .pending_screen
                    .is_some_and(|(generation, _)| generation == screen.screen_generation) =>
            {
                self.pending_screen = None;
                self.displayed_generation = screen.screen_generation;
                self.captured_at_unix_millis = Some(screen.captured_at_unix_millis);
                self.counts = screener_category_counts(&screen.markets);
                self.reported_columns = ScreenerReportedColumns::of(&screen.markets);
                if !self.reported_columns.shows(self.sort.column) {
                    self.sort = ScreenerSort::default();
                }
                if self.counts.get(self.category) == 0 {
                    self.category = ScreenerCategory::All;
                }
                self.markets = screen.markets;
                if self.pending_selection.is_none() {
                    self.message = None;
                }
                self.rebuild_rows();
                true
            }
            ProviderCatalogEvent::ScreenRejected(rejection)
                if self
                    .pending_screen
                    .is_some_and(|(generation, _)| generation == rejection.command_generation) =>
            {
                self.pending_screen = None;
                self.message = Some(self.unavailable_message());
                true
            }
            ProviderCatalogEvent::SelectionInstalled {
                command_generation,
                instrument,
            } if self
                .pending_selection
                .as_ref()
                .is_some_and(|selection| selection.generation == command_generation) =>
            {
                self.pending_selection = None;
                *opened = Some(instrument);
                true
            }
            ProviderCatalogEvent::CommandRejected { rejection, .. }
                if self.pending_selection.as_ref().is_some_and(|selection| {
                    selection.generation == rejection.command_generation
                }) =>
            {
                let selection = self.pending_selection.take();
                self.message = selection.map(|selection| {
                    format!("{} is not available to open", selection.display_symbol)
                });
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_contracts::ProviderInstrumentSummary;

    fn market(
        symbol: &str,
        kind: ProviderMarketKind,
        mark: Option<i64>,
        previous: Option<i64>,
        volume: Option<i64>,
        funding: Option<i64>,
    ) -> ProviderMarketStatistics {
        ProviderMarketStatistics {
            instrument: ProviderInstrumentSummary {
                symbol: symbol.to_string(),
                display_symbol: format!("{symbol}-USDC"),
                exchange: "Hyperliquid".to_string(),
                ..Default::default()
            },
            kind: Some(kind),
            price_scale: 2,
            mark_price: mark,
            previous_day_price: previous,
            notional_scale: 2,
            day_notional_volume: volume,
            open_interest_notional: None,
            funding_rate_scale: 8,
            funding_rate: funding,
            funding_interval_seconds: funding.map(|_| 3_600),
            volume_scale: 0,
            day_volume: None,
        }
    }

    fn markets() -> Vec<ProviderMarketStatistics> {
        vec![
            market(
                "BTC",
                ProviderMarketKind::Perpetual,
                Some(10_000_000),
                Some(9_500_000),
                Some(500),
                Some(1_300),
            ),
            market(
                "ETH",
                ProviderMarketKind::Perpetual,
                Some(300_000),
                Some(310_000),
                Some(900),
                Some(-500),
            ),
            market(
                "PURR",
                ProviderMarketKind::Spot,
                Some(20),
                Some(20),
                None,
                None,
            ),
        ]
    }

    /// Listed equities and futures report traded units, not quote-currency volume.
    fn unit_volume_markets() -> Vec<ProviderMarketStatistics> {
        let listed = |symbol: &str, kind, volume| ProviderMarketStatistics {
            day_volume: volume,
            volume_scale: 8,
            ..market(symbol, kind, Some(10_000), Some(9_000), None, None)
        };
        vec![
            listed(
                "SPY",
                ProviderMarketKind::Equity,
                Some(4_741_779_200_000_000),
            ),
            listed("SPX", ProviderMarketKind::Index, None),
            listed(
                "/ESU6",
                ProviderMarketKind::Future,
                Some(120_000_000_000_000),
            ),
        ]
    }

    fn symbols(rows: &[ScreenerRow]) -> Vec<&str> {
        rows.iter().map(|row| row.symbol.as_str()).collect()
    }

    #[test]
    fn default_order_is_most_traded_first_with_unreported_volume_last() {
        let rows = screener_rows(
            &markets(),
            ScreenerCategory::All,
            "",
            ScreenerSort::default(),
        );
        assert_eq!(symbols(&rows), ["ETH", "BTC", "PURR"]);

        let ascending = ScreenerSort::default().toggled(ScreenerColumn::Volume);
        assert!(!ascending.descending);
        let rows = screener_rows(&markets(), ScreenerCategory::All, "", ascending);
        assert_eq!(
            symbols(&rows),
            ["BTC", "ETH", "PURR"],
            "missing values stay last"
        );
    }

    #[test]
    fn change_sort_uses_the_day_change_ratio() {
        let sort = ScreenerSort::default().toggled(ScreenerColumn::Change);
        assert!(sort.descending);
        let rows = screener_rows(&markets(), ScreenerCategory::All, "", sort);
        assert_eq!(symbols(&rows), ["BTC", "PURR", "ETH"]);
        assert_eq!(rows[0].change, "+5.26%");
        assert_eq!(rows[0].change_direction, Some(CmpOrdering::Greater));
        assert_eq!(rows[2].change, "-3.23%");
    }

    #[test]
    fn market_column_starts_ascending_and_flips_on_repeat() {
        let sort = ScreenerSort::default().toggled(ScreenerColumn::Market);
        assert!(!sort.descending);
        assert_eq!(
            symbols(&screener_rows(&markets(), ScreenerCategory::All, "", sort)),
            ["BTC", "ETH", "PURR"]
        );
        let sort = sort.toggled(ScreenerColumn::Market);
        assert_eq!(
            symbols(&screener_rows(&markets(), ScreenerCategory::All, "", sort)),
            ["PURR", "ETH", "BTC"]
        );
    }

    #[test]
    fn category_and_query_filter_rows() {
        let markets = markets();
        let sort = ScreenerSort::default();
        assert_eq!(
            symbols(&screener_rows(
                &markets,
                ScreenerCategory::Kind(ProviderMarketKind::Spot),
                "",
                sort
            )),
            ["PURR"]
        );
        assert_eq!(
            symbols(&screener_rows(
                &markets,
                ScreenerCategory::Kind(ProviderMarketKind::Perpetual),
                "",
                sort
            )),
            ["ETH", "BTC"]
        );
        assert_eq!(
            symbols(&screener_rows(
                &markets,
                ScreenerCategory::All,
                " btc ",
                sort
            )),
            ["BTC"]
        );
        let counts = screener_category_counts(&markets);
        assert_eq!(counts.all, 3);
        assert_eq!(
            counts.get(ScreenerCategory::Kind(ProviderMarketKind::Perpetual)),
            2
        );
        assert_eq!(
            counts.categories(),
            [
                ScreenerCategory::All,
                ScreenerCategory::Kind(ProviderMarketKind::Perpetual),
                ScreenerCategory::Kind(ProviderMarketKind::Spot),
            ],
            "only listed categories are offered"
        );
    }

    #[test]
    fn unit_volume_ranks_and_formats_without_a_currency_and_hides_unreported_columns() {
        let markets = unit_volume_markets();
        let rows = screener_rows(&markets, ScreenerCategory::All, "", ScreenerSort::default());
        assert_eq!(symbols(&rows), ["SPY", "/ESU6", "SPX"]);
        assert_eq!(rows[0].volume, "47.42M");
        assert_eq!(rows[1].volume, "1.20M");
        assert_eq!(rows[2].volume, UNAVAILABLE_VALUE);
        assert_eq!(rows[0].kind, Some(ProviderMarketKind::Equity));
        assert_eq!(
            screener_category_counts(&markets).categories(),
            [
                ScreenerCategory::All,
                ScreenerCategory::Kind(ProviderMarketKind::Equity),
                ScreenerCategory::Kind(ProviderMarketKind::Index),
                ScreenerCategory::Kind(ProviderMarketKind::Future),
            ]
        );
        let columns = ScreenerReportedColumns::of(&markets);
        assert!(!columns.shows(ScreenerColumn::Funding));
        assert!(!columns.shows(ScreenerColumn::OpenInterest));
        assert!(columns.shows(ScreenerColumn::Volume));
        assert!(ScreenerReportedColumns::of(&self::markets()).shows(ScreenerColumn::Funding));
    }

    #[test]
    fn switching_provider_drops_the_previous_screen_and_its_filters() {
        let mut screener = MarketScreener::default();
        let ids = screener
            .providers()
            .iter()
            .map(|provider| provider.id)
            .collect::<Vec<_>>();
        assert!(ids.len() >= 2, "more than one provider screens markets");
        assert_eq!(screener.provider_id(), Some(ids[0]));
        screener.markets = markets();
        screener.counts = screener_category_counts(&screener.markets);
        screener.captured_at_unix_millis = Some(1);
        screener.set_category(ScreenerCategory::Kind(ProviderMarketKind::Spot));
        screener.set_sort_column(ScreenerColumn::Funding);
        assert!(screener.select_provider(ids[0]).is_none());
        assert_eq!(screener.rows().len(), 1, "reselecting keeps the screen");

        assert!(
            screener.select_provider(ids[1]).is_none(),
            "no consumer started"
        );
        assert_eq!(screener.provider_id(), Some(ids[1]));
        assert!(screener.rows().is_empty());
        assert_eq!(screener.status(), ScreenerStatus::Loading);
        assert_eq!(screener.category(), ScreenerCategory::All);
        assert_eq!(screener.sort(), ScreenerSort::default());
        assert!(screener.select_provider("unknown").is_none());
        assert_eq!(screener.provider_id(), Some(ids[1]));
    }

    #[test]
    fn values_format_with_explicit_scales() {
        let rows = screener_rows(
            &markets(),
            ScreenerCategory::All,
            "BTC",
            ScreenerSort::default(),
        );
        let btc = &rows[0];
        assert_eq!(btc.price, "100,000.00");
        assert_eq!(btc.volume, "$5.00");
        assert_eq!(btc.funding, "+0.0013%");
        assert_eq!(btc.funding_direction, Some(CmpOrdering::Greater));
        assert_eq!(btc.open_interest, UNAVAILABLE_VALUE);
        assert_eq!(compact_notional(Some(12_345_678_900), 2), "$123.46M");
        assert_eq!(compact_notional(Some(-250_000), 2), "-$2.50K");
        assert_eq!(grouped_decimal("-1234567.5"), "-1,234,567.5");
        assert_eq!(grouped_decimal("999"), "999");
    }

    #[test]
    fn capture_time_is_wall_clock_utc() {
        assert_eq!(capture_time_label(1_700_000_000_000), "22:13:20 UTC");
    }
}
