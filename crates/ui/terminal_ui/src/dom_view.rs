use crate::{DomColumnLevel, DomFrame, DomRow};
use axiusflow_design_system::{AxiusflowTheme, ThemeColor};
use axiusflow_market_data::{OrderBookRecoveryReason, OrderBookState};
use gpui::{
    Context, Div, Hsla, IntoElement, Render, ScrollHandle, Window, div, prelude::*, px, relative,
};

const HEADER_HEIGHT: f32 = 28.0;
const ROW_HEIGHT: f32 = 22.0;
const TEXT_SIZE: f32 = 11.0;
const PNL_WIDTH: f32 = 0.12;
const BOOK_WIDTH: f32 = 0.18;
const PRICE_WIDTH: f32 = 0.22;
const ORDERS_WIDTH: f32 = 0.14;
const VOLUME_WIDTH: f32 = 0.16;

/// Columns available in the read-only depth ladder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomColumn {
    ProfitLoss,
    Bid,
    Price,
    Ask,
    Orders,
    Volume,
}

impl DomColumn {
    pub const ALL: [Self; 6] = [
        Self::ProfitLoss,
        Self::Bid,
        Self::Price,
        Self::Ask,
        Self::Orders,
        Self::Volume,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ProfitLoss => "P/L",
            Self::Bid => "Bid",
            Self::Price => "Price",
            Self::Ask => "Ask",
            Self::Orders => "Orders",
            Self::Volume => "Volume",
        }
    }

    const fn weight(self) -> f32 {
        match self {
            Self::ProfitLoss => PNL_WIDTH,
            Self::Bid | Self::Ask => BOOK_WIDTH,
            Self::Price => PRICE_WIDTH,
            Self::Orders => ORDERS_WIDTH,
            Self::Volume => VOLUME_WIDTH,
        }
    }

    const fn bit(self) -> u8 {
        match self {
            Self::ProfitLoss => 1 << 0,
            Self::Bid => 1 << 1,
            Self::Price => 1 << 2,
            Self::Ask => 1 << 3,
            Self::Orders => 1 << 4,
            Self::Volume => 1 << 5,
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
pub struct DomColumnVisibility {
    visible: u8,
}

impl Default for DomColumnVisibility {
    fn default() -> Self {
        Self {
            visible: DomColumn::Bid.bit()
                | DomColumn::Price.bit()
                | DomColumn::Ask.bit()
                | DomColumn::Orders.bit()
                | DomColumn::Volume.bit(),
        }
    }
}

impl DomColumnVisibility {
    #[must_use]
    pub const fn is_visible(self, column: DomColumn) -> bool {
        self.visible & column.bit() != 0
    }

    fn toggle(&mut self, column: DomColumn) -> bool {
        if !column.available() {
            return false;
        }
        self.visible ^= column.bit();
        true
    }

    fn width(self, column: DomColumn) -> f32 {
        let total = DomColumn::ALL
            .into_iter()
            .filter(|candidate| self.is_visible(*candidate))
            .map(DomColumn::weight)
            .sum::<f32>();
        if total > 0.0 {
            column.weight() / total
        } else {
            0.0
        }
    }
}

/// Flush, square-edged GPUI view for one immutable read-only DOM frame.
pub struct ReadOnlyDomView {
    frame: Option<DomFrame>,
    theme: AxiusflowTheme,
    ask_scroll: ScrollHandle,
    columns: DomColumnVisibility,
}

impl ReadOnlyDomView {
    #[must_use]
    pub fn new(theme: AxiusflowTheme) -> Self {
        Self {
            frame: None,
            theme,
            ask_scroll: ScrollHandle::new(),
            columns: DomColumnVisibility::default(),
        }
    }

    #[must_use]
    pub const fn frame(&self) -> Option<&DomFrame> {
        self.frame.as_ref()
    }

    #[must_use]
    pub const fn columns(&self) -> DomColumnVisibility {
        self.columns
    }

    pub fn toggle_column(&mut self, column: DomColumn, cx: &mut Context<Self>) {
        if self.columns.toggle(column) {
            cx.notify();
        }
    }

    /// Replaces the immutable frame. Older selections and revisions are rejected.
    pub fn replace_frame(&mut self, frame: DomFrame, cx: &mut Context<Self>) -> bool {
        if self.frame.as_ref().is_some_and(|current| {
            frame.selection_generation < current.selection_generation
                || (frame.selection_generation == current.selection_generation
                    && frame.revision < current.revision)
        }) {
            return false;
        }
        let recenter = self.frame.as_ref().is_none_or(|current| {
            frame.selection_generation != current.selection_generation
                || (current.rows.is_empty() && !frame.rows.is_empty())
        });
        self.frame = Some(frame);
        if recenter {
            self.ask_scroll.scroll_to_bottom();
        }
        cx.notify();
        true
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.frame.take().is_some() {
            cx.notify();
        }
    }

    pub fn set_theme(&mut self, theme: AxiusflowTheme, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            cx.notify();
        }
    }
}

impl Render for ReadOnlyDomView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let colors = self.theme.colors;
        let state = self.frame.as_ref().map(|frame| frame.state);
        let rows = self
            .frame
            .as_ref()
            .map_or(&[][..], |frame| frame.rows.as_slice());
        let watermark = self
            .frame
            .as_ref()
            .map_or(0, |frame| frame.source_watermark);
        let ask_scroll = self.ask_scroll.clone();
        let columns = self.columns;

        div()
            .id("read_only_dom")
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .bg(gpui_color(colors.surface))
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
                    .children(state.and_then(|state| status_banner(state, watermark, &self.theme)))
                    .child(render_ladder(
                        rows,
                        state,
                        columns,
                        &self.theme,
                        &ask_scroll,
                    ))
                    .children(column_rails(columns, &self.theme)),
            )
    }
}

fn render_header(columns: DomColumnVisibility, theme: &AxiusflowTheme) -> impl IntoElement + use<> {
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
            DomColumn::ALL
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

fn render_ladder(
    rows: &[DomRow],
    state: Option<OrderBookState>,
    columns: DomColumnVisibility,
    theme: &AxiusflowTheme,
    ask_scroll: &ScrollHandle,
) -> impl IntoElement + use<> {
    let body = div()
        .id("read_only_dom_rows")
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .overflow_hidden();
    if rows.is_empty() {
        return body.child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(gpui_color(theme.colors.text_secondary))
                .child(if state.is_none() {
                    "Depth unavailable"
                } else {
                    "Waiting for depth snapshot"
                }),
        );
    }

    body.child(
        div()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .child(
                div()
                    .id("read_only_dom_asks")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(ask_scroll)
                    .children(rows.iter().rev().enumerate().filter_map(|(index, row)| {
                        row.ask.as_ref().map(|level| {
                            render_level_row(index, level, BookColumnSide::Ask, columns, theme)
                        })
                    })),
            )
            .children(spread_row(rows, theme))
            .child(
                div()
                    .id("read_only_dom_bids")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows.iter().enumerate().filter_map(|(index, row)| {
                        row.bid.as_ref().map(|level| {
                            render_level_row(index, level, BookColumnSide::Bid, columns, theme)
                        })
                    })),
            ),
    )
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

fn render_level_row(
    index: usize,
    level: &DomColumnLevel,
    side: BookColumnSide,
    columns: DomColumnVisibility,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let (price_color, row_id) = match side {
        BookColumnSide::Bid => (colors.bullish, "dom_bid_row"),
        BookColumnSide::Ask => (colors.bearish, "dom_ask_row"),
    };
    div()
        .id((row_id, index))
        .w_full()
        .h(px(ROW_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .overflow_hidden()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_size(px(TEXT_SIZE))
        .children(
            DomColumn::ALL
                .into_iter()
                .filter(|column| columns.is_visible(*column))
                .map(|column| render_level_cell(column, level, side, columns, price_color, theme)),
        )
}

fn render_level_cell(
    column: DomColumn,
    level: &DomColumnLevel,
    side: BookColumnSide,
    columns: DomColumnVisibility,
    price_color: ThemeColor,
    theme: &AxiusflowTheme,
) -> gpui::AnyElement {
    let width = columns.width(column);
    match column {
        DomColumn::ProfitLoss => table_cell(width).into_any_element(),
        DomColumn::Bid => quantity_cell(
            width,
            (side == BookColumnSide::Bid).then_some(level),
            theme.colors.bullish,
            true,
        )
        .into_any_element(),
        DomColumn::Price => table_cell(width)
            .px_1()
            .text_center()
            .text_color(gpui_color(price_color))
            .child(level.price_text.clone())
            .into_any_element(),
        DomColumn::Ask => quantity_cell(
            width,
            (side == BookColumnSide::Ask).then_some(level),
            theme.colors.bearish,
            false,
        )
        .into_any_element(),
        DomColumn::Orders => table_cell(width)
            .px_1()
            .text_right()
            .text_color(gpui_color(theme.colors.text_secondary))
            .child(
                level
                    .order_count
                    .map_or_else(String::new, |count| count.to_string()),
            )
            .into_any_element(),
        DomColumn::Volume => table_cell(width)
            .px_1()
            .text_right()
            .text_color(gpui_color(theme.colors.text_secondary))
            .child(level.traded_volume_text.clone())
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

const fn column_alignment(column: DomColumn) -> CellAlignment {
    match column {
        DomColumn::Ask => CellAlignment::Left,
        DomColumn::Price => CellAlignment::Center,
        DomColumn::ProfitLoss | DomColumn::Bid | DomColumn::Orders | DomColumn::Volume => {
            CellAlignment::Right
        }
    }
}

fn quantity_cell(
    column_width: f32,
    level: Option<&DomColumnLevel>,
    color: ThemeColor,
    align_right: bool,
) -> impl IntoElement + use<> {
    let width = level.map_or(0.0, |level| f32::from(level.relative_size_bps) / 10_000.0);
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

fn column_rails(
    columns: DomColumnVisibility,
    theme: &AxiusflowTheme,
) -> impl Iterator<Item = gpui::AnyElement> + use<> {
    let border = gpui_color(theme.colors.border);
    let mut edge = 0.0;
    DomColumn::ALL
        .into_iter()
        .filter(move |column| columns.is_visible(*column))
        .filter_map(move |column| {
            edge += columns.width(column);
            (edge < 0.999).then_some(edge)
        })
        .enumerate()
        .map(move |(index, edge)| {
            div()
                .id(("dom_column_rail", index))
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(edge))
                .w(px(1.0))
                .bg(border)
                .into_any_element()
        })
}

fn spread_row(rows: &[DomRow], theme: &AxiusflowTheme) -> Option<impl IntoElement + use<>> {
    let bid = rows.first()?.bid.as_ref()?;
    let ask = rows.first()?.ask.as_ref()?;
    Some(
        div()
            .w_full()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .border_b_1()
            .border_color(gpui_color(theme.colors.border))
            .bg(gpui_color(theme.colors.surface_secondary))
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
            format!("Depth stale · last sequence {watermark}"),
            |theme| theme.colors.bearish,
        )),
        OrderBookState::Recovering(reason) => Some((
            format!("Depth recovering · {}", recovery_label(reason)),
            |theme| theme.colors.bearish,
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

    #[test]
    fn routing_columns_start_hidden_and_cannot_be_enabled() {
        let mut columns = DomColumnVisibility::default();
        assert!(!columns.is_visible(DomColumn::ProfitLoss));
        assert!(!columns.toggle(DomColumn::ProfitLoss));
        assert!(!columns.is_visible(DomColumn::ProfitLoss));
    }

    #[test]
    fn available_columns_toggle_and_renormalize_widths() {
        let mut columns = DomColumnVisibility::default();
        assert!(columns.toggle(DomColumn::Orders));
        assert!(!columns.is_visible(DomColumn::Orders));
        let width = DomColumn::ALL
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
    fn stale_and_recovery_states_are_explicit() {
        let stale = status_presentation(OrderBookState::Stale, 42)
            .map(|value| value.0)
            .expect("stale banner");
        assert_eq!(stale, "Depth stale · last sequence 42");
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
            assert_eq!(label, format!("Depth recovering · {expected}"));
        }
    }
}
