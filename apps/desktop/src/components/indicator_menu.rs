use super::*;
use crate::desktop::workspace_surface::IndicatorMenuEntry;

use super::chrome_menu::{
    ChromeMenuExtent, chrome_menu_empty, chrome_menu_group_heading, chrome_menu_scroll_body,
    chrome_menu_search_header, chrome_menu_surface, compact_menu_add_button, scrollable_menu_body,
};

pub(super) fn indicator_selector(
    app: Entity<WorkspaceSurface>,
    _input: Entity<InputState>,
    _message: Option<String>,
    enabled: bool,
    theme: &AerisTheme,
) -> impl IntoElement {
    chrome_tooltip(
        "indicator_selector",
        "Indicators",
        Button::new("indicator_selector", theme)
            .variant(ButtonVariant::Ghost)
            .button_size(ButtonSize::Lg)
            .icon(header_icon(HugeIcon::Chart))
            .aria_label("Indicators")
            .disabled(!enabled)
            .on_click(move |event, window, cx| {
                app.update(cx, |app, app_cx| {
                    app.open_chrome_overlay_at(
                        ChromeOverlay::Indicator,
                        event.position(),
                        window,
                        app_cx,
                    );
                });
            }),
        theme,
    )
}

#[derive(Clone, Copy)]
pub(super) struct IndicatorDialogState<'a> {
    pub(super) extent: ChromeMenuExtent,
    pub(super) input: &'a Entity<InputState>,
    pub(super) message: Option<&'a str>,
    pub(super) keyboard_selection: usize,
    pub(super) keyboard_active: bool,
    pub(super) scroll: &'a ScrollHandle,
}

fn indicator_menu_row(
    app: &Entity<WorkspaceSurface>,
    index: usize,
    entry: IndicatorMenuEntry,
    highlighted: bool,
    theme: &AerisTheme,
) -> AnyElement {
    let row_app = app.clone();
    let add_app = app.clone();
    MenuRow::search_result(("indicator_dialog_row", index), entry.label(), theme)
        .highlighted(highlighted)
        .on_click(move |_, window, cx| {
            if row_app.update(cx, |app, cx| app.add_indicator_menu_entry(entry, cx)) {
                row_app.update(cx, |app, app_cx| {
                    app.close_chrome_overlay(window, app_cx);
                });
            }
        })
        .trailing(
            compact_menu_add_button(("add_indicator", index), theme).on_click(
                move |_, window, cx| {
                    if add_app.update(cx, |app, cx| app.add_indicator_menu_entry(entry, cx)) {
                        add_app.update(cx, |app, app_cx| {
                            app.close_chrome_overlay(window, app_cx);
                        });
                    }
                },
            ),
        )
        .into_any_element()
}

pub(super) fn indicator_dialog_content(
    app: &Entity<WorkspaceSurface>,
    state: IndicatorDialogState<'_>,
    theme: &AerisTheme,
    cx: &App,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let entries = app.read(cx).indicator_menu_entries(cx);
    let native_count = entries
        .iter()
        .filter(|entry| matches!(entry, IndicatorMenuEntry::Indicator(_)))
        .count();
    let hint = state
        .message
        .map_or_else(|| format!("{native_count} native"), str::to_string);
    let highlighted = state.keyboard_active.then_some(state.keyboard_selection);
    let mut list = chrome_menu_scroll_body();
    if entries.is_empty() {
        list = list.child(chrome_menu_empty(
            "No matching indicators",
            "Try “average”, “bands”, or a kind like SMA.",
            &colors,
        ));
    }
    for (index, &entry) in entries.iter().enumerate() {
        let starts_group = match entry {
            IndicatorMenuEntry::OrderFlow(_) => (index == 0).then_some("Order flow"),
            IndicatorMenuEntry::Indicator(_) => {
                (index + native_count == entries.len()).then_some("Indicators")
            }
        };
        if let Some(heading) = starts_group {
            list = list.child(chrome_menu_group_heading(heading, &colors));
        }
        list = list.child(indicator_menu_row(
            app,
            index,
            entry,
            highlighted == Some(index),
            theme,
        ));
    }
    chrome_menu_surface(&colors, state.extent)
        .child(chrome_menu_search_header(state.input, theme, app, hint))
        .child(scrollable_menu_body(
            list,
            state.scroll,
            colors.text_secondary,
            state.extent,
        ))
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
        chart_chrome::IndicatorKind::VolumeProfile => ChartIndicator::VolumeProfile,
    }
}
