//! Owner-only market facts and a non-blocking, bounded ingress mailbox.
use crate::{SimulatedMarketObservation, service::Command};
use aeris_instruments::InstrumentId;
use aeris_trading::RejectReason;
use aeris_trading::market::{
    FeedState, InstrumentSession, MAXIMUM_QUOTE_INTEREST, MarketFeedError, MarketFeedEvent,
    MarketRoute, TradingQuote,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{SyncSender, TrySendError},
    },
};

const CONTROL_CAPACITY: usize = 1_024;

#[derive(Default)]
pub(crate) struct QuoteBook {
    entries: BTreeMap<InstrumentId, QuoteEntry>,
}

#[derive(Default)]
struct QuoteEntry {
    generation: u64,
    /// Owner-local monotonic ingress order, never inferred from provider identifiers.
    revision: u64,
    quote: Option<TradingQuote>,
    feed: Option<FeedState>,
    session: Option<InstrumentSession>,
}

impl QuoteBook {
    pub(crate) fn check_session(&self, id: &InstrumentId, now: i64) -> Result<(), RejectReason> {
        let session = self
            .entries
            .get(id)
            .and_then(|entry| entry.session.as_ref())
            .ok_or(RejectReason::SessionUnknown)?;
        super::gating::tradable_phase(session.phase, session.next_open_unix_nanos)?;
        if session.session_end_unix_nanos.is_some_and(|end| now >= end) {
            return Err(RejectReason::MarketClosed {
                phase: aeris_instruments::SessionPhase::Closed,
                next_open: session.next_open_unix_nanos,
            });
        }
        Ok(())
    }

    pub(crate) fn check_quote(
        &self,
        id: &InstrumentId,
        now: i64,
        class: aeris_instruments::InstrumentClass,
        policy: super::gating::ExecutionPolicy,
    ) -> Result<(), RejectReason> {
        let entry = self.entries.get(id).ok_or(RejectReason::QuoteUnavailable)?;
        if entry.feed != Some(FeedState::Live) {
            return Err(RejectReason::QuoteUnavailable);
        }
        let quote = entry.quote.as_ref().ok_or(RejectReason::QuoteUnavailable)?;
        if quote.provider_generation != entry.generation
            || entry
                .session
                .as_ref()
                .is_none_or(|session| session.provider_generation != entry.generation)
        {
            return Err(RejectReason::QuoteUnavailable);
        }
        super::gating::fresh_quote(now, quote.received_unix_nanos, class, policy)
    }

    pub(crate) fn observation(&self, id: &InstrumentId) -> Option<SimulatedMarketObservation> {
        let entry = self.entries.get(id)?;
        let quote = entry.quote.as_ref()?;
        if entry.feed != Some(FeedState::Live) {
            return None;
        }
        Some(SimulatedMarketObservation {
            instrument_id: id.clone(),
            bid: quote.bid,
            ask: quote.ask,
            provenance: aeris_trading::TradingProvenance {
                venue_id: "aeris-sim".into(),
                provider_id: quote.provider_id.clone(),
                session_generation: quote.provider_generation,
                source_sequence: entry.revision,
                observed_unix_nanos: quote.received_unix_nanos,
            },
        })
    }

    pub(crate) fn retire_route(&mut self, id: &InstrumentId) {
        self.entries.remove(id);
    }

    pub(crate) fn set_generation_floor(&mut self, id: &InstrumentId, generation: u64) {
        if self.entries.len() == MAXIMUM_QUOTE_INTEREST && !self.entries.contains_key(id) {
            return;
        }
        let entry = self.entries.entry(id.clone()).or_default();
        if generation > entry.generation {
            *entry = QuoteEntry {
                generation,
                ..QuoteEntry::default()
            };
        }
    }

    #[cfg(test)]
    pub(crate) fn quote(&self, id: &InstrumentId) -> Option<&TradingQuote> {
        self.entries.get(id)?.quote.as_ref()
    }

    pub(crate) fn accept(
        &mut self,
        event: MarketFeedEvent,
        routes: &BTreeMap<InstrumentId, MarketRoute>,
    ) -> Result<bool, MarketFeedError> {
        let (id, generation) = match &event {
            MarketFeedEvent::Quote(quote) => (&quote.instrument_id, quote.provider_generation),
            MarketFeedEvent::Feed {
                instrument_id,
                provider_generation,
                ..
            } => (instrument_id, *provider_generation),
            MarketFeedEvent::Session(session) => {
                (&session.instrument_id, session.provider_generation)
            }
            MarketFeedEvent::Terms(_) => return Ok(false),
        };
        let Some(route) = routes.get(id) else {
            return Ok(false);
        };
        if generation == 0 {
            return Err(MarketFeedError::InvalidQuoteTime);
        }
        if self.entries.len() == MAXIMUM_QUOTE_INTEREST && !self.entries.contains_key(id) {
            return Err(MarketFeedError::InterestLimitExceeded);
        }
        let entry = self.entries.entry(id.clone()).or_default();
        if generation < entry.generation {
            return Ok(false);
        }
        // Validate first: malformed input must never retire a healthy generation.
        match &event {
            MarketFeedEvent::Quote(quote) => quote.validate(route)?,
            MarketFeedEvent::Session(session) => session.validate()?,
            _ => {}
        }
        if generation > entry.generation {
            *entry = QuoteEntry {
                generation,
                ..QuoteEntry::default()
            };
        }
        match event {
            MarketFeedEvent::Quote(quote) => {
                if entry.quote.as_ref().is_some_and(|previous| {
                    previous
                        .source_sequence
                        .zip(quote.source_sequence)
                        .is_some_and(|(old, new)| new < old)
                        || previous.source_sequence == quote.source_sequence
                            && quote.received_unix_nanos < previous.received_unix_nanos
                }) {
                    return Ok(false);
                }
                entry.revision = entry
                    .revision
                    .checked_add(1)
                    .ok_or(MarketFeedError::InvalidRevision)?;
                entry.quote = Some(quote);
            }
            MarketFeedEvent::Feed { state, .. } => {
                if state != FeedState::Live || entry.feed != Some(FeedState::Live) {
                    entry.quote = None;
                }
                entry.feed = Some(state);
            }
            MarketFeedEvent::Session(session) => {
                entry.session = Some(session);
            }
            MarketFeedEvent::Terms(_) => unreachable!("terms are handled by the owner"),
        }
        Ok(true)
    }

    pub(crate) fn retain(&mut self, instruments: &BTreeSet<InstrumentId>) {
        self.entries.retain(|id, _| instruments.contains(id));
    }
}

#[derive(Default)]
struct Pending {
    quotes: BTreeMap<InstrumentId, TradingQuote>,
    controls: VecDeque<MarketFeedEvent>,
    resync: bool,
    drain_queued: bool,
}

pub(crate) struct MarketFeedBatch {
    pub(crate) quotes: Vec<TradingQuote>,
    pub(crate) controls: Vec<MarketFeedEvent>,
    pub(crate) resync: bool,
}

/// One wake for all pending facts; the market callback never waits on the owner.
pub struct MarketFeedInbox {
    pending: Mutex<Pending>,
    commands: SyncSender<Command>,
    stopping: std::sync::Arc<AtomicBool>,
}

impl MarketFeedInbox {
    pub(crate) fn new(commands: SyncSender<Command>, stopping: std::sync::Arc<AtomicBool>) -> Self {
        Self {
            pending: Mutex::new(Pending::default()),
            commands,
            stopping,
        }
    }

    /// Offers one fact without blocking. Overflow requests a fresh interest snapshot.
    ///
    /// # Errors
    /// Returns `Unavailable` if the trading owner is stopping or disconnected.
    pub fn push(&self, event: MarketFeedEvent) -> Result<(), MarketFeedError> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(MarketFeedError::Unavailable);
        }
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match event {
            MarketFeedEvent::Quote(quote) => {
                if !pending.quotes.contains_key(&quote.instrument_id)
                    && pending.quotes.len() == MAXIMUM_QUOTE_INTEREST
                {
                    pending.resync = true;
                } else {
                    let replace = pending
                        .quotes
                        .get(&quote.instrument_id)
                        .is_none_or(|previous| {
                            (
                                quote.provider_generation,
                                quote.source_sequence.unwrap_or_default(),
                                quote.received_unix_nanos,
                            ) >= (
                                previous.provider_generation,
                                previous.source_sequence.unwrap_or_default(),
                                previous.received_unix_nanos,
                            )
                        });
                    if replace {
                        pending.quotes.insert(quote.instrument_id.clone(), quote);
                    }
                }
            }
            event => {
                if pending.controls.len() == CONTROL_CAPACITY {
                    // Prefer dropping the oldest Terms; a fresh snapshot recovers its route.
                    let index = pending
                        .controls
                        .iter()
                        .position(|event| matches!(event, MarketFeedEvent::Terms(_)))
                        .unwrap_or(0);
                    pending.controls.remove(index);
                    pending.resync = true;
                }
                pending.controls.push_back(event);
            }
        }
        if !pending.drain_queued {
            match self.commands.try_send(Command::DrainMarketFeed) {
                Ok(()) => pending.drain_queued = true,
                Err(TrySendError::Disconnected(_)) => return Err(MarketFeedError::Unavailable),
                Err(TrySendError::Full(_)) => {} // The owner retries after its next command.
            }
        }
        Ok(())
    }

    pub(crate) fn drain(&self) -> MarketFeedBatch {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.drain_queued = false;
        MarketFeedBatch {
            quotes: std::mem::take(&mut pending.quotes).into_values().collect(),
            controls: pending.controls.drain(..).collect(),
            resync: std::mem::take(&mut pending.resync),
        }
    }

    pub(crate) fn wake_if_pending(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !pending.drain_queued
            && (!pending.quotes.is_empty() || !pending.controls.is_empty() || pending.resync)
            && self.commands.try_send(Command::DrainMarketFeed).is_ok()
        {
            pending.drain_queued = true;
        }
    }
}
