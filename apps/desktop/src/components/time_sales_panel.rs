//! Docked Time & Sales panel: the filtered trade tape for the selected market.

use super::side_panel_dock::side_panel_header;
use super::*;
use gpui::Stateful;

pub(super) struct TimeSalesPanelState<'a> {
    pub(super) app: Entity<WorkspaceSurface>,
    pub(super) tape: Option<&'a aeris_market_runtime::MarketTradeTapeSnapshot>,
    pub(super) sweeps: &'a [OrderFlowSweep],
    pub(super) product: Option<&'a InstallProviderInstrument>,
    /// Current book, whose mid anchors the price-range filter.
    pub(super) book: Option<&'a aeris_market_data::OrderBookFrame>,
    pub(super) filter: super::TimeSalesFilter,
    pub(super) scroll: &'a ScrollHandle,
}

/// Docked width of the Time & Sales panel, including its leading border: a time column,
/// a flexible price column and the eight-decimal size column.
pub(super) const TIME_SALES_PANEL_WIDTH: f32 = 260.0;

pub(super) fn time_sales_panel(
    state: TimeSalesPanelState<'_>,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let TimeSalesPanelState {
        app,
        tape,
        sweeps,
        product,
        book,
        filter,
        scroll,
    } = state;
    let rows = filtered_time_sales_rows(tape, product, book, filter);
    let side_filter_app = app.clone();
    let volume_filter_app = app.clone();
    let range_app = app.clone();
    let symbol = div()
        .pr_1()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(product.map_or_else(String::new, |product| product.display_symbol.clone()))
        .into_any_element();
    let price_scale = tape.map_or(0, |snapshot| u32::from(snapshot.price_scale));
    let quantity_scale = tape.map_or(0, |snapshot| u32::from(snapshot.quantity_scale));
    let size_label = if filter.minimum_quantity == 0.0 {
        "Any size".to_string()
    } else {
        format!(">= {}", filter.minimum_quantity)
    };
    let range_label = filter.price_range_ticks.map_or_else(
        || "All prices".to_string(),
        |ticks| format!("±{ticks} ticks"),
    );

    div()
        .id("time_sales_panel")
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(gpui_color(theme.colors.surface))
        .child(side_panel_header(
            SidePanel::TimeSales,
            app,
            [symbol],
            theme,
        ))
        .child(
            div()
                .p_1()
                .flex()
                .gap_1()
                .child(time_sales_filter_button(
                    "time_sales_side",
                    filter.side.label(),
                    move |cx| {
                        side_filter_app.update(cx, WorkspaceSurface::cycle_time_sales_side_filter);
                    },
                    theme,
                ))
                .child(time_sales_filter_button(
                    "time_sales_size",
                    size_label,
                    move |cx| {
                        volume_filter_app
                            .update(cx, WorkspaceSurface::cycle_time_sales_size_filter);
                    },
                    theme,
                ))
                .child(time_sales_filter_button(
                    "time_sales_range",
                    range_label,
                    move |cx| {
                        range_app.update(cx, WorkspaceSurface::cycle_time_sales_price_filter);
                    },
                    theme,
                )),
        )
        .child(
            div()
                .id("time_sales_rows")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .children(rows.into_iter().map(|retained| {
                    time_sales_row(retained, sweeps, price_scale, quantity_scale, theme)
                })),
        )
}

fn filtered_time_sales_rows<'a>(
    tape: Option<&'a aeris_market_runtime::MarketTradeTapeSnapshot>,
    product: Option<&InstallProviderInstrument>,
    book: Option<&aeris_market_data::OrderBookFrame>,
    filter: super::TimeSalesFilter,
) -> Vec<&'a aeris_market_runtime::RetainedMarketTrade> {
    const MAXIMUM_VISIBLE_TRADES: usize = 96;
    let center = book
        .and_then(|frame| frame.best_bid.as_ref().zip(frame.best_ask.as_ref()))
        .map(|(bid, ask)| i128::from(bid.price) + i128::from(ask.price));
    let increment = product.and_then(|product| product.price_increment);
    tape.into_iter()
        .flat_map(|snapshot| snapshot.trades.iter().rev())
        .filter(|retained| match filter.side {
            super::TimeSalesSideFilter::All => true,
            super::TimeSalesSideFilter::Buy => {
                retained.trade.aggressor == aeris_market_data::AggressorSide::Buy
            }
            super::TimeSalesSideFilter::Sell => {
                retained.trade.aggressor == aeris_market_data::AggressorSide::Sell
            }
        })
        .filter(|retained| {
            tape.is_some_and(|snapshot| {
                retained.trade.quantity.to_f64().is_some_and(|quantity| {
                    quantity / 10_f64.powi(i32::from(snapshot.quantity_scale))
                        >= filter.minimum_quantity
                })
            })
        })
        .filter(|retained| {
            let Some((range, center, increment)) = filter
                .price_range_ticks
                .zip(center)
                .zip(increment)
                .map(|((range, center), increment)| (range, center, increment))
            else {
                return true;
            };
            let doubled_distance = (i128::from(retained.trade.price) * 2 - center).abs();
            doubled_distance <= i128::from(range) * i128::from(increment) * 2
        })
        .take(MAXIMUM_VISIBLE_TRADES)
        .collect()
}

/// Fits an eight-decimal size such as `12.34567891` beside the price column.
const TIME_SALES_SIZE_COLUMN_WIDTH: f32 = 84.0;

fn time_sales_row(
    retained: &aeris_market_runtime::RetainedMarketTrade,
    sweeps: &[OrderFlowSweep],
    price_scale: u32,
    quantity_scale: u32,
    theme: &AerisTheme,
) -> Div {
    let trade = retained.trade.as_ref();
    let timestamp_nanos = trade
        .metadata
        .timestamps
        .exchange_unix_nanos
        .or(trade.metadata.timestamps.provider_unix_nanos)
        .unwrap_or(trade.metadata.timestamps.received_unix_nanos);
    let seconds = timestamp_nanos.div_euclid(1_000_000_000).rem_euclid(86_400);
    let time = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        seconds % 3_600 / 60,
        seconds % 60
    );
    let tone = match trade.aggressor {
        aeris_market_data::AggressorSide::Buy => theme.colors.positive,
        aeris_market_data::AggressorSide::Sell => theme.colors.danger,
        aeris_market_data::AggressorSide::Unknown => theme.colors.text_muted,
    };
    let time = if trade_ordinal_is_in_sweep(sweeps, retained.ingestion_ordinal) {
        format!("{time} S")
    } else {
        time
    };
    div()
        .h(px(22.0))
        .px_2()
        .flex()
        .gap_2()
        .items_center()
        .font_features(platform_tabular_numerals())
        .text_xs()
        .text_color(gpui_color(tone))
        .child(div().w(px(62.0)).flex_none().child(time))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_right()
                .child(market_price_text(trade.price, price_scale)),
        )
        .child(
            div()
                .w(px(TIME_SALES_SIZE_COLUMN_WIDTH))
                .flex_none()
                .text_right()
                .overflow_hidden()
                .child(market_price_text(trade.quantity, quantity_scale)),
        )
}

fn trade_ordinal_is_in_sweep(sweeps: &[OrderFlowSweep], ordinal: u64) -> bool {
    let index = sweeps.partition_point(|sweep| sweep.first_ingestion_ordinal <= ordinal);
    index > 0 && ordinal <= sweeps[index - 1].last_ingestion_ordinal
}

fn time_sales_filter_button(
    id: &'static str,
    label: impl Into<SharedString>,
    on_click: impl Fn(&mut gpui::App) + 'static,
    theme: &AerisTheme,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(22.0))
        .px_1()
        .flex()
        .items_center()
        .rounded(px(3.0))
        .bg(gpui_color(theme.colors.surface_secondary))
        .text_color(gpui_color(theme.colors.text_secondary))
        .text_xs()
        .cursor_pointer()
        .child(label.into())
        .on_click(move |_, _, cx| on_click(cx))
}
