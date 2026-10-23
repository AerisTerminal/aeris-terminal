//! Bounded recording of the trading market feed behind a swappable owner inbox.
//! `MarketService::attach_trading_feed` allows exactly one closure for the process, so the
//! closure records every event and forwards it to whichever trading owner is live; probes
//! swap that inbox when they restart the trading service mid-run.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use aeris_market_runtime::{MarketService, market_service::TradingFeedLink};
use aeris_trading::{
    FixedPoint,
    market::{FeedState, MarketFeedEvent},
};
use aeris_trading_runtime::{MarketFeedInbox, TradingService};

use crate::{ProbeResult, market};

const MAXIMUM_RECORDED_QUOTES: usize = 10_000;

/// One recorded top-of-book quote; fill provenance matches on `received_unix_nanos`.
#[derive(Clone, Copy, Debug)]
pub struct RecordedQuote {
    pub bid: FixedPoint,
    pub ask: FixedPoint,
    pub provider_generation: u64,
    pub source_sequence: Option<u64>,
    pub received_unix_nanos: i64,
}

#[derive(Default)]
struct InstrumentRecord {
    quotes: VecDeque<RecordedQuote>,
    feed: Option<FeedState>,
    session_seen: bool,
    terms_seen: bool,
}

#[derive(Default)]
struct RecorderState {
    epoch: u64,
    instruments: HashMap<String, InstrumentRecord>,
    first_quote_in_epoch: HashMap<(u64, String), i64>,
    push_failures: u64,
    total_quotes: u64,
}

/// Thread-safe recorder fed by the single market-side trading-feed closure.
pub struct QuoteRecorder {
    state: Mutex<RecorderState>,
}

impl Default for QuoteRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl QuoteRecorder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RecorderState::default()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, RecorderState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts a new attachment epoch (a trading restart) and returns its identity.
    pub fn advance_epoch(&self) -> u64 {
        let mut state = self.lock();
        state.epoch += 1;
        state.epoch
    }

    fn record(&self, event: &MarketFeedEvent) {
        let mut state = self.lock();
        match event {
            MarketFeedEvent::Quote(quote) => {
                let key = quote.instrument_id.as_str().to_string();
                let epoch = state.epoch;
                let record = state.instruments.entry(key.clone()).or_default();
                if record.quotes.len() == MAXIMUM_RECORDED_QUOTES {
                    record.quotes.pop_front();
                }
                record.quotes.push_back(RecordedQuote {
                    bid: quote.bid,
                    ask: quote.ask,
                    provider_generation: quote.provider_generation,
                    source_sequence: quote.source_sequence,
                    received_unix_nanos: quote.received_unix_nanos,
                });
                state.total_quotes += 1;
                state
                    .first_quote_in_epoch
                    .entry((epoch, key))
                    .or_insert(quote.received_unix_nanos);
            }
            MarketFeedEvent::Feed {
                instrument_id,
                state: feed_state,
                ..
            } => {
                state
                    .instruments
                    .entry(instrument_id.as_str().to_string())
                    .or_default()
                    .feed = Some(*feed_state);
            }
            MarketFeedEvent::Session(session) => {
                state
                    .instruments
                    .entry(session.instrument_id.as_str().to_string())
                    .or_default()
                    .session_seen = true;
            }
            MarketFeedEvent::Terms(terms) => {
                state
                    .instruments
                    .entry(terms.route.instrument_id.as_str().to_string())
                    .or_default()
                    .terms_seen = true;
            }
        }
    }

    fn record_push_failure(&self) {
        self.lock().push_failures += 1;
    }

    /// The latest recorded top-of-book quote for one instrument.
    #[must_use]
    pub fn latest_quote(&self, instrument: &str) -> Option<RecordedQuote> {
        self.lock()
            .instruments
            .get(instrument)
            .and_then(|record| record.quotes.back().copied())
    }

    /// Finds the recorded quote the owner consumed for a fill: fill provenance carries
    /// the quote's `received_unix_nanos` as `observed_unix_nanos`.
    #[must_use]
    pub fn quote_at(&self, instrument: &str, received_unix_nanos: i64) -> Option<RecordedQuote> {
        self.lock().instruments.get(instrument).and_then(|record| {
            record
                .quotes
                .iter()
                .rev()
                .find(|quote| quote.received_unix_nanos == received_unix_nanos)
                .copied()
        })
    }

    /// The latest recorded feed state for one instrument.
    #[must_use]
    pub fn feed_state(&self, instrument: &str) -> Option<FeedState> {
        self.lock()
            .instruments
            .get(instrument)
            .and_then(|record| record.feed)
    }

    /// Arrival time of the first quote for one instrument in one attachment epoch.
    #[must_use]
    pub fn first_quote_in_epoch(&self, epoch: u64, instrument: &str) -> Option<i64> {
        self.lock()
            .first_quote_in_epoch
            .get(&(epoch, instrument.to_string()))
            .copied()
    }

    /// Counts recorded quotes and owner-inbox push failures.
    #[must_use]
    pub fn statistics(&self) -> (u64, u64) {
        let state = self.lock();
        (state.total_quotes, state.push_failures)
    }

    /// Bounded diagnostic dump of what the feed delivered, for timeout evidence.
    #[must_use]
    pub fn debug_state(&self, instruments: &[&str]) -> serde_json::Value {
        let state = self.lock();
        let entries = instruments
            .iter()
            .map(|instrument| {
                let record = state.instruments.get(*instrument);
                serde_json::json!({
                    "instrument": instrument,
                    "terms_seen": record.is_some_and(|record| record.terms_seen),
                    "session_seen": record.is_some_and(|record| record.session_seen),
                    "feed_state": record.and_then(|record| record.feed).map(|state| format!("{state:?}")),
                    "quotes": record.map_or(0, |record| record.quotes.len()),
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "instruments": entries,
            "total_quotes": state.total_quotes,
            "push_failures": state.push_failures,
            "epoch": state.epoch,
        })
    }
}

/// The trading owner inbox currently receiving recorded feed events.
#[derive(Default)]
pub struct FeedTarget {
    inbox: Mutex<Option<Arc<MarketFeedInbox>>>,
    suppressed_quote_instrument: Mutex<Option<String>>,
}

impl FeedTarget {
    /// Keeps a real instrument's Terms and session but withholds its quotes from the
    /// trading owner for the paper probe's negative no-quote admission run.
    pub fn suppress_quotes_for(&self, instrument: &str) {
        *self
            .suppressed_quote_instrument
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(instrument.to_string());
    }

    fn suppresses(&self, event: &MarketFeedEvent) -> bool {
        matches!(event, MarketFeedEvent::Quote(quote)
            if self.suppressed_quote_instrument.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_deref() == Some(quote.instrument_id.as_str()))
    }

    pub fn attach(&self, inbox: Arc<MarketFeedInbox>) {
        *self.inbox.lock().unwrap_or_else(PoisonError::into_inner) = Some(inbox);
    }

    pub fn detach(&self) {
        *self.inbox.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// Installs the one process-wide trading-feed closure; every event is recorded, then
/// forwarded to the current owner inbox when one is attached.
///
/// # Errors
/// Returns an error when the market service already has a trading feed attached.
pub fn attach_trading_feed(
    market_service: &MarketService,
    recorder: Arc<QuoteRecorder>,
    target: Arc<FeedTarget>,
) -> ProbeResult<()> {
    let events = Box::new(move |event: MarketFeedEvent| {
        recorder.record(&event);
        if target.suppresses(&event) {
            return;
        }
        let inbox = target
            .inbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(inbox) = inbox
            && inbox.push(event).is_err()
        {
            recorder.record_push_failure();
        }
    });
    market(market_service.attach_trading_feed(TradingFeedLink { events }))
}

/// Hands a trading owner the shared interest sink and points the feed closure at it.
///
/// # Errors
/// Returns an error when the owner already has a feed or the runtime is unavailable.
pub fn wire_trading_feed(
    market_service: &MarketService,
    trading: &TradingService,
    target: &FeedTarget,
) -> ProbeResult<()> {
    let inbox = market(trading.attach_market_feed(market_service.trading_quote_interest_sink()))?;
    target.attach(inbox);
    Ok(())
}
