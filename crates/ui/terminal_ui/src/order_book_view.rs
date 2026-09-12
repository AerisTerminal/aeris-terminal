#[cfg(test)]
use crate::OrderBookRow;
use crate::order_book::{compact_quantity_text, grouped_fixed_point_text};
use crate::{OrderBookColumnLevel, OrderBookFrame};
use axiusflow_design_system::{
    AxiusflowTheme, ThemeColor, TypographyRole, platform_font_family, platform_typography,
};
use axiusflow_market_data::{AggressorTradeVolumes, OrderBookRecoveryReason, OrderBookState};
use gpui::{
    AnyElement, Context, Div, Hsla, IntoElement, Render, ScrollStrategy, UniformListScrollHandle,
    Window, div, prelude::*, px, relative, uniform_list,
};
#[cfg(test)]
use std::cmp::Ordering;
use std::sync::Arc;

const HEADER_HEIGHT: f32 = 28.0;
const ROW_HEIGHT: f32 = 16.0;
const TEXT_SIZE: f32 = 11.0;
const MAXIMUM_TRADE_PRICES_FOR_GRID_INFERENCE: usize = 64;
// Match the reference ladder's four-significant-figure Hyperliquid grouping.
// Derive this from price magnitude rather than the current top-20 snapshot's
// sparse level gaps, which can otherwise make BTC jump between $10 and $100
// rows as liquidity changes.
const HYPERLIQUID_DISPLAY_SIGNIFICANT_FIGURES: u32 = 4;
/// A continuous presentation grid extends at least this many authoritative
/// ticks above and below the spread when a provider supplied a real increment.
/// This is UI runway only; canonical depth remains untouched and real levels
/// farther away extend the grid as needed.
const MINIMUM_PRICE_GRID_ROWS_PER_SIDE: usize = 4_096;
/// Protect GPUI/f32 layout precision from pathological sparse books. Crossing
/// this limit falls back to the all-real-level virtual ladder, so no depth is
/// hidden or truncated.
// 524,288 rows * 16 px ~= 8.4M px, comfortably below f32's 2^24
// integer-exact boundary. Wider sparse spans fall back to the real-level list.
const MAXIMUM_PRICE_GRID_ROWS: usize = 524_288;
const PNL_WIDTH: f32 = 0.10;
const BOOK_WIDTH: f32 = 0.30;
const TRADE_WIDTH: f32 = 0.10;
const PRICE_WIDTH: f32 = 0.20;
const ORDERS_WIDTH: f32 = 0.10;

fn platform_font_weight(role: TypographyRole) -> gpui::FontWeight {
    gpui::FontWeight(f32::from(platform_typography().weight(role)))
}

fn platform_tabular_numerals() -> gpui::FontFeatures {
    gpui::FontFeatures(Arc::new(vec![(
        platform_typography().tabular_numerals_feature().to_owned(),
        1,
    )]))
}

/// Columns available in the read-only order-book ladder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderBookColumn {
    ProfitLoss,
    Bid,
    SellTrades,
    Price,
    BuyTrades,
    Ask,
    Orders,
}

/// Provider connectivity shown independently from the last valid book frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderBookConnectionState {
    Online,
    Offline,
    Recovering,
}

impl OrderBookColumn {
    pub const ALL: [Self; 7] = [
        Self::ProfitLoss,
        Self::Bid,
        Self::SellTrades,
        Self::Price,
        Self::BuyTrades,
        Self::Ask,
        Self::Orders,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ProfitLoss => "P/L",
            Self::Bid => "Bid",
            Self::SellTrades => "Sell",
            Self::Price => "Price",
            Self::BuyTrades => "Buy",
            Self::Ask => "Ask",
            Self::Orders => "Orders",
        }
    }

    const fn weight(self) -> f32 {
        match self {
            Self::ProfitLoss => PNL_WIDTH,
            Self::Bid | Self::Ask => BOOK_WIDTH,
            Self::SellTrades | Self::BuyTrades => TRADE_WIDTH,
            Self::Price => PRICE_WIDTH,
            Self::Orders => ORDERS_WIDTH,
        }
    }

    const fn bit(self) -> u8 {
        match self {
            Self::ProfitLoss => 1 << 0,
            Self::Bid => 1 << 1,
            Self::SellTrades => 1 << 2,
            Self::Price => 1 << 3,
            Self::BuyTrades => 1 << 4,
            Self::Ask => 1 << 5,
            Self::Orders => 1 << 6,
        }
    }

    /// P/L stays unavailable until order routing can supply authoritative values.
    #[must_use]
    pub const fn available(self) -> bool {
        !matches!(self, Self::ProfitLoss)
    }
}

/// Current depth-ladder column visibility.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderBookColumnVisibility {
    visible: u8,
}

impl Default for OrderBookColumnVisibility {
    fn default() -> Self {
        Self {
            visible: OrderBookColumn::Bid.bit()
                | OrderBookColumn::SellTrades.bit()
                | OrderBookColumn::Price.bit()
                | OrderBookColumn::BuyTrades.bit()
                | OrderBookColumn::Ask.bit(),
        }
    }
}

impl OrderBookColumnVisibility {
    #[must_use]
    pub const fn is_visible(self, column: OrderBookColumn) -> bool {
        self.visible & column.bit() != 0
    }

    fn toggle(&mut self, column: OrderBookColumn) -> bool {
        if !column.available() {
            return false;
        }
        self.visible ^= column.bit();
        true
    }

    fn width(self, column: OrderBookColumn) -> f32 {
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

/// Flush, square-edged GPUI view for one immutable read-only Order Book frame.
pub struct ReadOnlyOrderBookView {
    frame: Option<Arc<OrderBookFrame>>,
    unavailable: bool,
    connection_state: OrderBookConnectionState,
    theme: AxiusflowTheme,
    ladder_scroll: UniformListScrollHandle,
    columns: OrderBookColumnVisibility,
}

impl ReadOnlyOrderBookView {
    #[must_use]
    pub fn new(theme: AxiusflowTheme) -> Self {
        Self {
            frame: None,
            unavailable: false,
            connection_state: OrderBookConnectionState::Online,
            theme,
            ladder_scroll: UniformListScrollHandle::new(),
            columns: OrderBookColumnVisibility::default(),
        }
    }

    #[must_use]
    pub fn frame(&self) -> Option<&OrderBookFrame> {
        self.frame.as_deref()
    }

    #[must_use]
    pub const fn columns(&self) -> OrderBookColumnVisibility {
        self.columns
    }

    pub fn toggle_column(&mut self, column: OrderBookColumn, cx: &mut Context<Self>) {
        if self.columns.toggle(column) {
            cx.notify();
        }
    }

    /// Replaces the immutable frame. Older sessions, selections, and revisions are rejected.
    ///
    /// The desktop drains the bounded market mailbox from GPUI's `on_next_frame`
    /// callback. Installing here therefore follows the window's actual display
    /// cadence, including refresh-rate changes and moves between displays. The
    /// mailbox coalesces superseded book publications before this boundary.
    pub fn replace_frame(&mut self, frame: OrderBookFrame, cx: &mut Context<Self>) -> bool {
        let frame = Arc::new(frame);
        if self
            .frame
            .as_deref()
            .is_some_and(|current| frame_precedes(frame.as_ref(), current))
        {
            return false;
        }
        self.install_frame(frame, cx);
        true
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        let had_frames = self.discard_frames();
        let was_unavailable = std::mem::replace(&mut self.unavailable, false);
        if had_frames || was_unavailable {
            cx.notify();
        }
    }

    /// Marks the order book as known-unavailable after a concrete failure (dead
    /// worker, stopped provider). The stale frame is dropped so a frozen
    /// book is never presented as live. Any later frame or demand clears it,
    /// so the panel returns to loading and then data on recovery.
    pub fn mark_unavailable(&mut self, cx: &mut Context<Self>) {
        let had_frames = self.discard_frames();
        if !self.unavailable || had_frames {
            self.unavailable = true;
            cx.notify();
        }
    }

    fn discard_frames(&mut self) -> bool {
        self.frame.take().is_some()
    }

    /// Updates the connectivity banner without discarding the last valid book.
    pub fn set_connection_state(
        &mut self,
        state: OrderBookConnectionState,
        cx: &mut Context<Self>,
    ) {
        if self.connection_state != state {
            self.connection_state = state;
            cx.notify();
        }
    }

    pub fn set_theme(&mut self, theme: AxiusflowTheme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            cx.notify();
        }
    }

    fn install_frame(&mut self, frame: Arc<OrderBookFrame>, cx: &mut Context<Self>) {
        let recenter = should_recenter_ladder(self.frame.as_deref(), frame.as_ref());
        let recenter_index = recenter.then(|| ladder_recenter_index(frame.as_ref()));
        self.frame = Some(frame);
        self.unavailable = false;
        if let Some(Some(index)) = recenter_index {
            self.ladder_scroll = UniformListScrollHandle::new();
            self.ladder_scroll
                .scroll_to_item_strict(index, ScrollStrategy::Center);
        }
        cx.notify();
    }
}

fn should_recenter_ladder(current: Option<&OrderBookFrame>, next: &OrderBookFrame) -> bool {
    current.is_none_or(|current| {
        let current_grid = price_grid_layout(current);
        let next_grid = price_grid_layout(next);
        next.selection_generation != current.selection_generation
            || next.session_generation != current.session_generation
            // A real-level fallback and the synthetic price grid use very different
            // item indexes. Keeping the old scroll offset across that transition can
            // strand the viewport at index zero, thousands of synthetic ask ticks
            // above the spread. An inferred-tick change has the same remapping risk.
            || current_grid.map(|grid| (grid.tick, grid.ask_rows))
                != next_grid.map(|grid| (grid.tick, grid.ask_rows))
            || (ladder_recenter_index(current).is_none() && ladder_recenter_index(next).is_some())
    })
}

fn frame_precedes(candidate: &OrderBookFrame, current: &OrderBookFrame) -> bool {
    candidate.session_generation < current.session_generation
        || (candidate.session_generation == current.session_generation
            && (candidate.selection_generation < current.selection_generation
                || (candidate.selection_generation == current.selection_generation
                    && (candidate.revision < current.revision
                        || (candidate.revision == current.revision
                            && candidate.trade_source_watermark
                                < current.trade_source_watermark)))))
}

impl Render for ReadOnlyOrderBookView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        let state = self.frame.as_deref().map(|frame| frame.state);
        let watermark = self
            .frame
            .as_deref()
            .map_or(0, |frame| frame.source_watermark);
        let empty_copy = empty_book_copy(self.unavailable, self.frame.is_some());
        let frame = self.frame.clone();
        let ladder_scroll = self.ladder_scroll.clone();
        let columns = self.columns;

        div()
            .id("read_only_order_book")
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .bg(gpui_color(colors.surface))
            .font_family(platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .text_color(gpui_color(colors.text_primary))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .child(render_header(columns, &self.theme))
                    .children(connection_status_banner(
                        self.connection_state,
                        self.frame.is_some(),
                        self.unavailable,
                        &self.theme,
                    ))
                    .children(
                        (self.connection_state == OrderBookConnectionState::Online)
                            .then(|| {
                                state.and_then(|state| status_banner(state, watermark, &self.theme))
                            })
                            .flatten(),
                    )
                    .child(render_ladder(
                        frame,
                        empty_copy,
                        columns,
                        &self.theme,
                        &ladder_scroll,
                    )),
            )
    }
}

fn connection_status_banner(
    state: OrderBookConnectionState,
    has_frame: bool,
    unavailable: bool,
    theme: &AxiusflowTheme,
) -> Option<impl IntoElement + use<>> {
    let (label, color): (&str, StatusColor) = match state {
        OrderBookConnectionState::Online => return None,
        OrderBookConnectionState::Offline => {
            ("Order Book offline · internet disconnected", |theme| {
                theme.colors.danger
            })
        }
        // Discovering/authenticating and the first provider snapshot all flow
        // through Recovering at the desktop boundary. Until a concrete book
        // has actually been presented, that is ordinary loading rather than
        // a reconnect/failure and the ladder's neutral loading copy is enough.
        OrderBookConnectionState::Recovering if !has_frame && !unavailable => return None,
        OrderBookConnectionState::Recovering => (
            "Order Book reconnecting · awaiting fresh snapshot",
            |theme| theme.colors.danger,
        ),
    };
    Some(
        div()
            .h(px(ROW_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .px_2()
            .border_b_1()
            .border_color(gpui_color(theme.colors.border))
            .bg(gpui_color(color(theme).with_alpha(0.12)))
            .text_size(px(TEXT_SIZE))
            .text_color(gpui_color(color(theme)))
            .child(label),
    )
}

fn render_header(
    columns: OrderBookColumnVisibility,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    div()
        .h(px(HEADER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(gpui_color(theme.colors.border))
        .text_size(px(TEXT_SIZE))
        .text_color(gpui_color(theme.colors.text_secondary))
        .bg(gpui_color(theme.colors.surface_secondary))
        .children(
            OrderBookColumn::ALL
                .into_iter()
                .filter(|column| columns.is_visible(*column))
                .map(|column| {
                    header_cell(
                        columns.width(column),
                        column.label().to_uppercase(),
                        column_alignment(column),
                    )
                }),
        )
}

/// Empty-panel copy: loading until the first frame, and the unavailable
/// verdict only after a concrete failure marked the book dead. An empty
/// book with no failure behind it is still loading, never an error.
const fn empty_book_copy(unavailable: bool, has_frame: bool) -> &'static str {
    if unavailable {
        "Order Book unavailable"
    } else if has_frame {
        "Waiting for order-book snapshot"
    } else {
        "Loading order book…"
    }
}

fn render_ladder(
    frame: Option<Arc<OrderBookFrame>>,
    empty_copy: &'static str,
    columns: OrderBookColumnVisibility,
    theme: &AxiusflowTheme,
    ladder_scroll: &UniformListScrollHandle,
) -> impl IntoElement + use<> {
    let body = div()
        .id("read_only_order_book_rows")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_hidden();
    let Some(frame) = frame else {
        return body.child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(gpui_color(theme.colors.text_secondary))
                .child(empty_copy),
        );
    };
    if frame.rows.is_empty() && price_grid_layout(frame.as_ref()).is_none() {
        return body
            .children(spread_row(
                frame.best_bid.as_ref(),
                frame.best_ask.as_ref(),
                theme,
            ))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(gpui_color(theme.colors.text_secondary))
                    .child(empty_copy),
            );
    }
    let list = render_virtualized_ladder_list(&frame, columns, theme, ladder_scroll);

    body.child(
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .child(list)
            .children(column_rails(columns, theme)),
    )
}

fn render_virtualized_ladder_list(
    frame: &Arc<OrderBookFrame>,
    columns: OrderBookColumnVisibility,
    theme: &AxiusflowTheme,
    ladder_scroll: &UniformListScrollHandle,
) -> AnyElement {
    let list_frame = Arc::clone(frame);
    let list_theme = *theme;
    if let Some(grid) = price_grid_layout(frame.as_ref()) {
        return uniform_list(
            "read_only_order_book_ladder",
            grid.item_count(),
            move |range, _, _| {
                let maximum_quantity =
                    price_grid_visible_max_quantity(list_frame.as_ref(), grid, range.clone());
                let maximum_trade_quantity =
                    price_grid_visible_max_trade_quantity(list_frame.as_ref(), grid, range.clone());
                range
                    .filter_map(|index| {
                        render_price_grid_item(
                            list_frame.as_ref(),
                            grid,
                            index,
                            columns,
                            &list_theme,
                            maximum_quantity,
                            maximum_trade_quantity,
                        )
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(ladder_scroll)
        .size_full()
        .into_any_element();
    }

    let layout = ladder_layout(frame.as_ref());
    uniform_list(
        "read_only_order_book_ladder",
        layout.item_count(),
        move |range, _, _| {
            let maximum_quantity =
                ladder_visible_max_quantity(list_frame.as_ref(), layout, range.clone());
            let maximum_trade_quantity =
                ladder_visible_max_trade_quantity(list_frame.as_ref(), layout, range.clone());
            range
                .filter_map(|index| {
                    render_ladder_item(
                        list_frame.as_ref(),
                        layout,
                        index,
                        columns,
                        &list_theme,
                        maximum_quantity,
                        maximum_trade_quantity,
                    )
                })
                .collect::<Vec<_>>()
        },
    )
    .track_scroll(ladder_scroll)
    .size_full()
    .into_any_element()
}

fn ladder_recenter_index(frame: &OrderBookFrame) -> Option<usize> {
    price_grid_layout(frame)
        .map(PriceGridLayout::recenter_index)
        .or_else(|| ladder_layout(frame).recenter_index())
}

/// Presentation-only fixed-tick layout. Prices without provider depth become
/// blank price rows; quantities/orders are looked up only from the immutable
/// real frame. If any real price is off the provider-declared lattice, fail
/// closed to the existing real-level ladder instead of snapping it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PriceGridLayout {
    best_bid: i64,
    tick: i64,
    ask_rows: usize,
    bid_rows: usize,
}

impl PriceGridLayout {
    const fn item_count(self) -> usize {
        self.ask_rows + 1 + self.bid_rows
    }

    const fn recenter_index(self) -> usize {
        self.ask_rows
    }
}

fn price_grid_layout(frame: &OrderBookFrame) -> Option<PriceGridLayout> {
    let tick = if frame.provider_id == "hyperliquid" {
        hyperliquid_display_tick(frame)?
    } else {
        frame
            .price_increment
            .filter(|increment| *increment > 0)
            .or_else(|| observed_price_increment(frame))?
    };
    let best_bid = price_grid_anchor_best_bid(frame, tick)?;
    let ask_start = best_bid.checked_add(tick)?;

    let mut bid_rows = 1usize;
    let mut ask_rows = 1usize;

    if let Some(best_ask) = display_best_ask(frame).map(|level| level.price) {
        let best_ask = ceil_price_to_tick(best_ask, tick)?;
        if best_ask < ask_start {
            return None;
        }
        ask_rows = ask_rows.max(
            usize::try_from((best_ask - ask_start) / tick)
                .ok()?
                .saturating_add(1),
        );
    }

    for level in frame.rows.iter().filter_map(|row| row.bid.as_ref()) {
        let price = floor_price_to_tick(level.price, tick)?;
        if price > best_bid {
            return None;
        }
        let distance = best_bid.checked_sub(price)?;
        bid_rows = bid_rows.max(usize::try_from(distance / tick).ok()?.saturating_add(1));
    }
    for level in frame.rows.iter().filter_map(|row| row.ask.as_ref()) {
        let price = ceil_price_to_tick(level.price, tick)?;
        if price < ask_start {
            return None;
        }
        let distance = price.checked_sub(ask_start)?;
        ask_rows = ask_rows.max(usize::try_from(distance / tick).ok()?.saturating_add(1));
    }
    // Grid rows are presentation ticks, but their prices still obey the
    // domain's positive-price invariant. Cap the empty runway at the natural
    // numeric boundary so `uniform_list` never receives an index that cannot
    // map to a concrete display price.
    let bid_capacity = usize::try_from((best_bid - 1) / tick)
        .ok()?
        .saturating_add(1);
    let ask_capacity = usize::try_from((i64::MAX - ask_start) / tick)
        .ok()?
        .saturating_add(1);
    bid_rows = bid_rows
        .max(MINIMUM_PRICE_GRID_ROWS_PER_SIDE)
        .min(bid_capacity);
    ask_rows = ask_rows
        .max(MINIMUM_PRICE_GRID_ROWS_PER_SIDE)
        .min(ask_capacity);
    let item_count = ask_rows.checked_add(1)?.checked_add(bid_rows)?;
    if item_count > MAXIMUM_PRICE_GRID_ROWS {
        return None;
    }
    Some(PriceGridLayout {
        best_bid,
        tick,
        ask_rows,
        bid_rows,
    })
}

fn hyperliquid_display_tick(frame: &OrderBookFrame) -> Option<i64> {
    let price = display_best_bid(frame)
        .or_else(|| display_best_ask(frame))
        .map(|level| level.price)
        .or_else(|| {
            frame
                .traded_volumes
                .last_key_value()
                .map(|(price, _)| *price)
        })?;
    if price <= 0 {
        return None;
    }
    let digits = price.ilog10().saturating_add(1);
    10_i64.checked_pow(digits.saturating_sub(HYPERLIQUID_DISPLAY_SIGNIFICANT_FIGURES))
}

fn floor_price_to_tick(price: i64, tick: i64) -> Option<i64> {
    if price <= 0 || tick <= 0 {
        return None;
    }
    price.div_euclid(tick).checked_mul(tick)
}

fn ceil_price_to_tick(price: i64, tick: i64) -> Option<i64> {
    let floor = floor_price_to_tick(price, tick)?;
    if floor == price {
        Some(price)
    } else {
        floor.checked_add(tick)
    }
}

fn observed_price_increment(frame: &OrderBookFrame) -> Option<i64> {
    let mut anchor = None;
    let mut increment = 0;
    for price in frame
        .best_bid
        .iter()
        .chain(frame.best_ask.iter())
        .map(|level| level.price)
        .chain(
            frame
                .rows
                .iter()
                .flat_map(|row| row.bid.iter().chain(row.ask.iter()))
                .map(|level| level.price),
        )
    {
        observe_price_increment(&mut anchor, &mut increment, price);
    }
    // Depth is authoritative for the presentation lattice. Trade retention can
    // contain tens of thousands of distinct prices, so scanning the whole map on
    // every 16 ms presentation frame is both unnecessary and expensive. Only use
    // a bounded trade sample when depth cannot establish a lattice at all (for
    // example, during a trade-only recovery frame).
    if increment == 0 {
        let sample_per_edge = MAXIMUM_TRADE_PRICES_FOR_GRID_INFERENCE / 2;
        for price in frame
            .traded_volumes
            .keys()
            .take(sample_per_edge)
            .chain(frame.traded_volumes.keys().rev().take(sample_per_edge))
            .copied()
        {
            observe_price_increment(&mut anchor, &mut increment, price);
        }
    }
    (increment > 0).then_some(increment)
}

fn observe_price_increment(anchor: &mut Option<i64>, increment: &mut i64, price: i64) {
    if price <= 0 {
        return;
    }
    let Some(anchor_price) = *anchor else {
        *anchor = Some(price);
        return;
    };
    let gap = (price - anchor_price).abs();
    if gap == 0 {
        return;
    }
    *increment = if *increment == 0 {
        gap
    } else {
        greatest_common_divisor(*increment, gap)
    };
}

const fn greatest_common_divisor(mut left: i64, mut right: i64) -> i64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.abs()
}

/// Flowsurface keeps the ladder usable with only one depth side, and can even
/// anchor from retained trades while a covering depth image is absent. Mirror
/// that presentation behavior without promoting quotes/trades into canonical
/// depth. Every generated row still uses the provider-declared increment.
fn price_grid_anchor_best_bid(frame: &OrderBookFrame, tick: i64) -> Option<i64> {
    if let Some(best_bid) = display_best_bid(frame).map(|level| level.price) {
        return floor_price_to_tick(best_bid, tick).filter(|price| *price > 0);
    }
    if let Some(best_ask) = display_best_ask(frame).map(|level| level.price) {
        return ceil_price_to_tick(best_ask, tick)?
            .checked_sub(tick)
            .filter(|price| *price > 0);
    }

    let (&minimum_trade, _) = frame.traded_volumes.first_key_value()?;
    let (&maximum_trade, _) = frame.traded_volumes.last_key_value()?;
    let distance = maximum_trade.checked_sub(minimum_trade)?;
    if minimum_trade <= 0 || distance % tick != 0 {
        return None;
    }
    let inclusive_steps = distance.checked_div(tick)?.checked_add(1)?;
    floor_price_to_tick(
        maximum_trade
            .checked_sub(tick.checked_mul(inclusive_steps / 2)?)
            .filter(|price| *price > 0)?,
        tick,
    )
}

fn display_best_bid(frame: &OrderBookFrame) -> Option<&OrderBookColumnLevel> {
    frame
        .rows
        .iter()
        .find_map(|row| row.bid.as_ref())
        .or(frame.best_bid.as_ref())
}

fn display_best_ask(frame: &OrderBookFrame) -> Option<&OrderBookColumnLevel> {
    frame
        .rows
        .iter()
        .find_map(|row| row.ask.as_ref())
        .or(frame.best_ask.as_ref())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PriceGridItem {
    Ask(i64),
    Spread,
    Bid(i64),
}

fn price_grid_item(layout: PriceGridLayout, index: usize) -> Option<PriceGridItem> {
    if index >= layout.item_count() {
        return None;
    }
    if index < layout.ask_rows {
        let steps_from_ask_start = layout.ask_rows.checked_sub(index + 1)?;
        let steps = i64::try_from(steps_from_ask_start).ok()?;
        let price = layout
            .best_bid
            .checked_add(layout.tick)?
            .checked_add(layout.tick.checked_mul(steps)?)?;
        return Some(PriceGridItem::Ask(price));
    }
    if index == layout.ask_rows {
        return Some(PriceGridItem::Spread);
    }
    let steps = i64::try_from(index.checked_sub(layout.ask_rows + 1)?).ok()?;
    let price = layout
        .best_bid
        .checked_sub(layout.tick.checked_mul(steps)?)?;
    Some(PriceGridItem::Bid(price))
}

#[cfg(test)]
fn real_level_at_price(
    frame: &OrderBookFrame,
    side: BookColumnSide,
    price: i64,
) -> Option<&OrderBookColumnLevel> {
    let index = frame
        .rows
        .binary_search_by(|row| match side {
            // Projection stores asks best-first in ascending price order; a
            // missing tail entry sorts after every concrete price.
            BookColumnSide::Ask => row
                .ask
                .as_ref()
                .map_or(Ordering::Greater, |level| level.price.cmp(&price)),
            // Bids are best-first in descending price order. Reverse the
            // comparison so binary_search still observes Less..Equal..Greater.
            BookColumnSide::Bid => row
                .bid
                .as_ref()
                .map_or(Ordering::Greater, |level| price.cmp(&level.price)),
        })
        .ok()?;
    match side {
        BookColumnSide::Bid => frame.rows.get(index)?.bid.as_ref(),
        BookColumnSide::Ask => frame.rows.get(index)?.ask.as_ref(),
    }
}

fn grouped_level_at_price(
    frame: &OrderBookFrame,
    side: BookColumnSide,
    price: i64,
    tick: i64,
) -> Option<OrderBookColumnLevel> {
    let mut quantity = 0i64;
    let mut order_count = Some(0u32);
    let mut found = false;
    for level in frame.rows.iter().filter_map(|row| match side {
        BookColumnSide::Bid => row.bid.as_ref(),
        BookColumnSide::Ask => row.ask.as_ref(),
    }) {
        let grouped_price = match side {
            BookColumnSide::Bid => floor_price_to_tick(level.price, tick),
            BookColumnSide::Ask => ceil_price_to_tick(level.price, tick),
        }?;
        if grouped_price != price {
            continue;
        }
        found = true;
        quantity = quantity.saturating_add(level.quantity);
        order_count = match (order_count, level.order_count) {
            (Some(total), Some(count)) => Some(total.saturating_add(count)),
            _ => None,
        };
    }
    found.then(|| OrderBookColumnLevel {
        price,
        quantity,
        order_count,
        price_text: grouped_fixed_point_text(price, frame.price_scale),
        quantity_text: compact_quantity_text(quantity, frame.quantity_scale),
        traded_volume: 0,
        traded_volume_text: String::new(),
        relative_size_bps: 0,
    })
}

fn grouped_trade_volumes_at_price(
    frame: &OrderBookFrame,
    price: i64,
    tick: i64,
) -> AggressorTradeVolumes {
    if tick <= 0 {
        return AggressorTradeVolumes::default();
    }
    let mut grouped = AggressorTradeVolumes::default();
    if let Some(sell_end) = price.checked_add(tick.saturating_sub(1)) {
        for volumes in frame
            .traded_volumes
            .range(price..=sell_end)
            .map(|(_, volumes)| *volumes)
        {
            grouped.sell = grouped.sell.saturating_add(volumes.sell);
        }
    }
    if let Some(buy_start) = price
        .checked_sub(tick)
        .and_then(|value| value.checked_add(1))
    {
        for volumes in frame
            .traded_volumes
            .range(buy_start..=price)
            .map(|(_, volumes)| *volumes)
        {
            grouped.buy = grouped.buy.saturating_add(volumes.buy);
        }
    }
    grouped
}

fn render_price_grid_item(
    frame: &OrderBookFrame,
    layout: PriceGridLayout,
    index: usize,
    columns: OrderBookColumnVisibility,
    theme: &AxiusflowTheme,
    maximum_quantity: i64,
    maximum_trade_quantity: i64,
) -> Option<AnyElement> {
    match price_grid_item(layout, index)? {
        PriceGridItem::Ask(price) => Some(
            grouped_level_at_price(frame, BookColumnSide::Ask, price, layout.tick).map_or_else(
                || {
                    render_empty_price_tick(
                        frame,
                        price,
                        frame.price_scale,
                        BookColumnSide::Ask,
                        columns,
                        theme,
                        RowRenderStats::grouped(
                            0,
                            maximum_trade_quantity,
                            grouped_trade_volumes_at_price(frame, price, layout.tick),
                        ),
                    )
                    .into_any_element()
                },
                |level| {
                    render_level_row(
                        frame,
                        &level,
                        BookColumnSide::Ask,
                        columns,
                        theme,
                        RowRenderStats::grouped(
                            maximum_quantity,
                            maximum_trade_quantity,
                            grouped_trade_volumes_at_price(frame, price, layout.tick),
                        ),
                    )
                    .into_any_element()
                },
            ),
        ),
        PriceGridItem::Spread => Some(price_grid_center_row(frame, theme)),
        PriceGridItem::Bid(price) => Some(
            grouped_level_at_price(frame, BookColumnSide::Bid, price, layout.tick).map_or_else(
                || {
                    render_empty_price_tick(
                        frame,
                        price,
                        frame.price_scale,
                        BookColumnSide::Bid,
                        columns,
                        theme,
                        RowRenderStats::grouped(
                            0,
                            maximum_trade_quantity,
                            grouped_trade_volumes_at_price(frame, price, layout.tick),
                        ),
                    )
                    .into_any_element()
                },
                |level| {
                    render_level_row(
                        frame,
                        &level,
                        BookColumnSide::Bid,
                        columns,
                        theme,
                        RowRenderStats::grouped(
                            maximum_quantity,
                            maximum_trade_quantity,
                            grouped_trade_volumes_at_price(frame, price, layout.tick),
                        ),
                    )
                    .into_any_element()
                },
            ),
        ),
    }
}

fn price_grid_visible_max_quantity(
    frame: &OrderBookFrame,
    layout: PriceGridLayout,
    range: std::ops::Range<usize>,
) -> i64 {
    range
        .filter_map(|index| match price_grid_item(layout, index)? {
            PriceGridItem::Ask(price) => {
                grouped_level_at_price(frame, BookColumnSide::Ask, price, layout.tick)
                    .map(|level| level.quantity)
            }
            PriceGridItem::Bid(price) => {
                grouped_level_at_price(frame, BookColumnSide::Bid, price, layout.tick)
                    .map(|level| level.quantity)
            }
            PriceGridItem::Spread => None,
        })
        .max()
        .unwrap_or(0)
}

fn price_grid_visible_max_trade_quantity(
    frame: &OrderBookFrame,
    layout: PriceGridLayout,
    range: std::ops::Range<usize>,
) -> i64 {
    range
        .filter_map(|index| match price_grid_item(layout, index)? {
            PriceGridItem::Ask(price) | PriceGridItem::Bid(price) => {
                Some(grouped_trade_volumes_at_price(frame, price, layout.tick).maximum_side())
            }
            PriceGridItem::Spread => None,
        })
        .max()
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LadderLayout {
    ask_count: usize,
    bid_count: usize,
    spread: bool,
}

impl LadderLayout {
    const fn item_count(self) -> usize {
        self.ask_count + self.bid_count + self.spread as usize
    }

    const fn recenter_index(self) -> Option<usize> {
        if self.spread {
            Some(self.ask_count)
        } else if self.ask_count > 0 {
            Some(self.ask_count - 1)
        } else if self.bid_count > 0 {
            Some(0)
        } else {
            None
        }
    }
}

fn ladder_layout(frame: &OrderBookFrame) -> LadderLayout {
    LadderLayout {
        ask_count: frame.rows.iter().filter(|row| row.ask.is_some()).count(),
        bid_count: frame.rows.iter().filter(|row| row.bid.is_some()).count(),
        spread: display_best_bid(frame).is_some() && display_best_ask(frame).is_some(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LadderItemIndex {
    Ask(usize),
    Spread,
    Bid(usize),
}

fn ladder_item_index(layout: LadderLayout, index: usize) -> Option<LadderItemIndex> {
    if index < layout.ask_count {
        return Some(LadderItemIndex::Ask(layout.ask_count - index - 1));
    }
    let after_asks = index - layout.ask_count;
    if layout.spread {
        if after_asks == 0 {
            return Some(LadderItemIndex::Spread);
        }
        let bid_index = after_asks - 1;
        return if bid_index < layout.bid_count {
            Some(LadderItemIndex::Bid(bid_index))
        } else {
            None
        };
    }
    if after_asks < layout.bid_count {
        Some(LadderItemIndex::Bid(after_asks))
    } else {
        None
    }
}

fn render_ladder_item(
    frame: &OrderBookFrame,
    layout: LadderLayout,
    index: usize,
    columns: OrderBookColumnVisibility,
    theme: &AxiusflowTheme,
    maximum_quantity: i64,
    maximum_trade_quantity: i64,
) -> Option<AnyElement> {
    match ladder_item_index(layout, index)? {
        LadderItemIndex::Ask(row_index) => frame.rows.get(row_index)?.ask.as_ref().map(|level| {
            render_level_row(
                frame,
                level,
                BookColumnSide::Ask,
                columns,
                theme,
                RowRenderStats::raw(maximum_quantity, maximum_trade_quantity),
            )
            .into_any_element()
        }),
        LadderItemIndex::Spread => {
            spread_row(frame.best_bid.as_ref(), frame.best_ask.as_ref(), theme)
                .map(gpui::IntoElement::into_any_element)
        }
        LadderItemIndex::Bid(row_index) => frame.rows.get(row_index)?.bid.as_ref().map(|level| {
            render_level_row(
                frame,
                level,
                BookColumnSide::Bid,
                columns,
                theme,
                RowRenderStats::raw(maximum_quantity, maximum_trade_quantity),
            )
            .into_any_element()
        }),
    }
}

fn ladder_visible_max_quantity(
    frame: &OrderBookFrame,
    layout: LadderLayout,
    range: std::ops::Range<usize>,
) -> i64 {
    range
        .filter_map(|index| match ladder_item_index(layout, index)? {
            LadderItemIndex::Ask(row_index) => frame.rows.get(row_index)?.ask.as_ref(),
            LadderItemIndex::Bid(row_index) => frame.rows.get(row_index)?.bid.as_ref(),
            LadderItemIndex::Spread => None,
        })
        .map(|level| level.quantity)
        .max()
        .unwrap_or(0)
}

fn ladder_visible_max_trade_quantity(
    frame: &OrderBookFrame,
    layout: LadderLayout,
    range: std::ops::Range<usize>,
) -> i64 {
    range
        .filter_map(|index| match ladder_item_index(layout, index)? {
            LadderItemIndex::Ask(row_index) => frame.rows.get(row_index)?.ask.as_ref(),
            LadderItemIndex::Bid(row_index) => frame.rows.get(row_index)?.bid.as_ref(),
            LadderItemIndex::Spread => None,
        })
        .filter_map(|level| frame.traded_volumes.get(&level.price).copied())
        .map(AggressorTradeVolumes::maximum_side)
        .max()
        .unwrap_or(0)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BookColumnSide {
    Bid,
    Ask,
}

#[derive(Clone, Copy)]
enum CellAlignment {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy)]
struct LevelCellContext<'a> {
    columns: OrderBookColumnVisibility,
    price_color: ThemeColor,
    theme: &'a AxiusflowTheme,
    maximum_quantity: i64,
    maximum_trade_quantity: i64,
    trade_volumes: AggressorTradeVolumes,
    quantity_scale: u8,
}

#[derive(Clone, Copy)]
struct RowRenderStats {
    maximum_quantity: i64,
    maximum_trade_quantity: i64,
    trade_volumes: Option<AggressorTradeVolumes>,
}

impl RowRenderStats {
    const fn raw(maximum_quantity: i64, maximum_trade_quantity: i64) -> Self {
        Self {
            maximum_quantity,
            maximum_trade_quantity,
            trade_volumes: None,
        }
    }

    const fn grouped(
        maximum_quantity: i64,
        maximum_trade_quantity: i64,
        trade_volumes: AggressorTradeVolumes,
    ) -> Self {
        Self {
            maximum_quantity,
            maximum_trade_quantity,
            trade_volumes: Some(trade_volumes),
        }
    }
}

fn render_level_row(
    frame: &OrderBookFrame,
    level: &OrderBookColumnLevel,
    side: BookColumnSide,
    columns: OrderBookColumnVisibility,
    theme: &AxiusflowTheme,
    stats: RowRenderStats,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let (price_color, row_id) = match side {
        BookColumnSide::Bid => (colors.primary, "order_book_bid_row"),
        BookColumnSide::Ask => (colors.danger, "order_book_ask_row"),
    };
    let trade_volumes = stats.trade_volumes.unwrap_or_else(|| {
        frame
            .traded_volumes
            .get(&level.price)
            .copied()
            .unwrap_or_default()
    });
    let context = LevelCellContext {
        columns,
        price_color,
        theme,
        maximum_quantity: stats.maximum_quantity,
        maximum_trade_quantity: stats.maximum_trade_quantity,
        trade_volumes,
        quantity_scale: frame.quantity_scale,
    };
    div()
        .id((row_id, u64::try_from(level.price).unwrap_or(0)))
        .w_full()
        .h(px(ROW_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .overflow_hidden()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .font_family(platform_font_family())
        .font_weight(platform_font_weight(TypographyRole::Normal))
        .font_features(platform_tabular_numerals())
        .text_size(px(TEXT_SIZE))
        .children(
            OrderBookColumn::ALL
                .into_iter()
                .filter(|column| columns.is_visible(*column))
                .map(|column| render_level_cell(column, level, side, context)),
        )
}

/// A price-grid tick with no corresponding provider depth. Resting quantity and
/// order-count stay blank, while real aggressor trades at this exact tick remain
/// visible. This is presentation geometry, never a zero-quantity level inserted
/// into the canonical book.
fn render_empty_price_tick(
    frame: &OrderBookFrame,
    price: i64,
    price_scale: u8,
    side: BookColumnSide,
    columns: OrderBookColumnVisibility,
    theme: &AxiusflowTheme,
    stats: RowRenderStats,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let (price_color, row_id) = match side {
        BookColumnSide::Bid => (colors.primary, "order_book_bid_price_tick"),
        BookColumnSide::Ask => (colors.danger, "order_book_ask_price_tick"),
    };
    let trade_volumes = stats.trade_volumes.unwrap_or_default();
    let quantity_scale = frame.quantity_scale;
    div()
        .id((row_id, u64::try_from(price).unwrap_or(0)))
        .w_full()
        .h(px(ROW_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .overflow_hidden()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .font_family(platform_font_family())
        .font_weight(platform_font_weight(TypographyRole::Normal))
        .font_features(platform_tabular_numerals())
        .text_size(px(TEXT_SIZE))
        .children(
            OrderBookColumn::ALL
                .into_iter()
                .filter(|column| columns.is_visible(*column))
                .map(|column| {
                    let width = columns.width(column);
                    match column {
                        OrderBookColumn::Price => table_cell(width)
                            .px_1()
                            .text_center()
                            .text_color(gpui_color(price_color))
                            .child(grouped_fixed_point_text(price, price_scale))
                            .into_any_element(),
                        OrderBookColumn::SellTrades => trade_volume_cell(
                            width,
                            trade_volumes.sell,
                            quantity_scale,
                            theme.colors.danger,
                            true,
                            stats.maximum_trade_quantity,
                        )
                        .into_any_element(),
                        OrderBookColumn::BuyTrades => trade_volume_cell(
                            width,
                            trade_volumes.buy,
                            quantity_scale,
                            theme.colors.primary,
                            false,
                            stats.maximum_trade_quantity,
                        )
                        .into_any_element(),
                        _ => table_cell(width).into_any_element(),
                    }
                }),
        )
}

fn price_grid_center_row(frame: &OrderBookFrame, theme: &AxiusflowTheme) -> AnyElement {
    if let Some(row) = spread_row(display_best_bid(frame), display_best_ask(frame), theme) {
        return row.into_any_element();
    }
    div()
        .w_full()
        .h(px(ROW_HEIGHT))
        .flex_none()
        .border_b_1()
        .border_color(gpui_color(theme.colors.border))
        .bg(gpui_color(theme.colors.surface_secondary))
        .into_any_element()
}

fn render_level_cell(
    column: OrderBookColumn,
    level: &OrderBookColumnLevel,
    side: BookColumnSide,
    context: LevelCellContext<'_>,
) -> gpui::AnyElement {
    let width = context.columns.width(column);
    match column {
        OrderBookColumn::ProfitLoss => table_cell(width).into_any_element(),
        OrderBookColumn::Bid => quantity_cell(
            width,
            (side == BookColumnSide::Bid).then_some(level),
            context.theme.colors.primary,
            true,
            context.maximum_quantity,
        )
        .into_any_element(),
        OrderBookColumn::SellTrades => trade_volume_cell(
            width,
            context.trade_volumes.sell,
            context.quantity_scale,
            context.theme.colors.danger,
            true,
            context.maximum_trade_quantity,
        )
        .into_any_element(),
        OrderBookColumn::Price => table_cell(width)
            .px_1()
            .text_center()
            .text_color(gpui_color(context.price_color))
            .child(level.price_text.clone())
            .into_any_element(),
        OrderBookColumn::BuyTrades => trade_volume_cell(
            width,
            context.trade_volumes.buy,
            context.quantity_scale,
            context.theme.colors.primary,
            false,
            context.maximum_trade_quantity,
        )
        .into_any_element(),
        OrderBookColumn::Ask => quantity_cell(
            width,
            (side == BookColumnSide::Ask).then_some(level),
            context.theme.colors.danger,
            false,
            context.maximum_quantity,
        )
        .into_any_element(),
        OrderBookColumn::Orders => table_cell(width)
            .px_1()
            .text_right()
            .text_color(gpui_color(context.theme.colors.text_secondary))
            .child(
                level
                    .order_count
                    .map_or_else(String::new, |count| count.to_string()),
            )
            .into_any_element(),
    }
}

fn table_cell(width: f32) -> Div {
    div()
        .w(relative(width))
        .h_full()
        .flex()
        .items_center()
        .flex_none()
        .overflow_hidden()
        .whitespace_nowrap()
}

fn header_cell(width: f32, label: String, alignment: CellAlignment) -> impl IntoElement {
    let cell = table_cell(width)
        .px_1()
        .child(div().w_full().truncate().child(label));
    match alignment {
        CellAlignment::Left => cell.text_left(),
        CellAlignment::Center => cell.text_center(),
        CellAlignment::Right => cell.text_right(),
    }
}

const fn column_alignment(column: OrderBookColumn) -> CellAlignment {
    match column {
        OrderBookColumn::Ask | OrderBookColumn::BuyTrades => CellAlignment::Left,
        OrderBookColumn::Price => CellAlignment::Center,
        OrderBookColumn::ProfitLoss
        | OrderBookColumn::Bid
        | OrderBookColumn::Orders
        | OrderBookColumn::SellTrades => CellAlignment::Right,
    }
}

fn trade_volume_cell(
    column_width: f32,
    quantity: i64,
    quantity_scale: u8,
    color: ThemeColor,
    align_right: bool,
    maximum_trade_quantity: i64,
) -> impl IntoElement + use<> {
    let width = visible_quantity_width(quantity, maximum_trade_quantity);
    let bar = div()
        .absolute()
        .top_0()
        .bottom_0()
        .w(relative(width))
        .bg(gpui_color(color.with_alpha(0.3)));
    let text = if quantity > 0 {
        compact_quantity_text(quantity, quantity_scale)
    } else {
        String::new()
    };
    let cell = table_cell(column_width)
        .relative()
        .px_1()
        .children((quantity > 0).then(|| {
            if align_right {
                bar.right_0().into_any_element()
            } else {
                bar.left_0().into_any_element()
            }
        }))
        .child(div().relative().w_full().truncate().child(text));
    if align_right { cell.text_right() } else { cell }
}

fn quantity_cell(
    column_width: f32,
    level: Option<&OrderBookColumnLevel>,
    color: ThemeColor,
    align_right: bool,
    maximum_quantity: i64,
) -> impl IntoElement + use<> {
    let width = level.map_or(0.0, |level| {
        visible_quantity_width(level.quantity, maximum_quantity)
    });
    let bar = div()
        .absolute()
        .top_0()
        .bottom_0()
        .w(relative(width))
        .bg(gpui_color(color.with_alpha(0.2)));
    let cell = table_cell(column_width)
        .relative()
        .px_1()
        .children(level.map(|_| {
            if align_right {
                bar.right_0().into_any_element()
            } else {
                bar.left_0().into_any_element()
            }
        }))
        .child(
            div()
                .relative()
                .w_full()
                .truncate()
                .child(level.map_or_else(String::new, |level| level.quantity_text.clone())),
        );
    if align_right { cell.text_right() } else { cell }
}

fn visible_quantity_width(quantity: i64, maximum_quantity: i64) -> f32 {
    if quantity <= 0 || maximum_quantity <= 0 {
        return 0.0;
    }
    let quantity = u128::try_from(quantity).unwrap_or_default();
    let maximum = u128::try_from(maximum_quantity).unwrap_or(1);
    let basis_points = quantity
        .saturating_mul(10_000)
        .checked_div(maximum)
        .unwrap_or_default()
        .min(10_000);
    let basis_points = u16::try_from(basis_points).unwrap_or(10_000);
    f32::from(basis_points) / 10_000.0
}

fn column_rails(
    columns: OrderBookColumnVisibility,
    theme: &AxiusflowTheme,
) -> impl Iterator<Item = gpui::AnyElement> + use<> {
    let border = gpui_color(theme.colors.border);
    let mut edge = 0.0;
    OrderBookColumn::ALL
        .into_iter()
        .filter(move |column| columns.is_visible(*column))
        .filter_map(move |column| {
            edge += columns.width(column);
            (edge < 0.999).then_some(edge)
        })
        .enumerate()
        .map(move |(index, edge)| {
            div()
                .id(("order_book_column_rail", index))
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(edge))
                .w(px(1.0))
                .bg(border)
                .into_any_element()
        })
}

fn spread_row(
    best_bid: Option<&OrderBookColumnLevel>,
    best_ask: Option<&OrderBookColumnLevel>,
    theme: &AxiusflowTheme,
) -> Option<impl IntoElement + use<>> {
    let bid = best_bid?;
    let ask = best_ask?;
    Some(
        div()
            .w_full()
            .h(px(ROW_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .border_b_1()
            .border_color(gpui_color(theme.colors.border))
            .bg(gpui_color(theme.colors.surface_secondary))
            .font_family(platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .font_features(platform_tabular_numerals())
            .text_size(px(TEXT_SIZE))
            .text_color(gpui_color(theme.colors.text_secondary))
            .child(format!("{}  —  {}", bid.price_text, ask.price_text)),
    )
}

fn status_banner(
    state: OrderBookState,
    watermark: u64,
    theme: &AxiusflowTheme,
) -> Option<impl IntoElement + use<>> {
    let (label, color) = status_presentation(state, watermark)?;
    Some(
        div()
            .h(px(ROW_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .px_2()
            .border_b_1()
            .border_color(gpui_color(theme.colors.border))
            .bg(gpui_color(color(theme).with_alpha(0.12)))
            .text_size(px(TEXT_SIZE))
            .text_color(gpui_color(color(theme)))
            .child(label),
    )
}

type StatusColor = fn(&AxiusflowTheme) -> ThemeColor;

fn status_presentation(state: OrderBookState, watermark: u64) -> Option<(String, StatusColor)> {
    match state {
        OrderBookState::Ready => None,
        OrderBookState::Stale => Some((
            format!("Order Book stale · last sequence {watermark}"),
            |theme| theme.colors.danger,
        )),
        // Awaiting the first snapshot is still loading, not a failure, so
        // it renders neutral. Red is reserved for a book that broke.
        OrderBookState::Recovering(OrderBookRecoveryReason::AwaitingSnapshot) => Some((
            format!(
                "Order Book recovering · {}",
                recovery_label(OrderBookRecoveryReason::AwaitingSnapshot)
            ),
            |theme| theme.colors.text_secondary,
        )),
        OrderBookState::Recovering(reason) => Some((
            format!("Order Book recovering · {}", recovery_label(reason)),
            |theme| theme.colors.danger,
        )),
    }
}

const fn recovery_label(reason: OrderBookRecoveryReason) -> &'static str {
    match reason {
        OrderBookRecoveryReason::AwaitingSnapshot => "awaiting snapshot",
        OrderBookRecoveryReason::SequenceGap => "sequence gap",
        OrderBookRecoveryReason::CrossedBook => "crossed book",
        OrderBookRecoveryReason::InvalidUpdate => "invalid update",
    }
}

fn gpui_color(color: ThemeColor) -> Hsla {
    let (h, s, l, a) = color.hsla_components();
    Hsla { h, s, l, a }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        selection_generation: u64,
        session_generation: u64,
        revision: u64,
        state: OrderBookState,
        has_rows: bool,
    ) -> OrderBookFrame {
        OrderBookFrame {
            provider_id: "rithmic".into(),
            instrument_id: "BTC-USD".into(),
            entitlement_id: "public".into(),
            session_generation,
            selection_generation,
            revision,
            source_watermark: revision,
            bbo_source_watermark: revision,
            state,
            price_scale: 2,
            quantity_scale: 0,
            price_increment: None,
            best_bid: None,
            best_ask: None,
            traded_volumes: std::collections::BTreeMap::default(),
            trade_source_watermark: revision,
            rows: has_rows
                .then_some(OrderBookRow {
                    bid: None,
                    ask: None,
                })
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn retiring_a_book_discards_the_displayed_frame() {
        let mut view = ReadOnlyOrderBookView::new(AxiusflowTheme::dark());
        view.frame = Some(Arc::new(frame(1, 8, 40, OrderBookState::Ready, true)));
        assert!(view.discard_frames());
        assert!(view.frame.is_none());
        assert!(!view.discard_frames());
    }

    #[test]
    fn empty_book_reports_loading_until_a_concrete_failure() {
        assert_eq!(empty_book_copy(false, false), "Loading order book…");
        assert_eq!(
            empty_book_copy(false, true),
            "Waiting for order-book snapshot"
        );
        assert_eq!(empty_book_copy(true, false), "Order Book unavailable");
        assert_eq!(empty_book_copy(true, true), "Order Book unavailable");
    }

    #[test]
    fn routing_columns_start_hidden_and_cannot_be_enabled() {
        let mut columns = OrderBookColumnVisibility::default();
        assert!(!columns.is_visible(OrderBookColumn::ProfitLoss));
        assert!(!columns.toggle(OrderBookColumn::ProfitLoss));
        assert!(!columns.is_visible(OrderBookColumn::ProfitLoss));
    }

    #[test]
    fn available_columns_toggle_and_renormalize_widths() {
        let mut columns = OrderBookColumnVisibility::default();
        assert_eq!(
            OrderBookColumn::ALL
                .into_iter()
                .filter(|column| columns.is_visible(*column))
                .collect::<Vec<_>>(),
            vec![
                OrderBookColumn::Bid,
                OrderBookColumn::SellTrades,
                OrderBookColumn::Price,
                OrderBookColumn::BuyTrades,
                OrderBookColumn::Ask,
            ]
        );
        assert!(columns.toggle(OrderBookColumn::Orders));
        assert!(columns.is_visible(OrderBookColumn::Orders));
        let width = OrderBookColumn::ALL
            .into_iter()
            .filter(|column| columns.is_visible(*column))
            .map(|column| columns.width(column))
            .sum::<f32>();
        assert!((width - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn ready_state_has_no_banner() {
        assert!(status_presentation(OrderBookState::Ready, 12).is_none());
    }

    #[test]
    fn provider_connectivity_has_stable_actionable_book_feedback() {
        let theme = AxiusflowTheme::default();
        assert!(
            connection_status_banner(OrderBookConnectionState::Online, false, false, &theme)
                .is_none()
        );
        assert!(
            connection_status_banner(OrderBookConnectionState::Offline, false, false, &theme)
                .is_some()
        );
        assert!(
            connection_status_banner(OrderBookConnectionState::Recovering, true, false, &theme)
                .is_some()
        );
        assert!(
            connection_status_banner(OrderBookConnectionState::Recovering, false, true, &theme)
                .is_some()
        );
    }

    #[test]
    fn first_snapshot_recovery_is_presented_as_loading_not_reconnecting() {
        let theme = AxiusflowTheme::default();
        assert!(
            connection_status_banner(OrderBookConnectionState::Recovering, false, false, &theme)
                .is_none()
        );
        assert_eq!(empty_book_copy(false, false), "Loading order book…");
    }

    #[test]
    fn stale_and_recovery_states_are_explicit() {
        let stale = status_presentation(OrderBookState::Stale, 42)
            .map(|value| value.0)
            .expect("stale banner");
        assert_eq!(stale, "Order Book stale · last sequence 42");
        for (reason, expected) in [
            (
                OrderBookRecoveryReason::AwaitingSnapshot,
                "awaiting snapshot",
            ),
            (OrderBookRecoveryReason::SequenceGap, "sequence gap"),
            (OrderBookRecoveryReason::CrossedBook, "crossed book"),
            (OrderBookRecoveryReason::InvalidUpdate, "invalid update"),
        ] {
            let label = status_presentation(OrderBookState::Recovering(reason), 0)
                .map(|value| value.0)
                .expect("recovery banner");
            assert_eq!(label, format!("Order Book recovering · {expected}"));
        }
        // Awaiting the first snapshot is loading, not failure: it must not
        // share the failure color used by stale and broken books.
        let theme = AxiusflowTheme::default();
        let awaiting = status_presentation(
            OrderBookState::Recovering(OrderBookRecoveryReason::AwaitingSnapshot),
            0,
        )
        .expect("awaiting banner");
        let stale = status_presentation(OrderBookState::Stale, 0).expect("stale banner");
        let gap = status_presentation(
            OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap),
            0,
        )
        .expect("gap banner");
        assert_ne!(awaiting.1(&theme), stale.1(&theme));
        assert_ne!(awaiting.1(&theme), gap.1(&theme));
        assert_eq!(stale.1(&theme), gap.1(&theme));
    }

    #[test]
    fn frame_ordering_resets_revision_only_for_a_new_session() {
        let current = frame(2, 4, 10, OrderBookState::Ready, true);
        assert!(frame_precedes(
            &frame(2, 4, 9, OrderBookState::Ready, true),
            &current
        ));
        assert!(!frame_precedes(
            &frame(1, 5, 1, OrderBookState::Ready, true),
            &current
        ));
        assert!(frame_precedes(
            &frame(9, 3, 99, OrderBookState::Ready, true),
            &current
        ));

        let mut stale_trade_frame = current.clone();
        stale_trade_frame.trade_source_watermark = current.trade_source_watermark.saturating_sub(1);
        assert!(frame_precedes(&stale_trade_frame, &current));
    }

    #[test]
    fn trade_only_grid_keeps_scroll_position_when_depth_arrives() {
        let mut trade_only = price_grid_frame();
        trade_only.best_bid = None;
        trade_only.best_ask = None;
        trade_only.rows.clear();
        assert!(ladder_recenter_index(&trade_only).is_some());

        let depth = price_grid_frame();
        assert!(!should_recenter_ladder(Some(&trade_only), &depth));
        assert!(should_recenter_ladder(None, &depth));

        let mut replacement = depth.clone();
        replacement.selection_generation += 1;
        assert!(should_recenter_ladder(Some(&depth), &replacement));
    }

    #[test]
    fn real_level_to_price_grid_transition_recenters_on_the_spread() {
        let mut real_only = price_grid_frame();
        real_only.price_increment = None;
        real_only.best_ask = None;
        real_only.traded_volumes.clear();
        real_only.rows.truncate(1);
        real_only.rows[0].ask = None;
        assert!(price_grid_layout(&real_only).is_none());
        assert!(ladder_recenter_index(&real_only).is_some());

        let mut grid = price_grid_frame();
        grid.price_increment = None;
        assert!(price_grid_layout(&grid).is_some());
        assert!(should_recenter_ladder(Some(&real_only), &grid));

        let mut different_tick = grid.clone();
        different_tick.price_increment = Some(25);
        assert!(should_recenter_ladder(Some(&grid), &different_tick));

        let mut wider_ask_runway = different_tick.clone();
        wider_ask_runway.rows.push(OrderBookRow {
            bid: None,
            ask: Some(grid_level(122_425, 1)),
        });
        assert!(should_recenter_ladder(
            Some(&different_tick),
            &wider_ask_runway
        ));

        let mut moved_market = different_tick.clone();
        moved_market.best_bid = Some(grid_level(20_025, 5));
        moved_market.best_ask = Some(grid_level(20_125, 7));
        moved_market.rows[0].bid = Some(grid_level(20_025, 5));
        moved_market.rows[1].bid = Some(grid_level(19_975, 3));
        moved_market.rows[0].ask = Some(grid_level(20_125, 7));
        moved_market.rows[1].ask = Some(grid_level(20_175, 2));
        assert!(!should_recenter_ladder(
            Some(&different_tick),
            &moved_market
        ));
    }

    #[test]
    fn divergent_independent_bbo_does_not_collapse_the_canonical_depth_grid() {
        let mut frame = price_grid_frame();
        frame.best_bid = Some(grid_level(19_000, 8));
        frame.best_ask = Some(grid_level(19_025, 9));

        let grid = price_grid_layout(&frame).expect("canonical depth keeps a continuous grid");

        assert_eq!(grid.best_bid, 20_000);
        assert_eq!(
            display_best_bid(&frame).map(|level| level.price),
            Some(20_000)
        );
        assert_eq!(
            display_best_ask(&frame).map(|level| level.price),
            Some(20_100)
        );
        assert_eq!(
            price_grid_item(grid, grid.recenter_index() - 1),
            Some(PriceGridItem::Ask(20_025))
        );
        assert_eq!(
            price_grid_item(grid, grid.recenter_index() + 1),
            Some(PriceGridItem::Bid(20_000))
        );
    }

    #[test]
    fn btc_like_grid_places_the_spread_at_the_center_not_the_top_ask_runway() {
        let mut frame = price_grid_frame();
        frame.price_increment = Some(1);
        frame.best_bid = Some(grid_level(77_110, 5));
        frame.best_ask = Some(grid_level(77_111, 7));
        frame.rows = vec![
            OrderBookRow {
                bid: Some(grid_level(77_110, 5)),
                ask: Some(grid_level(77_111, 7)),
            },
            OrderBookRow {
                bid: Some(grid_level(77_109, 3)),
                ask: Some(grid_level(77_112, 2)),
            },
        ];
        frame.traded_volumes.clear();

        let grid = price_grid_layout(&frame).expect("BTC-like grid");
        assert_eq!(grid.recenter_index(), 4_096);
        assert_eq!(price_grid_item(grid, 0), Some(PriceGridItem::Ask(81_206)));
        assert_eq!(
            price_grid_item(grid, grid.recenter_index() - 1),
            Some(PriceGridItem::Ask(77_111))
        );
        assert_eq!(
            price_grid_item(grid, grid.recenter_index()),
            Some(PriceGridItem::Spread)
        );
        assert_eq!(
            price_grid_item(grid, grid.recenter_index() + 1),
            Some(PriceGridItem::Bid(77_110))
        );
    }

    #[test]
    fn hyperliquid_display_uses_reference_ten_tick_rows() {
        let mut frame = price_grid_frame();
        frame.provider_id = "hyperliquid".into();
        frame.price_scale = 0;
        frame.price_increment = Some(1);
        frame.best_bid = Some(grid_level(77_110, 5));
        frame.best_ask = Some(grid_level(77_111, 7));
        frame.rows = vec![
            OrderBookRow {
                bid: Some(grid_level(77_110, 5)),
                ask: Some(grid_level(77_111, 7)),
            },
            OrderBookRow {
                bid: Some(grid_level(77_099, 3)),
                ask: Some(grid_level(77_135, 2)),
            },
        ];
        frame.traded_volumes = std::collections::BTreeMap::from([
            (77_110, AggressorTradeVolumes { buy: 3, sell: 5 }),
            (77_149, AggressorTradeVolumes { buy: 0, sell: 6 }),
            (77_150, AggressorTradeVolumes { buy: 4, sell: 0 }),
        ]);

        let grid = price_grid_layout(&frame).expect("Hyperliquid display grid");
        let center = grid.recenter_index();
        assert_eq!(grid.tick, 10);
        assert_eq!(
            price_grid_item(grid, center - 1),
            Some(PriceGridItem::Ask(77_120))
        );
        assert_eq!(
            price_grid_item(grid, center + 1),
            Some(PriceGridItem::Bid(77_110))
        );
        assert_eq!(
            grouped_level_at_price(&frame, BookColumnSide::Ask, 77_120, grid.tick)
                .map(|level| level.quantity),
            Some(7)
        );
        assert_eq!(
            grouped_level_at_price(&frame, BookColumnSide::Bid, 77_110, grid.tick)
                .map(|level| level.quantity),
            Some(5)
        );
        assert_eq!(
            grouped_trade_volumes_at_price(&frame, 77_150, grid.tick),
            AggressorTradeVolumes { buy: 4, sell: 0 }
        );
        assert_eq!(
            grouped_trade_volumes_at_price(&frame, 77_110, grid.tick),
            AggressorTradeVolumes { buy: 3, sell: 5 }
        );
        assert_eq!(
            frame.rows.len(),
            2,
            "display grouping must not mutate canonical rows"
        );
    }

    #[test]
    fn hyperliquid_display_tick_stays_stable_when_snapshot_level_gaps_widen() {
        let mut frame = price_grid_frame();
        frame.provider_id = "hyperliquid".into();
        frame.price_scale = 0;
        frame.price_increment = None;
        frame.best_bid = Some(grid_level(77_110, 5));
        frame.best_ask = Some(grid_level(77_120, 7));
        frame.rows = vec![
            OrderBookRow {
                bid: Some(grid_level(77_100, 5)),
                ask: Some(grid_level(77_200, 7)),
            },
            OrderBookRow {
                bid: Some(grid_level(77_000, 3)),
                ask: Some(grid_level(77_300, 2)),
            },
        ];
        frame.traded_volumes.clear();

        assert_eq!(observed_price_increment(&frame), Some(10));
        let grid = price_grid_layout(&frame).expect("aggregated provider book keeps display grid");
        assert_eq!(grid.tick, 10);
        let center = grid.recenter_index();
        assert_eq!(
            price_grid_item(grid, center - 1),
            Some(PriceGridItem::Ask(77_110))
        );
        assert_eq!(
            price_grid_item(grid, center + 1),
            Some(PriceGridItem::Bid(77_100))
        );
    }

    #[test]
    fn deep_ladder_indexes_every_real_level_once_around_the_spread() {
        let layout = LadderLayout {
            ask_count: 4_096,
            bid_count: 4_096,
            spread: true,
        };
        assert_eq!(layout.item_count(), 8_193);
        assert_eq!(layout.recenter_index(), Some(4_096));
        assert_eq!(
            ladder_item_index(layout, 0),
            Some(LadderItemIndex::Ask(4_095))
        );
        assert_eq!(
            ladder_item_index(layout, 4_095),
            Some(LadderItemIndex::Ask(0))
        );
        assert_eq!(
            ladder_item_index(layout, 4_096),
            Some(LadderItemIndex::Spread)
        );
        assert_eq!(
            ladder_item_index(layout, 4_097),
            Some(LadderItemIndex::Bid(0))
        );
        assert_eq!(
            ladder_item_index(layout, 8_192),
            Some(LadderItemIndex::Bid(4_095))
        );
        assert_eq!(ladder_item_index(layout, 8_193), None);
    }

    #[test]
    fn one_sided_ladder_recenters_on_the_best_real_level_without_inventing_rows() {
        let asks = LadderLayout {
            ask_count: 5,
            bid_count: 0,
            spread: false,
        };
        assert_eq!(asks.item_count(), 5);
        assert_eq!(asks.recenter_index(), Some(4));
        assert_eq!(ladder_item_index(asks, 4), Some(LadderItemIndex::Ask(0)));

        let bids = LadderLayout {
            ask_count: 0,
            bid_count: 5,
            spread: false,
        };
        assert_eq!(bids.item_count(), 5);
        assert_eq!(bids.recenter_index(), Some(0));
        assert_eq!(ladder_item_index(bids, 4), Some(LadderItemIndex::Bid(4)));
    }

    fn grid_level(price: i64, quantity: i64) -> OrderBookColumnLevel {
        OrderBookColumnLevel {
            price,
            quantity,
            order_count: Some(1),
            price_text: grouped_fixed_point_text(price, 2),
            quantity_text: quantity.to_string(),
            traded_volume: 0,
            traded_volume_text: String::new(),
            relative_size_bps: 10_000,
        }
    }

    fn price_grid_frame() -> OrderBookFrame {
        OrderBookFrame {
            provider_id: "rithmic".into(),
            instrument_id: "MNQ".into(),
            entitlement_id: "test".into(),
            session_generation: 1,
            selection_generation: 1,
            revision: 1,
            source_watermark: 1,
            bbo_source_watermark: 1,
            state: OrderBookState::Ready,
            price_scale: 2,
            quantity_scale: 0,
            price_increment: Some(25),
            best_bid: Some(grid_level(20_000, 5)),
            best_ask: Some(grid_level(20_100, 7)),
            traded_volumes: std::collections::BTreeMap::from([(
                20_025,
                AggressorTradeVolumes { buy: 8, sell: 3 },
            )]),
            trade_source_watermark: 2,
            rows: vec![
                OrderBookRow {
                    bid: Some(grid_level(20_000, 5)),
                    ask: Some(grid_level(20_100, 7)),
                },
                OrderBookRow {
                    bid: Some(grid_level(19_950, 3)),
                    ask: Some(grid_level(20_150, 2)),
                },
            ],
        }
    }

    #[test]
    fn authoritative_tick_builds_blank_price_rows_without_synthesizing_depth() {
        let frame = price_grid_frame();
        let grid = price_grid_layout(&frame).expect("Rithmic tick builds grid");
        assert_eq!(grid.recenter_index(), MINIMUM_PRICE_GRID_ROWS_PER_SIDE);
        assert_eq!(
            price_grid_item(grid, grid.recenter_index()),
            Some(PriceGridItem::Spread)
        );
        assert_eq!(
            price_grid_item(grid, grid.recenter_index() - 1),
            Some(PriceGridItem::Ask(20_025))
        );
        assert!(real_level_at_price(&frame, BookColumnSide::Ask, 20_025).is_none());
        assert_eq!(
            frame.traded_volumes.get(&20_025).copied(),
            Some(AggressorTradeVolumes { buy: 8, sell: 3 })
        );
        assert_eq!(
            price_grid_visible_max_trade_quantity(
                &frame,
                grid,
                (grid.recenter_index() - 1)..grid.recenter_index()
            ),
            8
        );
        assert_eq!(
            price_grid_item(grid, grid.recenter_index() - 4),
            Some(PriceGridItem::Ask(20_100))
        );
        assert_eq!(
            real_level_at_price(&frame, BookColumnSide::Ask, 20_100).map(|level| level.quantity),
            Some(7)
        );
        assert_eq!(
            price_grid_item(grid, grid.recenter_index() + 1),
            Some(PriceGridItem::Bid(20_000))
        );
        assert_eq!(
            price_grid_item(grid, grid.recenter_index() + 3),
            Some(PriceGridItem::Bid(19_950))
        );
    }

    #[test]
    fn authoritative_grid_survives_one_sided_depth_and_trade_only_recovery() {
        let mut ask_only = price_grid_frame();
        ask_only.best_bid = None;
        for row in &mut ask_only.rows {
            row.bid = None;
        }
        let grid = price_grid_layout(&ask_only).expect("ask-only depth keeps the provider grid");
        let center = grid.recenter_index();
        assert_eq!(price_grid_item(grid, center), Some(PriceGridItem::Spread));
        assert_eq!(
            price_grid_item(grid, center - 1),
            Some(PriceGridItem::Ask(20_100))
        );
        assert_eq!(
            price_grid_item(grid, center + 1),
            Some(PriceGridItem::Bid(20_075))
        );
        assert!(real_level_at_price(&ask_only, BookColumnSide::Bid, 20_075).is_none());

        let mut trade_only = price_grid_frame();
        trade_only.best_bid = None;
        trade_only.best_ask = None;
        trade_only.rows.clear();
        trade_only.traded_volumes = std::collections::BTreeMap::from([
            (20_025, AggressorTradeVolumes { buy: 0, sell: 5 }),
            (20_075, AggressorTradeVolumes { buy: 8, sell: 0 }),
        ]);
        let grid =
            price_grid_layout(&trade_only).expect("retained trades anchor the provider grid");
        let center = grid.recenter_index();
        assert_eq!(
            price_grid_item(grid, center - 1),
            Some(PriceGridItem::Ask(20_075))
        );
        assert_eq!(
            price_grid_item(grid, center + 2),
            Some(PriceGridItem::Bid(20_025))
        );
        assert_eq!(
            price_grid_visible_max_trade_quantity(&trade_only, grid, center - 1..center + 3),
            8
        );
        assert!(real_level_at_price(&trade_only, BookColumnSide::Ask, 20_075).is_none());
        assert!(real_level_at_price(&trade_only, BookColumnSide::Bid, 20_025).is_none());
        assert_eq!(
            trade_only.traded_volumes.get(&20_075).copied(),
            Some(AggressorTradeVolumes { buy: 8, sell: 0 })
        );
        assert_eq!(
            trade_only.traded_volumes.get(&20_025).copied(),
            Some(AggressorTradeVolumes { buy: 0, sell: 5 })
        );
    }

    #[test]
    fn missing_tick_metadata_uses_real_depth_lattice_for_presentation() {
        let mut frame = price_grid_frame();
        frame.price_increment = None;
        let grid = price_grid_layout(&frame).expect("real depth infers a presentation lattice");
        assert_eq!(grid.tick, 50);
        assert_eq!(frame.rows.len(), 2);

        // Retained trades are presentation annotations, not the authority for
        // rebuilding the depth lattice. An old off-lattice print must not force
        // a full retained-trade scan or collapse a healthy depth grid.
        frame
            .traded_volumes
            .insert(20_013, AggressorTradeVolumes { buy: 1, sell: 0 });
        assert_eq!(
            price_grid_layout(&frame)
                .expect("depth lattice survives retained trade noise")
                .tick,
            50
        );
    }

    #[test]
    fn insufficient_tick_evidence_falls_back_but_coarse_display_accepts_raw_levels() {
        let mut frame = price_grid_frame();
        frame.price_increment = None;
        frame.rows.truncate(1);
        frame.best_bid = None;
        frame.best_ask = None;
        frame.rows[0].ask = None;
        frame.traded_volumes.clear();
        assert!(price_grid_layout(&frame).is_none());

        let mut frame = price_grid_frame();
        frame.price_increment = Some(25);
        frame.rows[1].bid.as_mut().expect("bid").price = 19_949;
        let grid = price_grid_layout(&frame).expect("raw levels are grouped onto display ticks");
        assert_eq!(grid.tick, 25);
        assert_eq!(
            grouped_level_at_price(&frame, BookColumnSide::Bid, 19_925, grid.tick)
                .map(|level| level.quantity),
            Some(3)
        );
    }

    #[test]
    fn sparse_real_level_lookup_handles_descending_bids_and_ascending_asks() {
        let mut frame = price_grid_frame();
        frame.rows[1].ask.as_mut().expect("ask").quantity = 99;
        assert_eq!(
            real_level_at_price(&frame, BookColumnSide::Bid, 19_950).map(|level| level.quantity),
            Some(3)
        );
        assert_eq!(
            real_level_at_price(&frame, BookColumnSide::Ask, 20_150).map(|level| level.quantity),
            Some(99)
        );
        assert!(real_level_at_price(&frame, BookColumnSide::Bid, 19_975).is_none());
        assert!(real_level_at_price(&frame, BookColumnSide::Ask, 20_025).is_none());

        let grid = price_grid_layout(&frame).expect("grid");
        let center = grid.recenter_index();
        // The 99-lot ask at 201.50 is one row outside this visible slice, so
        // the visible 7-lot ask scales to full width like Flowsurface.
        assert_eq!(
            price_grid_visible_max_quantity(&frame, grid, center - 5..center + 2),
            7
        );
        assert!((visible_quantity_width(7, 7) - 1.0).abs() < f32::EPSILON);
        assert!((visible_quantity_width(3, 7) - (3.0 / 7.0)).abs() < 0.0001);
    }

    #[test]
    fn price_grid_runway_stops_at_the_positive_price_boundary() {
        let mut frame = price_grid_frame();
        frame.best_bid = Some(grid_level(50, 5));
        frame.best_ask = Some(grid_level(75, 7));
        frame.rows = vec![OrderBookRow {
            bid: Some(grid_level(50, 5)),
            ask: Some(grid_level(75, 7)),
        }];
        let grid = price_grid_layout(&frame).expect("low price still supports a grid");
        assert_eq!(grid.bid_rows, 2);
        assert_eq!(
            price_grid_item(grid, grid.recenter_index() + 1),
            Some(PriceGridItem::Bid(50))
        );
        assert_eq!(
            price_grid_item(grid, grid.recenter_index() + 2),
            Some(PriceGridItem::Bid(25))
        );
        assert_eq!(price_grid_item(grid, grid.recenter_index() + 3), None);
    }
}
