//! The pane's order-flow presentation: the accepted trade-tape ring, the
//! runtime-classified sweeps highlighted in it, the Time & Sales filter and
//! rows, and the order-book detail the market worker serves.

use super::time_sales_panel::TimeSalesRowsCache;
use super::{ChartAggressorSide, OrderBookDetail, OrderFlowTapeChange, OrderFlowTrade};
use aeris_market_runtime::{
    MAXIMUM_RECENT_LADDER_TRADES, MarketTradeSweep, MarketTradeTapeOperation,
    MarketTradeTapeUpdate, RetainedMarketTrade,
};
use std::collections::VecDeque;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum TimeSalesSideFilter {
    #[default]
    All,
    Buy,
    Sell,
}

impl TimeSalesSideFilter {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::All => "All sides",
            Self::Buy => "Buys",
            Self::Sell => "Sells",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::All => Self::Buy,
            Self::Buy => Self::Sell,
            Self::Sell => Self::All,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TimeSalesFilter {
    pub(super) side: TimeSalesSideFilter,
    pub(super) minimum_quantity: f64,
    pub(super) price_range_ticks: Option<u32>,
}

impl Default for TimeSalesFilter {
    fn default() -> Self {
        Self {
            side: TimeSalesSideFilter::All,
            minimum_quantity: 0.0,
            price_range_ticks: None,
        }
    }
}

/// What one applied tape update asks of the chart.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TapeApply {
    /// Install the covering retained window.
    Reset,
    /// Append the ring's trades from this index on.
    Append { start: usize },
    /// An older publication of the current stream; nothing changed.
    Stale,
    /// The append did not continue the ring. The tape was left unchanged and
    /// the stream restarts from the runtime's next covering `Reset`.
    Broken,
}

/// The pane's accepted tape: one bounded trailing window of retained prints
/// plus the sweeps the runtime classified over it. The runtime's incremental
/// `TradeTapeUpdate` stream mutates it; the desktop never reclassifies.
pub(super) struct OrderFlowTape {
    /// The selection generation this tape's stream belongs to; a new
    /// selection restarts the stream with a `Reset` regardless of the
    /// provider session's own generation numbering.
    pub(super) selection_generation: u64,
    pub(super) provider_generation: u64,
    pub(super) revision: u64,
    pub(super) price_scale: u8,
    pub(super) quantity_scale: u8,
    /// Newest prints last, bounded at the runtime's retained-tape bound. The
    /// runtime restarts the stream with a `Reset` when an eviction would open
    /// a gap, so an accepted `Append` never overflows it.
    pub(super) trades: VecDeque<RetainedMarketTrade>,
    pub(super) sweeps: Arc<[MarketTradeSweep]>,
}

impl OrderFlowTape {
    fn new(update: &MarketTradeTapeUpdate, trades: VecDeque<RetainedMarketTrade>) -> Self {
        Self {
            selection_generation: update.generation.0.get(),
            provider_generation: update.provider_generation,
            revision: update.revision,
            price_scale: update.price_scale,
            quantity_scale: update.quantity_scale,
            trades,
            sweeps: Arc::clone(&update.sweeps),
        }
    }

    fn refresh(&mut self, update: &MarketTradeTapeUpdate) {
        self.selection_generation = update.generation.0.get();
        self.provider_generation = update.provider_generation;
        self.revision = update.revision;
        self.price_scale = update.price_scale;
        self.quantity_scale = update.quantity_scale;
        self.sweeps = Arc::clone(&update.sweeps);
    }

    pub(super) fn sweeps(&self) -> &[MarketTradeSweep] {
        &self.sweeps
    }
}

pub(super) struct OrderFlowPresenter {
    tape: Option<OrderFlowTape>,
    time_sales_filter: TimeSalesFilter,
    time_sales_rows: TimeSalesRowsCache,
    /// The order-book detail the market worker last accepted for this pane.
    order_book_demand: OrderBookDetail,
}

impl Default for OrderFlowPresenter {
    fn default() -> Self {
        Self {
            tape: None,
            time_sales_filter: TimeSalesFilter::default(),
            time_sales_rows: TimeSalesRowsCache::default(),
            order_book_demand: OrderBookDetail::Hidden,
        }
    }
}

impl OrderFlowPresenter {
    pub(super) const fn tape(&self) -> Option<&OrderFlowTape> {
        self.tape.as_ref()
    }

    /// The accepted tape's sweeps, classified by the runtime over the
    /// tape's trailing window.
    pub(super) fn sweeps(&self) -> &[MarketTradeSweep] {
        self.tape.as_ref().map_or(&[], OrderFlowTape::sweeps)
    }

    pub(super) const fn time_sales_filter(&self) -> TimeSalesFilter {
        self.time_sales_filter
    }

    pub(super) const fn time_sales_rows(&self) -> &TimeSalesRowsCache {
        &self.time_sales_rows
    }

    /// Whether the update may replace the accepted tape. A newer selection
    /// generation always supersedes: the selection fence, not the provider
    /// session's own generation numbering, decides which stream is current.
    /// Within one selection an older provider generation or an older revision
    /// of the same provider generation never applies.
    fn supersedes(&self, update: &MarketTradeTapeUpdate) -> bool {
        let selection_generation = update.generation.0.get();
        self.tape.as_ref().is_none_or(|current| {
            selection_generation > current.selection_generation
                || (selection_generation == current.selection_generation
                    && (update.provider_generation > current.provider_generation
                        || (update.provider_generation == current.provider_generation
                            && update.revision >= current.revision)))
        })
    }

    /// Applies one fenced incremental tape publication to the ring.
    pub(super) fn apply_tape_update(&mut self, update: &MarketTradeTapeUpdate) -> TapeApply {
        if !self.supersedes(update) {
            return TapeApply::Stale;
        }
        match &update.operation {
            MarketTradeTapeOperation::Reset { trades } => {
                let mut ring: VecDeque<RetainedMarketTrade> = trades.iter().cloned().collect();
                while ring.len() > MAXIMUM_RECENT_LADDER_TRADES {
                    ring.pop_front();
                }
                self.tape = Some(OrderFlowTape::new(update, ring));
                TapeApply::Reset
            }
            MarketTradeTapeOperation::Append {
                from_ordinal,
                trades,
            } => {
                let Some(tape) = self.tape.as_mut() else {
                    return TapeApply::Broken;
                };
                let continues = trades.is_empty()
                    || (tape.trades.back().is_some_and(|last| {
                        last.ingestion_ordinal.saturating_add(1) == *from_ordinal
                    }) && trades
                        .first()
                        .is_some_and(|first| first.ingestion_ordinal == *from_ordinal));
                if !continues {
                    return TapeApply::Broken;
                }
                tape.trades.extend(trades.iter().cloned());
                while tape.trades.len() > MAXIMUM_RECENT_LADDER_TRADES {
                    tape.trades.pop_front();
                }
                let start = tape.trades.len() - trades.len();
                tape.refresh(update);
                TapeApply::Append { start }
            }
        }
    }

    /// The chart trades one applied update contributes: the covering window
    /// for a reset, or only the new suffix for an append. A trade whose price
    /// or size cannot be represented projects nothing.
    pub(super) fn chart_trades(
        &self,
        apply: TapeApply,
    ) -> Option<(Vec<OrderFlowTrade>, OrderFlowTapeChange)> {
        let tape = self.tape.as_ref()?;
        let (start, change) = match apply {
            TapeApply::Reset => (0, OrderFlowTapeChange::ReplaceWindow),
            TapeApply::Append { start } => (start, OrderFlowTapeChange::Append),
            TapeApply::Stale | TapeApply::Broken => return None,
        };
        chart_order_flow_trades(
            tape.trades.iter().skip(start),
            tape.price_scale,
            tape.quantity_scale,
        )
        .map(|trades| (trades, change))
    }

    /// Forgets the tape and projected rows when the pane leaves its
    /// instrument. The filter is the trader's choice and stays.
    pub(super) fn clear_instrument(&mut self) {
        self.tape = None;
        self.time_sales_rows = TimeSalesRowsCache::default();
    }

    pub(super) const fn cycle_time_sales_side(&mut self) {
        self.time_sales_filter.side = self.time_sales_filter.side.next();
    }

    pub(super) fn cycle_time_sales_size(&mut self) {
        self.time_sales_filter.minimum_quantity = match self.time_sales_filter.minimum_quantity {
            value if value < 1.0 => 1.0,
            value if value < 10.0 => 10.0,
            value if value < 100.0 => 100.0,
            _ => 0.0,
        };
    }

    pub(super) const fn cycle_time_sales_price_range(&mut self) {
        self.time_sales_filter.price_range_ticks = match self.time_sales_filter.price_range_ticks {
            None => Some(10),
            Some(10) => Some(50),
            Some(_) => None,
        };
    }

    /// Restores every filter default at once; reports whether anything changed.
    pub(super) fn reset_time_sales_filter(&mut self) -> bool {
        let default = TimeSalesFilter::default();
        if self.time_sales_filter == default {
            return false;
        }
        self.time_sales_filter = default;
        true
    }

    /// What moving to `demand` asks of the pane, or `None` when the worker already serves it.
    pub(super) fn order_book_demand_change(
        &self,
        demand: OrderBookDetail,
    ) -> Option<OrderBookDemandChange> {
        if demand == self.order_book_demand {
            None
        } else if self.order_book_demand == OrderBookDetail::Hidden {
            Some(OrderBookDemandChange::Opening)
        } else {
            Some(OrderBookDemandChange::Changing)
        }
    }

    /// Records the detail the market worker accepted. A refused request is not recorded, so
    /// the next change sends it again.
    pub(super) const fn record_order_book_demand(&mut self, demand: OrderBookDetail) {
        self.order_book_demand = demand;
    }
}

/// What moving the order-book demand to a new detail asks of the pane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OrderBookDemandChange {
    /// The book was hidden, so its view restarts from loading.
    Opening,
    /// The book stays open at another detail.
    Changing,
}

fn scaled_market_value(value: i64, scale: u8) -> Option<f64> {
    let divisor = 10_f64.powi(i32::from(scale));
    let converted = num_traits::ToPrimitive::to_f64(&value)? / divisor;
    converted.is_finite().then_some(converted)
}

fn chart_order_flow_trades<'a>(
    trades: impl Iterator<Item = &'a RetainedMarketTrade>,
    price_scale: u8,
    quantity_scale: u8,
) -> Option<Vec<OrderFlowTrade>> {
    trades
        .map(|retained| {
            let trade = retained.trade.as_ref();
            let timestamp_nanos = trade
                .metadata
                .timestamps
                .exchange_unix_nanos
                .or(trade.metadata.timestamps.provider_unix_nanos)
                .unwrap_or(trade.metadata.timestamps.received_unix_nanos);
            Some(OrderFlowTrade {
                ingestion_ordinal: retained.ingestion_ordinal,
                timestamp_micros: timestamp_nanos.div_euclid(1_000),
                price: scaled_market_value(trade.price, price_scale)?,
                volume: scaled_market_value(trade.quantity, quantity_scale)?,
                aggressor: match trade.aggressor {
                    aeris_market_data::AggressorSide::Buy => ChartAggressorSide::Buy,
                    aeris_market_data::AggressorSide::Sell => ChartAggressorSide::Sell,
                    aeris_market_data::AggressorSide::Unknown => ChartAggressorSide::Unknown,
                },
                session_id: retained.trading_day.and_then(|day| u64::try_from(day).ok()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    fn retained_tape_trade(ordinal: u64) -> RetainedMarketTrade {
        retained_trade_at(ordinal, ordinal * 1_000_000_000, 100, 1)
    }

    fn retained_trade_at(
        ordinal: u64,
        nanos: u64,
        price: i64,
        quantity: i64,
    ) -> RetainedMarketTrade {
        let nanos = i64::try_from(nanos).expect("small timestamp");
        RetainedMarketTrade {
            ingestion_ordinal: ordinal,
            observed_unix_nanos: nanos,
            trading_day: None,
            trade: Arc::new(aeris_market_data::MarketTrade {
                metadata: aeris_market_data::EventMetadata {
                    provider_id: "provider".into(),
                    instrument_id: "instrument".into(),
                    entitlement_id: "entitlement".into(),
                    session_generation: 1,
                    source_sequence: ordinal,
                    timestamps: aeris_market_data::QualifiedTimestamp {
                        exchange_unix_nanos: Some(nanos),
                        provider_unix_nanos: None,
                        received_unix_nanos: nanos,
                    },
                },
                trade_id: format!("trade-{ordinal}"),
                price,
                quantity,
                aggressor: aeris_market_data::AggressorSide::Buy,
            }),
        }
    }

    fn tape_update(
        provider_generation: u64,
        revision: u64,
        operation: MarketTradeTapeOperation,
    ) -> MarketTradeTapeUpdate {
        MarketTradeTapeUpdate {
            consumer_id: aeris_market_runtime::MarketConsumerId(NonZeroU64::MIN),
            generation: aeris_market_runtime::MarketGenerationId(NonZeroU64::MIN),
            provider_id: "provider".into(),
            instrument_id: "instrument".into(),
            entitlement_id: "entitlement".into(),
            provider_generation,
            revision,
            source_watermark: 0,
            rewrite_generation: 0,
            price_scale: 0,
            quantity_scale: 0,
            sweeps: Arc::from([]),
            operation,
        }
    }

    fn reset(
        provider_generation: u64,
        revision: u64,
        trades: Vec<RetainedMarketTrade>,
    ) -> MarketTradeTapeUpdate {
        tape_update(
            provider_generation,
            revision,
            MarketTradeTapeOperation::Reset {
                trades: trades.into(),
            },
        )
    }

    fn append(
        provider_generation: u64,
        revision: u64,
        from_ordinal: u64,
        trades: Vec<RetainedMarketTrade>,
    ) -> MarketTradeTapeUpdate {
        tape_update(
            provider_generation,
            revision,
            MarketTradeTapeOperation::Append {
                from_ordinal,
                trades: trades.into(),
            },
        )
    }

    fn ring_ordinals(presenter: &OrderFlowPresenter) -> Vec<u64> {
        presenter
            .tape()
            .expect("accepted tape")
            .trades
            .iter()
            .map(|trade| trade.ingestion_ordinal)
            .collect()
    }

    #[test]
    fn a_reset_installs_the_covering_window_and_projects_it_wholesale() {
        let mut presenter = OrderFlowPresenter::default();
        let trades = (1..=3).map(retained_tape_trade).collect();
        assert_eq!(
            presenter.apply_tape_update(&reset(1, 1, trades)),
            TapeApply::Reset
        );
        assert_eq!(ring_ordinals(&presenter), [1, 2, 3]);
        let (trades, change) = presenter
            .chart_trades(TapeApply::Reset)
            .expect("representable trades");
        assert_eq!(change, OrderFlowTapeChange::ReplaceWindow);
        assert_eq!(trades.len(), 3);
        assert_eq!(trades[0].ingestion_ordinal, 1);
    }

    #[test]
    fn an_append_extends_the_ring_and_projects_only_its_suffix() {
        let mut presenter = OrderFlowPresenter::default();
        assert_eq!(
            presenter.apply_tape_update(&reset(1, 1, (1..=3).map(retained_tape_trade).collect())),
            TapeApply::Reset
        );
        let apply = presenter.apply_tape_update(&append(
            1,
            2,
            4,
            (4..=5).map(retained_tape_trade).collect(),
        ));
        assert_eq!(apply, TapeApply::Append { start: 3 });
        assert_eq!(ring_ordinals(&presenter), [1, 2, 3, 4, 5]);
        let (trades, change) = presenter.chart_trades(apply).expect("representable");
        assert_eq!(change, OrderFlowTapeChange::Append);
        assert_eq!(
            trades
                .iter()
                .map(|trade| trade.ingestion_ordinal)
                .collect::<Vec<_>>(),
            [4, 5]
        );
    }

    #[test]
    fn an_append_must_continue_the_ring() {
        let mut presenter = OrderFlowPresenter::default();
        assert_eq!(
            presenter.apply_tape_update(&append(1, 1, 1, vec![retained_tape_trade(1)])),
            TapeApply::Broken,
            "the runtime's first publication is always a covering reset"
        );
        assert!(presenter.tape().is_none());
        assert_eq!(
            presenter.apply_tape_update(&reset(1, 1, (1..=3).map(retained_tape_trade).collect())),
            TapeApply::Reset
        );
        assert_eq!(
            presenter.apply_tape_update(&append(1, 2, 5, vec![retained_tape_trade(5)])),
            TapeApply::Broken,
            "a gap opens only when the runtime missed a reset"
        );
        assert_eq!(
            ring_ordinals(&presenter),
            [1, 2, 3],
            "a broken append leaves the accepted tape unchanged"
        );
        // The stream recovers from the next covering reset.
        assert_eq!(
            presenter.apply_tape_update(&reset(1, 3, (3..=6).map(retained_tape_trade).collect())),
            TapeApply::Reset
        );
        assert_eq!(ring_ordinals(&presenter), [3, 4, 5, 6]);
    }

    #[test]
    fn older_revisions_and_provider_generations_never_apply() {
        let mut presenter = OrderFlowPresenter::default();
        assert_eq!(
            presenter.apply_tape_update(&reset(2, 5, vec![retained_tape_trade(1)])),
            TapeApply::Reset
        );
        assert_eq!(
            presenter.apply_tape_update(&append(2, 4, 2, vec![retained_tape_trade(2)])),
            TapeApply::Stale,
            "an older revision of the same provider session"
        );
        assert_eq!(
            presenter.apply_tape_update(&reset(1, 9, vec![retained_tape_trade(1)])),
            TapeApply::Stale,
            "an older provider session"
        );
        assert_eq!(
            presenter.apply_tape_update(&append(2, 5, 2, vec![retained_tape_trade(2)])),
            TapeApply::Append { start: 1 },
            "a repeat revision still applies"
        );
        assert_eq!(ring_ordinals(&presenter), [1, 2]);
    }

    #[test]
    fn a_newer_selection_generation_supersedes_regardless_of_provider_numbering() {
        let mut presenter = OrderFlowPresenter::default();
        assert_eq!(
            presenter.apply_tape_update(&reset(9, 5, vec![retained_tape_trade(1)])),
            TapeApply::Reset
        );
        // A new selection can sit on a provider session whose generation
        // numbering starts lower; its covering reset still replaces the tape.
        let mut reselection = reset(1, 1, vec![retained_tape_trade(7)]);
        reselection.generation =
            aeris_market_runtime::MarketGenerationId(NonZeroU64::new(2).expect("nonzero"));
        assert_eq!(presenter.apply_tape_update(&reselection), TapeApply::Reset);
        assert_eq!(ring_ordinals(&presenter), [7]);
    }

    #[test]
    fn an_overflow_reset_replaces_the_ring_after_eviction() {
        let mut presenter = OrderFlowPresenter::default();
        assert_eq!(
            presenter.apply_tape_update(&reset(1, 1, (1..=4).map(retained_tape_trade).collect())),
            TapeApply::Reset
        );
        // The consumer's queue overflowed; the runtime covered it with one reset.
        assert_eq!(
            presenter.apply_tape_update(&reset(1, 2, (3..=6).map(retained_tape_trade).collect())),
            TapeApply::Reset
        );
        assert_eq!(ring_ordinals(&presenter), [3, 4, 5, 6]);
    }

    #[test]
    fn the_ring_stays_bounded_at_the_runtime_tape_bound() {
        let mut presenter = OrderFlowPresenter::default();
        assert_eq!(
            presenter.apply_tape_update(&reset(
                1,
                1,
                (1..=u64::try_from(MAXIMUM_RECENT_LADDER_TRADES).expect("capacity"))
                    .map(retained_tape_trade)
                    .collect(),
            )),
            TapeApply::Reset
        );
        let appended = u64::try_from(MAXIMUM_RECENT_LADDER_TRADES).expect("capacity") + 1;
        assert_eq!(
            presenter.apply_tape_update(&append(
                1,
                2,
                appended,
                vec![retained_tape_trade(appended)],
            )),
            TapeApply::Append {
                start: MAXIMUM_RECENT_LADDER_TRADES - 1
            }
        );
        let ring = ring_ordinals(&presenter);
        assert_eq!(ring.len(), MAXIMUM_RECENT_LADDER_TRADES);
        assert_eq!(ring.first(), Some(&2));
        assert_eq!(ring.last(), Some(&appended));
    }

    #[test]
    fn sweeps_come_from_the_update_and_survive_appends() {
        let sweep = MarketTradeSweep {
            first_ingestion_ordinal: 2,
            last_ingestion_ordinal: 3,
            timestamp_micros: 3_000_000,
            terminal_price: 100.0,
            total_volume: 9.0,
            price_levels: 2,
            aggressor: aeris_market_data::AggressorSide::Buy,
        };
        let mut initial = reset(1, 1, (1..=3).map(retained_tape_trade).collect());
        initial.sweeps = Arc::from([sweep]);
        let mut presenter = OrderFlowPresenter::default();
        assert_eq!(presenter.apply_tape_update(&initial), TapeApply::Reset);
        assert_eq!(presenter.sweeps(), &[sweep]);

        let mut next = append(1, 2, 4, vec![retained_tape_trade(4)]);
        next.sweeps = Arc::from([sweep]);
        assert_eq!(
            presenter.apply_tape_update(&next),
            TapeApply::Append { start: 3 }
        );
        assert_eq!(
            presenter.sweeps(),
            &[sweep],
            "an append adopts the runtime's current classification"
        );
    }

    #[test]
    fn leaving_the_instrument_forgets_the_tape_but_keeps_the_filter() {
        let mut presenter = OrderFlowPresenter::default();
        assert_eq!(
            presenter.apply_tape_update(&reset(1, 1, (1..=3).map(retained_tape_trade).collect())),
            TapeApply::Reset
        );
        presenter.cycle_time_sales_side();

        presenter.clear_instrument();
        assert!(presenter.tape().is_none());
        assert_eq!(presenter.sweeps(), &[]);
        assert_eq!(presenter.time_sales_filter().side, TimeSalesSideFilter::Buy);
    }

    #[test]
    fn time_sales_filters_cycle_and_reset_atomically() {
        let mut presenter = OrderFlowPresenter::default();
        assert!(
            !presenter.reset_time_sales_filter(),
            "defaults reset to nothing"
        );

        presenter.cycle_time_sales_side();
        presenter.cycle_time_sales_side();
        assert_eq!(
            presenter.time_sales_filter().side,
            TimeSalesSideFilter::Sell
        );
        presenter.cycle_time_sales_side();
        assert_eq!(presenter.time_sales_filter().side, TimeSalesSideFilter::All);

        let sizes = (0..4)
            .map(|_| {
                presenter.cycle_time_sales_size();
                presenter.time_sales_filter().minimum_quantity
            })
            .collect::<Vec<_>>();
        assert_eq!(sizes, [1.0, 10.0, 100.0, 0.0]);

        let ranges = (0..3)
            .map(|_| {
                presenter.cycle_time_sales_price_range();
                presenter.time_sales_filter().price_range_ticks
            })
            .collect::<Vec<_>>();
        assert_eq!(ranges, [Some(10), Some(50), None]);

        presenter.cycle_time_sales_side();
        presenter.cycle_time_sales_size();
        presenter.cycle_time_sales_price_range();
        assert!(presenter.reset_time_sales_filter());
        assert_eq!(presenter.time_sales_filter(), TimeSalesFilter::default());
        assert!(
            !presenter.reset_time_sales_filter(),
            "resetting defaults is an idempotent no-op"
        );
    }

    #[test]
    fn order_book_demand_changes_only_after_the_worker_accepts_them() {
        let mut presenter = OrderFlowPresenter::default();
        assert_eq!(
            presenter.order_book_demand_change(OrderBookDetail::Hidden),
            None
        );
        assert_eq!(
            presenter.order_book_demand_change(OrderBookDetail::Depth),
            Some(OrderBookDemandChange::Opening)
        );
        // A refused request is not recorded, so the same change is asked for again.
        assert_eq!(
            presenter.order_book_demand_change(OrderBookDetail::Depth),
            Some(OrderBookDemandChange::Opening)
        );
        presenter.record_order_book_demand(OrderBookDetail::Depth);
        assert_eq!(
            presenter.order_book_demand_change(OrderBookDetail::Depth),
            None
        );
        assert_eq!(
            presenter.order_book_demand_change(OrderBookDetail::OrderQueues),
            Some(OrderBookDemandChange::Changing)
        );
        assert_eq!(
            presenter.order_book_demand_change(OrderBookDetail::Hidden),
            Some(OrderBookDemandChange::Changing)
        );
    }
}
