use aeris_localization::{chart_ui, tr};

use super::{
    BOOK_WIDTH, DELTA_WIDTH, ORDERS_WIDTH, PNL_WIDTH, PRICE_WIDTH, QUEUE_WIDTH, TRADE_WIDTH,
    VOLUME_WIDTH,
};

/// Columns available in the read-only order-book ladder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderBookColumn {
    ProfitLoss,
    Bid,
    SellTrades,
    Price,
    BuyTrades,
    Ask,
    /// The trader's own working orders and position, else the level's order count.
    Orders,
    /// The individual resting orders at each price in queue-priority order.
    Queue,
    /// Buy-aggressor minus sell-aggressor volume traded at each price over the
    /// runtime's retained tape (`OrderBookFrame::traded_volumes`).
    Delta,
    /// Total aggressor volume traded at each price over the same retained tape,
    /// drawn as a profile with its point of control marked.
    Volume,
}

/// Provider connectivity shown independently from the last valid book frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderBookConnectionState {
    Online,
    Offline,
    Recovering,
}

impl OrderBookColumn {
    pub const ALL: [Self; 10] = [
        Self::ProfitLoss,
        Self::Bid,
        Self::SellTrades,
        Self::Price,
        Self::BuyTrades,
        Self::Ask,
        Self::Delta,
        Self::Volume,
        Self::Orders,
        Self::Queue,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ProfitLoss => tr!(chart_ui::BOOK_COLUMN_PNL),
            Self::Bid => tr!(chart_ui::BOOK_COLUMN_BID),
            Self::SellTrades => tr!(chart_ui::BOOK_COLUMN_SELL),
            Self::Price => tr!(chart_ui::BOOK_COLUMN_PRICE),
            Self::BuyTrades => tr!(chart_ui::BOOK_COLUMN_BUY),
            Self::Ask => tr!(chart_ui::BOOK_COLUMN_ASK),
            Self::Orders => tr!(chart_ui::BOOK_COLUMN_ORDERS),
            Self::Queue => tr!(chart_ui::BOOK_COLUMN_QUEUE),
            Self::Delta => tr!(chart_ui::BOOK_COLUMN_DELTA),
            Self::Volume => tr!(chart_ui::BOOK_COLUMN_VOLUME),
        }
    }

    const fn weight(self) -> f32 {
        match self {
            Self::ProfitLoss => PNL_WIDTH,
            Self::Bid | Self::Ask => BOOK_WIDTH,
            Self::SellTrades | Self::BuyTrades => TRADE_WIDTH,
            Self::Price => PRICE_WIDTH,
            Self::Orders => ORDERS_WIDTH,
            Self::Queue => QUEUE_WIDTH,
            Self::Delta => DELTA_WIDTH,
            Self::Volume => VOLUME_WIDTH,
        }
    }

    /// The column's bit in persisted column choices; never renumber a bit.
    pub(super) const fn bit(self) -> u16 {
        match self {
            Self::ProfitLoss => 1 << 0,
            Self::Bid => 1 << 1,
            Self::SellTrades => 1 << 2,
            Self::Price => 1 << 3,
            Self::BuyTrades => 1 << 4,
            Self::Ask => 1 << 5,
            Self::Orders => 1 << 6,
            Self::Queue => 1 << 7,
            Self::Delta => 1 << 8,
            Self::Volume => 1 << 9,
        }
    }

    const fn all_bits() -> u16 {
        let mut bits = 0;
        let mut index = 0;
        while index < Self::ALL.len() {
            bits |= Self::ALL[index].bit();
            index += 1;
        }
        bits
    }
}

/// Current depth-ladder column visibility. A column shows only while it is both
/// chosen and offered; the queue column is offered only for a provider that
/// streams individual resting orders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderBookColumnVisibility {
    visible: u16,
    offered: u16,
}

impl Default for OrderBookColumnVisibility {
    fn default() -> Self {
        Self {
            visible: OrderBookColumn::Bid.bit()
                | OrderBookColumn::SellTrades.bit()
                | OrderBookColumn::Price.bit()
                | OrderBookColumn::BuyTrades.bit()
                | OrderBookColumn::Ask.bit(),
            offered: !OrderBookColumn::Queue.bit(),
        }
    }
}

impl OrderBookColumnVisibility {
    #[must_use]
    pub const fn is_visible(self, column: OrderBookColumn) -> bool {
        self.visible & self.offered & column.bit() != 0
    }

    #[must_use]
    pub const fn is_offered(self, column: OrderBookColumn) -> bool {
        self.offered & column.bit() != 0
    }

    /// The trader's column choices, including a chosen column not offered right now, as
    /// the stable bits a workspace persists.
    #[must_use]
    pub const fn chosen_bits(self) -> u16 {
        self.visible
    }

    /// Restores persisted column choices; bits of no known column are dropped.
    #[must_use]
    pub const fn with_chosen_bits(self, bits: u16) -> Self {
        Self {
            visible: bits & OrderBookColumn::all_bits(),
            offered: self.offered,
        }
    }

    pub(super) fn toggle(&mut self, column: OrderBookColumn) -> bool {
        if !self.is_offered(column) {
            return false;
        }
        self.visible ^= column.bit();
        true
    }

    /// Offers or withdraws the queue column, keeping the trader's choice for
    /// when it is offered again. Returns whether the offer changed.
    pub(super) fn offer_order_queues(&mut self, offered: bool) -> bool {
        let offered = if offered {
            self.offered | OrderBookColumn::Queue.bit()
        } else {
            self.offered & !OrderBookColumn::Queue.bit()
        };
        let changed = offered != self.offered;
        self.offered = offered;
        changed
    }

    pub(super) fn width(self, column: OrderBookColumn) -> f32 {
        let total = OrderBookColumn::ALL
            .into_iter()
            .filter(|candidate| self.is_visible(*candidate))
            .map(OrderBookColumn::weight)
            .sum::<f32>();
        if total > 0.0 {
            column.weight() / total
        } else {
            0.0
        }
    }
}

/// Trader-chosen highlights on the depth ladder.
///
/// The imbalance highlight compares the volume bought (lifting the ask) with the
/// volume sold (hitting the bid) at the same ladder price over the runtime's
/// retained tape, the same `OrderBookFrame::traded_volumes` the Buy, Sell, Delta
/// and Volume columns read: when one side traded at least `ratio` times the
/// other, that side's trade cell is outlined. A price that traded on one side
/// only is not an imbalance. The large-lot highlight outlines a resting bid or
/// ask whose size is at least the threshold in whole instrument units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderBookHighlights {
    imbalance_ratio: Option<u32>,
    large_lot_quantity: Option<u32>,
}

impl Default for OrderBookHighlights {
    fn default() -> Self {
        Self {
            imbalance_ratio: Some(Self::DEFAULT_IMBALANCE_RATIO),
            large_lot_quantity: None,
        }
    }
}

impl OrderBookHighlights {
    /// The offered imbalance ratios, each read as `ratio`:1.
    pub const IMBALANCE_RATIOS: [u32; 4] = [2, 3, 4, 5];
    pub const DEFAULT_IMBALANCE_RATIO: u32 = 3;
    /// The offered large-lot thresholds in whole instrument units.
    pub const LARGE_LOT_QUANTITIES: [u32; 9] = [1, 5, 10, 25, 50, 100, 250, 500, 1_000];

    /// Highlights from offered choices (`None` turns one off); a value that is not
    /// offered yields `None`.
    #[must_use]
    pub fn from_choices(
        imbalance_ratio: Option<u32>,
        large_lot_quantity: Option<u32>,
    ) -> Option<Self> {
        let offered = |value: Option<u32>, choices: &[u32]| {
            value.is_none_or(|value| choices.contains(&value))
        };
        (offered(imbalance_ratio, &Self::IMBALANCE_RATIOS)
            && offered(large_lot_quantity, &Self::LARGE_LOT_QUANTITIES))
        .then_some(Self {
            imbalance_ratio,
            large_lot_quantity,
        })
    }

    #[must_use]
    pub const fn imbalance_ratio(self) -> Option<u32> {
        self.imbalance_ratio
    }

    #[must_use]
    pub const fn large_lot_quantity(self) -> Option<u32> {
        self.large_lot_quantity
    }

    /// The next offered ratio, then off, then the smallest ratio again.
    #[must_use]
    pub fn with_next_imbalance_ratio(self) -> Self {
        Self {
            imbalance_ratio: next_choice(self.imbalance_ratio, &Self::IMBALANCE_RATIOS),
            ..self
        }
    }

    /// The next offered threshold, then off, then the smallest threshold again.
    #[must_use]
    pub fn with_next_large_lot_quantity(self) -> Self {
        Self {
            large_lot_quantity: next_choice(self.large_lot_quantity, &Self::LARGE_LOT_QUANTITIES),
            ..self
        }
    }
}

fn next_choice(current: Option<u32>, choices: &[u32]) -> Option<u32> {
    let next = current.map_or(0, |value| {
        choices
            .iter()
            .position(|choice| *choice == value)
            .map_or(0, |index| index + 1)
    });
    choices.get(next).copied()
}
