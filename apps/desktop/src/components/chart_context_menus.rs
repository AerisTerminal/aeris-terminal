use super::*;

pub(super) fn overlay_height(rows: f32, separators: f32) -> f32 {
    // 1px border on each side. Compact dropdowns have no extra panel padding.
    2.0 + CHART_CONTEXT_MENU_ROW_HEIGHT * rows + CHART_CONTEXT_MENU_SEPARATOR_HEIGHT * separators
}

pub(super) fn clamp_overlay_origin(
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    width: f32,
    rows: f32,
    separators: f32,
) -> gpui::Point<Pixels> {
    let width = px(width);
    let height = px(overlay_height(rows, separators));
    let margin = px(OVERLAY_EDGE_MARGIN);
    let max_x = (viewport.width - width - margin).max(margin);
    let max_y = (viewport.height - height - margin).max(margin);
    point(
        origin.x.max(margin).min(max_x),
        origin.y.max(margin).min(max_y),
    )
}

pub(super) fn clamp_chart_context_menu_origin(
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
) -> gpui::Point<Pixels> {
    clamp_overlay_origin(origin, viewport, CHART_CONTEXT_MENU_WIDTH, 8.0, 5.0)
}

/// Prefer opening the Y-axis menu into the chart, then keep an edge margin so it
/// never sits flush against the window.
pub(super) fn clamp_price_axis_menu_origin(
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    axis_on_left: bool,
) -> gpui::Point<Pixels> {
    let width = px(CHART_CONTEXT_MENU_WIDTH);
    let height = px(overlay_height(7.0, 2.0));
    let margin = px(OVERLAY_EDGE_MARGIN);
    let gap = px(PRICE_AXIS_MENU_GAP);
    let preferred_x = if axis_on_left {
        origin.x + gap
    } else {
        origin.x - width - gap
    };
    let max_x = (viewport.width - width - margin).max(margin);
    let max_y = (viewport.height - height - margin).max(margin);
    point(
        preferred_x.max(margin).min(max_x),
        origin.y.max(margin).min(max_y),
    )
}

pub(super) fn clamp_price_axis_flyout_origin(
    root: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    flyout: PriceAxisMenuFlyout,
) -> gpui::Point<Pixels> {
    let (rows, separators, row, separators_before) = flyout.geometry();
    let width = px(PRICE_AXIS_FLYOUT_WIDTH);
    let height = px(overlay_height(rows, separators));
    let margin = px(OVERLAY_EDGE_MARGIN);
    let gap = px(PRICE_AXIS_FLYOUT_GAP);
    let parent_y = root.y
        + px(CHART_CONTEXT_MENU_ROW_HEIGHT * row)
        + px(CHART_CONTEXT_MENU_SEPARATOR_HEIGHT * separators_before);
    let left_x = root.x - width - gap;
    let right_x = root.x + px(CHART_CONTEXT_MENU_WIDTH) + gap;
    let max_x = (viewport.width - width - margin).max(margin);
    let x = if left_x >= margin {
        left_x
    } else if right_x <= max_x {
        right_x
    } else if left_x.max(margin) + width <= viewport.width - margin {
        left_x.max(margin)
    } else {
        right_x.min(max_x)
    };
    point(
        x.max(margin).min(max_x),
        parent_y
            .max(margin)
            .min((viewport.height - height - margin).max(margin)),
    )
}

pub(super) fn chart_context_menu_layer(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: ChartContextMenuState,
    viewport: gpui::Size<Pixels>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let origin = clamp_chart_context_menu_origin(menu.position, viewport);
    let dismiss = terminal.clone();
    div()
        .id("chart_context_menu_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .on_any_mouse_down(move |_, _, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_chart_context_menu(terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(chart_context_menu_panel(
            terminal, menu, state, origin, theme,
        ))
        .into_any_element()
}

pub(super) fn chart_context_menu_panel(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: ChartContextMenuState,
    origin: gpui::Point<Pixels>,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let mut panel = compact_menu_panel(
        "chart_context_menu",
        origin,
        px(CHART_CONTEXT_MENU_WIDTH),
        theme,
    );
    let items = chart_context_menu_items(state);
    let last = items.len().saturating_sub(1);
    for (index, item) in items.into_iter().enumerate() {
        if matches!(index, 1 | 2 | 4 | 6 | 7) {
            panel = panel.child(menu_separator(theme));
        }
        panel = panel.child(chart_context_menu_item(
            terminal,
            item,
            theme,
            menu.clone(),
            index == 0,
            index == last,
        ));
    }
    panel
}

pub(super) fn chart_context_menu_items(state: ChartContextMenuState) -> [ChartContextMenuItem; 8] {
    [
        ChartContextMenuItem {
            id: "chart_context_reset_view",
            icon: HugeIcon::Refresh01Icon,
            label: "Reset view",
            enabled: state.enabled(ChartContextMenuState::READY),
            action: ChartContextAction::Reset,
        },
        ChartContextMenuItem {
            id: "chart_context_copy_price",
            icon: HugeIcon::Copy01Icon,
            label: "Copy price",
            enabled: state.enabled(ChartContextMenuState::COPY_PRICE),
            action: ChartContextAction::CopyPrice,
        },
        ChartContextMenuItem {
            id: "chart_context_remove_drawings",
            icon: HugeIcon::DeleteIcon02,
            label: "Remove drawings",
            enabled: state.enabled(ChartContextMenuState::DRAWINGS),
            action: ChartContextAction::ClearDrawings,
        },
        ChartContextMenuItem {
            id: "chart_context_remove_indicators",
            icon: HugeIcon::DeleteIcon02,
            label: "Remove indicators",
            enabled: state.enabled(ChartContextMenuState::INDICATORS),
            action: ChartContextAction::ClearIndicators,
        },
        ChartContextMenuItem {
            id: "chart_context_split_horizontal",
            icon: HugeIcon::SplitSideBySide,
            label: "Split side by side",
            enabled: state.enabled(ChartContextMenuState::SPLIT),
            action: ChartContextAction::Split(ChartSplitDirection::Horizontal),
        },
        ChartContextMenuItem {
            id: "chart_context_split_vertical",
            icon: HugeIcon::SplitStacked,
            label: "Split top and bottom",
            enabled: state.enabled(ChartContextMenuState::SPLIT),
            action: ChartContextAction::Split(ChartSplitDirection::Vertical),
        },
        ChartContextMenuItem {
            id: "chart_context_close_pane",
            icon: HugeIcon::CancelIcon01,
            label: "Close chart",
            enabled: state.pane_count > 1,
            action: ChartContextAction::Close,
        },
        ChartContextMenuItem {
            id: "chart_context_settings",
            icon: HugeIcon::Settings01,
            label: "Settings",
            enabled: true,
            action: ChartContextAction::Settings,
        },
    ]
}

pub(super) fn chart_context_menu_item(
    terminal: &Entity<TerminalApp>,
    item: ChartContextMenuItem,
    theme: &AxiusflowTheme,
    menu: ChartContextMenu,
    first: bool,
    last: bool,
) -> impl IntoElement {
    let action_terminal = terminal.clone();
    let destructive = item.action.is_destructive();
    let icon_color = gpui_color(if destructive {
        if item.enabled {
            theme.colors.danger
        } else {
            theme.colors.danger.with_alpha(0.55)
        }
    } else if item.enabled {
        theme.colors.icon
    } else {
        theme.colors.text_muted
    });
    let ChartContextMenuItem {
        id,
        icon,
        label,
        enabled,
        action,
    } = item;
    let mut row = MenuRow::compact(id, label, theme)
        .leading(header_icon(icon).with_size(px(16.0)).color(icon_color))
        .disabled(!enabled)
        .destructive(destructive)
        .flush_in_panel(first, last);
    if action == ChartContextAction::CopyPrice
        && let Some(price) = menu.copy_price.clone()
    {
        row = row.trailing(copy_price_chip(price, enabled, theme));
    }
    row.on_click(move |_, window, cx| {
        if enabled {
            action_terminal.update(cx, |terminal, terminal_cx| {
                terminal.finish_chart_context_menu(menu.clone(), action, window, terminal_cx);
            });
        }
    })
}

pub(super) fn copy_price_chip(
    price: SharedString,
    enabled: bool,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let ink = if enabled {
        colors.text_muted
    } else {
        colors.text_muted.with_alpha(0.55)
    };
    div()
        .flex_none()
        .h(px(18.0))
        .px(px(6.0))
        .flex()
        .items_center()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .border_1()
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
        .text_xs()
        .text_color(gpui_color(ink))
        .child(price)
}

pub(super) fn price_axis_menu_layer(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: PriceAxisMenuState,
    viewport: gpui::Size<Pixels>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let origin = clamp_price_axis_menu_origin(menu.position, viewport, state.left);
    let dismiss = terminal.clone();
    let mut layer = div()
        .id("price_axis_menu_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .on_any_mouse_down(move |_, _, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_chart_context_menu(terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(price_axis_menu_panel(terminal, menu, state, origin, theme));
    if menu.flyout != PriceAxisMenuFlyout::None {
        layer = layer.child(price_axis_flyout_panel(
            terminal,
            menu,
            state,
            clamp_price_axis_flyout_origin(origin, viewport, menu.flyout),
            viewport,
            theme,
        ));
    }
    layer.into_any_element()
}

pub(super) fn price_axis_menu_panel(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: PriceAxisMenuState,
    origin: gpui::Point<Pixels>,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let mut panel = compact_menu_panel(
        "price_axis_menu",
        origin,
        px(CHART_CONTEXT_MENU_WIDTH),
        theme,
    );
    let rows = price_axis_root_rows(menu.flyout, state);
    let last = rows.len().saturating_sub(1);
    for (index, row) in rows.into_iter().enumerate() {
        if matches!(index, 2 | 4) {
            panel = panel.child(menu_separator(theme));
        }
        panel = panel.child(price_axis_menu_item(
            terminal,
            menu,
            row,
            theme,
            index == 0,
            index == last,
        ));
    }
    panel
}

pub(super) fn price_axis_flyout_panel(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: PriceAxisMenuState,
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let mut panel = compact_menu_panel(
        "price_axis_flyout",
        origin,
        px(PRICE_AXIS_FLYOUT_WIDTH),
        theme,
    )
    .max_h(viewport.height)
    .overflow_y_scroll();
    let rows = price_axis_flyout_rows(menu.flyout, state);
    let last = rows.len().saturating_sub(1);
    for (index, row) in rows.into_iter().enumerate() {
        if menu.flyout == PriceAxisMenuFlyout::Labels && index == 9 {
            panel = panel.child(menu_separator(theme));
        }
        panel = panel.child(price_axis_menu_item(
            terminal,
            menu,
            row,
            theme,
            index == 0,
            index == last,
        ));
    }
    panel
}

pub(super) fn price_axis_root_rows(
    flyout: PriceAxisMenuFlyout,
    state: PriceAxisMenuState,
) -> [PriceAxisMenuRow; 7] {
    [
        PriceAxisMenuRow::flyout("Labels", PriceAxisMenuFlyout::Labels, flyout),
        PriceAxisMenuRow::flyout("Lines", PriceAxisMenuFlyout::Lines, flyout),
        PriceAxisMenuRow::toggle(
            "Auto scale",
            state.enabled(PriceAxisMenuState::AUTO_SCALE),
            PriceAxisMenuAction::ToggleAutoScale,
        ),
        PriceAxisMenuRow::toggle(
            "Invert scale",
            state.enabled(PriceAxisMenuState::INVERT_SCALE),
            PriceAxisMenuAction::ToggleInvertScale,
        ),
        PriceAxisMenuRow::flyout("Scale mode", PriceAxisMenuFlyout::ScaleMode, flyout),
        PriceAxisMenuRow::flyout("Y-axis", PriceAxisMenuFlyout::YAxis, flyout),
        PriceAxisMenuRow::flyout("Precision", PriceAxisMenuFlyout::Precision, flyout),
    ]
}

pub(super) fn price_axis_flyout_rows(
    flyout: PriceAxisMenuFlyout,
    state: PriceAxisMenuState,
) -> Vec<PriceAxisMenuRow> {
    match flyout {
        PriceAxisMenuFlyout::None => Vec::new(),
        PriceAxisMenuFlyout::Labels => vec![
            PriceAxisMenuRow::toggle(
                "Symbol name label",
                state.enabled(PriceAxisMenuState::TITLE),
                PriceAxisMenuAction::ToggleTitle,
            ),
            PriceAxisMenuRow::toggle(
                "Symbol last price label",
                state.enabled(PriceAxisMenuState::LAST_VALUE),
                PriceAxisMenuAction::ToggleLastValue,
            ),
            PriceAxisMenuRow::unavailable("Symbol previous day close price label"),
            PriceAxisMenuRow::unavailable("Pre/post/night market price label"),
            PriceAxisMenuRow::unavailable("High and low price labels"),
            PriceAxisMenuRow::toggle(
                "Bid and ask labels",
                state.enabled(PriceAxisMenuState::BID_ASK),
                PriceAxisMenuAction::ToggleBidAsk,
            ),
            PriceAxisMenuRow::toggle(
                "Indicators and financials name labels",
                state.enabled(PriceAxisMenuState::INDICATOR_NAMES),
                PriceAxisMenuAction::ToggleIndicatorNameLabels,
            ),
            PriceAxisMenuRow::toggle(
                "Indicators and financials value labels",
                state.enabled(PriceAxisMenuState::INDICATOR_VALUES),
                PriceAxisMenuAction::ToggleIndicatorValueLabels,
            ),
            PriceAxisMenuRow::toggle(
                "Countdown to bar close",
                state.enabled(PriceAxisMenuState::COUNTDOWN),
                PriceAxisMenuAction::ToggleCountdown,
            ),
            PriceAxisMenuRow::toggle(
                "No overlapping labels",
                state.enabled(PriceAxisMenuState::ALIGN_LABELS),
                PriceAxisMenuAction::ToggleAlignLabels,
            ),
        ],
        PriceAxisMenuFlyout::Lines => vec![
            PriceAxisMenuRow::toggle(
                "Price line",
                state.enabled(PriceAxisMenuState::PRICE_LINE),
                PriceAxisMenuAction::TogglePriceLine,
            ),
            PriceAxisMenuRow::unavailable("Previous day close price line"),
            PriceAxisMenuRow::unavailable("Pre/post/night market price line"),
            PriceAxisMenuRow::unavailable("High and low price lines"),
            PriceAxisMenuRow::toggle(
                "Bid and ask lines",
                state.enabled(PriceAxisMenuState::BID_ASK),
                PriceAxisMenuAction::ToggleBidAsk,
            ),
            PriceAxisMenuRow::toggle(
                "Indicators and financials price lines",
                state.enabled(PriceAxisMenuState::INDICATOR_PRICE_LINES),
                PriceAxisMenuAction::ToggleIndicatorPriceLines,
            ),
        ],
        PriceAxisMenuFlyout::ScaleMode => vec![
            PriceAxisMenuRow::toggle("Normal", state.mode == 0, PriceAxisMenuAction::SetMode(0)),
            PriceAxisMenuRow::toggle(
                "Logarithmic",
                state.mode == 1,
                PriceAxisMenuAction::SetMode(1),
            ),
            PriceAxisMenuRow::toggle(
                "Percentage",
                state.mode == 2,
                PriceAxisMenuAction::SetMode(2),
            ),
            PriceAxisMenuRow::toggle(
                "Indexed to 100",
                state.mode == 3,
                PriceAxisMenuAction::SetMode(3),
            ),
        ],
        PriceAxisMenuFlyout::YAxis => vec![
            PriceAxisMenuRow::toggle("Right", !state.left, PriceAxisMenuAction::SetLeft(false)),
            PriceAxisMenuRow::toggle("Left", state.left, PriceAxisMenuAction::SetLeft(true)),
        ],
        PriceAxisMenuFlyout::Precision => std::iter::once(PriceAxisMenuRow::toggle(
            "Auto",
            state.precision.is_none(),
            PriceAxisMenuAction::SetPrecision(None),
        ))
        .chain([0_u8, 1, 2, 3, 4, 5, 6, 8].into_iter().map(|digits| {
            PriceAxisMenuRow::toggle(
                price_axis_precision_label(digits),
                state.precision == Some(digits),
                PriceAxisMenuAction::SetPrecision(Some(digits)),
            )
        }))
        .collect(),
    }
}

pub(super) const fn price_axis_precision_label(digits: u8) -> &'static str {
    match digits {
        0 => "0 decimals",
        1 => "1 decimal",
        2 => "2 decimals",
        3 => "3 decimals",
        4 => "4 decimals",
        5 => "5 decimals",
        6 => "6 decimals",
        _ => "8 decimals",
    }
}

#[derive(Clone, Copy)]
pub(super) enum PriceAxisMenuRow {
    Toggle {
        label: &'static str,
        checked: bool,
        action: PriceAxisMenuAction,
    },
    Unavailable {
        label: &'static str,
    },
    Flyout {
        label: &'static str,
        flyout: PriceAxisMenuFlyout,
        open: bool,
    },
}

impl PriceAxisMenuRow {
    const fn toggle(label: &'static str, checked: bool, action: PriceAxisMenuAction) -> Self {
        Self::Toggle {
            label,
            checked,
            action,
        }
    }

    const fn unavailable(label: &'static str) -> Self {
        Self::Unavailable { label }
    }

    fn flyout(label: &'static str, flyout: PriceAxisMenuFlyout, open: PriceAxisMenuFlyout) -> Self {
        Self::Flyout {
            label,
            flyout,
            open: open == flyout,
        }
    }

    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Toggle { label, .. }
            | Self::Unavailable { label }
            | Self::Flyout { label, .. } => label,
        }
    }

    pub(super) const fn enabled(self) -> bool {
        !matches!(self, Self::Unavailable { .. })
    }
}

pub(super) fn price_axis_menu_item(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    row: PriceAxisMenuRow,
    theme: &AxiusflowTheme,
    first: bool,
    last: bool,
) -> impl IntoElement {
    let colors = theme.colors;
    let action_terminal = terminal.clone();
    let enabled = row.enabled();
    let checked = matches!(row, PriceAxisMenuRow::Toggle { checked: true, .. });
    let open = matches!(row, PriceAxisMenuRow::Flyout { open: true, .. });
    let chevron = matches!(row, PriceAxisMenuRow::Flyout { .. });
    let label = row.label();
    let menu = menu.clone();
    let mut item = MenuRow::compact(label, label, theme)
        .highlighted(open)
        .disabled(!enabled)
        .flush_in_panel(first, last)
        .on_click(move |_, _, cx| match row {
            PriceAxisMenuRow::Toggle { action, .. } => {
                action_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.apply_price_axis_menu(&menu, action, terminal_cx);
                });
            }
            PriceAxisMenuRow::Flyout { flyout, .. } => {
                action_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.toggle_price_axis_flyout(flyout, terminal_cx);
                });
            }
            PriceAxisMenuRow::Unavailable { .. } => {}
        });
    if checked {
        item = item.trailing(
            header_icon(HugeIcon::CheckIcon)
                .with_size(px(16.0))
                .color(gpui_color(colors.icon)),
        );
    }
    if chevron {
        item = item.trailing(
            header_icon(HugeIcon::ArrowRightIcon01)
                .with_size(px(16.0))
                .color(gpui_color(if enabled {
                    colors.icon
                } else {
                    colors.text_muted
                })),
        );
    }
    item
}

pub(super) fn chart_settings_menu_layer(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: LifecyclePresentation,
    error: Option<&str>,
    viewport: gpui::Size<Pixels>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    let origin = clamp_overlay_origin(menu.position, viewport, CHART_SETTINGS_MENU_WIDTH, 5.0, 2.0);
    let dismiss = terminal.clone();
    let pending = state.pending;
    div()
        .id("chart_settings_menu_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .on_any_mouse_down(move |_, _, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_chart_settings_menu(terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(
            compact_menu_panel(
                "chart_settings_menu",
                origin,
                px(CHART_SETTINGS_MENU_WIDTH),
                theme,
            )
            .child(settings_mode_row(
                terminal,
                "settings_exit_fully",
                DesktopLifetimeMode::ExitWithDesktop,
                state.mode,
                !pending,
                theme,
            ))
            .child(settings_mode_row(
                terminal,
                "settings_engine_warm",
                DesktopLifetimeMode::KeepEngineWarm,
                state.mode,
                !pending,
                theme,
            ))
            .child(settings_mode_row(
                terminal,
                "settings_markets_live",
                DesktopLifetimeMode::KeepMarketsLive,
                state.mode,
                !pending && state.markets_live_permitted,
                theme,
            ))
            .child(menu_separator(theme))
            .child(settings_toggle_row(
                terminal,
                LifecycleToggle::AutoStart,
                state.autostart_enabled,
                !pending,
                theme,
            ))
            .child(menu_separator(theme))
            .child(settings_toggle_row(
                terminal,
                LifecycleToggle::LiveRetention,
                state.markets_live_permitted,
                !pending,
                theme,
            ))
            .children(error.map(|error| {
                div()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(gpui_color(colors.danger))
                    .child(error.to_string())
            })),
        )
        .into_any_element()
}

pub(super) fn settings_mode_row(
    terminal: &Entity<TerminalApp>,
    id: &'static str,
    mode: DesktopLifetimeMode,
    current: DesktopLifetimeMode,
    enabled: bool,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let selected = current == mode;
    let action_terminal = terminal.clone();
    div()
        .id(id)
        .occlude()
        .h(px(CHART_CONTEXT_MENU_ROW_HEIGHT))
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .px_3()
        .text_sm()
        .when(selected, |row| row.text_color(gpui_color(colors.primary)))
        .when(enabled, |row| {
            row.cursor_pointer()
                .hover(|row| row.bg(gpui_color(colors.hover_bg.over(colors.surface_secondary))))
        })
        .when(!enabled, |row| {
            row.text_color(gpui_color(colors.text_muted))
                .cursor_not_allowed()
        })
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            if enabled {
                action_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.set_lifetime_mode(mode, terminal_cx);
                });
            }
            cx.stop_propagation();
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(mode.label())
                .child(settings_help_button(id, mode.description(), theme)),
        )
        .children(selected.then(|| {
            header_icon(HugeIcon::CheckIcon)
                .with_size(px(16.0))
                .color(gpui_color(colors.primary))
        }))
}

pub(super) fn settings_toggle_row(
    terminal: &Entity<TerminalApp>,
    setting: LifecycleToggle,
    selected: bool,
    enabled: bool,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let id = setting.id();
    let label = setting.label();
    let colors = theme.colors;
    let switch_terminal = terminal.clone();
    div()
        .id(id)
        .occlude()
        .h(px(CHART_CONTEXT_MENU_ROW_HEIGHT))
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .px_3()
        .text_sm()
        .when(!enabled, |row| {
            row.text_color(gpui_color(colors.text_muted))
        })
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(label)
                .child(settings_help_button(id, setting.description(), theme)),
        )
        .child(
            Toggle::new(format!("{id}_switch"), theme)
                .selected(selected)
                .disabled(!enabled)
                .aria_label(label)
                .on_click(move |_, _, cx| {
                    if enabled {
                        switch_terminal.update(cx, setting.toggle());
                    }
                }),
        )
}

/// Circular account avatar for the header toolbar. A primitive person glyph
/// keeps the button asset-free: signed-out sessions render muted, signed-in
/// sessions render in primary tones with a presence dot for session state.
pub(super) fn account_avatar_button(
    terminal: &Entity<TerminalApp>,
    account: &axiusflow_desktop::account::AccountMenuState,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let presence = if account.signed_in() {
        colors.bullish
    } else if account.authorizing() {
        colors.bearish
    } else {
        colors.text_muted
    };
    let glyph = if account.signed_in() {
        colors.text_primary
    } else {
        colors.text_muted
    };
    let tooltip = if account.signed_in() {
        format!(
            "Account — {} · {}",
            account.presentation.state, account.presentation.plan
        )
    } else {
        "Account — Sign in".to_string()
    };
    let toggle_terminal = terminal.clone();
    chrome_tooltip(
        "account_avatar",
        tooltip,
        div()
            .id("account_avatar")
            .relative()
            .size(px(28.0))
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(2.0))
            .rounded_full()
            .border_1()
            .border_color(gpui_color(colors.border_secondary))
            .bg(gpui_color(colors.surface_secondary))
            .cursor_pointer()
            .hover(|button| button.border_color(gpui_color(colors.border)))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                toggle_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.toggle_account_menu(terminal_cx);
                });
                cx.stop_propagation();
            })
            .child(div().size(px(7.0)).rounded_full().bg(gpui_color(glyph)))
            .child(
                div()
                    .w(px(14.0))
                    .h(px(6.0))
                    .rounded_t(px(7.0))
                    .bg(gpui_color(glyph)),
            )
            .child(
                div()
                    .absolute()
                    .bottom(px(-1.0))
                    .right(px(-1.0))
                    .size(px(8.0))
                    .rounded_full()
                    .border_1()
                    .border_color(gpui_color(colors.surface))
                    .bg(gpui_color(presence)),
            ),
        theme,
    )
}

/// Account dropdown anchored under the header avatar. Shows sanitized state
/// plus exactly one recovery action: sign in, cancel the browser wait, or
/// sign out.
pub(super) fn account_menu_layer(
    terminal: &Entity<TerminalApp>,
    account: &axiusflow_desktop::account::AccountMenuState,
    viewport: gpui::Size<Pixels>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    let dismiss = terminal.clone();
    let action_terminal = terminal.clone();
    let presentation = &account.presentation;
    let status = if presentation.detail.is_empty() {
        format!("{} · {}", presentation.state, presentation.plan)
    } else {
        presentation.detail.clone()
    };
    let origin = clamp_overlay_origin(
        point(
            px(f32::from(viewport.width) - CHART_SETTINGS_MENU_WIDTH - OVERLAY_EDGE_MARGIN),
            px(theme.dimensions.app_header_height.logical_pixels + 4.0),
        ),
        viewport,
        CHART_SETTINGS_MENU_WIDTH,
        5.0,
        2.0,
    );
    div()
        .id("account_menu_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .on_any_mouse_down(move |_, _, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_account_menu(terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(
            compact_menu_panel("account_menu", origin, px(CHART_SETTINGS_MENU_WIDTH), theme)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .pt_2()
                        .pb_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(gpui_color(colors.text_primary))
                                .child(presentation.action),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(gpui_color(colors.text_muted))
                                .child(status),
                        ),
                )
                .child(account_menu_action(
                    action_terminal,
                    account.authorizing(),
                    account.signed_in(),
                    account.presentation.pending,
                    theme,
                ))
                .children(account.error.as_deref().map(|error| {
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(gpui_color(colors.danger))
                        .child(error.to_string())
                })),
        )
        .into_any_element()
}

fn account_menu_action(
    action_terminal: Entity<TerminalApp>,
    authorizing: bool,
    signed_in: bool,
    pending: bool,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let (id, label, danger) = if authorizing {
        ("account_menu_cancel", "Cancel sign-in", false)
    } else if signed_in {
        ("account_menu_sign_out", "Sign out", true)
    } else {
        ("account_menu_sign_in", "Sign in", false)
    };
    let enabled = !pending;
    div()
        .id(id)
        .occlude()
        .h(px(CHART_CONTEXT_MENU_ROW_HEIGHT))
        .flex()
        .items_center()
        .px_3()
        .text_sm()
        .text_color(gpui_color(if danger {
            colors.danger
        } else {
            colors.text_primary
        }))
        .when(enabled, |row| {
            row.cursor_pointer()
                .hover(|row| row.bg(gpui_color(colors.hover_bg.over(colors.surface_secondary))))
        })
        .when(!enabled, |row| {
            row.text_color(gpui_color(colors.text_muted))
                .cursor_not_allowed()
        })
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            if enabled {
                action_terminal.update(cx, |terminal, terminal_cx| {
                    if authorizing {
                        TerminalApp::cancel_sign_in(terminal_cx);
                    } else if signed_in {
                        TerminalApp::sign_out(terminal_cx);
                    } else {
                        TerminalApp::request_sign_in(terminal_cx);
                    }
                    terminal.close_account_menu(terminal_cx);
                });
            }
            cx.stop_propagation();
        })
        .child(label)
}

pub(super) fn settings_help_button(
    id: &'static str,
    description: &'static str,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    chrome_tooltip(
        id,
        description,
        div()
            .id((id, 0_usize))
            .size(px(16.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
            .border_1()
            .border_color(gpui_color(colors.border_secondary))
            .text_xs()
            .text_color(gpui_color(colors.text_secondary))
            .cursor_pointer()
            .role(Role::Button)
            .aria_label(description)
            .hover(move |button| {
                button
                    .border_color(gpui_color(colors.border))
                    .text_color(gpui_color(colors.text_primary))
            })
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .child("?"),
        theme,
    )
}
