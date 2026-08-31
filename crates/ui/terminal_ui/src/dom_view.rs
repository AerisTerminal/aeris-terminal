use crate::{DomColumnLevel, DomFrame, DomRow};
use axiusflow_design_system::{AxiusflowTheme, ThemeColor};
use axiusflow_market_data::{OrderBookRecoveryReason, OrderBookState};
use gpui::{Context, Hsla, IntoElement, Render, Window, div, prelude::*, px, relative};

const HEADER_HEIGHT: f32 = 28.0;
const ROW_HEIGHT: f32 = 22.0;
const PRICE_WIDTH: f32 = 112.0;

/// Flush, square-edged GPUI view for one immutable read-only DOM frame.
pub struct ReadOnlyDomView {
    frame: Option<DomFrame>,
    theme: AxiusflowTheme,
}

impl ReadOnlyDomView {
    #[must_use]
    pub const fn new(theme: AxiusflowTheme) -> Self {
        Self { frame: None, theme }
    }

    #[must_use]
    pub const fn frame(&self) -> Option<&DomFrame> {
        self.frame.as_ref()
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
        self.frame = Some(frame);
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
                    .h(px(HEADER_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(gpui_color(colors.border))
                    .text_xs()
                    .text_color(gpui_color(colors.text_secondary))
                    .child(div().flex_1().px_2().text_right().child("BID SIZE"))
                    .child(div().w(px(PRICE_WIDTH)).px_1().text_center().child("PRICE"))
                    .child(div().flex_1().px_2().child("ASK SIZE")),
            )
            .children(state.and_then(|state| status_banner(state, watermark, &self.theme)))
            .child(
                div()
                    .id("read_only_dom_rows")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_y_scroll()
                    .children(rows.iter().rev().enumerate().filter_map(|(index, row)| {
                        row.ask.as_ref().map(|level| {
                            render_level_row(index, level, BookColumnSide::Ask, &self.theme)
                        })
                    }))
                    .children(spread_row(rows, &self.theme))
                    .children(rows.iter().enumerate().filter_map(|(index, row)| {
                        row.bid.as_ref().map(|level| {
                            render_level_row(index, level, BookColumnSide::Bid, &self.theme)
                        })
                    }))
                    .children(rows.is_empty().then(|| {
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_sm()
                            .text_color(gpui_color(colors.text_secondary))
                            .child(if state.is_none() {
                                "Depth unavailable"
                            } else {
                                "Waiting for depth snapshot"
                            })
                    })),
            )
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BookColumnSide {
    Bid,
    Ask,
}

fn render_level_row(
    index: usize,
    level: &DomColumnLevel,
    side: BookColumnSide,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let (price_color, row_id) = match side {
        BookColumnSide::Bid => (colors.bullish, "dom_bid_row"),
        BookColumnSide::Ask => (colors.bearish, "dom_ask_row"),
    };
    div()
        .id((row_id, index))
        .h(px(ROW_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .child(quantity_cell(
            (side == BookColumnSide::Bid).then_some(level),
            colors.bullish,
            true,
        ))
        .child(
            div()
                .w(px(PRICE_WIDTH))
                .px_1()
                .text_center()
                .text_color(gpui_color(price_color))
                .child(level.price_text.clone()),
        )
        .child(quantity_cell(
            (side == BookColumnSide::Ask).then_some(level),
            colors.bearish,
            false,
        ))
}

fn quantity_cell(
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
    let cell = div()
        .flex_1()
        .h_full()
        .relative()
        .overflow_hidden()
        .flex()
        .items_center()
        .px_2()
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
                .child(level.map_or_else(String::new, |level| level.quantity_text.clone())),
        );
    if align_right { cell.text_right() } else { cell }
}

fn spread_row(rows: &[DomRow], theme: &AxiusflowTheme) -> Option<impl IntoElement + use<>> {
    let bid = rows.first()?.bid.as_ref()?;
    let ask = rows.first()?.ask.as_ref()?;
    Some(
        div()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .border_b_1()
            .border_color(gpui_color(theme.colors.border))
            .bg(gpui_color(theme.colors.surface_secondary))
            .text_xs()
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
            .text_xs()
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
