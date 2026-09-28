//! Order ticket: account, risk, copier, working orders and order entry controls.

use super::*;
use gpui::Stateful;

#[derive(Clone, Copy)]
pub(super) struct TradingOrderControlsState<'a> {
    pub(super) app: &'a Entity<WorkspaceSurface>,
    pub(super) frame: Option<&'a aeris_market_data::OrderBookFrame>,
    pub(super) trading_pnl: Option<&'a aeris_trading::AccountPnl>,
    pub(super) accounts: &'a [aeris_trading::TradingAccount],
    pub(super) orders: &'a [aeris_trading::Order],
    pub(super) positions: &'a [aeris_trading_runtime::PositionPnl],
    pub(super) risk_meters: &'a [aeris_trading_runtime::RiskMeter],
    pub(super) risk_locks: &'a [aeris_trading_runtime::RiskLock],
    pub(super) session_plans: &'a [aeris_trading_runtime::SessionPlan],
    pub(super) session_reviews: &'a [aeris_trading_runtime::SessionAdherenceReview],
    pub(super) trade_copiers: &'a [aeris_trading_runtime::TradeCopierConfig],
    pub(super) copy_dispatches: &'a [aeris_trading_runtime::TradeCopyDispatch],
    pub(super) strategy_templates: &'a [aeris_trading_runtime::BracketStrategyTemplate],
    pub(super) managed_brackets: &'a [aeris_trading_runtime::ManagedBracket],
    pub(super) feedback: Option<&'a aeris_desktop::trading::TradingCommandFeedback>,
    pub(super) market_error: Option<&'a str>,
    pub(super) order_entry: &'a super::TradingOrderEntryState,
    pub(super) theme: &'a AerisTheme,
}

/// Fixed label column shared by every account, ticket and action row so values align.
const TRADING_FIELD_LABEL_WIDTH: f32 = 44.0;

const TRADING_FIELD_HEIGHT: f32 = 22.0;

const TRADING_CONTROL_HEIGHT: f32 = 22.0;

const TRADING_SECTION_GAP: f32 = 2.0;

pub(super) fn trading_order_controls(
    state: &TradingOrderControlsState<'_>,
) -> impl IntoElement + use<> {
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
        feedback,
        market_error,
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
        .flex_none()
        .border_t_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .child(
            trading_section()
                .child(trading_account_selector(app, accounts, order_entry, theme))
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
                )),
        )
        .child(
            trading_section()
                .border_t_1()
                .border_color(gpui_color(colors.border))
                .child(trading_order_entry(
                    app,
                    strategy_templates,
                    managed_brackets,
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
                .children(trading_status_rows(feedback, market_error, theme)),
        )
        .child(
            trading_section()
                .border_t_1()
                .border_color(gpui_color(colors.border))
                .child(order_management_buttons(
                    frame,
                    risk_locks,
                    order_entry,
                    theme,
                )),
        )
}

/// Latest command outcome, plus any failure applying live prices to the simulated venue.
fn trading_status_rows(
    feedback: Option<&aeris_desktop::trading::TradingCommandFeedback>,
    market_error: Option<&str>,
    theme: &AerisTheme,
) -> Vec<Div> {
    let status = feedback.map(|feedback| {
        trading_field(
            "Status",
            trading_field_text(
                feedback.message.clone(),
                if feedback.is_error {
                    theme.colors.danger
                } else {
                    theme.colors.text_secondary
                },
            ),
            theme,
        )
    });
    let venue = market_error.map(|error| {
        trading_field(
            "Venue",
            trading_field_text(error.to_string(), theme.colors.danger),
            theme,
        )
    });
    status.into_iter().chain(venue).collect()
}

fn trading_section() -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(TRADING_SECTION_GAP))
        .px_2()
        .py(px(6.0))
}

/// One aligned `label | value` row of the trading controls.
fn trading_field(label: &'static str, value: impl IntoElement, theme: &AerisTheme) -> Div {
    div()
        .min_h(px(TRADING_FIELD_HEIGHT))
        .flex()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(TRADING_FIELD_LABEL_WIDTH))
                .flex_none()
                .text_color(gpui_color(theme.colors.text_muted))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_1()
                .child(value),
        )
}

/// A value-column row nested under a field, such as a copier target or working order.
fn trading_field_detail() -> Div {
    div()
        .h(px(20.0))
        .pl(px(TRADING_FIELD_LABEL_WIDTH + 8.0))
        .flex()
        .items_center()
        .gap_1()
}

fn trading_field_text(
    text: impl Into<SharedString>,
    color: aeris_design_system::ThemeColor,
) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .truncate()
        .text_color(gpui_color(color))
        .child(text.into())
}

fn trading_text_action<T: Into<gpui::ElementId>>(
    id: T,
    label: &'static str,
    aria_label: &'static str,
    color: aeris_design_system::ThemeColor,
    colors: &aeris_design_system::ThemeColors,
) -> Stateful<Div> {
    let hover = colors.hover_bg;
    div()
        .id(id)
        .flex_none()
        .h(px(18.0))
        .px_1()
        .flex()
        .items_center()
        .rounded(px(3.0))
        .text_color(gpui_color(color))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(aria_label)
        .hover(move |button| button.bg(gpui_color(hover)))
        .child(label)
}

fn trading_order_entry(
    app: &Entity<WorkspaceSurface>,
    strategy_templates: &[aeris_trading_runtime::BracketStrategyTemplate],
    managed_brackets: &[aeris_trading_runtime::ManagedBracket],
    order_entry: &super::TradingOrderEntryState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let quantity_buttons = trading_quantity_buttons(app, order_entry.quantity, theme);
    let type_buttons = trading_order_type_buttons(app, order_entry.order_type, theme);
    let tif_buttons = trading_time_in_force_buttons(app, order_entry.time_in_force, theme);
    let strategy_selector = trading_strategy_selector(
        app,
        strategy_templates,
        managed_brackets,
        order_entry,
        theme,
    );
    div()
        .flex()
        .flex_col()
        .gap(px(TRADING_SECTION_GAP))
        .child(trading_field("Qty", quantity_buttons, theme))
        .child(trading_field("Type", type_buttons, theme))
        .child(trading_field("TIF", tif_buttons, theme))
        .child(trading_field("Bracket", strategy_selector, theme))
}

fn trading_strategy_selector(
    app: &Entity<WorkspaceSurface>,
    templates: &[aeris_trading_runtime::BracketStrategyTemplate],
    brackets: &[aeris_trading_runtime::ManagedBracket],
    order_entry: &super::TradingOrderEntryState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
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
    let label = selected.map_or_else(|| "Off".to_string(), |template| template.name.clone());
    let detail = selected.map_or_else(
        || format!("{active} active"),
        |_| {
            format!(
                "{} · {active} active",
                aeris_trading_runtime::ManagedBracket::MANAGEMENT_LABEL.to_lowercase()
            )
        },
    );
    let strategy_app = (*app).clone();
    let hover = colors.hover_bg;
    div()
        .id("trading_strategy_selector")
        .flex_1()
        .min_w_0()
        .h(px(TRADING_CONTROL_HEIGHT))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .rounded(px(3.0))
        .bg(gpui_color(colors.surface_secondary))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label("Select a locally managed bracket strategy")
        .hover(move |button| button.bg(gpui_color(hover)))
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
        .child(trading_field_text(label, colors.text_primary))
        .child(
            div()
                .flex_none()
                .text_color(gpui_color(colors.text_muted))
                .child(detail),
        )
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
    let (account_label, account_color) = selected_account.map_or_else(
        || ("Awaiting runtime".to_string(), colors.text_muted),
        |account| (account.display_name.clone(), colors.text_primary),
    );
    let account_app = (*app).clone();
    let account_count = accounts.len();
    let hover = colors.hover_bg;
    let selector = div()
        .id("trading_account_selector")
        .flex_1()
        .min_w_0()
        .h(px(TRADING_CONTROL_HEIGHT))
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .rounded(px(3.0))
        .bg(gpui_color(colors.surface_secondary))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label("Select the simulated trading account")
        .hover(move |button| button.bg(gpui_color(hover)))
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
        .child(trading_field_text(account_label, account_color))
        .children((account_count > 1).then(|| {
            div()
                .flex_none()
                .text_color(gpui_color(colors.text_muted))
                .child(format!("{account_count} accounts"))
        }));
    trading_field("Acct", selector, theme)
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
    trading_segmented_control(&colors).children(buttons)
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
    trading_segmented_control(&colors).children(buttons)
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
    trading_segmented_control(&colors).children(buttons)
}

/// Full-width track whose equal segments keep every ticket row on the same grid.
fn trading_segmented_control(colors: &aeris_design_system::ThemeColors) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .h(px(TRADING_CONTROL_HEIGHT))
        .flex()
        .gap(px(2.0))
        .p(px(2.0))
        .rounded(px(4.0))
        .bg(gpui_color(colors.surface_secondary))
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
    let hover = colors.hover_bg;
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .when(selected, |button| button.bg(gpui_color(colors.primary)))
        .when(!selected, move |button| {
            button.hover(move |button| button.bg(gpui_color(hover)))
        })
        .text_color(gpui_color(if selected {
            colors.surface
        } else {
            colors.text_secondary
        }))
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
        || ("Awaiting runtime".to_string(), colors.text_muted),
        |pnl| {
            let total = pnl.realized.units().saturating_add(pnl.unrealized.units());
            let label = format!(
                "{}{} realized · {}{} open",
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
        || "Flat or awaiting mark".to_string(),
        |position| {
            let realized = position
                .realized_ticks
                .map_or_else(|| "n/a".to_string(), format_fixed_point);
            let unrealized = position
                .unrealized_ticks
                .map_or_else(|| "n/a".to_string(), format_fixed_point);
            format!(
                "{} · R {}t · U {}t",
                format_trimmed_fixed_point(position.position.net_quantity),
                realized,
                unrealized,
            )
        },
    );
    div()
        .flex()
        .flex_col()
        .gap(px(TRADING_SECTION_GAP))
        .child(trading_field(
            "P/L",
            trading_field_text(label, color),
            theme,
        ))
        .child(trading_field(
            "Pos",
            trading_field_text(position_label, colors.text_secondary),
            theme,
        ))
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
                || ("No profile".to_string(), colors.text_muted),
                |meter| {
                    if !meter.enabled {
                        return ("Disabled".to_string(), colors.text_muted);
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
                        "Loss {} · ctr {}{}{}{}",
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
        |lock| (format!("Locked · {}", lock.reason), colors.danger),
    );
    trading_field("Rule", trading_field_text(label, color), theme)
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
        || ("None".to_string(), colors.text_muted),
        |plan| {
            let bias = match plan.bias {
                aeris_trading_runtime::SessionBias::Long => "Long",
                aeris_trading_runtime::SessionBias::Short => "Short",
                aeris_trading_runtime::SessionBias::Neutral => "Neutral",
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
                format!("{bias} · {setup}{adherence}"),
                if has_violation {
                    colors.danger
                } else {
                    colors.text_muted
                },
            )
        },
    );
    trading_field("Plan", trading_field_text(label, color), theme)
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
        let (label, color) = if config.enabled {
            ("Enable", colors.primary)
        } else {
            ("Pause", colors.text_secondary)
        };
        trading_text_action(
            "trade_copier_toggle",
            label,
            "Enable or pause copying from the selected account",
            color,
            &colors,
        )
        .on_click(move |_, _, cx| {
            aeris_desktop::trading::register_trade_copier(config.clone(), cx);
        })
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
        .child(trading_field(
            "Copy",
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_1()
                .child(trading_field_text(status, status_color))
                .children(toggle),
            theme,
        ))
        .children(target_rows)
        .children(last_dispatch.map(|label| {
            trading_field_detail().child(trading_field_text(label, colors.text_muted))
        }))
}

fn trade_copier_status(
    source_selected: bool,
    current: Option<&aeris_trading_runtime::TradeCopierConfig>,
    eligible_targets: usize,
) -> String {
    match (source_selected, current, eligible_targets) {
        (false, _, _) => "Select an account".to_string(),
        (true, _, 0) => "No same-mode target accounts".to_string(),
        (true, Some(config), _) if config.enabled => {
            format!("On · {} targets", config.targets.len())
        }
        (true, Some(config), _) => format!("Off · {} targets", config.targets.len()),
        (true, None, _) => format!("Off · {eligible_targets} targets available"),
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
        format!("Last · {target} accepted")
    } else {
        let detail = dispatch.detail.as_deref().unwrap_or("rejected");
        format!("Last · {target} · {}", bounded_copier_detail(detail))
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
    let hover = colors.hover_bg;
    trading_field_detail()
        .id(("trade_copier_target", index))
        .child(trading_field_text(account_label, colors.text_secondary))
        .child(
            div()
                .id(("trade_copier_multiplier", index))
                .flex_none()
                .h(px(18.0))
                .px_1()
                .flex()
                .items_center()
                .rounded(px(3.0))
                .cursor_pointer()
                .role(Role::Button)
                .aria_label("Change this account's trade-copy quantity multiplier")
                .text_color(gpui_color(colors.primary))
                .hover(move |button| button.bg(gpui_color(hover)))
                .on_click(move |_, _, cx| {
                    aeris_desktop::trading::register_trade_copier(multiplier_config.clone(), cx);
                })
                .child(multiplier_label),
        )
        .child(
            trading_text_action(
                ("trade_copier_target_toggle", index),
                if enabled { "Live" } else { "Paused" },
                "Enable or pause trade copying to this account",
                if enabled {
                    colors.bullish
                } else {
                    colors.danger
                },
                colors,
            )
            .on_click(move |_, _, cx| {
                aeris_desktop::trading::register_trade_copier(toggle_config.clone(), cx);
            }),
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
    let (header, header_color) = if rows.is_empty() {
        ("None".to_string(), colors.text_muted)
    } else {
        (format!("{} working", rows.len()), colors.text_primary)
    };
    div()
        .flex()
        .flex_col()
        .child(trading_field(
            "Orders",
            trading_field_text(header, header_color),
            theme,
        ))
        .children(rows)
}

fn working_order_row(
    order: &aeris_trading::Order,
    index: usize,
    frame: Option<&aeris_market_data::OrderBookFrame>,
    order_entry_locked: bool,
    colors: &aeris_design_system::ThemeColors,
) -> impl IntoElement + use<> {
    let (direction, direction_color) = match order.side {
        aeris_trading::OrderSide::Buy => ("Buy", colors.bullish),
        aeris_trading::OrderSide::Sell => ("Sell", colors.bearish),
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
        .map_or_else(String::new, |price| {
            format_trimmed_fixed_point(price)
                .trim_start_matches('+')
                .to_string()
        });
    let label = format!(
        "{direction} {instruction} {}{}",
        format_trimmed_fixed_point(order.quantity).trim_start_matches('+'),
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
            trading_text_action(
                ("reprice_working_order", index),
                "Reprice",
                "Reprice working simulated limit order",
                colors.primary,
                colors,
            )
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
        });
    trading_field_detail()
        .id(("working_order", index))
        .child(trading_field_text(label, direction_color))
        .children(reprice_button)
        .child(
            trading_text_action(
                ("cancel_working_order", index),
                "Cancel",
                "Cancel working simulated order",
                colors.danger,
                colors,
            )
            .on_click(move |_, _, cx| {
                aeris_desktop::trading::cancel_simulated_order(client_order_key.clone(), cx);
            }),
        )
}

/// Quantities and prices arrive at provider storage scale (eight places for Hyperliquid), so
/// display drops trailing fractional zeros. Money keeps its currency scale via
/// [`format_fixed_point`].
fn format_trimmed_fixed_point(value: aeris_trading::FixedPoint) -> String {
    let formatted = format_fixed_point(value);
    if formatted.contains('.') {
        formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    } else {
        formatted
    }
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
    let quantity = order_entry.quantity;
    let order = TradingOrderButton {
        frame,
        order_entry,
        order_type: order_entry.order_type,
        locked: order_entry_locked,
        prominent: true,
        theme,
    };
    div()
        .flex()
        .gap_1()
        .child(order.render(
            "trading_buy_market",
            format!("Buy {quantity}"),
            "Buy the selected simulated order",
            aeris_trading::OrderSide::Buy,
        ))
        .child(order.render(
            "trading_sell_market",
            format!("Sell {quantity}"),
            "Sell the selected simulated order",
            aeris_trading::OrderSide::Sell,
        ))
}

fn book_order_buttons(
    frame: Option<&aeris_market_data::OrderBookFrame>,
    order_entry: &super::TradingOrderEntryState,
    order_entry_locked: bool,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let order = TradingOrderButton {
        frame,
        order_entry,
        order_type: aeris_trading::OrderType::Limit,
        locked: order_entry_locked,
        prominent: false,
        theme,
    };
    div()
        .flex()
        .gap_1()
        .child(order.render(
            "trading_buy_ask",
            best_book_order_label(frame, OrderBookSide::Ask),
            "Buy the selected simulated order at the best ask",
            aeris_trading::OrderSide::Buy,
        ))
        .child(order.render(
            "trading_sell_bid",
            best_book_order_label(frame, OrderBookSide::Bid),
            "Sell the selected simulated order at the best bid",
            aeris_trading::OrderSide::Sell,
        ))
}

/// Shared order-ticket submit button: prominent market-style fills, or tinted touch-price limits.
#[derive(Clone, Copy)]
struct TradingOrderButton<'a> {
    frame: Option<&'a aeris_market_data::OrderBookFrame>,
    order_entry: &'a super::TradingOrderEntryState,
    order_type: aeris_trading::OrderType,
    locked: bool,
    prominent: bool,
    theme: &'a AerisTheme,
}

impl TradingOrderButton<'_> {
    fn render(
        self,
        id: &'static str,
        label: String,
        aria_label: &'static str,
        side: aeris_trading::OrderSide,
    ) -> Stateful<Div> {
        let colors = self.theme.colors;
        let tone = match side {
            aeris_trading::OrderSide::Buy => colors.bullish,
            aeris_trading::OrderSide::Sell => colors.bearish,
        };
        let (background, foreground) = match (self.locked, self.prominent) {
            (true, _) => (colors.hover_bg, colors.text_muted),
            (false, true) => (tone, colors.surface),
            (false, false) => (tone.with_alpha(0.16), tone),
        };
        let frame = self.frame.cloned();
        let account_key = self
            .order_entry
            .selected_account_id
            .as_ref()
            .map(|id| id.as_str().to_string());
        let template_id = self.order_entry.selected_strategy_template_id.clone();
        let (quantity, order_type, time_in_force) = (
            self.order_entry.quantity,
            self.order_type,
            self.order_entry.time_in_force,
        );
        div()
            .id(id)
            .flex_1()
            .min_w_0()
            .h(px(if self.prominent {
                28.0
            } else {
                TRADING_CONTROL_HEIGHT
            }))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.0))
            .bg(gpui_color(background))
            .text_color(gpui_color(foreground))
            .when(self.prominent, |button| {
                button.font_weight(gpui::FontWeight::SEMIBOLD)
            })
            .role(Role::Button)
            .aria_label(aria_label)
            .when(self.locked, gpui::Styled::cursor_not_allowed)
            .when(!self.locked, move |button| {
                button.cursor_pointer().on_click(move |_, _, cx| {
                    if let Some(frame) = frame.clone() {
                        aeris_desktop::trading::dispatch_simulated_selected_order(
                            &frame,
                            side,
                            aeris_desktop::trading::SimulatedOrderSelection {
                                account_key: account_key.clone(),
                                quantity,
                                order_type,
                                time_in_force,
                                template_id: template_id.clone(),
                            },
                            cx,
                        );
                    }
                })
            })
            .child(label)
    }
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
        OrderBookSide::Bid => ("Sell bid", frame.and_then(|book| book.best_bid.as_ref())),
        OrderBookSide::Ask => ("Buy ask", frame.and_then(|book| book.best_ask.as_ref())),
    };
    level.map_or_else(
        || prefix.to_string(),
        |level| format!("{prefix} {}", level.price_text),
    )
}

fn order_management_buttons(
    frame: Option<&aeris_market_data::OrderBookFrame>,
    risk_locks: &[aeris_trading_runtime::RiskLock],
    order_entry: &super::TradingOrderEntryState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let account_key = order_entry
        .selected_account_id
        .as_ref()
        .map(|id| id.as_str().to_string());
    let account_locked = order_entry
        .selected_account_id
        .as_ref()
        .is_some_and(|account_id| risk_locks.iter().any(|lock| &lock.account_id == account_id));
    let row = |label: &'static str, buttons: [Stateful<Div>; 3]| {
        trading_field(
            label,
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .gap(px(TRADING_SECTION_GAP))
                .children(buttons),
            theme,
        )
    };
    div()
        .flex()
        .flex_col()
        .gap(px(TRADING_SECTION_GAP))
        .child(row(
            "Acct",
            [
                cancel_account_button(account_key.clone(), &colors),
                flatten_account_button(frame.cloned(), account_key.clone(), &colors),
                account_lock_button(account_key, account_locked, &colors),
            ],
        ))
        .child(row(
            "All",
            [
                cancel_all_button(&colors),
                flatten_all_button(frame.cloned(), &colors),
                kill_all_button(&colors),
            ],
        ))
}

/// Equal-width tinted action; the tone carries severity without a wall of solid fills.
fn trading_management_button(
    id: &'static str,
    label: &'static str,
    aria_label: &'static str,
    tone: aeris_design_system::ThemeColor,
) -> Stateful<Div> {
    let background = tone.with_alpha(0.12);
    let hover = tone.with_alpha(0.22);
    div()
        .id(id)
        .flex_1()
        .min_w_0()
        .h(px(TRADING_CONTROL_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.0))
        .bg(gpui_color(background))
        .hover(move |button| button.bg(gpui_color(hover)))
        .text_color(gpui_color(tone))
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
        "Cancel",
        "Cancel simulated orders for the selected account",
        colors.text_secondary,
    )
    .on_click(move |_, _, cx| {
        aeris_desktop::trading::cancel_simulated_account(account_key.clone(), cx);
    })
}

fn cancel_all_button(colors: &aeris_design_system::ThemeColors) -> Stateful<Div> {
    trading_management_button(
        "trading_cancel_all",
        "Cancel",
        "Cancel simulated orders for every account",
        colors.text_secondary,
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
        "Flatten",
        "Flatten the selected simulated account",
        colors.warning,
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
        "Flatten",
        "Flatten all simulated accounts",
        colors.warning,
    )
    .on_click(move |_, _, cx| {
        if let Some(frame) = frame.clone() {
            aeris_desktop::trading::flatten_simulated_accounts(&frame, cx);
        }
    })
}

fn account_lock_button(
    account_key: Option<String>,
    locked: bool,
    colors: &aeris_design_system::ThemeColors,
) -> Stateful<Div> {
    if locked {
        trading_management_button(
            "trading_unlock_account",
            "Unlock",
            "Unlock the selected simulated account",
            colors.positive,
        )
        .on_click(move |_, _, cx| {
            aeris_desktop::trading::unlock_simulated_account(account_key.clone(), cx);
        })
    } else {
        trading_management_button(
            "trading_kill_switch_account",
            "Kill",
            "Lock the selected simulated account",
            colors.danger,
        )
        .on_click(move |_, _, cx| {
            aeris_desktop::trading::kill_simulated_account(account_key.clone(), cx);
        })
    }
}

fn kill_all_button(colors: &aeris_design_system::ThemeColors) -> Stateful<Div> {
    trading_management_button(
        "trading_kill_switch",
        "Kill",
        "Lock every simulated account",
        colors.danger,
    )
    .on_click(move |_, _, cx| {
        aeris_desktop::trading::kill_simulated_accounts(cx);
    })
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
    fn storage_scale_quantities_and_prices_display_without_trailing_zeros() {
        let value = |units, scale| aeris_trading::FixedPoint::try_new(units, scale).expect("value");
        assert_eq!(
            super::format_trimmed_fixed_point(value(100_000_000, 8)),
            "+1"
        );
        assert_eq!(
            super::format_trimmed_fixed_point(value(8_370_400_000_000, 8)),
            "+83704"
        );
        assert_eq!(
            super::format_trimmed_fixed_point(value(-12_000, 8)),
            "-0.00012"
        );
        assert_eq!(super::format_fixed_point(value(0, 2)), "+0.00");
    }

    #[test]
    fn copier_error_presentation_is_bounded() {
        let detail = bounded_copier_detail(&"x".repeat(100));
        assert_eq!(detail.chars().count(), 65);
        assert!(detail.ends_with('…'));
    }
}
