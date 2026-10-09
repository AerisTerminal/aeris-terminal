use super::*;

use super::chrome_menu::{
    CHROME_MENU_ROW_ICON_WELL, CHROME_MENU_SEARCH_HEIGHT, CHROME_MENU_SEARCH_ICON_SIZE,
    ChromeMenuExtent, chrome_menu_close_button, chrome_menu_empty, chrome_menu_scroll_body,
    chrome_menu_surface, compact_menu_add_button, scrollable_menu_body,
};

pub(super) fn provider_exchange_mark(
    provider: TerminalProvider,
    size: Pixels,
    well: bool,
    colors: &aeris_design_system::ThemeColors,
) -> AnyElement {
    match super::provider_presentation(provider)
        .and_then(|descriptor| assets::ExchangeLogo::for_logo_key(descriptor.logo_key))
    {
        Some(logo) => exchange_mark(logo, size, well, colors).into_any_element(),
        // No square logo exists for this provider; a generic glyph sits in the same tile and
        // is inset like the logos' own artwork so every mark reads at one size.
        None => mark_tile(size, well, colors)
            .child(header_icon(HugeIcon::Chart).with_size(size * 0.6))
            .into_any_element(),
    }
}

fn show_hosted_broker_connect_prompt(provider: TerminalProvider, disconnected: bool) -> bool {
    disconnected
        && super::provider_presentation(provider).is_some_and(|descriptor| {
            descriptor.connection_kind == ProviderConnectionKind::HostedBroker
        })
}

pub(super) fn instrument_selector(
    app: Entity<WorkspaceSurface>,
    state: &InstrumentSelectorState,
    theme: &AerisTheme,
) -> impl IntoElement {
    chrome_tooltip(
        "instrument_selector",
        format!(
            "Search or select a {} market",
            terminal_provider_display(state.provider)
        ),
        Button::new("instrument_selector", theme)
            .variant(ButtonVariant::Secondary)
            .button_size(ButtonSize::Lg)
            .round()
            .leading(provider_exchange_mark(
                state.provider,
                px(16.0),
                false,
                &theme.colors,
            ))
            .loading_icon(header_icon(HugeIcon::Loader))
            .label(state.label.clone())
            .caret(header_icon(HugeIcon::ChevronDown))
            .disabled(!state.availability.enabled)
            .loading(state.availability.selection_pending)
            .on_click(move |event, window, cx| {
                app.update(cx, |app, app_cx| {
                    app.open_chrome_overlay_at(
                        ChromeOverlay::Instrument,
                        event.position(),
                        window,
                        app_cx,
                    );
                });
            }),
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
    /// The runtime reported no stored connection for the menu's hosted-broker provider;
    /// the menu points to Accounts.
    pub(super) hosted_broker_disconnected: bool,
    /// The provider and category dropdown opened from the search-field logo.
    pub(super) provider_menu_open: bool,
    pub(super) markets_flyout_open: bool,
    pub(super) search_categories: aeris_contracts::InstrumentSearchCategories,
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
    let mut list = chrome_menu_scroll_body();
    if state.instruments.is_empty() {
        list = list.child(chrome_menu_empty(
            "No markets to display",
            state.message.clone(),
            &colors,
        ));
    } else {
        list = list.children(
            state
                .instruments
                .iter()
                .enumerate()
                .map(|(index, instrument)| {
                    instrument_dialog_row(app, instrument, index, state, theme)
                }),
        );
    }
    chrome_menu_surface(&colors, extent)
        .child(state.input.as_ref().map_or_else(
            || div().into_any_element(),
            |input| instrument_search_header(input, theme, app, state).into_any_element(),
        ))
        .when(
            show_hosted_broker_connect_prompt(
                state.menu_provider,
                state.hosted_broker_disconnected,
            ),
            |menu| {
                let accounts = app.clone();
                let provider_name = terminal_provider_display(state.menu_provider).to_string();
                menu.child(
                    div().px_3().py_1().child(
                        Button::new("symbol_menu_connect_hosted_broker", theme)
                            .variant(ButtonVariant::Secondary)
                            .label(format!("Connect {provider_name} in Accounts"))
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
        .when(state.provider_menu_open, |menu| {
            menu.child(symbol_provider_menu(app, state, theme))
        })
}

const SYMBOL_PROVIDER_MENU_WIDTH: f32 = 200.0;

fn symbol_menu_providers() -> Vec<TerminalProvider> {
    aeris_market_runtime::built_in_provider_presentations()
        .iter()
        .filter_map(|descriptor| known_terminal_provider(descriptor.id))
        .filter(|provider| super::provider_presentation(*provider).is_some())
        .collect()
}

/// Provider dropdown anchored under the search-field logo. Clicking a provider switches the
/// listing and closes it. A provider that serves several instrument categories opens its
/// category flyout when its own row is hovered, exactly like a timeframe group row.
fn symbol_provider_menu(
    app: &Entity<WorkspaceSurface>,
    state: &InstrumentSelectorState,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let providers = symbol_menu_providers();
    let has_categories = |provider: TerminalProvider| {
        super::provider_presentation(provider)
            .is_some_and(|descriptor| descriptor.search_categories_available)
    };
    let flyout_row = providers
        .iter()
        .position(|provider| has_categories(*provider));
    let last_row = providers.len().saturating_sub(1);
    let provider_rows = providers.into_iter().enumerate().map(|(index, provider)| {
        let row_app = app.clone();
        let hover_app = app.clone();
        let active = provider == state.menu_provider;
        let opens_flyout = has_categories(provider);
        let mut trailing = div().flex().items_center().gap_1();
        if active {
            trailing = trailing.child(header_icon(HugeIcon::CheckIcon).with_size(px(14.0)));
        }
        if opens_flyout {
            trailing = trailing.child(
                header_icon(HugeIcon::ArrowRight)
                    .with_size(px(16.0))
                    .color(gpui_color(colors.icon)),
            );
        }
        MenuRow::compact(
            ("symbol_provider_row", index),
            terminal_provider_display(provider),
            theme,
        )
        .resting_fill(colors.surface)
        .leading(provider_exchange_mark(provider, px(18.0), false, &colors))
        .highlighted(active || (opens_flyout && state.markets_flyout_open))
        .disabled(state.availability.selection_pending)
        .trailing(trailing)
        .flush_in_panel(index == 0, index == last_row)
        .on_hover(move |hovered, _, cx| {
            if *hovered {
                hover_app.update(cx, |surface, surface_cx| {
                    surface.set_symbol_markets_flyout(opens_flyout, surface_cx);
                });
            }
        })
        .on_click(move |_, window, cx| {
            row_app.update(cx, |surface, surface_cx| {
                surface.choose_symbol_provider(provider, window, surface_cx);
            });
        })
    });
    let root = div()
        .id("symbol_provider_menu")
        .w(px(SYMBOL_PROVIDER_MENU_WIDTH))
        .flex()
        .flex_col()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border_1()
        .border_color(gpui_color(colors.border))
        .bg(gpui_color(colors.surface))
        .overflow_hidden()
        .children(provider_rows);
    div()
        .id("symbol_provider_menu_host")
        .absolute()
        .top(px(CHROME_MENU_SEARCH_HEIGHT + 4.0))
        .left(px(12.0))
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(animate_popup_from_origin(
            root,
            "symbol_provider_menu_enter",
            PopupAnimationOrigin::TOP_LEFT,
        ))
        .when_some(
            flyout_row.filter(|_| state.markets_flyout_open),
            |host, row| host.child(symbol_markets_flyout(app, state, row, theme)),
        )
}

/// Category toggles beside the provider menu, aligned to the hovered provider's row and styled
/// like the timeframe flyout. Rows toggle in place; the last included category cannot be cleared.
fn symbol_markets_flyout(
    app: &Entity<WorkspaceSurface>,
    state: &InstrumentSelectorState,
    provider_row_index: usize,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let last = SymbolSearchCategory::ALL.len().saturating_sub(1);
    let rows = SymbolSearchCategory::ALL
        .into_iter()
        .enumerate()
        .map(|(index, category)| {
            let row_app = app.clone();
            let row = MenuRow::compact(("symbol_category_row", index), category.label(), theme)
                .resting_fill(colors.surface_secondary)
                .flush_in_panel(index == 0, index == last)
                .on_click(move |_, _, cx| {
                    row_app.update(cx, |surface, surface_cx| {
                        surface.toggle_symbol_search_category(category, surface_cx);
                    });
                });
            if category.included(state.search_categories) {
                row.trailing(header_icon(HugeIcon::CheckIcon).with_size(px(14.0)))
            } else {
                row
            }
        });
    let panel = div()
        .id("symbol_markets_flyout")
        .w(px(TIMEFRAME_FLYOUT_WIDTH))
        .flex()
        .flex_col()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border_1()
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
        .overflow_hidden()
        .children(rows);
    let top =
        CHART_CONTEXT_MENU_ROW_HEIGHT * f32::from(u16::try_from(provider_row_index).unwrap_or(0));
    div()
        .id("symbol_markets_flyout_host")
        .absolute()
        .left(px(SYMBOL_PROVIDER_MENU_WIDTH + TIMEFRAME_FLYOUT_GAP))
        .top(px(top))
        .child(animate_popup_from_origin(
            panel,
            "symbol_markets_flyout_enter",
            PopupAnimationOrigin::new(0.0, 0.25),
        ))
}

pub(super) fn instrument_dialog_row(
    app: &Entity<WorkspaceSurface>,
    instrument: &InstrumentMenuEntry,
    index: usize,
    state: &InstrumentSelectorState,
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
    if let Some(asset_class) = &instrument.asset_class {
        row = row.detail(asset_class.clone());
    }
    row = row.leading(provider_exchange_mark(
        state.menu_provider,
        px(CHROME_MENU_ROW_ICON_WELL),
        true,
        &theme.colors,
    ));
    if state.target == SymbolSelectionTarget::Watchlist {
        row = row.trailing(
            compact_menu_add_button(("add_watchlist_symbol", index), theme)
                .disabled(state.availability.selection_pending)
                .on_click(move |_, _, cx| {
                    add_app.update(cx, |app, cx| {
                        app.select_instrument(selection, SymbolSelectionTarget::Watchlist, cx);
                    });
                }),
        );
    } else if checked {
        row = row.trailing(header_icon(HugeIcon::CheckIcon).with_size(px(14.0)));
    }
    row
}

pub(super) fn instrument_search_header(
    input: &Entity<InputState>,
    theme: &AerisTheme,
    app: &Entity<WorkspaceSurface>,
    state: &InstrumentSelectorState,
) -> Div {
    let colors = theme.colors;
    div()
        .h(px(CHROME_MENU_SEARCH_HEIGHT))
        .relative()
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px(px(12.0))
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_sm()
        .text_color(gpui_color(colors.text_primary))
        .child({
            let toggle_app = app.clone();
            Button::new("symbol_provider_switcher", theme)
                .variant(ButtonVariant::Secondary)
                .round()
                .open(state.provider_menu_open)
                .aria_label("Switch market provider")
                .leading(provider_exchange_mark(
                    state.menu_provider,
                    px(CHROME_MENU_SEARCH_ICON_SIZE),
                    false,
                    &colors,
                ))
                .on_press(move |_, _, cx| {
                    toggle_app.update(cx, WorkspaceSurface::toggle_symbol_provider_menu);
                })
        })
        .child(
            Input::new(input)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                .flex_1(),
        )
        .child(chrome_menu_close_button(app, theme))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_hosted_broker_keeps_the_accounts_prompt() {
        assert!(show_hosted_broker_connect_prompt(
            TerminalProvider::Tastytrade,
            true,
        ));
        assert!(!show_hosted_broker_connect_prompt(
            TerminalProvider::Tastytrade,
            false,
        ));
        assert!(show_hosted_broker_connect_prompt(
            TerminalProvider::Ctrader,
            true,
        ));
        assert!(!show_hosted_broker_connect_prompt(
            TerminalProvider::Rithmic,
            true,
        ));
        assert!(!show_hosted_broker_connect_prompt(
            TerminalProvider::Hyperliquid,
            true,
        ));
    }

    #[test]
    fn provider_menu_lists_each_desktop_provider_once() {
        let providers = symbol_menu_providers();
        for (index, provider) in providers.iter().enumerate() {
            assert!(
                !providers[index + 1..].contains(provider),
                "{provider:?} listed twice"
            );
        }
        for provider in TerminalProvider::ALL {
            assert!(providers.contains(&provider), "{provider:?} missing");
        }
    }

    #[test]
    fn only_the_menu_providers_broker_reports_disconnected() {
        let mut connections = HostedBrokerConnections::default();
        connections.ctrader.connected = Some(false);
        connections.tastytrade.connected = Some(true);
        assert!(connections.disconnected(TerminalProvider::Ctrader));
        assert!(!connections.disconnected(TerminalProvider::Tastytrade));
        assert!(!connections.disconnected(TerminalProvider::Hyperliquid));
        // An unchecked connection is not reported as disconnected.
        assert!(!HostedBrokerConnections::default().disconnected(TerminalProvider::Ctrader));
    }
}
