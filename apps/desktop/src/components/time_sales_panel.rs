//! Docked Time & Sales panel: the filtered trade tape for the selected market.

use super::side_panel_dock::side_panel_header;
use super::*;
use gpui::Stateful;
use std::cell::RefCell;

/// The visible rows and history label derived from one tape publication and filter, so
/// renders between publications (chart, book, and pointer repaints) never rescan the
/// retained tape. Holding the projected tape's `Arc` makes its identity check exact.
#[derive(Default)]
pub(super) struct TimeSalesRowsCache(RefCell<Option<TimeSalesRows>>);

struct TimeSalesRows {
    trades: Arc<[aeris_market_runtime::RetainedMarketTrade]>,
    quantity_scale: u8,
    filter: super::TimeSalesFilter,
    range: Option<(i128, i128)>,
    indices: Vec<usize>,
    history_label: String,
}

impl TimeSalesRowsCache {
    fn rows(
        &self,
        snapshot: &aeris_market_runtime::MarketTradeTapeSnapshot,
        product: Option<&InstallProviderInstrument>,
        book: Option<&aeris_market_data::OrderBookFrame>,
        filter: super::TimeSalesFilter,
    ) -> (Vec<usize>, String) {
        // The book mid only matters while the price-range filter is active.
        let range = filter.price_range_ticks.and(
            book.and_then(|frame| frame.best_bid.as_ref().zip(frame.best_ask.as_ref()))
                .map(|(bid, ask)| i128::from(bid.price) + i128::from(ask.price))
                .zip(
                    product
                        .and_then(|product| product.price_increment)
                        .map(i128::from),
                ),
        );
        let mut cached = self.0.borrow_mut();
        let current = cached.as_ref().is_some_and(|rows| {
            Arc::ptr_eq(&rows.trades, &snapshot.trades)
                && rows.quantity_scale == snapshot.quantity_scale
                && rows.filter == filter
                && rows.range == range
        });
        if !current {
            let history_label = match cached.as_mut() {
                Some(rows) if Arc::ptr_eq(&rows.trades, &snapshot.trades) => {
                    std::mem::take(&mut rows.history_label)
                }
                _ => tick_history_label(snapshot),
            };
            *cached = Some(TimeSalesRows {
                trades: Arc::clone(&snapshot.trades),
                quantity_scale: snapshot.quantity_scale,
                filter,
                range,
                indices: filtered_time_sales_rows(snapshot, filter, range),
                history_label,
            });
        }
        cached.as_ref().map_or_else(Default::default, |rows| {
            (rows.indices.clone(), rows.history_label.clone())
        })
    }
}

pub(super) struct TimeSalesPanelState<'a> {
    pub(super) app: Entity<WorkspaceSurface>,
    pub(super) tape: Option<&'a aeris_market_runtime::MarketTradeTapeSnapshot>,
    pub(super) rows_cache: &'a TimeSalesRowsCache,
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
        rows_cache,
        sweeps,
        product,
        book,
        filter,
        scroll,
    } = state;
    let (history_header, rows) = tape.map_or_else(
        || (None, Vec::new()),
        |snapshot| {
            let (indices, history_label) = rows_cache.rows(snapshot, product, book, filter);
            (
                Some(tick_history_header(history_label, theme)),
                time_sales_rows(snapshot, &indices, sweeps, theme),
            )
        },
    );
    let side_filter_app = app.clone();
    let volume_filter_app = app.clone();
    let range_app = app.clone();
    let reset_app = app.clone();
    let symbol = div()
        .pr_1()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(product.map_or_else(String::new, |product| product.display_symbol.clone()))
        .into_any_element();
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
        .children(history_header)
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
                ))
                .child(time_sales_filter_button(
                    "time_sales_reset",
                    "Reset",
                    move |cx| {
                        reset_app.update(cx, WorkspaceSurface::reset_time_sales_filter);
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
                .children(rows),
        )
}

fn time_sales_rows(
    snapshot: &aeris_market_runtime::MarketTradeTapeSnapshot,
    indices: &[usize],
    sweeps: &[OrderFlowSweep],
    theme: &AerisTheme,
) -> Vec<Div> {
    let price_scale = u32::from(snapshot.price_scale);
    let quantity_scale = u32::from(snapshot.quantity_scale);
    indices
        .iter()
        .filter_map(|&index| snapshot.trades.get(index))
        .map(|retained| time_sales_row(retained, sweeps, price_scale, quantity_scale, theme))
        .collect()
}

fn tick_history_header(label: String, theme: &AerisTheme) -> Div {
    div()
        .px_2()
        .py_1()
        .text_color(gpui_color(theme.colors.text_secondary))
        .child(label)
}

fn tick_history_label(snapshot: &aeris_market_runtime::MarketTradeTapeSnapshot) -> String {
    let oldest = snapshot
        .trades
        .iter()
        .filter_map(|trade| trade.trade.metadata.timestamps.exchange_unix_nanos)
        .min();
    oldest.map_or_else(
        || "Waiting for available tick history".to_string(),
        |time| {
            let time = chrono::DateTime::from_timestamp(time.div_euclid(1_000_000_000), 0)
                .map_or_else(String::new, |time| {
                    time.format("%m-%d %H:%M:%S UTC").to_string()
                });
            format!("{} ticks · from {time}", snapshot.trades.len())
        },
    )
}

/// Indices into the tape of the newest matching trades, newest exchange time first.
/// `range` is the doubled book mid and the price increment while the range filter applies.
fn filtered_time_sales_rows(
    snapshot: &aeris_market_runtime::MarketTradeTapeSnapshot,
    filter: super::TimeSalesFilter,
    range: Option<(i128, i128)>,
) -> Vec<usize> {
    const MAXIMUM_VISIBLE_TRADES: usize = 96;
    let quantity_divisor = 10_f64.powi(i32::from(snapshot.quantity_scale));
    let mut rows: Vec<_> = snapshot
        .trades
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, retained)| match filter.side {
            super::TimeSalesSideFilter::All => true,
            super::TimeSalesSideFilter::Buy => {
                retained.trade.aggressor == aeris_market_data::AggressorSide::Buy
            }
            super::TimeSalesSideFilter::Sell => {
                retained.trade.aggressor == aeris_market_data::AggressorSide::Sell
            }
        })
        .filter(|(_, retained)| {
            retained
                .trade
                .quantity
                .to_f64()
                .is_some_and(|quantity| quantity / quantity_divisor >= filter.minimum_quantity)
        })
        .filter(|(_, retained)| {
            let Some((ticks, (center, increment))) = filter.price_range_ticks.zip(range) else {
                return true;
            };
            let doubled_distance = (i128::from(retained.trade.price) * 2 - center).abs();
            doubled_distance <= i128::from(ticks) * increment * 2
        })
        .collect();
    // A covering history reply can arrive after live trades. Display exchange
    // time, preserving ingestion ordinals as local delivery evidence only.
    let key = |(_, retained): &(usize, &aeris_market_runtime::RetainedMarketTrade)| {
        std::cmp::Reverse((
            retained
                .trade
                .metadata
                .timestamps
                .exchange_unix_nanos
                .or(retained.trade.metadata.timestamps.provider_unix_nanos)
                .unwrap_or(retained.trade.metadata.timestamps.received_unix_nanos),
            retained.ingestion_ordinal,
        ))
    };
    if rows.len() > MAXIMUM_VISIBLE_TRADES {
        rows.select_nth_unstable_by_key(MAXIMUM_VISIBLE_TRADES, key);
    }
    rows.truncate(MAXIMUM_VISIBLE_TRADES);
    rows.sort_unstable_by_key(key);
    rows.into_iter().map(|(index, _)| index).collect()
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
        .border(px(theme.dimensions.border_width))
        .border_color(gpui_color(theme.colors.border))
        .bg(gpui_color(theme.colors.surface_secondary))
        .text_color(gpui_color(theme.colors.text_primary))
        .text_xs()
        .whitespace_nowrap()
        .cursor_pointer()
        .child(label.into())
        .on_click(move |_, _, cx| on_click(cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tape(
        trades: Vec<aeris_market_runtime::RetainedMarketTrade>,
    ) -> aeris_market_runtime::MarketTradeTapeSnapshot {
        aeris_market_runtime::MarketTradeTapeSnapshot {
            consumer_id: aeris_market_runtime::MarketConsumerId(std::num::NonZeroU64::MIN),
            generation: aeris_market_runtime::MarketGenerationId(std::num::NonZeroU64::MIN),
            provider_id: "provider".into(),
            instrument_id: "instrument".into(),
            entitlement_id: "entitlement".into(),
            provider_generation: 1,
            revision: 1,
            source_watermark: 0,
            rewrite_generation: 0,
            price_scale: 0,
            quantity_scale: 0,
            trades: trades.into(),
        }
    }

    fn trade(
        ordinal: u64,
        seconds: i64,
        quantity: i64,
    ) -> aeris_market_runtime::RetainedMarketTrade {
        aeris_market_runtime::RetainedMarketTrade {
            ingestion_ordinal: ordinal,
            observed_unix_nanos: seconds * 1_000_000_000,
            trade: Arc::new(aeris_market_data::MarketTrade {
                metadata: aeris_market_data::EventMetadata {
                    provider_id: "provider".into(),
                    instrument_id: "instrument".into(),
                    entitlement_id: "entitlement".into(),
                    session_generation: 1,
                    source_sequence: ordinal,
                    timestamps: aeris_market_data::QualifiedTimestamp {
                        exchange_unix_nanos: Some(seconds * 1_000_000_000),
                        provider_unix_nanos: None,
                        received_unix_nanos: seconds * 1_000_000_000,
                    },
                },
                trade_id: format!("trade-{ordinal}"),
                price: 100,
                quantity,
                aggressor: aeris_market_data::AggressorSide::Buy,
            }),
        }
    }

    #[test]
    fn rows_follow_exchange_time_and_refresh_only_for_a_new_publication_or_filter() {
        // A late covering history trade (ordinal 3) is older than the live trades.
        let snapshot = tape(vec![trade(1, 10, 1), trade(2, 20, 5), trade(3, 5, 5)]);
        let cache = TimeSalesRowsCache::default();
        let filter = super::super::TimeSalesFilter::default();
        let (rows, label) = cache.rows(&snapshot, None, None, filter);
        assert_eq!(rows, vec![1, 0, 2]);
        assert!(label.starts_with("3 ticks"));

        let large = super::super::TimeSalesFilter {
            minimum_quantity: 2.0,
            ..filter
        };
        assert_eq!(cache.rows(&snapshot, None, None, large).0, vec![1, 2]);

        let next = tape(vec![trade(1, 10, 1), trade(2, 20, 5), trade(4, 30, 9)]);
        assert_eq!(cache.rows(&next, None, None, large).0, vec![2, 1]);
    }
}
