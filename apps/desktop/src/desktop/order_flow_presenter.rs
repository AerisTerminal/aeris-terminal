//! The pane's order-flow presentation: the accepted trade tape, the sweeps highlighted in it,
//! the Time & Sales filter and rows, and the order-book detail the market worker serves.

use super::time_sales_panel::TimeSalesRowsCache;
use super::{
    ChartAggressorSide, OrderBookDetail, OrderFlowSweep, OrderFlowTrade, classify_order_flow_sweeps,
};
use aeris_market_runtime::{MarketTradeTapeSnapshot, RetainedMarketTrade};
use std::sync::Arc;

/// The trailing trades the sweep classifier reads; every chart projection includes them.
const SWEEP_CLASSIFICATION_WINDOW: usize = 512;

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

/// Which trades a chart update takes from the tape.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChartTradeProjection {
    /// The footprint continues after the chart's last applied ordinal, when it has one.
    Footprint { resume_ordinal: Option<u64> },
    /// The chart draws no footprint; only the sweep window is projected.
    SweepsOnly,
}

/// What moving the order-book demand to a new detail asks of the pane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OrderBookDemandChange {
    /// The book was hidden, so its view restarts from loading.
    Opening,
    /// The book stays open at another detail.
    Changing,
}

pub(super) struct OrderFlowPresenter {
    tape: Option<MarketTradeTapeSnapshot>,
    sweeps: Arc<[OrderFlowSweep]>,
    time_sales_filter: TimeSalesFilter,
    time_sales_rows: TimeSalesRowsCache,
    /// The order-book detail the market worker last accepted for this pane.
    order_book_demand: OrderBookDetail,
}

impl Default for OrderFlowPresenter {
    fn default() -> Self {
        Self {
            tape: None,
            sweeps: Arc::from([]),
            time_sales_filter: TimeSalesFilter::default(),
            time_sales_rows: TimeSalesRowsCache::default(),
            order_book_demand: OrderBookDetail::Hidden,
        }
    }
}

impl OrderFlowPresenter {
    pub(super) const fn tape(&self) -> Option<&MarketTradeTapeSnapshot> {
        self.tape.as_ref()
    }

    pub(super) fn sweeps(&self) -> &[OrderFlowSweep] {
        &self.sweeps
    }

    pub(super) const fn time_sales_filter(&self) -> TimeSalesFilter {
        self.time_sales_filter
    }

    pub(super) const fn time_sales_rows(&self) -> &TimeSalesRowsCache {
        &self.time_sales_rows
    }

    /// Whether `snapshot` may replace the accepted tape. A new selection generation always
    /// does; within one, an older provider generation or an older revision of the same
    /// provider generation never does.
    pub(super) fn supersedes(&self, snapshot: &MarketTradeTapeSnapshot) -> bool {
        self.tape.as_ref().is_none_or(|current| {
            current.generation != snapshot.generation
                || (snapshot.provider_generation >= current.provider_generation
                    && (current.provider_generation != snapshot.provider_generation
                        || snapshot.revision >= current.revision))
        })
    }

    /// Whether `snapshot` rewrote the prefix of the accepted tape, so the chart must drop the
    /// order flow it projected from it.
    pub(super) fn rewrites(&self, snapshot: &MarketTradeTapeSnapshot) -> bool {
        self.tape
            .as_ref()
            .is_some_and(|prior| prior.rewrite_generation != snapshot.rewrite_generation)
    }

    pub(super) fn adopt_tape(&mut self, snapshot: MarketTradeTapeSnapshot) {
        self.tape = Some(snapshot);
    }

    /// Forgets the tape, its sweeps and projected rows when the pane leaves its instrument.
    /// The filter is the trader's choice and stays.
    pub(super) fn clear_instrument(&mut self) {
        self.tape = None;
        self.sweeps = Arc::from([]);
        self.time_sales_rows = TimeSalesRowsCache::default();
    }

    /// The chart trades `snapshot` contributes under `projection`, classifying the sweeps of
    /// their trailing window. A trade whose price or size cannot be represented projects
    /// nothing and keeps the previous sweeps.
    pub(super) fn project_chart_trades(
        &mut self,
        snapshot: &MarketTradeTapeSnapshot,
        projection: ChartTradeProjection,
    ) -> Option<Vec<OrderFlowTrade>> {
        let start = match projection {
            ChartTradeProjection::Footprint { resume_ordinal } => order_flow_projection_start(
                &snapshot.trades,
                resume_ordinal,
                SWEEP_CLASSIFICATION_WINDOW,
            ),
            ChartTradeProjection::SweepsOnly => snapshot
                .trades
                .len()
                .saturating_sub(SWEEP_CLASSIFICATION_WINDOW),
        };
        let trades = chart_order_flow_trades(snapshot, start)?;
        let recent_start = trades.len().saturating_sub(SWEEP_CLASSIFICATION_WINDOW);
        self.sweeps = classify_order_flow_sweeps(&trades[recent_start..]).into();
        Some(trades)
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

fn scaled_market_value(value: i64, scale: u8) -> Option<f64> {
    let divisor = 10_f64.powi(i32::from(scale));
    let converted = num_traits::ToPrimitive::to_f64(&value)? / divisor;
    converted.is_finite().then_some(converted)
}

/// The first retained trade a chart update still needs. Keeping one trade the chart
/// already applied preserves the window's continuity evidence, and the trailing
/// `tail_window` trades are always projected for the sweep classifier.
fn order_flow_projection_start(
    trades: &[RetainedMarketTrade],
    resume_ordinal: Option<u64>,
    tail_window: usize,
) -> usize {
    let Some(applied) = resume_ordinal else {
        return 0;
    };
    let next = trades.partition_point(|trade| trade.ingestion_ordinal <= applied);
    next.saturating_sub(1)
        .min(trades.len().saturating_sub(tail_window))
}

fn chart_order_flow_trades(
    snapshot: &MarketTradeTapeSnapshot,
    start: usize,
) -> Option<Vec<OrderFlowTrade>> {
    snapshot
        .trades
        .get(start..)?
        .iter()
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
                price: scaled_market_value(trade.price, snapshot.price_scale)?,
                volume: scaled_market_value(trade.quantity, snapshot.quantity_scale)?,
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

    fn tape(
        selection: u64,
        provider_generation: u64,
        revision: u64,
        trades: Vec<RetainedMarketTrade>,
    ) -> MarketTradeTapeSnapshot {
        MarketTradeTapeSnapshot {
            consumer_id: aeris_market_runtime::MarketConsumerId(NonZeroU64::MIN),
            generation: aeris_market_runtime::MarketGenerationId(
                NonZeroU64::new(selection).expect("selection generation is nonzero"),
            ),
            provider_id: "provider".into(),
            instrument_id: "instrument".into(),
            entitlement_id: "entitlement".into(),
            provider_generation,
            revision,
            source_watermark: 0,
            rewrite_generation: 0,
            price_scale: 0,
            quantity_scale: 0,
            trades: trades.into(),
        }
    }

    #[test]
    fn tape_ordering_rejects_only_older_publications_of_one_selection() {
        let mut presenter = OrderFlowPresenter::default();
        assert!(presenter.supersedes(&tape(1, 2, 5, Vec::new())));
        presenter.adopt_tape(tape(1, 2, 5, Vec::new()));

        assert!(
            presenter.supersedes(&tape(1, 2, 5, Vec::new())),
            "a repeat revision applies"
        );
        assert!(presenter.supersedes(&tape(1, 2, 6, Vec::new())));
        assert!(!presenter.supersedes(&tape(1, 2, 4, Vec::new())));
        assert!(
            presenter.supersedes(&tape(1, 3, 1, Vec::new())),
            "a newer provider generation restarts revisions"
        );
        assert!(!presenter.supersedes(&tape(1, 1, 9, Vec::new())));
        assert!(
            presenter.supersedes(&tape(2, 1, 1, Vec::new())),
            "a new selection replaces the tape outright"
        );
    }

    #[test]
    fn a_rewrite_is_reported_only_against_an_accepted_tape() {
        let mut presenter = OrderFlowPresenter::default();
        let mut rewritten = tape(1, 1, 2, Vec::new());
        rewritten.rewrite_generation = 1;
        assert!(!presenter.rewrites(&rewritten));
        presenter.adopt_tape(tape(1, 1, 1, Vec::new()));
        assert!(!presenter.rewrites(&tape(1, 1, 2, Vec::new())));
        assert!(presenter.rewrites(&rewritten));
    }

    #[test]
    fn leaving_the_instrument_forgets_the_tape_and_sweeps_but_keeps_the_filter() {
        let mut presenter = OrderFlowPresenter::default();
        let trades = (1..=3)
            .map(|ordinal| {
                retained_trade_at(
                    ordinal,
                    ordinal * 1_000,
                    100 + i64::try_from(ordinal).expect("small"),
                    50,
                )
            })
            .collect();
        let snapshot = tape(1, 1, 1, trades);
        assert!(
            presenter
                .project_chart_trades(&snapshot, ChartTradeProjection::SweepsOnly)
                .is_some()
        );
        presenter.adopt_tape(snapshot);
        presenter.cycle_time_sales_side();

        presenter.clear_instrument();
        assert!(presenter.tape().is_none());
        assert_eq!(presenter.sweeps(), &[]);
        assert_eq!(presenter.time_sales_filter().side, TimeSalesSideFilter::Buy);
    }

    #[test]
    fn order_flow_projection_keeps_one_applied_trade_and_the_sweep_window() {
        let trades = (10..=20).map(retained_tape_trade).collect::<Vec<_>>();
        assert_eq!(order_flow_projection_start(&trades, None, 2), 0);
        // Ordinal 15 is index 5; keep it as continuity evidence for ordinals 16..=20.
        assert_eq!(order_flow_projection_start(&trades, Some(15), 2), 5);
        // The sweep window reaches further back than the unapplied suffix.
        assert_eq!(order_flow_projection_start(&trades, Some(20), 2), 9);
        // An applied ordinal older than the window still sends the covering window.
        assert_eq!(order_flow_projection_start(&trades, Some(3), 2), 0);
        assert_eq!(order_flow_projection_start(&[], Some(3), 2), 0);
    }

    #[test]
    fn chart_projection_classifies_sweeps_over_the_trailing_window_only() {
        let count = SWEEP_CLASSIFICATION_WINDOW as u64 + 100;
        let trades = (1..=count)
            .map(|ordinal| {
                retained_trade_at(
                    ordinal,
                    ordinal * 1_000,
                    i64::try_from(ordinal).expect("small ordinal"),
                    1,
                )
            })
            .collect::<Vec<_>>();
        let snapshot = tape(1, 1, 1, trades);
        let mut presenter = OrderFlowPresenter::default();

        let footprint = presenter
            .project_chart_trades(
                &snapshot,
                ChartTradeProjection::Footprint {
                    resume_ordinal: None,
                },
            )
            .expect("representable trades");
        assert_eq!(
            footprint.len(),
            snapshot.trades.len(),
            "a fresh chart takes the whole tape"
        );
        let tail = &footprint[footprint.len() - SWEEP_CLASSIFICATION_WINDOW..];
        assert_eq!(
            presenter.sweeps(),
            classify_order_flow_sweeps(tail).as_slice()
        );

        let sweeps_only = presenter
            .project_chart_trades(&snapshot, ChartTradeProjection::SweepsOnly)
            .expect("representable trades");
        assert_eq!(sweeps_only.len(), SWEEP_CLASSIFICATION_WINDOW);
        assert_eq!(
            sweeps_only.first().map(|trade| trade.ingestion_ordinal),
            Some(101)
        );
        assert_eq!(
            presenter.sweeps(),
            classify_order_flow_sweeps(&sweeps_only).as_slice()
        );
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
