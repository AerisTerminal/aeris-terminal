use super::*;
use crate::desktop::workspace_surface::OrderFlowMenuStudy;

use super::chrome_menu::{
    CHROME_MENU_SEARCH_HEIGHT, ChromeMenuExtent, chrome_menu_empty, chrome_menu_group_heading,
    chrome_menu_scroll_body, chrome_menu_search_header, chrome_menu_surface,
    compact_menu_add_button, scrollable_menu_body,
};

pub(super) fn indicator_selector(
    app: Entity<WorkspaceSurface>,
    _input: Entity<InputState>,
    _message: Option<String>,
    enabled: bool,
    theme: &AerisTheme,
) -> impl IntoElement {
    let trigger = Button::new("indicator_selector")
        .icon(header_icon(HugeIcon::Chart))
        .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
        .w(px(chart_chrome::CHART_CONTROL_SIZE))
        .h(px(chart_chrome::CHART_CONTROL_SIZE))
        .rounded(px(f32::from(
            chart_chrome::CHART_CONTROL_RADIUS.logical_pixels(),
        )))
        .disabled(!enabled)
        .when(enabled, Button::cursor_pointer)
        .when(!enabled, Button::cursor_not_allowed);
    chrome_tooltip(
        "indicator_selector",
        "Indicators",
        button_activation_at(
            chrome_button_style(trigger, theme, false, enabled),
            enabled,
            move |trigger_position, window, cx| {
                app.update(cx, |app, app_cx| {
                    app.open_chrome_overlay_at(
                        ChromeOverlay::Indicator,
                        trigger_position,
                        window,
                        app_cx,
                    );
                });
            },
        ),
        theme,
    )
}

#[derive(Clone, Copy)]
pub(super) struct IndicatorDialogState<'a> {
    pub(super) extent: ChromeMenuExtent,
    pub(super) input: &'a Entity<InputState>,
    pub(super) message: Option<&'a str>,
    pub(super) keyboard_selection: usize,
    pub(super) scroll: &'a ScrollHandle,
}

fn available_indicator_rows(
    app: &Entity<WorkspaceSurface>,
    specs: &[&chart_chrome::IndicatorSpec],
    keyboard_selection: usize,
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    specs
        .iter()
        .enumerate()
        .map(|(index, spec)| {
            let row_app = app.clone();
            let add_app = app.clone();
            let indicator = native_indicator(spec.kind);
            MenuRow::search_result(("indicator_dialog_row", index), spec.label, theme)
                .highlighted(keyboard_selection == index)
                .on_click(move |_, window, cx| {
                    if row_app.update(cx, |app, cx| app.add_indicator(indicator, cx)) {
                        row_app.update(cx, |app, app_cx| {
                            app.close_chrome_overlay(window, app_cx);
                        });
                    }
                })
                .trailing(button_activation(
                    compact_menu_add_button(("add_indicator", index), theme),
                    true,
                    move |window, cx| {
                        if add_app.update(cx, |app, cx| app.add_indicator(indicator, cx)) {
                            add_app.update(cx, |app, app_cx| {
                                app.close_chrome_overlay(window, app_cx);
                            });
                        }
                    },
                ))
                .into_any_element()
        })
        .collect()
}

pub(super) fn indicator_dialog_content(
    app: &Entity<WorkspaceSurface>,
    state: IndicatorDialogState<'_>,
    theme: &AerisTheme,
    cx: &App,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let indicator_specs =
        chart_chrome::filter_indicator_specs(state.input.read(cx).value().as_ref());
    let hint = state.message.map_or_else(
        || format!("{} native", indicator_specs.len()),
        str::to_string,
    );
    let query = state.input.read(cx).value().to_ascii_lowercase();
    let order_flow_studies = order_flow_menu_studies(app, &query, cx);
    let mut list = chrome_menu_scroll_body();
    if !order_flow_studies.is_empty() {
        list = list
            .child(chrome_menu_group_heading("Order flow", &colors))
            .children(order_flow_study_rows(app, &order_flow_studies, theme));
    }
    if indicator_specs.is_empty() && order_flow_studies.is_empty() {
        list = list.child(chrome_menu_empty(
            "No matching indicators",
            "Try “average”, “bands”, or a kind like SMA.",
            &colors,
        ));
    } else if !indicator_specs.is_empty() {
        list = list
            .child(chrome_menu_group_heading("Indicators", &colors))
            .children(available_indicator_rows(
                app,
                &indicator_specs,
                state.keyboard_selection,
                theme,
            ));
    }
    chrome_menu_surface(&colors, state.extent)
        .child(chrome_menu_search_header(
            state.input,
            theme,
            app,
            hint,
            CHROME_MENU_SEARCH_HEIGHT,
        ))
        .child(scrollable_menu_body(
            list,
            state.scroll,
            colors.text_secondary,
            state.extent,
        ))
}

/// Tape-derived study panes that are currently hidden and match the search.
fn order_flow_menu_studies(
    app: &Entity<WorkspaceSurface>,
    query: &str,
    cx: &App,
) -> Vec<OrderFlowMenuStudy> {
    let surface = app.read(cx);
    let Some(settings) = surface.chart_order_flow_settings(cx) else {
        return Vec::new();
    };
    OrderFlowMenuStudy::ALL
        .into_iter()
        .filter(|study| !study.is_shown(settings))
        .filter(|study| {
            query.trim().is_empty() || study.label().to_ascii_lowercase().contains(query.trim())
        })
        .collect()
}

fn order_flow_study_rows(
    app: &Entity<WorkspaceSurface>,
    studies: &[OrderFlowMenuStudy],
    theme: &AerisTheme,
) -> Vec<AnyElement> {
    studies
        .iter()
        .enumerate()
        .map(|(index, &study)| {
            let row_app = app.clone();
            let add_app = app.clone();
            MenuRow::search_result(("order_flow_study_row", index), study.label(), theme)
                .on_click(move |_, window, cx| {
                    if row_app.update(cx, |app, cx| app.add_order_flow_study(study, cx)) {
                        row_app.update(cx, |app, app_cx| {
                            app.close_chrome_overlay(window, app_cx);
                        });
                    }
                })
                .trailing(button_activation(
                    compact_menu_add_button(("add_order_flow_study", index), theme),
                    true,
                    move |window, cx| {
                        if add_app.update(cx, |app, cx| app.add_order_flow_study(study, cx)) {
                            add_app.update(cx, |app, app_cx| {
                                app.close_chrome_overlay(window, app_cx);
                            });
                        }
                    },
                ))
                .into_any_element()
        })
        .collect()
}

pub(super) const fn native_indicator(kind: chart_chrome::IndicatorKind) -> ChartIndicator {
    match kind {
        chart_chrome::IndicatorKind::Sma => ChartIndicator::Sma,
        chart_chrome::IndicatorKind::Ema => ChartIndicator::Ema,
        chart_chrome::IndicatorKind::EmaRibbon => ChartIndicator::EmaRibbon,
        chart_chrome::IndicatorKind::Wma => ChartIndicator::Wma,
        chart_chrome::IndicatorKind::BollingerBands => ChartIndicator::Bollinger,
        chart_chrome::IndicatorKind::Vwap => ChartIndicator::Vwap,
        chart_chrome::IndicatorKind::Volume => ChartIndicator::Volume,
        chart_chrome::IndicatorKind::Rsi => ChartIndicator::Rsi,
        chart_chrome::IndicatorKind::Macd => ChartIndicator::Macd,
        chart_chrome::IndicatorKind::Stochastic => ChartIndicator::Stochastic,
        chart_chrome::IndicatorKind::Atr => ChartIndicator::Atr,
    }
}
