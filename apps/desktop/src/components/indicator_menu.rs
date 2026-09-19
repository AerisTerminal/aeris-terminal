use super::*;

use super::chrome_menu::{
    CHROME_MENU_INDICATOR_SEARCH_HEIGHT, ChromeMenuExtent, chrome_menu_empty, chrome_menu_footer,
    chrome_menu_group_heading, chrome_menu_scroll_body, chrome_menu_search_header,
    chrome_menu_surface, compact_menu_add_button, scrollable_menu_body,
};

pub(super) fn indicator_selector(
    app: Entity<WorkspaceSurface>,
    _input: Entity<InputState>,
    _message: Option<String>,
    enabled: bool,
    theme: &TradingPlotTheme,
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
    theme: &TradingPlotTheme,
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
    theme: &TradingPlotTheme,
    cx: &App,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let indicator_specs =
        chart_chrome::filter_indicator_specs(state.input.read(cx).value().as_ref());
    let hint = state.message.map_or_else(
        || format!("{} native", indicator_specs.len()),
        str::to_string,
    );
    let mut list = chrome_menu_scroll_body();
    if indicator_specs.is_empty() {
        list = list.child(chrome_menu_empty(
            "No matching indicators",
            "Try “average”, “bands”, or a kind like SMA.",
            &colors,
        ));
    } else {
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
            CHROME_MENU_INDICATOR_SEARCH_HEIGHT,
        ))
        .child(scrollable_menu_body(
            list,
            state.scroll,
            colors.text_secondary,
            state.extent,
        ))
        .child(chrome_menu_footer(&colors, "Add", "Publisher: Native"))
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
