use super::*;

use super::chrome_menu::{
    CHROME_MENU_ROW_ICON_WELL, CHROME_MENU_SEARCH_HEIGHT, CHROME_MENU_SEARCH_ICON_SIZE,
    ChromeMenuExtent, chrome_menu_close_button, chrome_menu_empty, chrome_menu_footer,
    chrome_menu_group_heading, chrome_menu_scroll_body, chrome_menu_surface, scrollable_menu_body,
};

pub(super) fn instrument_selector(
    app: Entity<WorkspaceSurface>,
    state: &InstrumentSelectorState,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let trigger = Button::new("instrument_selector")
        .leading(exchange_mark(
            match state.provider {
                TerminalProvider::Rithmic => assets::ExchangeLogo::Rithmic,
                TerminalProvider::Hyperliquid => assets::ExchangeLogo::Hyperliquid,
            },
            px(16.0),
            false,
            &theme.colors,
        ))
        .loading_icon(header_icon(HugeIcon::Loader))
        .label(state.label.clone())
        .caret(header_icon(HugeIcon::ChevronDown))
        .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
        .border_1()
        .border_color(gpui_color(theme.colors.input_border))
        .bg(gpui_color(theme.colors.surface))
        .text_color(gpui_color(theme.colors.text_primary))
        .theme(theme)
        .resting_fill(theme.colors.surface)
        .h(px(chart_chrome::CHART_CONTROL_SIZE))
        .px_3()
        .rounded(px(f32::from(
            chart_chrome::SYMBOL_TRIGGER_RADIUS.logical_pixels(),
        )))
        .disabled(!state.availability.enabled)
        .when(state.availability.enabled, Button::cursor_pointer)
        .when(!state.availability.enabled, Button::cursor_not_allowed);
    let trigger = trigger.when(!state.availability.enabled, |trigger| {
        trigger.text_color(gpui_color(theme.colors.text_muted))
    });
    chrome_tooltip(
        "instrument_selector",
        format!(
            "Search or select a {} market",
            match state.provider {
                TerminalProvider::Rithmic => "Rithmic",
                TerminalProvider::Hyperliquid => "Hyperliquid",
            }
        ),
        button_activation(
            trigger.loading(state.availability.selection_pending),
            state.availability.enabled,
            move |window, cx| {
                app.update(cx, |app, app_cx| {
                    app.open_chrome_overlay(ChromeOverlay::Instrument, window, app_cx);
                });
            },
        ),
        theme,
    )
}

pub(super) struct InstrumentSelectorState {
    pub(super) label: String,
    pub(super) instruments: Vec<InstrumentMenuEntry>,
    pub(super) input: Option<Entity<InputState>>,
    pub(super) availability: InstrumentSelectorAvailability,
    pub(super) provider: TerminalProvider,
    pub(super) catalog_exchange: assets::ExchangeLogo,
    pub(super) menu: InstrumentSelectorMenu,
    pub(super) scroll: ScrollHandle,
    pub(super) target: SymbolSelectionTarget,
}

pub(super) struct InstrumentSelectorAvailability {
    pub(super) selection_pending: bool,
    pub(super) enabled: bool,
}

pub(super) struct InstrumentSelectorMenu {
    pub(super) exchange_open: bool,
    pub(super) keyboard_selection: usize,
    pub(super) keyboard_active: bool,
}

pub(super) fn instrument_dialog_content(
    app: &Entity<WorkspaceSurface>,
    extent: ChromeMenuExtent,
    state: &InstrumentSelectorState,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let count = state.instruments.len();
    let current = state
        .instruments
        .iter()
        .enumerate()
        .filter(|(_, instrument)| instrument.checked);
    let markets = state
        .instruments
        .iter()
        .enumerate()
        .filter(|(_, instrument)| !instrument.checked);
    let market_heading = match state.provider {
        TerminalProvider::Rithmic => "Rithmic markets",
        TerminalProvider::Hyperliquid => "Hyperliquid markets",
    };
    let trailing = state
        .instruments
        .iter()
        .find(|instrument| instrument.checked)
        .map_or_else(
            || match state.provider {
                TerminalProvider::Rithmic => "Rithmic Test".to_string(),
                TerminalProvider::Hyperliquid => "Hyperliquid public feed".to_string(),
            },
            |instrument| format!("Current stream: {}", instrument.symbol),
        );
    let mut list = chrome_menu_scroll_body();
    if count == 0 {
        let (title, detail) = if state.provider == TerminalProvider::Rithmic
            && state.catalog_exchange != assets::ExchangeLogo::Rithmic
        {
            (
                "No markets for this exchange",
                "This terminal currently lists Rithmic spot. Switch the filter back to Rithmic.",
            )
        } else if state.provider == TerminalProvider::Hyperliquid
            && state.catalog_exchange != assets::ExchangeLogo::Hyperliquid
        {
            (
                "No markets for this exchange",
                "This terminal currently lists Hyperliquid. Switch the filter back to Hyperliquid.",
            )
        } else {
            (
                "No matching markets",
                "Try a symbol like BTC, ETH, SOL, or the quote asset.",
            )
        };
        list = list.child(chrome_menu_empty(title, detail, &colors));
    } else {
        if current.clone().next().is_some() {
            list = list
                .child(chrome_menu_group_heading("Current market", &colors))
                .children(current.map(|(index, instrument)| {
                    instrument_dialog_row(app, instrument, index, state, theme)
                }));
        }
        if markets.clone().next().is_some() {
            list = list
                .child(chrome_menu_group_heading(market_heading, &colors))
                .children(markets.map(|(index, instrument)| {
                    instrument_dialog_row(app, instrument, index, state, theme)
                }));
        }
    }
    let action = match state.target {
        SymbolSelectionTarget::Chart => "Select",
        SymbolSelectionTarget::Watchlist => "Add",
    };
    let trailing = match state.target {
        SymbolSelectionTarget::Chart => trailing,
        SymbolSelectionTarget::Watchlist => "Add symbols to watchlist".to_string(),
    };
    chrome_menu_surface(&colors, extent)
        .child(state.input.as_ref().map_or_else(
            || div().into_any_element(),
            |input| {
                instrument_search_header(input, theme, app, format!("{count} markets"), state)
                    .into_any_element()
            },
        ))
        .child(scrollable_menu_body(
            list,
            &state.scroll,
            colors.text_secondary,
            extent,
        ))
        .child(chrome_menu_footer(&colors, action, trailing))
        .when(state.menu.exchange_open, |surface| {
            surface.child(instrument_exchange_menu(app, state.catalog_exchange, theme))
        })
}

pub(super) fn instrument_dialog_row(
    app: &Entity<WorkspaceSurface>,
    instrument: &InstrumentMenuEntry,
    index: usize,
    state: &InstrumentSelectorState,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let checked = instrument.checked;
    let app = app.clone();
    let add_app = app.clone();
    let selection = instrument.selection;
    let mut row = MenuRow::search_result(
        ("instrument_dialog_row", index),
        instrument.symbol.clone(),
        theme,
    )
    .highlighted(instrument_row_highlighted(
        checked,
        index,
        state.menu.keyboard_selection,
        state.menu.keyboard_active,
    ))
    .on_click(move |_, window, cx| {
        if app.update(cx, |app, cx| app.select_instrument(selection, cx)) {
            app.update(cx, |app, app_cx| {
                app.close_chrome_overlay(window, app_cx);
            });
        }
    });
    row = row.leading(exchange_mark(
        state.catalog_exchange,
        px(CHROME_MENU_ROW_ICON_WELL),
        true,
        &theme.colors,
    ));
    if state.target == SymbolSelectionTarget::Watchlist {
        row = row.trailing(button_activation(
            Button::new(("add_watchlist_symbol", index))
                .icon(header_icon(HugeIcon::AddIcon01).with_size(px(14.0)))
                .theme(theme)
                .resting_fill(theme.colors.surface)
                .w(px(24.0))
                .h(px(24.0))
                .compact()
                .border_1()
                .border_color(gpui_color(theme.colors.border))
                .cursor_pointer()
                .tab_stop(false),
            true,
            move |window, cx| {
                if add_app.update(cx, |app, cx| app.select_instrument(selection, cx)) {
                    add_app.update(cx, |app, app_cx| {
                        app.close_chrome_overlay(window, app_cx);
                    });
                }
            },
        ));
    } else if checked {
        row = row.trailing(header_icon(HugeIcon::CheckIcon).with_size(px(14.0)));
    }
    row
}

pub(super) fn instrument_search_header(
    input: &Entity<InputState>,
    theme: &AxiusflowTheme,
    app: &Entity<WorkspaceSurface>,
    hint: impl Into<gpui::SharedString>,
    state: &InstrumentSelectorState,
) -> Div {
    let colors = theme.colors;
    let rithmic = state.provider == TerminalProvider::Rithmic;
    div()
        .h(px(CHROME_MENU_SEARCH_HEIGHT))
        .relative()
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_sm()
        .text_color(gpui_color(colors.text_primary))
        .when(rithmic, |header| {
            let toggle_app = app.clone();
            let selected = state.catalog_exchange;
            header.child(
                div().relative().flex_none().child(
                    div()
                        .id("instrument_exchange_switcher")
                        .size(px(28.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                        .cursor_pointer()
                        .hover(|hit| hit.bg(gpui_color(colors.hover_bg.over(colors.surface))))
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            toggle_app
                                .update(cx, WorkspaceSurface::toggle_instrument_exchange_menu);
                            cx.stop_propagation();
                        })
                        .child(exchange_mark(selected, px(20.0), false, &colors)),
                ),
            )
        })
        .when(!rithmic, |header| {
            header.child(exchange_mark(
                assets::ExchangeLogo::Hyperliquid,
                px(CHROME_MENU_SEARCH_ICON_SIZE),
                false,
                &colors,
            ))
        })
        .child(
            Input::new(input)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                .flex_1(),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(gpui_color(colors.text_muted))
                .child(hint.into()),
        )
        .child(chrome_menu_close_button(app, theme))
}

pub(super) fn instrument_exchange_menu(
    app: &Entity<WorkspaceSurface>,
    selected: assets::ExchangeLogo,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let panel_fill = colors.surface_secondary.over(colors.surface);
    div()
        .id("instrument_exchange_menu")
        .absolute()
        .top(px(CHROME_MENU_SEARCH_HEIGHT + 4.0))
        .left(px(12.0))
        .w(px(168.0))
        .flex()
        .flex_col()
        .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
        .border_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(panel_fill))
        .p_1()
        .gap(px(2.0))
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .children(
            assets::ExchangeLogo::ALL
                .into_iter()
                .enumerate()
                .map(|(index, exchange)| {
                    let row_app = app.clone();
                    let active = exchange == selected;
                    let mut row = MenuRow::compact_inset(
                        ("instrument_exchange_row", index),
                        exchange.label(),
                        theme,
                    )
                    .resting_fill(panel_fill)
                    .leading(exchange_mark(exchange, px(20.0), false, &colors))
                    .highlighted(active)
                    .on_click(move |_, _, cx| {
                        row_app.update(cx, |app, cx| {
                            app.set_instrument_catalog_exchange(exchange, cx);
                        });
                    });
                    if active {
                        row = row.trailing(header_icon(HugeIcon::CheckIcon).with_size(px(14.0)));
                    }
                    row
                }),
        )
}
