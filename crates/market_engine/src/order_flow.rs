use crate::{ConsumerId, EngineError, GenerationId, ProviderGeneration};
use axiusflow_market_data::{AggressorSide, BarSeriesKey, MarketTrade};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

const MAXIMUM_ORDER_FLOW_LEVELS: usize = 2_048;
const MAXIMUM_TAPE_TRADES: usize = 256;
const MAXIMUM_RECONSTRUCTION_TRADES: usize = 100_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OrderFlowLevel {
    pub price: i64,
    pub bid_volume: i64,
    pub ask_volume: i64,
    pub trade_count: u64,
    pub time_at_price_count: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderFlowTrade {
    pub source_sequence: u64,
    pub exchange_timestamp_unix_nanos: i64,
    pub price: i64,
    pub quantity: i64,
    pub aggressor: AggressorSide,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderFlowSnapshot {
    pub series: BarSeriesKey,
    pub provider_generation: ProviderGeneration,
    pub source_watermark: u64,
    pub cumulative_delta: i64,
    pub levels: Vec<OrderFlowLevel>,
    pub tape: Vec<OrderFlowTrade>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderFlowUpdate {
    pub provider_generation: ProviderGeneration,
    pub cumulative_delta: i64,
    pub level: OrderFlowLevel,
    pub trade: OrderFlowTrade,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderFlowPublicationKind {
    Snapshot(Arc<OrderFlowSnapshot>),
    Update(OrderFlowUpdate),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConsumerOrderFlowPublication {
    pub consumer_id: ConsumerId,
    pub generation: GenerationId,
    pub publication_generation: u64,
    pub series: BarSeriesKey,
    pub kind: OrderFlowPublicationKind,
}

struct Accumulator {
    provider_generation: ProviderGeneration,
    source_watermark: u64,
    cumulative_delta: i64,
    levels: BTreeMap<i64, OrderFlowLevel>,
    tape: VecDeque<OrderFlowTrade>,
}

impl Accumulator {
    fn apply(&mut self, trade: &MarketTrade) -> Result<OrderFlowUpdate, EngineError> {
        trade.validate()?;
        if trade.metadata.session_generation != self.provider_generation.0.get() {
            return Err(EngineError::StaleSeriesGeneration {
                current: self.provider_generation,
                received: ProviderGeneration(
                    std::num::NonZeroU64::new(trade.metadata.session_generation)
                        .unwrap_or(std::num::NonZeroU64::MIN),
                ),
            });
        }
        if trade.metadata.source_sequence <= self.source_watermark {
            return Err(EngineError::NonIncreasingOrderFlowSequence);
        }
        if !self.levels.contains_key(&trade.price) && self.levels.len() == MAXIMUM_ORDER_FLOW_LEVELS
        {
            return Err(EngineError::OrderFlowLevelLimitExceeded {
                maximum: MAXIMUM_ORDER_FLOW_LEVELS,
            });
        }
        let level = self.levels.entry(trade.price).or_insert(OrderFlowLevel {
            price: trade.price,
            ..OrderFlowLevel::default()
        });
        match trade.aggressor {
            AggressorSide::Buy => {
                level.ask_volume = level
                    .ask_volume
                    .checked_add(trade.quantity)
                    .ok_or(EngineError::CapacityOverflow)?;
                self.cumulative_delta = self
                    .cumulative_delta
                    .checked_add(trade.quantity)
                    .ok_or(EngineError::CapacityOverflow)?;
            }
            AggressorSide::Sell => {
                level.bid_volume = level
                    .bid_volume
                    .checked_add(trade.quantity)
                    .ok_or(EngineError::CapacityOverflow)?;
                self.cumulative_delta = self
                    .cumulative_delta
                    .checked_sub(trade.quantity)
                    .ok_or(EngineError::CapacityOverflow)?;
            }
            AggressorSide::Unknown => {}
        }
        level.trade_count = level
            .trade_count
            .checked_add(1)
            .ok_or(EngineError::CapacityOverflow)?;
        level.time_at_price_count = level
            .time_at_price_count
            .checked_add(1)
            .ok_or(EngineError::CapacityOverflow)?;
        let item = OrderFlowTrade {
            source_sequence: trade.metadata.source_sequence,
            exchange_timestamp_unix_nanos: trade
                .metadata
                .timestamps
                .exchange_unix_nanos
                .unwrap_or(trade.metadata.timestamps.received_unix_nanos),
            price: trade.price,
            quantity: trade.quantity,
            aggressor: trade.aggressor,
        };
        if self.tape.len() == MAXIMUM_TAPE_TRADES {
            self.tape.pop_front();
        }
        self.tape.push_back(item);
        self.source_watermark = trade.metadata.source_sequence;
        Ok(OrderFlowUpdate {
            provider_generation: self.provider_generation,
            cumulative_delta: self.cumulative_delta,
            level: *level,
            trade: item,
        })
    }

    fn snapshot(&self, series: &BarSeriesKey) -> Arc<OrderFlowSnapshot> {
        Arc::new(OrderFlowSnapshot {
            series: series.clone(),
            provider_generation: self.provider_generation,
            source_watermark: self.source_watermark,
            cumulative_delta: self.cumulative_delta,
            levels: self.levels.values().copied().collect(),
            tape: self.tape.iter().copied().collect(),
        })
    }
}

pub(crate) struct OrderFlowStore {
    accumulators: BTreeMap<BarSeriesKey, Accumulator>,
    publication_generations: BTreeMap<ConsumerId, ConsumerPublicationState>,
}

#[derive(Clone, Copy)]
struct ConsumerPublicationState {
    generation: u64,
    active: bool,
}

impl OrderFlowStore {
    pub(crate) fn new() -> Self {
        Self {
            accumulators: BTreeMap::new(),
            publication_generations: BTreeMap::new(),
        }
    }

    pub(crate) fn apply_trade(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        trade: &MarketTrade,
    ) -> Result<(bool, OrderFlowUpdate, Arc<OrderFlowSnapshot>), EngineError> {
        if trade.metadata.provider_id != series.provider_id
            || trade.metadata.instrument_id != series.instrument_id
            || trade.metadata.entitlement_id != series.entitlement_id
        {
            return Err(EngineError::OrderFlowIdentityMismatch);
        }
        let is_new = !self.accumulators.contains_key(series);
        let accumulator = self
            .accumulators
            .entry(series.clone())
            .or_insert_with(|| Accumulator {
                provider_generation,
                source_watermark: 0,
                cumulative_delta: 0,
                levels: BTreeMap::new(),
                tape: VecDeque::with_capacity(MAXIMUM_TAPE_TRADES),
            });
        if accumulator.provider_generation != provider_generation {
            if provider_generation < accumulator.provider_generation {
                return Err(EngineError::StaleSeriesGeneration {
                    current: accumulator.provider_generation,
                    received: provider_generation,
                });
            }
            *accumulator = Accumulator {
                provider_generation,
                source_watermark: 0,
                cumulative_delta: 0,
                levels: BTreeMap::new(),
                tape: VecDeque::with_capacity(MAXIMUM_TAPE_TRADES),
            };
        }
        let update = accumulator.apply(trade)?;
        Ok((is_new, update, accumulator.snapshot(series)))
    }

    pub(crate) fn replace_history(
        &mut self,
        series: &BarSeriesKey,
        provider_generation: ProviderGeneration,
        trades: &[MarketTrade],
    ) -> Result<Arc<OrderFlowSnapshot>, EngineError> {
        if trades.len() > MAXIMUM_RECONSTRUCTION_TRADES {
            return Err(EngineError::OrderFlowHistoryLimitExceeded {
                maximum: MAXIMUM_RECONSTRUCTION_TRADES,
            });
        }
        let mut replacement = Accumulator {
            provider_generation,
            source_watermark: 0,
            cumulative_delta: 0,
            levels: BTreeMap::new(),
            tape: VecDeque::with_capacity(MAXIMUM_TAPE_TRADES),
        };
        for trade in trades {
            if trade.metadata.provider_id != series.provider_id
                || trade.metadata.instrument_id != series.instrument_id
                || trade.metadata.entitlement_id != series.entitlement_id
            {
                return Err(EngineError::OrderFlowIdentityMismatch);
            }
            replacement.apply(trade)?;
        }
        let snapshot = replacement.snapshot(series);
        self.accumulators.insert(series.clone(), replacement);
        Ok(snapshot)
    }

    pub(crate) fn next_publication_generation(
        &mut self,
        consumer_id: ConsumerId,
    ) -> Result<(u64, bool), EngineError> {
        let previous = self.publication_generations.get(&consumer_id).copied();
        let next = previous
            .map_or(0, |state| state.generation)
            .checked_add(1)
            .ok_or(EngineError::CapacityOverflow)?;
        self.publication_generations.insert(
            consumer_id,
            ConsumerPublicationState {
                generation: next,
                active: true,
            },
        );
        Ok((next, previous.is_none_or(|state| !state.active)))
    }

    pub(crate) fn suspend_consumer(&mut self, consumer_id: ConsumerId) {
        if let Some(publication) = self.publication_generations.get_mut(&consumer_id) {
            publication.active = false;
        }
    }

    pub(crate) fn remove_consumer(&mut self, consumer_id: ConsumerId) {
        self.publication_generations.remove(&consumer_id);
    }

    pub(crate) fn invalidate(&mut self, series: &BarSeriesKey) {
        self.accumulators.remove(series);
    }
}
