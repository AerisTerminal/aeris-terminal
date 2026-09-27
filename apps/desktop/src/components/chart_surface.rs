use super::{
    AerisChartView, AerisTheme, ChartNoticePlacement, ChartNoticeTone, ChartState,
    ChartSurfaceNotice, Context, Div, Entity, FluentBuilder, HugeIcon, InstallProviderInstrument,
    InteractiveElement, IntoElement, Loader, MenuRow, MouseButton, OrderBookColumn,
    OrderBookColumnVisibility, OrderFlowSweep, ParentElement, PopupAnimationOrigin, RadiusToken,
    ReadOnlyOrderBookView, Render, Role, SIDE_PANEL_MAXIMUM_WIDTH, SIDE_PANEL_MINIMUM_WIDTH,
    SIDE_PANEL_RESIZE_HANDLE_WIDTH, ScrollHandle, SharedString, SidePanel, SidePanelVisibility,
    StatefulInteractiveElement, Styled, TerminalApp, ToPrimitive, WORKSPACE_TAB_ICON_GLYPH,
    WORKSPACE_TAB_ICON_HIT, WatchlistDragState, WatchlistRow, Window, WorkspaceSurface,
    animate_popup_from_origin, chart_chrome, chart_surface_notice, chrome_close_button,
    chrome_tooltip, div, exchange_mark, gpui_color, header_icon, market_price_text,
    market_summary_change, market_summary_price, market_summary_values, platform_tabular_numerals,
    px, watchlist_drag_translation,
};
use gpui::{AppContext, Stateful};

const SIDE_PANEL_HEADER_HEIGHT: f32 = 30.0;
const SIDE_PANEL_SPLIT_DIVIDER_WIDTH: f32 = 1.0;
const SIDE_PANEL_SPLIT_HANDLE_WIDTH: f32 = 8.0;
const WATCHLIST_COLUMNS_HEIGHT: f32 = 26.0;
pub(super) const WATCHLIST_ROW_HEIGHT: f32 = 30.0;
const WATCHLIST_LAST_WIDTH: f32 = 70.0;
const WATCHLIST_CHANGE_WIDTH: f32 = 66.0;
const WATCHLIST_CHANGE_PERCENT_WIDTH: f32 = 64.0;
const WATCHLIST_VOLUME_WIDTH: f32 = 60.0;

pub(super) struct MarketWorkspaceState<'a> {
    pub(super) pane_id: u64,
    pub(super) chart: Option<&'a Entity<AerisChartView>>,
    pub(super) chart_has_market_data: bool,
    pub(super) chart_is_superseded: bool,
    pub(super) chart_state: ChartState,
    pub(super) chart_status_detail: String,
    pub(super) theme: &'a AerisTheme,
}

#[allow(clippy::too_many_lines)]
pub(super) fn market_workspace(state: MarketWorkspaceState<'_>) -> impl IntoElement + use<> {
    let MarketWorkspaceState {
        pane_id,
        chart,
        chart_has_market_data,
        chart_is_superseded,
        chart_state,
        chart_status_detail,
        theme,
    } = state;
    let colors = theme.colors;
    let notice = chart_surface_notice(
        chart_state,
        chart_has_market_data,
        chart_is_superseded,
        &chart_status_detail,
    );
    let chart_surface = chart_pane_host(chart)
        .id(("primary_chart", pane_id))
        .bg(gpui_color(colors.surface))
        .children(notice.map(|notice| chart_notice(notice, theme)));
    div().size_full().overflow_hidden().child(chart_surface)
}

pub(super) struct WorkspaceSidePanelState<'a> {
    pub(super) app: Entity<WorkspaceSurface>,
    pub(super) terminal: Entity<TerminalApp>,
    pub(super) workspace_id: u64,
    pub(super) visible: SidePanelVisibility,
    pub(super) width: f32,
    pub(super) split_basis_points: u32,
    pub(super) order_book: &'a Entity<ReadOnlyOrderBookView>,
    pub(super) order_book_frame: Option<aeris_market_data::OrderBookFrame>,
    pub(super) trade_tape: Option<&'a aeris_market_runtime::MarketTradeTapeSnapshot>,
    pub(super) trade_sweeps: &'a [OrderFlowSweep],
    pub(super) product: Option<&'a InstallProviderInstrument>,
    pub(super) time_sales_filter: super::TimeSalesFilter,
    pub(super) time_sales_scroll: ScrollHandle,
    pub(super) trading_pnl: Option<&'a aeris_trading::AccountPnl>,
    pub(super) trading_accounts: &'a [aeris_trading::TradingAccount],
    pub(super) trading_orders: &'a [aeris_trading::Order],
    pub(super) trading_positions: &'a [aeris_trading_runtime::PositionPnl],
    pub(super) trading_risk_meters: &'a [aeris_trading_runtime::RiskMeter],
    pub(super) trading_risk_locks: &'a [aeris_trading_runtime::RiskLock],
    pub(super) session_plans: &'a [aeris_trading_runtime::SessionPlan],
    pub(super) session_reviews: &'a [aeris_trading_runtime::SessionAdherenceReview],
    pub(super) trade_copiers: &'a [aeris_trading_runtime::TradeCopierConfig],
    pub(super) copy_dispatches: &'a [aeris_trading_runtime::TradeCopyDispatch],
    pub(super) strategy_templates: &'a [aeris_trading_runtime::BracketStrategyTemplate],
    pub(super) managed_brackets: &'a [aeris_trading_runtime::ManagedBracket],
    pub(super) trading_order_entry: &'a super::TradingOrderEntryState,
    pub(super) watchlist: WatchlistPanelState,
    pub(super) order_book_column_menu_open: bool,
    pub(super) order_book_columns: OrderBookColumnVisibility,
    pub(super) theme: &'a AerisTheme,
}

pub(super) struct WatchlistPanelState {
    pub(super) rows: Vec<WatchlistRow>,
    pub(super) drag: Option<WatchlistDragState>,
    pub(super) scroll: ScrollHandle,
}

#[derive(Clone, Copy)]
struct OrderBookPanelState<'a> {
    app: &'a Entity<WorkspaceSurface>,
    order_book: &'a Entity<ReadOnlyOrderBookView>,
    order_book_frame: Option<&'a aeris_market_data::OrderBookFrame>,
    trade_tape: Option<&'a aeris_market_runtime::MarketTradeTapeSnapshot>,
    trade_sweeps: &'a [OrderFlowSweep],
    product: Option<&'a InstallProviderInstrument>,
    time_sales_filter: super::TimeSalesFilter,
    time_sales_scroll: &'a ScrollHandle,
    trading_pnl: Option<&'a aeris_trading::AccountPnl>,
    trading_accounts: &'a [aeris_trading::TradingAccount],
    trading_orders: &'a [aeris_trading::Order],
    trading_positions: &'a [aeris_trading_runtime::PositionPnl],
    trading_risk_meters: &'a [aeris_trading_runtime::RiskMeter],
    trading_risk_locks: &'a [aeris_trading_runtime::RiskLock],
    session_plans: &'a [aeris_trading_runtime::SessionPlan],
    session_reviews: &'a [aeris_trading_runtime::SessionAdherenceReview],
    trade_copiers: &'a [aeris_trading_runtime::TradeCopierConfig],
    copy_dispatches: &'a [aeris_trading_runtime::TradeCopyDispatch],
    strategy_templates: &'a [aeris_trading_runtime::BracketStrategyTemplate],
    managed_brackets: &'a [aeris_trading_runtime::ManagedBracket],
    trading_order_entry: &'a super::TradingOrderEntryState,
    column_menu_open: bool,
    columns: OrderBookColumnVisibility,
    theme: &'a AerisTheme,
}

fn order_book_side_panel(state: &OrderBookPanelState<'_>) -> Div {
    let OrderBookPanelState {
        app,
        order_book,
        order_book_frame,
        trade_tape,
        trade_sweeps,
        product,
        time_sales_filter,
        time_sales_scroll,
        trading_pnl,
        trading_accounts,
        trading_orders,
        trading_positions,
        trading_risk_meters,
        trading_risk_locks,
        session_plans,
        session_reviews,
        trade_copiers,
        copy_dispatches,
        strategy_templates,
        managed_brackets,
        trading_order_entry,
        column_menu_open,
        columns,
        theme,
    } = *state;
    div()
        .relative()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(gpui_color(theme.colors.surface))
        .size_full()
        .child(side_panel_header(
            SidePanel::OrderBook,
            (*app).clone(),
            column_menu_open,
            theme,
        ))
        .child(trading_order_controls(&TradingOrderControlsState {
            app,
            frame: order_book_frame,
            trading_pnl,
            accounts: trading_accounts,
            orders: trading_orders,
            positions: trading_positions,
            risk_meters: trading_risk_meters,
            risk_locks: trading_risk_locks,
            session_plans,
            session_reviews,
            trade_copiers,
            copy_dispatches,
            strategy_templates,
            managed_brackets,
            order_entry: trading_order_entry,
            theme,
        }))
        .child(
            div()
                .id("order_book_rows")
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(order_book.clone()),
        )
        .child(time_sales_panel(
            TimeSalesPanelState {
                app: (*app).clone(),
                tape: trade_tape,
                sweeps: trade_sweeps,
                product,
                book: order_book_frame,
                filter: time_sales_filter,
                scroll: time_sales_scroll,
            },
            theme,
        ))
        .children(
            column_menu_open
                .then(|| order_book_column_menu_layer((*app).clone(), order_book, columns, theme)),
        )
}

struct TimeSalesPanelState<'a> {
    app: Entity<WorkspaceSurface>,
    tape: Option<&'a aeris_market_runtime::MarketTradeTapeSnapshot>,
    sweeps: &'a [OrderFlowSweep],
    product: Option<&'a InstallProviderInstrument>,
    book: Option<&'a aeris_market_data::OrderBookFrame>,
    filter: super::TimeSalesFilter,
    scroll: &'a ScrollHandle,
}

fn time_sales_panel(state: TimeSalesPanelState<'_>, theme: &AerisTheme) -> Div {
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
    let range_app = app;
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
        .h(px(210.0))
        .flex_none()
        .flex()
        .flex_col()
        .border_t_1()
        .border_color(gpui_color(theme.colors.border_secondary))
        .child(
            div()
                .h(px(28.0))
                .px_2()
                .flex()
                .items_center()
                .justify_between()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("TIME & SALES")
                .child(product.map_or_else(String::new, |product| product.display_symbol.clone())),
        )
        .child(
            div()
                .px_1()
                .pb_1()
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
        aeris_market_data::AggressorSide::Buy => theme.colors.success,
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
        .bg(gpui_color(theme.colors.input_fill))
        .text_color(gpui_color(theme.colors.text_secondary))
        .text_xs()
        .cursor_pointer()
        .child(label.into())
        .on_click(move |_, _, cx| on_click(cx))
}

#[derive(Clone, Copy)]
struct TradingOrderControlsState<'a> {
    app: &'a Entity<WorkspaceSurface>,
    frame: Option<&'a aeris_market_data::OrderBookFrame>,
    trading_pnl: Option<&'a aeris_trading::AccountPnl>,
    accounts: &'a [aeris_trading::TradingAccount],
    orders: &'a [aeris_trading::Order],
    positions: &'a [aeris_trading_runtime::PositionPnl],
    risk_meters: &'a [aeris_trading_runtime::RiskMeter],
    risk_locks: &'a [aeris_trading_runtime::RiskLock],
    session_plans: &'a [aeris_trading_runtime::SessionPlan],
    session_reviews: &'a [aeris_trading_runtime::SessionAdherenceReview],
    trade_copiers: &'a [aeris_trading_runtime::TradeCopierConfig],
    copy_dispatches: &'a [aeris_trading_runtime::TradeCopyDispatch],
    strategy_templates: &'a [aeris_trading_runtime::BracketStrategyTemplate],
    managed_brackets: &'a [aeris_trading_runtime::ManagedBracket],
    order_entry: &'a super::TradingOrderEntryState,
    theme: &'a AerisTheme,
}

fn trading_order_controls(state: &TradingOrderControlsState<'_>) -> impl IntoElement + use<> {
    let TradingOrderControlsState {
        app,
        frame,
        trading_pnl,
        accounts,
        orders,
        positions,
        risk_meters,
        risk_locks,
        session_plans,
        session_reviews,
        trade_copiers,
        copy_dispatches,
        strategy_templates,
        managed_brackets,
        order_entry,
        theme,
    } = *state;
    let colors = theme.colors;
    let order_entry_locked = order_entry
        .selected_account_id
        .as_ref()
        .is_some_and(|account_id| risk_locks.iter().any(|lock| &lock.account_id == account_id));
    div()
        .flex()
        .flex_col()
        .gap_1()
        .p_1()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .child(trading_pnl_summary(trading_pnl, positions, frame, theme))
        .child(trading_risk_summary(
            risk_meters,
            risk_locks,
            order_entry,
            theme,
        ))
        .child(trading_plan_summary(
            session_plans,
            session_reviews,
            order_entry,
            theme,
        ))
        .child(trading_copier_controls(
            accounts,
            orders,
            trade_copiers,
            copy_dispatches,
            order_entry,
            theme,
        ))
        .child(trading_working_orders(
            frame,
            orders,
            order_entry,
            order_entry_locked,
            theme,
        ))
        .child(trading_strategy_summary(
            strategy_templates,
            managed_brackets,
            order_entry,
            theme,
        ))
        .child(trading_order_entry(
            app,
            accounts,
            strategy_templates,
            order_entry,
            theme,
        ))
        .child(market_order_buttons(
            frame,
            order_entry,
            order_entry_locked,
            theme,
        ))
        .child(book_order_buttons(
            frame,
            order_entry,
            order_entry_locked,
            theme,
        ))
        .child(order_management_buttons(frame, order_entry, theme))
}

fn trading_order_entry(
    app: &Entity<WorkspaceSurface>,
    accounts: &[aeris_trading::TradingAccount],
    strategy_templates: &[aeris_trading_runtime::BracketStrategyTemplate],
    order_entry: &super::TradingOrderEntryState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let account_selector = trading_account_selector(app, accounts, order_entry, theme);
    let quantity_buttons = trading_quantity_buttons(app, order_entry.quantity, theme);
    let type_buttons = trading_order_type_buttons(app, order_entry.order_type, theme);
    let tif_buttons = trading_time_in_force_buttons(app, order_entry.time_in_force, theme);
    let strategy_selector = trading_strategy_selector(app, strategy_templates, order_entry, theme);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(account_selector)
        .child(quantity_buttons)
        .child(type_buttons)
        .child(tif_buttons)
        .child(strategy_selector)
}

fn trading_strategy_summary(
    templates: &[aeris_trading_runtime::BracketStrategyTemplate],
    brackets: &[aeris_trading_runtime::ManagedBracket],
    order_entry: &super::TradingOrderEntryState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let selected = order_entry
        .selected_strategy_template_id
        .as_ref()
        .and_then(|id| {
            templates
                .iter()
                .find(|template| &template.template_id == id)
        });
    let active = brackets
        .iter()
        .filter(|bracket| {
            matches!(
                bracket.status,
                aeris_trading_runtime::ManagedBracketStatus::AwaitingEntry
                    | aeris_trading_runtime::ManagedBracketStatus::Active
            )
        })
        .count();
    let label = selected.map_or_else(
        || format!("STRATEGY OFF · {active} active"),
        |template| {
            format!(
                "{} · {} · {active} active",
                aeris_trading_runtime::ManagedBracket::MANAGEMENT_LABEL,
                template.name
            )
        },
    );
    div()
        .h(px(18.0))
        .px_1()
        .flex()
        .items_center()
        .text_xs()
        .text_color(gpui_color(theme.colors.text_muted))
        .child(label)
}

fn trading_strategy_selector(
    app: &Entity<WorkspaceSurface>,
    templates: &[aeris_trading_runtime::BracketStrategyTemplate],
    order_entry: &super::TradingOrderEntryState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let selected = order_entry
        .selected_strategy_template_id
        .as_ref()
        .and_then(|id| {
            templates
                .iter()
                .find(|template| &template.template_id == id)
        });
    let label = selected.map_or_else(
        || "STRAT · OFF".to_string(),
        |template| format!("STRAT · {}", template.name),
    );
    let strategy_app = (*app).clone();
    div()
        .id("trading_strategy_selector")
        .h(px(24.0))
        .flex()
        .items_center()
        .px_1()
        .rounded(px(4.0))
        .bg(gpui_color(theme.colors.hover_bg))
        .text_color(gpui_color(theme.colors.text_primary))
        .text_xs()
        .cursor_pointer()
        .role(Role::Button)
        .aria_label("Select a locally managed bracket strategy")
        .on_click(move |_, _, cx| {
            strategy_app.update(cx, |surface, surface_cx| {
                let enabled = surface
                    .trading_pnl
                    .strategy_templates
                    .iter()
                    .filter(|template| template.enabled)
                    .collect::<Vec<_>>();
                let next = surface
                    .trading_pnl
                    .order_entry
                    .selected_strategy_template_id
                    .as_ref()
                    .and_then(|selected| {
                        enabled
                            .iter()
                            .position(|template| &template.template_id == selected)
                    })
                    .and_then(|index| enabled.get(index + 1))
                    .map(|template| template.template_id.clone())
                    .or_else(|| {
                        surface
                            .trading_pnl
                            .order_entry
                            .selected_strategy_template_id
                            .is_none()
                            .then(|| enabled.first().map(|template| template.template_id.clone()))
                            .flatten()
                    });
                surface
                    .trading_pnl
                    .order_entry
                    .selected_strategy_template_id = next;
                surface_cx.notify();
            });
        })
        .child(label)
}

fn trading_account_selector(
    app: &Entity<WorkspaceSurface>,
    accounts: &[aeris_trading::TradingAccount],
    order_entry: &super::TradingOrderEntryState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let selected_account = order_entry
        .selected_account_id
        .as_ref()
        .and_then(|id| accounts.iter().find(|account| &account.id == id));
    let account_label = selected_account.map_or_else(
        || "ACCT · awaiting runtime".to_string(),
        |account| format!("ACCT · {}", account.display_name),
    );
    let account_app = (*app).clone();
    let account_count = accounts.len();
    div()
        .id("trading_account_selector")
        .h(px(24.0))
        .flex_1()
        .flex()
        .items_center()
        .px_1()
        .rounded(px(4.0))
        .bg(gpui_color(colors.hover_bg))
        .text_color(gpui_color(colors.text_primary))
        .text_xs()
        .cursor_pointer()
        .role(Role::Button)
        .aria_label("Select the simulated trading account")
        .on_click(move |_, _, cx| {
            if account_count > 1 {
                account_app.update(cx, |surface, surface_cx| {
                    let current = surface
                        .trading_pnl
                        .order_entry
                        .selected_account_id
                        .as_ref()
                        .and_then(|id| {
                            surface
                                .trading_pnl
                                .accounts
                                .iter()
                                .position(|account| &account.id == id)
                        })
                        .unwrap_or(0);
                    let next = (current + 1) % surface.trading_pnl.accounts.len();
                    surface.trading_pnl.order_entry.selected_account_id = surface
                        .trading_pnl
                        .accounts
                        .get(next)
                        .map(|account| account.id.clone());
                    surface.trading_pnl.current = None;
                    surface_cx.notify();
                });
            }
        })
        .child(account_label)
}

fn trading_order_type_buttons(
    app: &Entity<WorkspaceSurface>,
    selected_type: aeris_trading::OrderType,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let options = [
        (aeris_trading::OrderType::Market, "MKT"),
        (aeris_trading::OrderType::Limit, "LMT"),
        (aeris_trading::OrderType::Stop, "STP"),
        (aeris_trading::OrderType::StopLimit, "STP-LMT"),
    ];
    let buttons = options
        .into_iter()
        .enumerate()
        .map(|(index, (order_type, label))| {
            trading_choice_button(
                (*app).clone(),
                ("trading_order_type", index),
                label,
                format!("Use {label} order type"),
                selected_type == order_type,
                move |surface| surface.trading_pnl.order_entry.order_type = order_type,
                &colors,
            )
        });
    div().flex().gap_1().children(buttons)
}

fn trading_time_in_force_buttons(
    app: &Entity<WorkspaceSurface>,
    selected_tif: aeris_trading::TimeInForce,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let options = [
        (aeris_trading::TimeInForce::Day, "DAY"),
        (aeris_trading::TimeInForce::GoodTillCancelled, "GTC"),
        (aeris_trading::TimeInForce::ImmediateOrCancel, "IOC"),
        (aeris_trading::TimeInForce::FillOrKill, "FOK"),
    ];
    let buttons = options
        .into_iter()
        .enumerate()
        .map(|(index, (time_in_force, label))| {
            trading_choice_button(
                (*app).clone(),
                ("trading_time_in_force", index),
                label,
                format!("Use {label} time in force"),
                selected_tif == time_in_force,
                move |surface| surface.trading_pnl.order_entry.time_in_force = time_in_force,
                &colors,
            )
        });
    div().flex().gap_1().children(buttons)
}

fn trading_quantity_buttons(
    app: &Entity<WorkspaceSurface>,
    selected_quantity: u64,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let buttons = [1_u64, 2, 5, 10].into_iter().map(|quantity| {
        trading_choice_button(
            (*app).clone(),
            ("trading_quantity", quantity),
            quantity.to_string(),
            format!("Use quantity {quantity}"),
            selected_quantity == quantity,
            move |surface| surface.trading_pnl.order_entry.quantity = quantity,
            &colors,
        )
    });
    div().flex().gap_1().children(buttons)
}

fn trading_choice_button<T: Into<gpui::ElementId>>(
    app: Entity<WorkspaceSurface>,
    id: T,
    label: impl Into<SharedString>,
    aria_label: impl Into<SharedString>,
    selected: bool,
    update: impl Fn(&mut super::WorkspaceSurface) + 'static,
    colors: &aeris_design_system::ThemeColors,
) -> Stateful<Div> {
    let label = label.into();
    let aria_label = aria_label.into();
    div()
        .id(id)
        .h(px(22.0))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .bg(gpui_color(if selected {
            colors.primary
        } else {
            colors.hover_bg
        }))
        .text_color(gpui_color(if selected {
            colors.surface
        } else {
            colors.text_muted
        }))
        .text_xs()
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(aria_label)
        .on_click(move |_, _, cx| {
            app.update(cx, |surface, surface_cx| {
                update(surface);
                surface_cx.notify();
            });
        })
        .child(label)
}

fn trading_pnl_summary(
    pnl: Option<&aeris_trading::AccountPnl>,
    positions: &[aeris_trading_runtime::PositionPnl],
    frame: Option<&aeris_market_data::OrderBookFrame>,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let (label, color) = pnl.map_or_else(
        || ("P/L · awaiting runtime".to_string(), colors.text_muted),
        |pnl| {
            let total = pnl.realized.units().saturating_add(pnl.unrealized.units());
            let label = format!(
                "P/L {}{} · U {}{}",
                pnl.currency,
                format_fixed_point(pnl.realized),
                pnl.currency,
                format_fixed_point(pnl.unrealized),
            );
            let color = if total >= 0 {
                colors.bullish
            } else {
                colors.bearish
            };
            (label, color)
        },
    );
    let position_label = selected_position_pnl(pnl, positions, frame).map_or_else(
        || "POS · flat or awaiting mark".to_string(),
        |position| {
            let realized = position
                .realized_ticks
                .map_or_else(|| "n/a".to_string(), format_fixed_point);
            let unrealized = position
                .unrealized_ticks
                .map_or_else(|| "n/a".to_string(), format_fixed_point);
            format!(
                "POS {} · R {}t · U {}t",
                format_fixed_point(position.position.net_quantity),
                realized,
                unrealized,
            )
        },
    );
    div()
        .h(px(36.0))
        .px_1()
        .flex_col()
        .flex()
        .items_center()
        .text_xs()
        .text_color(gpui_color(color))
        .child(label)
        .child(
            div()
                .text_color(gpui_color(colors.text_muted))
                .child(position_label),
        )
}

fn selected_position_pnl<'a>(
    pnl: Option<&aeris_trading::AccountPnl>,
    positions: &'a [aeris_trading_runtime::PositionPnl],
    frame: Option<&aeris_market_data::OrderBookFrame>,
) -> Option<&'a aeris_trading_runtime::PositionPnl> {
    let account_id = pnl?.account_id.as_str();
    let instrument_id = frame?.instrument_id.as_str();
    positions.iter().find(|item| {
        item.position.account_id.as_str() == account_id
            && item.position.instrument_id.as_str() == instrument_id
    })
}

fn trading_risk_summary(
    risk_meters: &[aeris_trading_runtime::RiskMeter],
    risk_locks: &[aeris_trading_runtime::RiskLock],
    order_entry: &super::TradingOrderEntryState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let lock = order_entry
        .selected_account_id
        .as_ref()
        .and_then(|account_id| {
            risk_locks
                .iter()
                .find(|lock| &lock.account_id == account_id)
        });
    let meter = order_entry
        .selected_account_id
        .as_ref()
        .and_then(|account_id| {
            risk_meters
                .iter()
                .find(|meter| &meter.account_id == account_id)
        });
    let (label, color) = lock.map_or_else(
        || {
            meter.map_or_else(
                || ("RULE · no profile".to_string(), colors.text_muted),
                |meter| {
                    if !meter.enabled {
                        return ("RULE · disabled".to_string(), colors.text_muted);
                    }
                    let trailing = meter
                        .trailing_drawdown_remaining
                        .map_or_else(String::new, |remaining| {
                            format!(" · trail {}", format_fixed_point(remaining))
                        });
                    let consistency = meter.consistency_max_single_trade_percent.map_or_else(
                        String::new,
                        |percent| {
                            let current = meter.consistency_current_percent.unwrap_or(0);
                            let required = meter
                                .consistency_additional_profit_required
                                .map_or_else(String::new, |value| {
                                    format!(" +{}", format_fixed_point(value))
                                });
                            format!(" · consistency {current}/{percent}%{required}")
                        },
                    );
                    let restriction = if meter.restricted_until_unix_nanos.is_some() {
                        " · news window"
                    } else {
                        ""
                    };
                    let label = format!(
                        "RULE · loss {} · ctr {}{}{}{}",
                        format_fixed_point(meter.daily_loss_remaining),
                        format_fixed_point(meter.contracts_remaining),
                        trailing,
                        consistency,
                        restriction,
                    );
                    let color = if meter.daily_loss_remaining.units() == 0
                        || meter.contracts_remaining.units() == 0
                    {
                        colors.danger
                    } else {
                        colors.text_muted
                    };
                    (label, color)
                },
            )
        },
        |lock| (format!("RULE LOCKED · {}", lock.reason), colors.danger),
    );
    div()
        .h(px(18.0))
        .px_1()
        .flex()
        .items_center()
        .text_xs()
        .text_color(gpui_color(color))
        .child(label)
}

fn trading_plan_summary(
    plans: &[aeris_trading_runtime::SessionPlan],
    reviews: &[aeris_trading_runtime::SessionAdherenceReview],
    order_entry: &super::TradingOrderEntryState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let plan = order_entry
        .selected_account_id
        .as_ref()
        .and_then(|account_id| plans.iter().find(|plan| &plan.account_id == account_id));
    let review = plan.and_then(|plan| {
        reviews.iter().find(|review| {
            review.account_id == plan.account_id
                && review.plan_id == plan.plan_id
                && review.plan_revision == plan.revision
        })
    });
    let (label, color) = plan.map_or_else(
        || ("PLAN · none".to_string(), colors.text_muted),
        |plan| {
            let bias = match plan.bias {
                aeris_trading_runtime::SessionBias::Long => "long",
                aeris_trading_runtime::SessionBias::Short => "short",
                aeris_trading_runtime::SessionBias::Neutral => "neutral",
            };
            let setup = plan.active_setup.as_deref().unwrap_or("setup pending");
            let adherence = review.map_or_else(String::new, |review| {
                format!(
                    " · checklist {}/{} · outside {}",
                    review.checklist_completed,
                    review.checklist_total,
                    review.fills_outside_planned_hours,
                )
            });
            let has_violation = !plan.is_ready()
                || review.is_some_and(|review| {
                    !review.maximum_loss_respected || review.fills_outside_planned_hours > 0
                });
            (
                format!("PLAN · {bias} · {setup}{adherence}"),
                if has_violation {
                    colors.danger
                } else {
                    colors.text_muted
                },
            )
        },
    );
    div()
        .h(px(18.0))
        .px_1()
        .flex()
        .items_center()
        .text_xs()
        .text_color(gpui_color(color))
        .child(label)
}

fn trading_copier_controls(
    accounts: &[aeris_trading::TradingAccount],
    orders: &[aeris_trading::Order],
    configs: &[aeris_trading_runtime::TradeCopierConfig],
    dispatches: &[aeris_trading_runtime::TradeCopyDispatch],
    order_entry: &super::TradingOrderEntryState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let source = order_entry
        .selected_account_id
        .as_ref()
        .and_then(|source_id| accounts.iter().find(|account| &account.id == source_id));
    let current = source.and_then(|source| {
        configs
            .iter()
            .find(|config| config.source_account_id == source.id)
    });
    let proposed =
        source.and_then(|source| reconciled_trade_copier_config(accounts, configs, source));
    let eligible_targets = proposed.as_ref().map_or(0, |config| config.targets.len());
    let status = trade_copier_status(source.is_some(), current, eligible_targets);
    let status_color = if current.is_some_and(|config| config.enabled) {
        colors.bullish
    } else {
        colors.text_muted
    };
    let toggle = proposed.clone().map(|mut config| {
        config.enabled = !current.is_some_and(|current| current.enabled);
        let label = if config.enabled {
            "COPY ON"
        } else {
            "COPY OFF"
        };
        div()
            .id("trade_copier_toggle")
            .h(px(20.0))
            .px_1()
            .flex()
            .items_center()
            .rounded(px(3.0))
            .bg(gpui_color(colors.hover_bg))
            .text_color(gpui_color(status_color))
            .cursor_pointer()
            .role(Role::Button)
            .aria_label("Enable or pause copying from the selected account")
            .on_click(move |_, _, cx| {
                aeris_desktop::trading::register_trade_copier(config.clone(), cx);
            })
            .child(label)
    });
    let target_rows = proposed
        .as_ref()
        .map(|config| {
            config
                .targets
                .iter()
                .enumerate()
                .filter_map(|(index, target)| {
                    let account = accounts
                        .iter()
                        .find(|account| account.id == target.account_id)?;
                    Some(trading_copier_target_row(
                        index, account, target, config, &colors,
                    ))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let last_dispatch = source
        .and_then(|source| latest_copy_dispatch(source, orders, dispatches))
        .map(|dispatch| trade_copy_dispatch_label(accounts, dispatch));
    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_1()
        .py_1()
        .border_t_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .child(
            div()
                .h(px(20.0))
                .flex()
                .items_center()
                .text_color(gpui_color(status_color))
                .child(div().flex_1().truncate().child(status))
                .children(toggle),
        )
        .children(target_rows)
        .children(last_dispatch.map(|label| {
            div()
                .truncate()
                .text_color(gpui_color(colors.text_muted))
                .child(label)
        }))
}

fn trade_copier_status(
    source_selected: bool,
    current: Option<&aeris_trading_runtime::TradeCopierConfig>,
    eligible_targets: usize,
) -> String {
    match (source_selected, current, eligible_targets) {
        (false, _, _) => "COPY · select an account".to_string(),
        (true, _, 0) => "COPY · no same-mode target accounts".to_string(),
        (true, Some(config), _) if config.enabled => {
            format!("COPY ON · {} targets", config.targets.len())
        }
        (true, Some(config), _) => format!("COPY OFF · {} targets", config.targets.len()),
        (true, None, _) => format!("COPY OFF · {eligible_targets} targets available"),
    }
}

fn trade_copy_dispatch_label(
    accounts: &[aeris_trading::TradingAccount],
    dispatch: &aeris_trading_runtime::TradeCopyDispatch,
) -> String {
    let target = accounts
        .iter()
        .find(|account| account.id == dispatch.target_account_id)
        .map_or(dispatch.target_account_id.as_str(), |account| {
            account.display_name.as_str()
        });
    if dispatch.accepted {
        format!("LAST · {target} accepted")
    } else {
        let detail = dispatch.detail.as_deref().unwrap_or("rejected");
        format!("LAST · {target} · {}", bounded_copier_detail(detail))
    }
}

fn reconciled_trade_copier_config(
    accounts: &[aeris_trading::TradingAccount],
    configs: &[aeris_trading_runtime::TradeCopierConfig],
    source: &aeris_trading::TradingAccount,
) -> Option<aeris_trading_runtime::TradeCopierConfig> {
    let current = configs
        .iter()
        .find(|config| config.source_account_id == source.id);
    let revision = current.map_or(Some(1), |config| config.revision.checked_add(1))?;
    let targets = accounts
        .iter()
        .filter(|account| account.id != source.id && account.environment == source.environment)
        .take(aeris_trading_runtime::MAXIMUM_COPIER_TARGETS)
        .filter_map(|account| {
            current
                .and_then(|config| {
                    config
                        .targets
                        .iter()
                        .find(|target| target.account_id == account.id)
                })
                .cloned()
                .or_else(|| {
                    Some(aeris_trading_runtime::TradeCopierTarget {
                        account_id: account.id.clone(),
                        quantity_multiplier: aeris_trading::FixedPoint::try_new(1, 0).ok()?,
                        enabled: true,
                    })
                })
        })
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return None;
    }
    Some(aeris_trading_runtime::TradeCopierConfig {
        source_account_id: source.id.clone(),
        revision,
        enabled: current.is_some_and(|config| config.enabled),
        targets,
    })
}

fn trading_copier_target_row(
    index: usize,
    account: &aeris_trading::TradingAccount,
    target: &aeris_trading_runtime::TradeCopierTarget,
    proposed: &aeris_trading_runtime::TradeCopierConfig,
    colors: &aeris_design_system::ThemeColors,
) -> Stateful<Div> {
    let mut toggle_config = proposed.clone();
    if let Some(target) = toggle_config.targets.get_mut(index) {
        target.enabled = !target.enabled;
    }
    let mut multiplier_config = proposed.clone();
    if let Some(target) = multiplier_config.targets.get_mut(index) {
        target.quantity_multiplier = next_copier_multiplier(target.quantity_multiplier);
    }
    let enabled = target.enabled;
    let account_label = account.display_name.clone();
    let multiplier_label = format!(
        "{}×",
        format_fixed_point(target.quantity_multiplier).trim_start_matches('+')
    );
    div()
        .id(("trade_copier_target", index))
        .h(px(20.0))
        .flex()
        .items_center()
        .gap_1()
        .text_color(gpui_color(colors.text_secondary))
        .child(div().flex_1().truncate().child(account_label))
        .child(
            div()
                .id(("trade_copier_multiplier", index))
                .px_1()
                .cursor_pointer()
                .role(Role::Button)
                .aria_label("Change this account's trade-copy quantity multiplier")
                .text_color(gpui_color(colors.primary))
                .on_click(move |_, _, cx| {
                    aeris_desktop::trading::register_trade_copier(multiplier_config.clone(), cx);
                })
                .child(multiplier_label),
        )
        .child(
            div()
                .id(("trade_copier_target_toggle", index))
                .px_1()
                .cursor_pointer()
                .role(Role::Button)
                .aria_label("Enable or pause trade copying to this account")
                .text_color(gpui_color(if enabled {
                    colors.bullish
                } else {
                    colors.danger
                }))
                .on_click(move |_, _, cx| {
                    aeris_desktop::trading::register_trade_copier(toggle_config.clone(), cx);
                })
                .child(if enabled { "LIVE" } else { "PAUSED" }),
        )
}

fn next_copier_multiplier(current: aeris_trading::FixedPoint) -> aeris_trading::FixedPoint {
    let next = match (current.units(), current.scale()) {
        (1, 0) => 2,
        (2, 0) => 3,
        _ => 1,
    };
    match aeris_trading::FixedPoint::try_new(next, 0) {
        Ok(value) => value,
        Err(_) => current,
    }
}

fn latest_copy_dispatch<'a>(
    source: &aeris_trading::TradingAccount,
    orders: &[aeris_trading::Order],
    dispatches: &'a [aeris_trading_runtime::TradeCopyDispatch],
) -> Option<&'a aeris_trading_runtime::TradeCopyDispatch> {
    dispatches.iter().rev().find(|dispatch| {
        orders.iter().any(|order| {
            order.account_id == source.id
                && order.client_order_id.as_str() == dispatch.source_client_order_id.as_str()
        })
    })
}

fn bounded_copier_detail(detail: &str) -> String {
    const MAXIMUM_CHARS: usize = 64;
    let mut chars = detail.chars();
    let bounded = chars.by_ref().take(MAXIMUM_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

fn trading_working_orders(
    frame: Option<&aeris_market_data::OrderBookFrame>,
    orders: &[aeris_trading::Order],
    order_entry: &super::TradingOrderEntryState,
    order_entry_locked: bool,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let selected_account = order_entry.selected_account_id.as_ref();
    let selected_instrument = frame.map(|book| book.instrument_id.as_str());
    let rows = orders
        .iter()
        .filter(|order| order.status.is_open())
        .filter(|order| selected_account.is_none_or(|account_id| &order.account_id == account_id))
        .filter(|order| {
            selected_instrument
                .is_none_or(|instrument_id| order.instrument_id.as_str() == instrument_id)
        })
        .take(8)
        .enumerate()
        .map(|(index, order)| working_order_row(order, index, frame, order_entry_locked, &colors))
        .collect::<Vec<_>>();
    let header = if rows.is_empty() {
        "ORDERS · none".to_string()
    } else {
        format!("ORDERS · {} working", rows.len())
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_1()
        .text_xs()
        .text_color(gpui_color(colors.text_muted))
        .child(header)
        .children(rows)
}

fn working_order_row(
    order: &aeris_trading::Order,
    index: usize,
    frame: Option<&aeris_market_data::OrderBookFrame>,
    order_entry_locked: bool,
    colors: &aeris_design_system::ThemeColors,
) -> impl IntoElement + use<> {
    let direction = match order.side {
        aeris_trading::OrderSide::Buy => "B",
        aeris_trading::OrderSide::Sell => "S",
    };
    let instruction = match order.order_type {
        aeris_trading::OrderType::Market => "MKT",
        aeris_trading::OrderType::Limit => "LMT",
        aeris_trading::OrderType::Stop => "STP",
        aeris_trading::OrderType::StopLimit => "STP-LMT",
    };
    let price = order
        .limit_price
        .or(order.stop_price)
        .map_or_else(String::new, format_fixed_point);
    let label = format!(
        "ORD {direction} {instruction} {}{}",
        format_fixed_point(order.quantity),
        if price.is_empty() {
            String::new()
        } else {
            format!(" @ {price}")
        },
    );
    let client_order_key = order.client_order_id.as_str().to_string();
    let reprice_frame = frame.cloned();
    let reprice_side = order.side;
    let reprice_time_in_force = order.time_in_force;
    let reprice_order_key = client_order_key.clone();
    let reprice_button =
        (order.order_type == aeris_trading::OrderType::Limit && !order_entry_locked).then(|| {
            div()
                .id(("reprice_working_order", index))
                .px_1()
                .cursor_pointer()
                .role(Role::Button)
                .aria_label("Reprice working simulated limit order")
                .text_color(gpui_color(colors.primary))
                .on_click(move |_, _, cx| {
                    if let Some(frame) = reprice_frame.clone() {
                        aeris_desktop::trading::reprice_simulated_order(
                            reprice_order_key.clone(),
                            reprice_side,
                            reprice_time_in_force,
                            &frame,
                            cx,
                        );
                    }
                })
                .child("REPRICE")
        });
    div()
        .id(("working_order", index))
        .h(px(20.0))
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .text_color(gpui_color(colors.text_secondary))
        .child(div().flex_1().truncate().child(label))
        .child(
            div()
                .id(("cancel_working_order", index))
                .px_1()
                .cursor_pointer()
                .role(Role::Button)
                .aria_label("Cancel working simulated order")
                .text_color(gpui_color(colors.danger))
                .on_click(move |_, _, cx| {
                    aeris_desktop::trading::cancel_simulated_order(client_order_key.clone(), cx);
                })
                .child("CANCEL"),
        )
        .children(reprice_button)
}

fn format_fixed_point(value: aeris_trading::FixedPoint) -> String {
    let scale = usize::from(value.scale());
    let units = value.units();
    let sign = if units < 0 { "-" } else { "+" };
    let magnitude = units.unsigned_abs();
    if scale == 0 {
        return format!("{sign}{magnitude}");
    }
    let base = 10_u64.saturating_pow(u32::try_from(scale).unwrap_or(18));
    format!(
        "{sign}{}.{:0scale$}",
        magnitude / base,
        magnitude % base,
        scale = scale
    )
}

fn market_order_buttons(
    frame: Option<&aeris_market_data::OrderBookFrame>,
    order_entry: &super::TradingOrderEntryState,
    order_entry_locked: bool,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let buy_frame = frame.cloned();
    let sell_frame = frame.cloned();
    let account_key = order_entry
        .selected_account_id
        .as_ref()
        .map(|id| id.as_str().to_string());
    let buy_order_type = order_entry.order_type;
    let buy_time_in_force = order_entry.time_in_force;
    let buy_quantity = order_entry.quantity;
    let buy_template = order_entry.selected_strategy_template_id.clone();
    let sell_account_key = account_key.clone();
    let sell_order_type = buy_order_type;
    let sell_time_in_force = buy_time_in_force;
    let sell_quantity = buy_quantity;
    let sell_template = buy_template.clone();
    let buy = div()
        .id("trading_buy_market")
        .flex_1()
        .h(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .bg(gpui_color(if order_entry_locked {
            colors.hover_bg
        } else {
            colors.bullish
        }))
        .text_color(gpui_color(if order_entry_locked {
            colors.text_muted
        } else {
            colors.surface
        }))
        .text_xs()
        .role(Role::Button)
        .aria_label("Buy the selected simulated order")
        .when(order_entry_locked, gpui::Styled::cursor_not_allowed)
        .when(!order_entry_locked, move |button| {
            button.cursor_pointer().on_click(move |_, _, cx| {
                if let Some(frame) = buy_frame.clone() {
                    aeris_desktop::trading::dispatch_simulated_selected_order(
                        &frame,
                        aeris_trading::OrderSide::Buy,
                        aeris_desktop::trading::SimulatedOrderSelection {
                            account_key: account_key.clone(),
                            quantity: buy_quantity,
                            order_type: buy_order_type,
                            time_in_force: buy_time_in_force,
                            template_id: buy_template.clone(),
                        },
                        cx,
                    );
                }
            })
        })
        .child(format!("BUY {buy_quantity}"));
    let sell = div()
        .id("trading_sell_market")
        .flex_1()
        .h(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .bg(gpui_color(if order_entry_locked {
            colors.hover_bg
        } else {
            colors.bearish
        }))
        .text_color(gpui_color(if order_entry_locked {
            colors.text_muted
        } else {
            colors.surface
        }))
        .text_xs()
        .role(Role::Button)
        .aria_label("Sell the selected simulated order")
        .when(order_entry_locked, gpui::Styled::cursor_not_allowed)
        .when(!order_entry_locked, move |button| {
            button.cursor_pointer().on_click(move |_, _, cx| {
                if let Some(frame) = sell_frame.clone() {
                    aeris_desktop::trading::dispatch_simulated_selected_order(
                        &frame,
                        aeris_trading::OrderSide::Sell,
                        aeris_desktop::trading::SimulatedOrderSelection {
                            account_key: sell_account_key.clone(),
                            quantity: sell_quantity,
                            order_type: sell_order_type,
                            time_in_force: sell_time_in_force,
                            template_id: sell_template.clone(),
                        },
                        cx,
                    );
                }
            })
        })
        .child(format!("SELL {sell_quantity}"));
    div().flex().gap_1().child(buy).child(sell)
}

fn book_order_buttons(
    frame: Option<&aeris_market_data::OrderBookFrame>,
    order_entry: &super::TradingOrderEntryState,
    order_entry_locked: bool,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let buy_frame = frame.cloned();
    let sell_frame = frame.cloned();
    let account_key = order_entry
        .selected_account_id
        .as_ref()
        .map(|id| id.as_str().to_string());
    let sell_account_key = account_key.clone();
    let quantity = order_entry.quantity;
    let time_in_force = order_entry.time_in_force;
    let buy_template = order_entry.selected_strategy_template_id.clone();
    let sell_template = buy_template.clone();
    let ask_label = best_book_order_label(frame, OrderBookSide::Ask);
    let bid_label = best_book_order_label(frame, OrderBookSide::Bid);
    let buy = div()
        .id("trading_buy_ask")
        .flex_1()
        .h(px(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .bg(gpui_color(if order_entry_locked {
            colors.hover_bg
        } else {
            colors.bullish.with_alpha(0.75)
        }))
        .text_color(gpui_color(if order_entry_locked {
            colors.text_muted
        } else {
            colors.surface
        }))
        .text_xs()
        .role(Role::Button)
        .aria_label("Buy the selected simulated order at the best ask")
        .when(order_entry_locked, gpui::Styled::cursor_not_allowed)
        .when(!order_entry_locked, move |button| {
            button.cursor_pointer().on_click(move |_, _, cx| {
                if let Some(frame) = buy_frame.clone() {
                    aeris_desktop::trading::dispatch_simulated_selected_order(
                        &frame,
                        aeris_trading::OrderSide::Buy,
                        aeris_desktop::trading::SimulatedOrderSelection {
                            account_key: account_key.clone(),
                            quantity,
                            order_type: aeris_trading::OrderType::Limit,
                            time_in_force,
                            template_id: buy_template.clone(),
                        },
                        cx,
                    );
                }
            })
        })
        .child(ask_label);
    let sell = div()
        .id("trading_sell_bid")
        .flex_1()
        .h(px(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .bg(gpui_color(if order_entry_locked {
            colors.hover_bg
        } else {
            colors.bearish.with_alpha(0.75)
        }))
        .text_color(gpui_color(if order_entry_locked {
            colors.text_muted
        } else {
            colors.surface
        }))
        .text_xs()
        .role(Role::Button)
        .aria_label("Sell the selected simulated order at the best bid")
        .when(order_entry_locked, gpui::Styled::cursor_not_allowed)
        .when(!order_entry_locked, move |button| {
            button.cursor_pointer().on_click(move |_, _, cx| {
                if let Some(frame) = sell_frame.clone() {
                    aeris_desktop::trading::dispatch_simulated_selected_order(
                        &frame,
                        aeris_trading::OrderSide::Sell,
                        aeris_desktop::trading::SimulatedOrderSelection {
                            account_key: sell_account_key.clone(),
                            quantity,
                            order_type: aeris_trading::OrderType::Limit,
                            time_in_force,
                            template_id: sell_template.clone(),
                        },
                        cx,
                    );
                }
            })
        })
        .child(bid_label);
    div().flex().gap_1().child(buy).child(sell)
}

#[derive(Clone, Copy)]
enum OrderBookSide {
    Bid,
    Ask,
}

fn best_book_order_label(
    frame: Option<&aeris_market_data::OrderBookFrame>,
    side: OrderBookSide,
) -> String {
    let (prefix, level) = match side {
        OrderBookSide::Bid => ("SELL BID", frame.and_then(|book| book.best_bid.as_ref())),
        OrderBookSide::Ask => ("BUY ASK", frame.and_then(|book| book.best_ask.as_ref())),
    };
    level.map_or_else(
        || prefix.to_string(),
        |level| format!("{prefix} {}", level.price_text),
    )
}

fn order_management_buttons(
    frame: Option<&aeris_market_data::OrderBookFrame>,
    order_entry: &super::TradingOrderEntryState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .gap_1()
                .child(cancel_account_button(
                    order_entry
                        .selected_account_id
                        .as_ref()
                        .map(|id| id.as_str().to_string()),
                    &colors,
                ))
                .child(flatten_account_button(
                    frame.cloned(),
                    order_entry
                        .selected_account_id
                        .as_ref()
                        .map(|id| id.as_str().to_string()),
                    &colors,
                )),
        )
        .child(
            div()
                .flex()
                .gap_1()
                .child(cancel_all_button(&colors))
                .child(flatten_all_button(frame.cloned(), &colors)),
        )
        .child(
            div()
                .flex()
                .gap_1()
                .child(kill_account_button(
                    order_entry
                        .selected_account_id
                        .as_ref()
                        .map(|id| id.as_str().to_string()),
                    &colors,
                ))
                .child(kill_all_button(&colors)),
        )
}

fn trading_management_button(
    id: &'static str,
    label: &'static str,
    aria_label: &'static str,
    background: aeris_design_system::ThemeColor,
    foreground: aeris_design_system::ThemeColor,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(24.0))
        .px_2()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .bg(gpui_color(background))
        .text_color(gpui_color(foreground))
        .text_xs()
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(aria_label)
        .child(label)
}

fn cancel_account_button(
    account_key: Option<String>,
    colors: &aeris_design_system::ThemeColors,
) -> Stateful<Div> {
    trading_management_button(
        "trading_cancel_account",
        "CANCEL ACCT",
        "Cancel simulated orders for the selected account",
        colors.hover_bg,
        colors.text_primary,
    )
    .on_click(move |_, _, cx| {
        aeris_desktop::trading::cancel_simulated_account(account_key.clone(), cx);
    })
}

fn cancel_all_button(colors: &aeris_design_system::ThemeColors) -> Stateful<Div> {
    trading_management_button(
        "trading_cancel_all",
        "CANCEL ALL",
        "Cancel simulated orders for every account",
        colors.hover_bg,
        colors.text_primary,
    )
    .on_click(move |_, _, cx| {
        aeris_desktop::trading::cancel_simulated_accounts(cx);
    })
}

fn flatten_account_button(
    frame: Option<aeris_market_data::OrderBookFrame>,
    account_key: Option<String>,
    colors: &aeris_design_system::ThemeColors,
) -> Stateful<Div> {
    trading_management_button(
        "trading_flatten",
        "FLAT ACCT",
        "Flatten the selected simulated account",
        colors.warning,
        colors.text_primary,
    )
    .on_click(move |_, _, cx| {
        if let Some(frame) = frame.clone() {
            aeris_desktop::trading::flatten_simulated_account_for(&frame, account_key.clone(), cx);
        }
    })
}

fn flatten_all_button(
    frame: Option<aeris_market_data::OrderBookFrame>,
    colors: &aeris_design_system::ThemeColors,
) -> Stateful<Div> {
    trading_management_button(
        "trading_flatten_all",
        "FLAT ALL",
        "Flatten all simulated accounts",
        colors.warning,
        colors.text_primary,
    )
    .on_click(move |_, _, cx| {
        if let Some(frame) = frame.clone() {
            aeris_desktop::trading::flatten_simulated_accounts(&frame, cx);
        }
    })
}

fn kill_account_button(
    account_key: Option<String>,
    colors: &aeris_design_system::ThemeColors,
) -> Stateful<Div> {
    trading_management_button(
        "trading_kill_switch_account",
        "KILL ACCT",
        "Lock the selected simulated account",
        colors.danger,
        colors.surface,
    )
    .on_click(move |_, _, cx| {
        aeris_desktop::trading::kill_simulated_account(account_key.clone(), cx);
    })
}

fn kill_all_button(colors: &aeris_design_system::ThemeColors) -> Stateful<Div> {
    trading_management_button(
        "trading_kill_switch",
        "KILL ALL",
        "Lock every simulated account",
        colors.danger,
        colors.surface,
    )
    .on_click(move |_, _, cx| {
        aeris_desktop::trading::kill_simulated_accounts(cx);
    })
}

fn watchlist_side_panel(
    app: Entity<WorkspaceSurface>,
    terminal: &Entity<TerminalApp>,
    watchlist: WatchlistPanelState,
    theme: &AerisTheme,
) -> Div {
    let WatchlistPanelState { rows, drag, scroll } = watchlist;
    div()
        .relative()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(gpui_color(theme.colors.surface))
        .size_full()
        .child(side_panel_header(SidePanel::Watchlist, app, false, theme))
        .child(watchlist_table(
            terminal,
            rows,
            drag.as_ref(),
            &scroll,
            theme,
        ))
}

fn side_panel_region(content: Div, panel: SidePanel, both_visible: bool, ratio: f32) -> Div {
    if !both_visible {
        return content;
    }
    div()
        .h_full()
        .min_w_0()
        .flex_basis(px(0.0))
        .flex_grow(match panel {
            SidePanel::OrderBook => ratio,
            SidePanel::Watchlist => 1.0 - ratio,
        })
        .overflow_hidden()
        .child(content)
}

fn side_panel_total_width(width: f32, both_visible: bool) -> f32 {
    if both_visible {
        width * 2.0 + SIDE_PANEL_SPLIT_DIVIDER_WIDTH
    } else {
        width
    }
}

fn side_panel_horizontal_ratio(width: f32, split_basis_points: u32) -> f32 {
    let requested = split_basis_points.to_f32().unwrap_or(5_000.0) / 10_000.0;
    let total_width = width * 2.0;
    let lower = (SIDE_PANEL_MINIMUM_WIDTH / total_width)
        .max(1.0 - SIDE_PANEL_MAXIMUM_WIDTH / total_width)
        .clamp(0.05, 0.5);
    let upper = (SIDE_PANEL_MAXIMUM_WIDTH / total_width)
        .min(1.0 - SIDE_PANEL_MINIMUM_WIDTH / total_width)
        .clamp(0.5, 0.95);
    requested.clamp(lower, upper)
}

fn side_panel_split_ratio_from_drag(left: f32, total_width: f32, pointer_x: f32) -> Option<f32> {
    let content_width = total_width - SIDE_PANEL_SPLIT_DIVIDER_WIDTH;
    (content_width > 0.0)
        .then(|| (pointer_x - left - SIDE_PANEL_SPLIT_DIVIDER_WIDTH / 2.0) / content_width)
}

fn side_panel_width_from_drag(right: f32, pointer_x: f32, both_visible: bool) -> f32 {
    let panel_count = if both_visible { 2.0 } else { 1.0 };
    let split_width = if both_visible {
        SIDE_PANEL_SPLIT_DIVIDER_WIDTH
    } else {
        0.0
    };
    super::clamped_side_panel_width((right - pointer_x - split_width) / panel_count)
}

fn side_panel_visibility(visible: SidePanelVisibility) -> (bool, bool, bool) {
    let order_book_visible = visible.contains(SidePanel::OrderBook);
    let watchlist_visible = visible.contains(SidePanel::Watchlist);
    (
        order_book_visible,
        watchlist_visible,
        order_book_visible && watchlist_visible,
    )
}

fn side_panel_ratio(width: f32, split_basis_points: u32, both_visible: bool) -> f32 {
    if both_visible {
        side_panel_horizontal_ratio(width, split_basis_points)
    } else {
        1.0
    }
}

pub(super) fn workspace_side_panel(state: WorkspaceSidePanelState<'_>) -> impl IntoElement + use<> {
    let (order_book_visible, watchlist_visible, both_visible) =
        side_panel_visibility(state.visible);
    let order_book_panel = workspace_order_book_panel(&state, order_book_visible, both_visible);
    let WorkspaceSidePanelState {
        app,
        terminal,
        workspace_id,
        width,
        split_basis_points,
        watchlist,
        theme,
        ..
    } = state;
    let split_drag_app = app.clone();
    div()
        .id(("workspace_side_panel", workspace_id))
        .w(px(side_panel_total_width(width, both_visible)))
        .h_full()
        .flex_none()
        .relative()
        .flex()
        .overflow_hidden()
        .bg(gpui_color(theme.colors.surface))
        .border_l_1()
        .border_color(gpui_color(theme.colors.border))
        .children(order_book_panel)
        .children(
            both_visible
                .then(|| side_panel_split_handle(workspace_id, gpui_color(theme.colors.border))),
        )
        .children(watchlist_visible.then(|| {
            side_panel_region(
                watchlist_side_panel(app.clone(), &terminal, watchlist, theme),
                SidePanel::Watchlist,
                both_visible,
                side_panel_ratio(width, split_basis_points, both_visible),
            )
        }))
        .on_drag_move::<SidePanelSplitDrag>(move |event, _, cx| {
            let Some(ratio) = side_panel_split_ratio_from_drag(
                f32::from(event.bounds.left()),
                f32::from(event.bounds.size.width),
                f32::from(event.event.position.x),
            ) else {
                return;
            };
            split_drag_app.update(cx, |surface, surface_cx| {
                surface.set_side_panel_split_ratio(ratio, surface_cx);
            });
        })
        .on_drag_move::<SidePanelWidthDrag>(move |event, _, cx| {
            let width = side_panel_width_from_drag(
                f32::from(event.bounds.right()),
                f32::from(event.event.position.x),
                both_visible,
            );
            app.update(cx, |surface, surface_cx| {
                surface.set_side_panel_width(width, surface_cx);
            });
        })
        .child(side_panel_width_resize_handle(workspace_id))
}

fn workspace_order_book_panel(
    state: &WorkspaceSidePanelState<'_>,
    order_book_visible: bool,
    both_visible: bool,
) -> Option<Div> {
    order_book_visible.then(|| {
        side_panel_region(
            order_book_side_panel(&OrderBookPanelState {
                app: &state.app,
                order_book: state.order_book,
                order_book_frame: state.order_book_frame.as_ref(),
                trade_tape: state.trade_tape,
                trade_sweeps: state.trade_sweeps,
                product: state.product,
                time_sales_filter: state.time_sales_filter,
                time_sales_scroll: &state.time_sales_scroll,
                trading_pnl: state.trading_pnl,
                trading_accounts: state.trading_accounts,
                trading_orders: state.trading_orders,
                trading_positions: state.trading_positions,
                trading_risk_meters: state.trading_risk_meters,
                trading_risk_locks: state.trading_risk_locks,
                session_plans: state.session_plans,
                session_reviews: state.session_reviews,
                trade_copiers: state.trade_copiers,
                copy_dispatches: state.copy_dispatches,
                strategy_templates: state.strategy_templates,
                managed_brackets: state.managed_brackets,
                trading_order_entry: state.trading_order_entry,
                column_menu_open: state.order_book_column_menu_open,
                columns: state.order_book_columns,
                theme: state.theme,
            }),
            SidePanel::OrderBook,
            both_visible,
            side_panel_ratio(state.width, state.split_basis_points, both_visible),
        )
    })
}

pub(super) fn chart_pane_host(chart: Option<&Entity<AerisChartView>>) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .size_full()
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .children(chart.cloned())
}

#[derive(Clone)]
struct SidePanelSplitDrag;

impl Render for SidePanelSplitDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

#[derive(Clone)]
struct SidePanelWidthDrag;

impl Render for SidePanelWidthDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

#[derive(Clone)]
struct WatchlistRowDrag {
    provider: String,
    instrument_id: String,
}

impl Render for WatchlistRowDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

fn side_panel_split_handle(workspace_id: u64, border: gpui::Hsla) -> impl IntoElement {
    div()
        .id(("side_panel_split", workspace_id))
        .relative()
        .flex_none()
        .h_full()
        .w(px(SIDE_PANEL_SPLIT_DIVIDER_WIDTH))
        .bg(border)
        .child(
            div()
                .id(("side_panel_split_hit", workspace_id))
                .absolute()
                .top_0()
                .left(px(-(SIDE_PANEL_SPLIT_HANDLE_WIDTH
                    - SIDE_PANEL_SPLIT_DIVIDER_WIDTH)
                    / 2.0))
                .h_full()
                .w(px(SIDE_PANEL_SPLIT_HANDLE_WIDTH))
                .occlude()
                .cursor_col_resize()
                .on_drag(SidePanelSplitDrag, |drag, _, _, cx| {
                    cx.new(|_| drag.clone())
                }),
        )
}

fn side_panel_width_resize_handle(workspace_id: u64) -> impl IntoElement {
    div()
        .id(("side_panel_resize", workspace_id))
        .absolute()
        .occlude()
        .top_0()
        .left(px(-SIDE_PANEL_RESIZE_HANDLE_WIDTH / 2.0))
        .h_full()
        .w(px(SIDE_PANEL_RESIZE_HANDLE_WIDTH))
        .cursor_col_resize()
        .on_drag(SidePanelWidthDrag, |drag, _, _, cx| {
            cx.new(|_| drag.clone())
        })
}

fn watchlist_table(
    terminal: &Entity<TerminalApp>,
    rows: Vec<WatchlistRow>,
    watchlist_drag: Option<&WatchlistDragState>,
    watchlist_scroll: &ScrollHandle,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let mut body = div()
        .flex_1()
        .min_h_0()
        .font_features(platform_tabular_numerals());
    if rows.is_empty() {
        body = body.child(
            div()
                .px_3()
                .py_4()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child("Add symbols with +"),
        );
    } else {
        for (index, row) in rows.into_iter().enumerate() {
            body = body.child(watchlist_row(terminal, &row, index, watchlist_drag, theme));
        }
    }
    let move_terminal = terminal.clone();
    let move_scroll = watchlist_scroll.clone();
    let end_terminal = terminal.clone();
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(gpui_color(colors.surface))
        .child(watchlist_columns(theme))
        .child(
            body.id("watchlist_body")
                .overflow_y_scroll()
                .track_scroll(watchlist_scroll)
                .on_drag_move::<WatchlistRowDrag>(move |event, _, cx| {
                    let drag = event.drag(cx).clone();
                    move_terminal.update(cx, |terminal, terminal_cx| {
                        terminal.move_watchlist_drag(
                            &drag.provider,
                            &drag.instrument_id,
                            f32::from(event.event.position.y),
                            f32::from(event.bounds.top()),
                            f32::from(move_scroll.offset().y),
                            terminal_cx,
                        );
                    });
                })
                .on_drop(move |_: &WatchlistRowDrag, _, cx| {
                    end_terminal.update(cx, TerminalApp::end_watchlist_drag);
                }),
        )
}

fn watchlist_columns(theme: &AerisTheme) -> impl IntoElement + use<> {
    let colors = theme.colors;
    div()
        .h(px(WATCHLIST_COLUMNS_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .text_color(gpui_color(colors.text_muted))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .px_2()
                .whitespace_nowrap()
                .text_ellipsis()
                .child("ASSET"),
        )
        .child(watchlist_header_cell("LAST", WATCHLIST_LAST_WIDTH, theme))
        .child(watchlist_header_cell("CHG", WATCHLIST_CHANGE_WIDTH, theme))
        .child(watchlist_header_cell(
            "CHG %",
            WATCHLIST_CHANGE_PERCENT_WIDTH,
            theme,
        ))
        .child(watchlist_header_cell(
            "VOLUME",
            WATCHLIST_VOLUME_WIDTH,
            theme,
        ))
}

fn watchlist_header_cell(
    value: impl Into<gpui::SharedString>,
    width: f32,
    theme: &AerisTheme,
) -> Div {
    div()
        .w(px(width))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_end()
        .px_1()
        .border_l_1()
        .border_color(gpui_color(theme.colors.border))
        .text_right()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(value.into())
}

fn watchlist_value_cell(value: impl Into<gpui::SharedString>, width: f32) -> Div {
    div()
        .w(px(width))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_end()
        .px_1()
        .text_right()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(value.into())
}

fn watchlist_asset_cell(row: &WatchlistRow, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let asset_tone = if row.message.is_some() {
        colors.text_muted
    } else {
        colors.text_primary
    };
    let logo = match row.instrument.provider.as_str() {
        "hyperliquid" => Some(super::assets::ExchangeLogo::Hyperliquid),
        "rithmic" => Some(super::assets::ExchangeLogo::Rithmic),
        _ => None,
    };
    div()
        .min_w_0()
        .flex_1()
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .whitespace_nowrap()
        .text_color(gpui_color(asset_tone))
        .children(logo.map(|logo| exchange_mark(logo, px(16.0), false, &colors)))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .text_ellipsis()
                .child(row.instrument.display_symbol.clone()),
        )
}

fn watchlist_row_content(row: &WatchlistRow, theme: &AerisTheme) -> Stateful<Div> {
    let colors = theme.colors;
    let scale = row.instrument.price_scale;
    let values = market_summary_values(row.last, row.previous_close);
    let tone = values
        .change
        .map_or(colors.text_muted, |value| match value.cmp(&0) {
            std::cmp::Ordering::Less => colors.market_down,
            std::cmp::Ordering::Greater => colors.market_up,
            std::cmp::Ordering::Equal => colors.text_secondary,
        });
    div()
        .id(gpui::SharedString::from(format!(
            "watchlist_row_{}_{}",
            row.instrument.provider, row.instrument.instrument_id
        )))
        .group("watchlist_asset_row")
        .h(px(WATCHLIST_ROW_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .text_xs()
        .bg(gpui_color(if row.active {
            colors.active_bg.over(colors.surface)
        } else {
            colors.surface
        }))
        .when(!row.active, |item| {
            item.hover(move |item| item.bg(gpui_color(colors.hover_bg.over(colors.surface))))
        })
        .child(watchlist_asset_cell(row, theme))
        .child(watchlist_value_cell(
            values.last.map_or_else(
                || "—".to_string(),
                |value| market_summary_price(value, scale),
            ),
            WATCHLIST_LAST_WIDTH,
        ))
        .child(
            watchlist_value_cell(
                values.change.map_or_else(
                    || "—".to_string(),
                    |value| market_summary_change(value, scale),
                ),
                WATCHLIST_CHANGE_WIDTH,
            )
            .text_color(gpui_color(tone)),
        )
        .child(
            watchlist_value_cell(
                values
                    .change_percent
                    .map_or_else(|| "—".to_string(), |value| format!("{value:+.2}%")),
                WATCHLIST_CHANGE_PERCENT_WIDTH,
            )
            .text_color(gpui_color(tone)),
        )
        .child(watchlist_value_cell(
            row.last.map_or_else(
                || "—".to_string(),
                |bar| compact_watchlist_volume(bar.volume, row.instrument.quantity_scale),
            ),
            WATCHLIST_VOLUME_WIDTH,
        ))
}

fn watchlist_row(
    terminal: &Entity<TerminalApp>,
    row: &WatchlistRow,
    index: usize,
    drag: Option<&WatchlistDragState>,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let instrument = row.instrument.clone();
    let active = row.active;
    let dragging = drag.is_some_and(|drag| {
        drag.provider == row.instrument.provider
            && drag.instrument_id == row.instrument.instrument_id
    });
    let drag_translation = watchlist_drag_translation(
        drag,
        &row.instrument.provider,
        &row.instrument.instrument_id,
        index,
    );
    let content = watchlist_row_content(row, theme);
    let row = interactive_watchlist_row(content, terminal, instrument, active)
        .when(dragging, gpui::Styled::shadow_md)
        .when_some(drag_translation, |row, translation| {
            row.relative().top(px(translation))
        });
    div()
        .relative()
        .h(px(WATCHLIST_ROW_HEIGHT))
        .flex_none()
        .child(row)
        .children(dragging.then(|| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .top_0()
                .h(px(2.0))
                .bg(gpui_color(colors.primary))
        }))
}

fn interactive_watchlist_row(
    row: Stateful<Div>,
    terminal: &Entity<TerminalApp>,
    instrument: InstallProviderInstrument,
    active: bool,
) -> Stateful<Div> {
    let provider = instrument.provider.clone();
    let instrument_id = instrument.instrument_id.clone();
    let remove_terminal = terminal.clone();
    let select_terminal = terminal.clone();
    let begin_terminal = terminal.clone();
    let drag = WatchlistRowDrag {
        provider: provider.clone(),
        instrument_id: instrument_id.clone(),
    };
    row.cursor_pointer()
        .role(Role::Button)
        .aria_selected(active)
        .aria_label(format!("Select {}", instrument.display_symbol))
        .on_click(move |_, _, cx| {
            select_terminal.update(cx, |terminal, terminal_cx| {
                terminal.select_watchlist_instrument(&instrument, terminal_cx);
            });
        })
        .on_drag(drag, move |drag, cursor_offset, _, cx| {
            begin_terminal.update(cx, |terminal, terminal_cx| {
                terminal.begin_watchlist_drag(
                    &drag.provider,
                    &drag.instrument_id,
                    f32::from(cursor_offset.y),
                    terminal_cx,
                );
            });
            cx.new(|_| drag.clone())
        })
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            remove_terminal.update(cx, |terminal, terminal_cx| {
                terminal.remove_watchlist_instrument(&provider, &instrument_id, terminal_cx);
            });
            cx.stop_propagation();
        })
}

fn compact_watchlist_volume(value: i64, scale: u32) -> String {
    let exponent = i32::try_from(scale).unwrap_or(i32::MAX);
    let divisor = 10_f64.powi(exponent);
    let value = value.to_f64().unwrap_or(0.0) / divisor;
    for (threshold, suffix) in [(1_000_000_000.0, "B"), (1_000_000.0, "M"), (1_000.0, "K")] {
        if value.abs() >= threshold {
            return format!("{:.2}{suffix}", value / threshold);
        }
    }
    format!("{value:.2}")
}

pub(super) fn side_panel_header(
    panel: SidePanel,
    app: Entity<WorkspaceSurface>,
    order_book_column_menu_open: bool,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let settings_app = app.clone();
    let close_id = match panel {
        SidePanel::OrderBook => "close_order_book_panel",
        SidePanel::Watchlist => "close_watchlist_panel",
    };
    div()
        .h(px(SIDE_PANEL_HEADER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .px_2()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .text_xs()
        .text_color(gpui_color(colors.text_secondary))
        .child(div().flex_1().child(panel.title().to_uppercase()))
        .children((panel == SidePanel::OrderBook).then(|| {
            chrome_tooltip(
                "order_book_column_settings",
                "Choose order-book columns",
                div()
                    .id("order_book_column_settings")
                    .occlude()
                    .size(px(WORKSPACE_TAB_ICON_HIT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
                    .text_color(gpui_color(if order_book_column_menu_open {
                        colors.icon_active
                    } else {
                        colors.icon
                    }))
                    .cursor_pointer()
                    .role(Role::Button)
                    .aria_label("Choose order-book columns")
                    .when(order_book_column_menu_open, |button| {
                        button.bg(gpui_color(colors.active_bg.over(colors.surface)))
                    })
                    .hover(move |button| {
                        button.bg(gpui_color(colors.hover_bg.over(colors.surface)))
                    })
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        settings_app.update(cx, WorkspaceSurface::toggle_order_book_column_menu);
                        cx.stop_propagation();
                    })
                    .child(header_icon(HugeIcon::Settings).with_size(px(WORKSPACE_TAB_ICON_GLYPH))),
                theme,
            )
        }))
        .children(
            (panel == SidePanel::Watchlist)
                .then(|| watchlist_add_symbol_control(app.clone(), theme)),
        )
        .child(chrome_tooltip(
            close_id,
            "Close side panel",
            chrome_close_button(close_id, theme, move |_, cx| {
                app.update(cx, |surface, surface_cx| {
                    surface.close_side_panel(panel, surface_cx);
                });
            }),
            theme,
        ))
}

fn watchlist_add_symbol_control(
    app: Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    chrome_tooltip(
        "watchlist_add_symbol",
        "Add symbol to watchlist",
        div()
            .id("watchlist_add_symbol")
            .occlude()
            .size(px(WORKSPACE_TAB_ICON_HIT))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
            .text_color(gpui_color(colors.icon))
            .cursor_pointer()
            .role(Role::Button)
            .aria_label("Add symbol to watchlist")
            .hover(move |button| button.bg(gpui_color(colors.hover_bg.over(colors.surface))))
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                app.update(cx, |surface, surface_cx| {
                    surface.open_watchlist_symbol_menu_at(event.position, window, surface_cx);
                });
                cx.stop_propagation();
            })
            .child(header_icon(HugeIcon::Add).with_size(px(WORKSPACE_TAB_ICON_GLYPH))),
        theme,
    )
}

pub(super) fn order_book_column_menu_layer(
    app: Entity<WorkspaceSurface>,
    order_book: &Entity<ReadOnlyOrderBookView>,
    columns: OrderBookColumnVisibility,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let dismiss_app = app;
    let mut panel = div()
        .id("order_book_column_menu")
        .absolute()
        .top(px(28.0))
        .right(px(30.0))
        .w(px(196.0))
        .occlude()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border_1()
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
        .text_color(gpui_color(colors.text_primary))
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation());
    let last = OrderBookColumn::ALL.len().saturating_sub(1);
    for (index, column) in OrderBookColumn::ALL.into_iter().enumerate() {
        panel = panel.child(order_book_column_menu_item(
            order_book.clone(),
            column,
            columns.is_visible(column),
            index == 0,
            index == last,
            theme,
        ));
    }
    div()
        .id("order_book_column_menu_layer")
        .absolute()
        .inset_0()
        .child(div().absolute().inset_0().occlude().on_mouse_down(
            MouseButton::Left,
            move |_, _, cx| {
                dismiss_app.update(cx, WorkspaceSurface::close_order_book_column_menu);
                cx.stop_propagation();
            },
        ))
        .child(animate_popup_from_origin(
            panel,
            "order_book_column_menu_enter",
            PopupAnimationOrigin::TOP_RIGHT,
        ))
}

pub(super) fn order_book_column_menu_item(
    order_book: Entity<ReadOnlyOrderBookView>,
    column: OrderBookColumn,
    checked: bool,
    first: bool,
    last: bool,
    theme: &AerisTheme,
) -> impl IntoElement {
    let available = column.available();
    let label = if available {
        column.label().to_string()
    } else {
        "P/L · routing unavailable".to_string()
    };
    let id = match column {
        OrderBookColumn::ProfitLoss => "order_book_column_profit_loss",
        OrderBookColumn::Bid => "order_book_column_bid",
        OrderBookColumn::SellTrades => "order_book_column_sell_trades",
        OrderBookColumn::Price => "order_book_column_price",
        OrderBookColumn::BuyTrades => "order_book_column_buy_trades",
        OrderBookColumn::Ask => "order_book_column_ask",
        OrderBookColumn::Orders => "order_book_column_orders",
    };
    let item_order_book = order_book;
    let mut item = MenuRow::compact(id, label, theme)
        .disabled(!available)
        .flush_in_panel(first, last)
        .on_click(move |_, _, cx| {
            item_order_book.update(cx, |order_book, order_book_cx| {
                order_book.toggle_column(column, order_book_cx);
            });
        });
    if checked {
        item = item.trailing(
            header_icon(HugeIcon::CheckIcon)
                .with_size(px(16.0))
                .color(gpui_color(theme.colors.icon)),
        );
    }
    item
}
pub(super) fn chart_notice(
    notice: ChartSurfaceNotice,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    if notice.label == ChartState::Loading.label() {
        if notice.placement != ChartNoticePlacement::Center {
            // A repair behind the chart the trader is still reading is announced by
            // the symbol legend's own spinner, beside the symbol it belongs to. A
            // second one in the corner lands on top of that legend.
            return div().into_any_element();
        }
        let spinner = Loader::from_path("chart_notice_loader", HugeIcon::Loader.path())
            .with_size(px(40.0))
            .color(gpui_color(colors.icon));
        let overlay = div()
            .id("chart_loading_status")
            .absolute()
            .occlude()
            .role(Role::Status)
            .aria_label(notice.label)
            // A loading surface is deliberately opaque. On first launch there
            // is no chart to read, and during a switch the retained chart belongs
            // to the previous selection.
            .bg(gpui_color(colors.surface))
            .gap_2()
            .child(spinner)
            .child(
                div()
                    .text_sm()
                    .text_color(gpui_color(colors.text_primary))
                    .child(notice.label),
            )
            .children(notice.detail.clone().map(|detail| {
                div()
                    .text_xs()
                    .text_color(gpui_color(colors.text_secondary))
                    .child(detail)
            }));
        return overlay
            .inset_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .into_any_element();
    }
    let tone = match notice.tone {
        ChartNoticeTone::Muted => colors.text_secondary,
        ChartNoticeTone::Warning => colors.warning,
        ChartNoticeTone::Loss => colors.danger,
    };
    let label = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_2()
        .py_1()
        .border_1()
        .rounded(px(f32::from(
            chart_chrome::CHART_SURFACE_RADIUS.logical_pixels(),
        )))
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface.with_alpha(0.94)))
        .text_xs()
        .text_color(gpui_color(tone))
        .child(notice.label)
        .children(notice.detail.map(|detail| {
            div()
                .text_color(gpui_color(colors.text_secondary))
                .child(detail)
        }));
    match notice.placement {
        ChartNoticePlacement::Center => div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .child(label)
            .into_any_element(),
        ChartNoticePlacement::BottomRight => div()
            .absolute()
            .right_2()
            .bottom_2()
            .child(label)
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trading_account(
        id: &str,
        environment: aeris_trading::AccountEnvironment,
    ) -> aeris_trading::TradingAccount {
        aeris_trading::TradingAccount {
            id: aeris_trading::TradingAccountId::try_new(id).expect("account id"),
            display_name: match environment {
                aeris_trading::AccountEnvironment::Simulated => format!("SIM {id}"),
                aeris_trading::AccountEnvironment::Live => format!("LIVE {id}"),
            },
            environment,
            currency: "USD".to_string(),
            currency_scale: 2,
        }
    }

    #[test]
    fn simultaneous_side_panels_allocate_two_docked_columns() {
        assert!((side_panel_total_width(400.0, false) - 400.0).abs() < f32::EPSILON);
        assert!(
            (side_panel_total_width(400.0, true) - (800.0 + SIDE_PANEL_SPLIT_DIVIDER_WIDTH)).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn horizontal_split_keeps_both_panels_renderable() {
        assert!((side_panel_horizontal_ratio(400.0, 5_000) - 0.5).abs() < f32::EPSILON);
        assert!((side_panel_horizontal_ratio(400.0, 500) - 0.45).abs() < f32::EPSILON);
        assert!((side_panel_horizontal_ratio(400.0, 9_500) - 0.55).abs() < f32::EPSILON);
        assert!((side_panel_horizontal_ratio(480.0, 500) - 0.5).abs() < f32::EPSILON);
        assert!((side_panel_horizontal_ratio(480.0, 9_500) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn copier_configuration_keeps_same_mode_targets_and_advances_revision() {
        let accounts = vec![
            trading_account("source", aeris_trading::AccountEnvironment::Simulated),
            trading_account("target-a", aeris_trading::AccountEnvironment::Simulated),
            trading_account("target-b", aeris_trading::AccountEnvironment::Simulated),
            trading_account("live", aeris_trading::AccountEnvironment::Live),
        ];
        let current = aeris_trading_runtime::TradeCopierConfig {
            source_account_id: accounts[0].id.clone(),
            revision: 7,
            enabled: true,
            targets: vec![aeris_trading_runtime::TradeCopierTarget {
                account_id: accounts[1].id.clone(),
                quantity_multiplier: aeris_trading::FixedPoint::try_new(3, 0).expect("multiplier"),
                enabled: false,
            }],
        };
        let config =
            reconciled_trade_copier_config(&accounts, std::slice::from_ref(&current), &accounts[0])
                .expect("copier config");

        assert_eq!(config.revision, 8);
        assert!(config.enabled);
        assert_eq!(config.targets.len(), 2);
        assert_eq!(config.targets[0], current.targets[0]);
        assert_eq!(config.targets[1].account_id, accounts[2].id);
        assert_eq!(config.targets[1].quantity_multiplier.units(), 1);
        assert!(config.targets[1].enabled);
        assert!(
            config
                .targets
                .iter()
                .all(|target| target.account_id != accounts[3].id)
        );
    }

    #[test]
    fn copier_multiplier_cycles_through_exact_whole_contract_values() {
        let one = aeris_trading::FixedPoint::try_new(1, 0).expect("one");
        let two = next_copier_multiplier(one);
        let three = next_copier_multiplier(two);
        assert_eq!(two.units(), 2);
        assert_eq!(three.units(), 3);
        assert_eq!(next_copier_multiplier(three), one);
    }

    #[test]
    fn copier_error_presentation_is_bounded() {
        let detail = bounded_copier_detail(&"x".repeat(100));
        assert_eq!(detail.chars().count(), 65);
        assert!(detail.ends_with('…'));
    }
}
