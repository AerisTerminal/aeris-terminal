use crate::{DomColumnLevel, DomFrame, DomRow};
use axiusflow_design_system::{AxiusflowTheme, ThemeColor};
use axiusflow_market_data::{OrderBookRecoveryReason, OrderBookState};
use gpui::{
    Context, Div, Hsla, IntoElement, Render, ScrollHandle, Task, Window, div, prelude::*, px,
    relative,
};
use std::time::{Duration, Instant};

const HEADER_HEIGHT: f32 = 28.0;
const ROW_HEIGHT: f32 = 22.0;
const TEXT_SIZE: f32 = 11.0;
const PRESENTATION_INTERVAL: Duration = Duration::from_millis(100);
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

/// Provider connectivity shown independently from the last valid book frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomConnectionState {
    Online,
    Offline,
    Recovering,
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
    pending_frame: Option<DomFrame>,
    unavailable: bool,
    connection_state: DomConnectionState,
    last_presented: Option<Instant>,
    presentation_task: Option<Task<()>>,
    theme: AxiusflowTheme,
    ask_scroll: ScrollHandle,
    columns: DomColumnVisibility,
}

impl ReadOnlyDomView {
    #[must_use]
    pub fn new(theme: AxiusflowTheme) -> Self {
        Self {
            frame: None,
            pending_frame: None,
            unavailable: false,
            connection_state: DomConnectionState::Online,
            last_presented: None,
            presentation_task: None,
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
        let newest = self.pending_frame.as_ref().or(self.frame.as_ref());
        if newest.is_some_and(|current| frame_precedes(&frame, current)) {
            return false;
        }

        if requires_immediate_presentation(self.frame.as_ref(), &frame) {
            self.pending_frame = None;
            self.presentation_task = None;
            self.install_frame(frame, cx);
            return true;
        }

        self.pending_frame = Some(frame);
        self.schedule_presentation(cx);
        true
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        let had_frame = self.frame.take().is_some() || self.pending_frame.take().is_some();
        let was_unavailable = std::mem::replace(&mut self.unavailable, false);
        self.last_presented = None;
        self.presentation_task = None;
        if had_frame || was_unavailable {
            cx.notify();
        }
    }

    /// Marks depth as known-unavailable after a concrete failure (dead
    /// worker, stopped provider). The stale frame is dropped so a frozen
    /// book is never presented as live. Any later frame or demand clears it,
    /// so the panel returns to loading and then data on recovery.
    pub fn mark_unavailable(&mut self, cx: &mut Context<Self>) {
        let had_frame = self.frame.take().is_some() || self.pending_frame.take().is_some();
        if !self.unavailable || had_frame {
            self.unavailable = true;
            self.last_presented = None;
            self.presentation_task = None;
            cx.notify();
        }
    }

    /// Updates the connectivity banner without discarding the last valid book.
    pub fn set_connection_state(&mut self, state: DomConnectionState, cx: &mut Context<Self>) {
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

    fn install_frame(&mut self, frame: DomFrame, cx: &mut Context<Self>) {
        let recenter = self.frame.as_ref().is_none_or(|current| {
            frame.selection_generation != current.selection_generation
                || frame.session_generation != current.session_generation
                || (current.rows.is_empty() && !frame.rows.is_empty())
        });
        self.frame = Some(frame);
        self.unavailable = false;
        self.last_presented = Some(Instant::now());
        if recenter {
            self.ask_scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    fn schedule_presentation(&mut self, cx: &mut Context<Self>) {
        if self.presentation_task.is_some() {
            return;
        }
        let delay = self.last_presented.map_or(Duration::ZERO, |presented| {
            PRESENTATION_INTERVAL.saturating_sub(presented.elapsed())
        });
        self.presentation_task = Some(cx.spawn(async move |view, cx| {
            cx.background_executor().timer(delay).await;
            let _ = view.update(cx, |view, view_cx| {
                view.presentation_task = None;
                if let Some(frame) = view.pending_frame.take() {
                    view.install_frame(frame, view_cx);
                }
            });
        }));
    }
}

fn frame_precedes(candidate: &DomFrame, current: &DomFrame) -> bool {
    candidate.selection_generation < current.selection_generation
        || (candidate.selection_generation == current.selection_generation
            && (candidate.session_generation < current.session_generation
                || (candidate.session_generation == current.session_generation
                    && candidate.revision < current.revision)))
}

fn requires_immediate_presentation(current: Option<&DomFrame>, next: &DomFrame) -> bool {
    current.is_none_or(|current| {
        next.selection_generation != current.selection_generation
            || next.session_generation != current.session_generation
            || next.state != current.state
            || (current.rows.is_empty() && !next.rows.is_empty())
    })
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
        let empty_copy = empty_book_copy(self.unavailable, self.frame.is_some());
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
                    .children(connection_status_banner(self.connection_state, &self.theme))
                    .children(
                        (self.connection_state == DomConnectionState::Online)
                            .then(|| {
                                state.and_then(|state| status_banner(state, watermark, &self.theme))
                            })
                            .flatten(),
                    )
                    .child(render_ladder(
                        rows,
                        empty_copy,
                        columns,
                        &self.theme,
                        &ask_scroll,
                    ))
                    .children(column_rails(columns, &self.theme)),
            )
    }
}

fn connection_status_banner(
    state: DomConnectionState,
    theme: &AxiusflowTheme,
) -> Option<impl IntoElement + use<>> {
    let (label, color): (&str, StatusColor) = match state {
        DomConnectionState::Online => return None,
        DomConnectionState::Offline => ("Depth offline · internet disconnected", |theme| {
            theme.colors.danger
        }),
        DomConnectionState::Recovering => {
            ("Depth reconnecting · awaiting fresh snapshot", |theme| {
                theme.colors.bearish
            })
        }
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

/// Empty-panel copy: loading until the first frame, and the unavailable
/// verdict only after a concrete failure marked the book dead. An empty
/// book with no failure behind it is still loading, never an error.
const fn empty_book_copy(unavailable: bool, has_frame: bool) -> &'static str {
    if unavailable {
        "Depth unavailable"
    } else if has_frame {
        "Waiting for depth snapshot"
    } else {
        "Loading depth…"
    }
}

fn render_ladder(
    rows: &[DomRow],
    empty_copy: &'static str,
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
                .child(empty_copy),
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
                    .children(rows.iter().rev().filter_map(|row| {
                        row.ask.as_ref().map(|level| {
                            render_level_row(level, BookColumnSide::Ask, columns, theme)
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
                    .children(rows.iter().filter_map(|row| {
                        row.bid.as_ref().map(|level| {
                            render_level_row(level, BookColumnSide::Bid, columns, theme)
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
        .id((row_id, u64::try_from(level.price).unwrap_or(0)))
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
        // Awaiting the first snapshot is still loading, not a failure, so
        // it renders neutral. Red is reserved for a book that broke.
        OrderBookState::Recovering(OrderBookRecoveryReason::AwaitingSnapshot) => Some((
            format!(
                "Depth recovering · {}",
                recovery_label(OrderBookRecoveryReason::AwaitingSnapshot)
            ),
            |theme| theme.colors.text_secondary,
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

    fn frame(
        selection_generation: u64,
        session_generation: u64,
        revision: u64,
        state: OrderBookState,
        has_rows: bool,
    ) -> DomFrame {
        DomFrame {
            provider_id: "rithmic".into(),
            instrument_id: "BTC-USD".into(),
            entitlement_id: "public".into(),
            session_generation,
            selection_generation,
            revision,
            source_watermark: revision,
            state,
            rows: has_rows
                .then_some(DomRow {
                    bid: None,
                    ask: None,
                })
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn empty_book_reports_loading_until_a_concrete_failure() {
        assert_eq!(empty_book_copy(false, false), "Loading depth…");
        assert_eq!(empty_book_copy(false, true), "Waiting for depth snapshot");
        assert_eq!(empty_book_copy(true, false), "Depth unavailable");
        assert_eq!(empty_book_copy(true, true), "Depth unavailable");
    }

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
    fn provider_connectivity_has_stable_actionable_book_feedback() {
        let theme = AxiusflowTheme::default();
        assert!(connection_status_banner(DomConnectionState::Online, &theme).is_none());
        assert!(connection_status_banner(DomConnectionState::Offline, &theme).is_some());
        assert!(connection_status_banner(DomConnectionState::Recovering, &theme).is_some());
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
    fn active_book_updates_are_conflated_but_transitions_present_immediately() {
        let current = frame(2, 4, 10, OrderBookState::Ready, true);
        let update = frame(2, 4, 11, OrderBookState::Ready, true);
        assert!(!requires_immediate_presentation(Some(&current), &update));

        let recovering = frame(
            2,
            4,
            12,
            OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap),
            true,
        );
        assert!(requires_immediate_presentation(Some(&current), &recovering));
        assert!(requires_immediate_presentation(
            Some(&current),
            &frame(3, 4, 1, OrderBookState::Ready, false)
        ));
        assert!(requires_immediate_presentation(
            Some(&current),
            &frame(2, 5, 1, OrderBookState::Ready, true)
        ));
        assert!(requires_immediate_presentation(
            Some(&frame(2, 4, 1, OrderBookState::Ready, false)),
            &current
        ));
    }

    #[test]
    fn frame_ordering_resets_revision_only_for_a_new_session() {
        let current = frame(2, 4, 10, OrderBookState::Ready, true);
        assert!(frame_precedes(
            &frame(2, 4, 9, OrderBookState::Ready, true),
            &current
        ));
        assert!(!frame_precedes(
            &frame(2, 5, 1, OrderBookState::Ready, true),
            &current
        ));
        assert!(frame_precedes(
            &frame(1, 9, 99, OrderBookState::Ready, true),
            &current
        ));
    }
}
