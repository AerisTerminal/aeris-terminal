use super::indicator_templates_view::indicator_templates_in_menu_order;
use super::*;
use crate::desktop::workspace_surface::{IndicatorMenuEntry, IndicatorMenuGroup};
use aeris_localization::{chart_menu, tr};

use super::chrome_menu::{
    ChromeMenuExtent, chrome_menu_empty, chrome_menu_group_heading, chrome_menu_scroll_body,
    chrome_menu_search_header, chrome_menu_surface, compact_menu_add_button, favorite_star_button,
    scrollable_menu_body,
};

pub(super) fn indicator_selector(
    app: Entity<WorkspaceSurface>,
    _input: Entity<InputState>,
    enabled: bool,
    theme: &AerisTheme,
) -> impl IntoElement {
    chrome_tooltip(
        "indicator_selector",
        tr!(chart_menu::INDICATORS),
        Button::new("indicator_selector", theme)
            .variant(ButtonVariant::Ghost)
            .button_size(ButtonSize::Lg)
            .icon(header_icon(HugeIcon::Chart))
            .aria_label(tr!(chart_menu::INDICATORS))
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
    pub(super) keyboard_selection: usize,
    pub(super) keyboard_active: bool,
    pub(super) scroll: &'a ScrollHandle,
    pub(super) templates: IndicatorDialogTemplates<'a>,
}

/// The saved indicator templates the menu offers under "My templates", and the window that
/// owns and applies them.
#[derive(Clone, Copy)]
pub(super) struct IndicatorDialogTemplates<'a> {
    pub(super) terminal: &'a Entity<TerminalApp>,
    pub(super) saved: &'a [WorkspaceIndicatorTemplateState],
}

fn indicator_menu_row(
    app: &Entity<WorkspaceSurface>,
    index: usize,
    entry: IndicatorMenuEntry,
    state: (bool, bool),
    theme: &AerisTheme,
) -> AnyElement {
    let (highlighted, favorite) = state;
    let row_app = app.clone();
    let add_app = app.clone();
    let star_app = app.clone();
    let identifier = entry.favorite_identifier();
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
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    favorite_star_button(("indicator_favorite_star", index), favorite, theme)
                        .debug_selector(|| "indicator_favorite_star".into())
                        .on_click(move |_, _, cx| {
                            star_app.update(cx, |_, app_cx| {
                                app_cx.emit(IndicatorFavoriteToggledEvent(identifier));
                            });
                            cx.stop_propagation();
                        }),
                )
                .child(
                    compact_menu_add_button(("add_indicator", index), theme).on_click(
                        move |_, window, cx| {
                            if add_app.update(cx, |app, cx| app.add_indicator_menu_entry(entry, cx))
                            {
                                add_app.update(cx, |app, app_cx| {
                                    app.close_chrome_overlay(window, app_cx);
                                });
                            }
                        },
                    ),
                ),
        )
        .into_any_element()
}

/// A saved template under "My templates": the row replaces the chart's indicators with the
/// template's, its star keeps the template at the top of the template menus.
fn indicator_template_row(
    app: &Entity<WorkspaceSurface>,
    templates: IndicatorDialogTemplates<'_>,
    index: usize,
    template: &WorkspaceIndicatorTemplateState,
    theme: &AerisTheme,
) -> AnyElement {
    let apply = templates.terminal.clone();
    let star = templates.terminal.clone();
    let close_app = app.clone();
    MenuRow::search_result(
        ("indicator_dialog_template", index),
        template.name.clone(),
        theme,
    )
    .on_click(move |_, window, cx| {
        apply.update(cx, |terminal, terminal_cx| {
            terminal.apply_indicator_template(index, terminal_cx);
        });
        close_app.update(cx, |app, app_cx| app.close_chrome_overlay(window, app_cx));
    })
    .trailing(
        favorite_star_button(
            ("indicator_template_favorite_star", index),
            template.favorite,
            theme,
        )
        .on_click(move |_, _, cx| {
            star.update(cx, |terminal, terminal_cx| {
                terminal.toggle_indicator_template_favorite(index, terminal_cx);
            });
            cx.stop_propagation();
        }),
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
    let surface = app.read(cx);
    let entries = surface.indicator_menu_entries(cx);
    let favorites = surface.chart_chrome.indicator_favorites;
    let query_key = chart_chrome::indicator_search_key(&state.input.read(cx).value());
    let saved = if surface.chart.is_some() {
        state.templates.saved
    } else {
        &[]
    };
    let templates = indicator_templates_in_menu_order(saved)
        .into_iter()
        .filter(|(_, template)| {
            chart_chrome::indicator_search_key(&template.name).contains(&query_key)
        })
        .collect::<Vec<_>>();
    let highlighted = state.keyboard_active.then_some(state.keyboard_selection);
    let mut list = chrome_menu_scroll_body();
    if entries.is_empty() && templates.is_empty() {
        list = list.child(chrome_menu_empty(
            tr!(chart_menu::NO_MATCHING_INDICATORS),
            tr!(chart_menu::NO_MATCHING_INDICATORS_HINT),
            &colors,
        ));
    }
    let mut previous_group = None;
    for (index, &entry) in entries.iter().enumerate() {
        let group = entry.group(favorites);
        if previous_group != Some(group) {
            let heading = match group {
                IndicatorMenuGroup::Favorites => tr!(chart_menu::FAVORITES),
                IndicatorMenuGroup::OrderFlow => tr!(chart_menu::ORDER_FLOW),
                IndicatorMenuGroup::Indicators => tr!(chart_menu::INDICATORS),
            };
            list = list.child(chrome_menu_group_heading(heading, &colors));
            previous_group = Some(group);
        }
        list = list.child(indicator_menu_row(
            app,
            index,
            entry,
            (
                highlighted == Some(index),
                group == IndicatorMenuGroup::Favorites,
            ),
            theme,
        ));
    }
    // The templates follow every addable row, so arrow keys and scrolling keep indexing the
    // entries alone.
    if !templates.is_empty() {
        list = list.child(chrome_menu_group_heading(
            tr!(chart_menu::MY_TEMPLATES),
            &colors,
        ));
    }
    for (index, template) in templates {
        list = list.child(indicator_template_row(
            app,
            state.templates,
            index,
            template,
            theme,
        ));
    }
    chrome_menu_surface(&colors, state.extent)
        .child(chrome_menu_search_header(state.input, theme, app))
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
