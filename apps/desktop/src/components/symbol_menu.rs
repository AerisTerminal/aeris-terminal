use super::*;

use super::chrome_menu::{
    CHROME_MENU_ROW_ICON_WELL, CHROME_MENU_SEARCH_ICON_SIZE, ChromeMenuExtent,
    chrome_menu_close_button, chrome_menu_empty, chrome_menu_scroll_body, chrome_menu_surface,
    compact_menu_add_button, scrollable_menu_body,
};

pub(super) fn provider_exchange_mark(
    provider: TerminalProvider,
    size: Pixels,
    well: bool,
    colors: &aeris_design_system::ThemeColors,
) -> AnyElement {
    match super::provider_presentation(provider).map(|descriptor| descriptor.logo_key) {
        Some("rithmic") => {
            exchange_mark(assets::ExchangeLogo::Rithmic, size, well, colors).into_any_element()
        }
        Some("hyperliquid") => {
            exchange_mark(assets::ExchangeLogo::Hyperliquid, size, well, colors).into_any_element()
        }
        _ => header_icon(HugeIcon::Chart)
            .with_size(size)
            .into_any_element(),
    }
}

pub(super) fn instrument_selector(
    app: Entity<WorkspaceSurface>,
    state: &InstrumentSelectorState,
    theme: &AerisTheme,
) -> impl IntoElement {
    let trigger = Button::new("instrument_selector")
        .leading(provider_exchange_mark(
            state.provider,
            px(16.0),
            false,
            &theme.colors,
        ))
        .loading_icon(header_icon(HugeIcon::Loader))
        .label(state.label.clone())
        .caret(header_icon(HugeIcon::ChevronDown))
        .with_size(px(chart_chrome::HEADER_CONTROL_CONTENT_SIZE))
        .border_1()
        .border_color(gpui_color(theme.colors.border_secondary))
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
            terminal_provider_display(state.provider)
        ),
        button_activation_at(
            trigger.loading(state.availability.selection_pending),
            state.availability.enabled,
            move |trigger_position, window, cx| {
                app.update(cx, |app, app_cx| {
                    app.open_chrome_overlay_at(
                        ChromeOverlay::Instrument,
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

pub(super) struct InstrumentSelectorState {
    pub(super) label: String,
    pub(super) message: String,
    pub(super) instruments: Vec<InstrumentMenuEntry>,
    pub(super) input: Option<Entity<InputState>>,
    pub(super) availability: InstrumentSelectorAvailability,
    pub(super) provider: TerminalProvider,
    pub(super) menu_provider: TerminalProvider,
    pub(super) menu: InstrumentSelectorMenu,
    pub(super) scroll: ScrollHandle,
    pub(super) target: SymbolSelectionTarget,
    /// The runtime reported no stored tastytrade connection; the menu points to Accounts.
    pub(super) tastytrade_disconnected: bool,
}

pub(super) struct InstrumentSelectorAvailability {
    pub(super) selection_pending: bool,
    pub(super) enabled: bool,
}

pub(super) struct InstrumentSelectorMenu {
    pub(super) keyboard_selection: usize,
    pub(super) keyboard_active: bool,
}

pub(super) fn instrument_dialog_content(
    app: &Entity<WorkspaceSurface>,
    extent: ChromeMenuExtent,
    state: &InstrumentSelectorState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let mut list = chrome_menu_scroll_body(extent.scale);
    if state.instruments.is_empty() {
        list = list.child(chrome_menu_empty(
            "No markets to display",
            state.message.clone(),
            extent.scale,
            &colors,
        ));
    } else {
        list = list.children(
            state
                .instruments
                .iter()
                .enumerate()
                .map(|(index, instrument)| {
                    instrument_dialog_row(app, instrument, index, state, extent.scale, theme)
                }),
        );
    }
    let provider_buttons = aeris_market_runtime::built_in_provider_presentations()
        .iter()
        .filter_map(|descriptor| {
            let provider = terminal_provider_from_id(descriptor.id);
            (terminal_provider_id(provider) == descriptor.id).then_some(provider)
        })
        .enumerate()
        .map(|(index, provider)| {
            let app = app.clone();
            Button::new(("symbol_provider", index))
                .label(terminal_provider_display(provider))
                .theme(theme)
                .text_color(gpui_color(if provider == state.menu_provider {
                    colors.text_primary
                } else {
                    colors.text_muted
                }))
                .disabled(state.availability.selection_pending)
                .on_click(move |_, window, cx| {
                    app.update(cx, |surface, surface_cx| {
                        surface.choose_symbol_provider(provider, window, surface_cx);
                    });
                })
        });
    chrome_menu_surface(&colors, extent)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .children(provider_buttons),
        )
        .child(state.input.as_ref().map_or_else(
            || div().into_any_element(),
            |input| instrument_search_header(input, theme, app, state, extent).into_any_element(),
        ))
        .when(
            super::provider_presentation(state.menu_provider).is_some_and(|descriptor| {
                descriptor.connection_kind == ProviderConnectionKind::Credentials
                    && state.tastytrade_disconnected
            }),
            |menu| {
                let accounts = app.clone();
                menu.child(
                    div().px_3().py_1().child(
                        Button::new("symbol_menu_connect_tastytrade")
                            .label("Connect tastytrade in Accounts")
                            .theme(theme)
                            .on_click(move |_, window, cx| {
                                accounts.update(cx, |surface, surface_cx| {
                                    surface.open_chrome_overlay(
                                        ChromeOverlay::Accounts,
                                        window,
                                        surface_cx,
                                    );
                                });
                            }),
                    ),
                )
            },
        )
        .child(scrollable_menu_body(
            list,
            &state.scroll,
            colors.text_secondary,
            extent,
        ))
        .when(!state.instruments.is_empty(), |menu| {
            menu.child(
                div()
                    .px_3()
                    .py_2()
                    .text_xs()
                    .text_color(gpui_color(colors.text_secondary))
                    .child(state.message.clone()),
            )
        })
}

pub(super) fn instrument_dialog_row(
    app: &Entity<WorkspaceSurface>,
    instrument: &InstrumentMenuEntry,
    index: usize,
    state: &InstrumentSelectorState,
    scale: MenuScale,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let checked = instrument.checked;
    let app = app.clone();
    let add_app = app.clone();
    let selection = instrument.selection;
    let target = state.target;
    let mut row = MenuRow::search_result(
        ("instrument_dialog_row", index),
        instrument.label.clone(),
        theme,
    )
    .scale(scale)
    .highlighted(instrument_row_highlighted(
        checked,
        index,
        state.menu.keyboard_selection,
        state.menu.keyboard_active,
    ))
    .disabled(target == SymbolSelectionTarget::Watchlist && state.availability.selection_pending)
    .on_click(move |_, window, cx| {
        if app.update(cx, |app, cx| app.select_instrument(selection, target, cx))
            && symbol_menu_closes_after_selection(target)
        {
            app.update(cx, |app, app_cx| {
                app.close_chrome_overlay(window, app_cx);
            });
        }
    });
    row = row.leading(provider_exchange_mark(
        state.menu_provider,
        scale.px(CHROME_MENU_ROW_ICON_WELL),
        true,
        &theme.colors,
    ));
    if state.target == SymbolSelectionTarget::Watchlist {
        row = row.trailing(button_activation(
            compact_menu_add_button(("add_watchlist_symbol", index), scale, theme)
                .disabled(state.availability.selection_pending),
            !state.availability.selection_pending,
            move |_, cx| {
                add_app.update(cx, |app, cx| {
                    app.select_instrument(selection, SymbolSelectionTarget::Watchlist, cx);
                });
            },
        ));
    } else if checked {
        row = row.trailing(header_icon(HugeIcon::CheckIcon).with_size(scale.px(14.0)));
    }
    row
}

pub(super) fn instrument_search_header(
    input: &Entity<InputState>,
    theme: &AerisTheme,
    app: &Entity<WorkspaceSurface>,
    state: &InstrumentSelectorState,
    extent: ChromeMenuExtent,
) -> Div {
    let colors = theme.colors;
    let scale = extent.scale;
    div()
        .h(px(extent.search_height))
        .relative()
        .flex_none()
        .flex()
        .items_center()
        .gap(scale.rems(0.5))
        .px(scale.px(12.0))
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_size(scale.rems(0.875))
        .text_color(gpui_color(colors.text_primary))
        .child(provider_exchange_mark(
            state.menu_provider,
            scale.px(CHROME_MENU_SEARCH_ICON_SIZE),
            false,
            &colors,
        ))
        .child(
            Input::new(input)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                .flex_1(),
        )
        .child(chrome_menu_close_button(app, theme))
}
