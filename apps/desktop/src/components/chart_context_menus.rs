use super::*;

// The refresh artwork spans 18/24 of its SVG viewbox while the close artwork spans
// 14/24. Scale the refresh canvas so both header actions have the same optical size.
const CHART_SETTINGS_RESET_ICON_GLYPH: f32 = WORKSPACE_TAB_ICON_GLYPH * 14.0 / 18.0;

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
    let mut panel = flat_compact_menu_panel(
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
        .font_family(axiusflow_design_system::platform_font_family())
        .font_features(platform_tabular_numerals())
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
    let mut panel = flat_compact_menu_panel(
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
    let mut panel = flat_compact_menu_panel(
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
    view: ChartSettingsView<'_>,
    viewport: gpui::Size<Pixels>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let panel_size = chart_settings_panel_size(viewport);
    let origin = chart_settings_centered_origin(viewport, panel_size);
    let dismiss = terminal.clone();
    let content = match view.section {
        ChartSettingsSection::Series => {
            chart_series_settings(terminal, menu, view.snapshot, view.color_picker, theme)
        }
        ChartSettingsSection::Canvas => {
            chart_canvas_settings(terminal, menu, view.snapshot, view.color_picker, theme)
        }
    };
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
        .child(chart_settings_panel(
            terminal, menu, origin, panel_size, content, view, theme,
        ))
        .into_any_element()
}

#[derive(Clone, Copy)]
pub(super) struct ChartSettingsView<'a> {
    pub(super) section: ChartSettingsSection,
    pub(super) snapshot: &'a ChartSettingsSnapshot,
    pub(super) color_picker: Option<&'a ChartColorPickerState>,
    pub(super) templates: ChartSettingsTemplateView<'a>,
}

#[derive(Clone, Copy)]
pub(super) struct ChartSettingsTemplateView<'a> {
    pub(super) overlay: ChartSettingsTemplateOverlay,
    pub(super) name_input: Option<&'a Entity<InputState>>,
    pub(super) error: Option<&'a str>,
    pub(super) templates: &'a [WorkspaceChartSettingsTemplateState],
    pub(super) apply_to_all: bool,
}

fn chart_settings_panel(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    origin: gpui::Point<Pixels>,
    panel_size: gpui::Size<Pixels>,
    content: AnyElement,
    view: ChartSettingsView<'_>,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let dismiss_overlays = terminal.clone();
    let actions = chart_settings_actions(terminal, menu, theme);
    div()
        .id("chart_settings_menu")
        .absolute()
        .left(origin.x)
        .top(origin.y)
        .w(panel_size.width)
        .h(panel_size.height)
        .flex()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border_1()
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface))
        .font_family(axiusflow_design_system::platform_font_family())
        .font_weight(platform_font_weight(TypographyRole::Normal))
        .text_color(gpui_color(colors.text_primary))
        .occlude()
        .on_any_mouse_down(move |_, _, cx| {
            dismiss_overlays.update(cx, |terminal, terminal_cx| {
                terminal.dismiss_chart_settings_overlays(terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(chart_settings_sidebar(
            terminal,
            menu,
            view.section,
            view.templates,
            theme,
        ))
        .child(
            div().flex_1().min_w_0().min_h_0().p_2().child(
                div()
                    .id("chart_settings_content_surface")
                    .relative()
                    .size_full()
                    .min_h_0()
                    .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                    .border_1()
                    .border_color(gpui_color(colors.border_secondary))
                    .bg(gpui_color(colors.surface_secondary))
                    .overflow_hidden()
                    .child(
                        div()
                            .id("chart_settings_content_scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .px_4()
                            .py_3()
                            .child(content),
                    )
                    .child(actions),
            ),
        )
        .children(
            (view.templates.overlay == ChartSettingsTemplateOverlay::SaveDialog).then(|| {
                chart_settings_template_save_dialog(
                    terminal,
                    menu,
                    view.templates.name_input,
                    view.templates.error,
                    theme,
                )
            }),
        )
}

fn chart_settings_actions(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let close_terminal = terminal.clone();
    let reset_terminal = terminal.clone();
    let reset_menu = menu.clone();
    div()
        .absolute()
        .top(px(10.0))
        .right(px(10.0))
        .flex()
        .items_center()
        .gap_1()
        .child(chrome_icon_button(
            "chart_settings_reset",
            HugeIcon::Refresh01Icon,
            CHART_SETTINGS_RESET_ICON_GLYPH,
            "Reset settings",
            ChromeIconButtonTone::Neutral,
            theme,
            move |_, cx| {
                reset_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.reset_chart_settings(&reset_menu, terminal_cx);
                });
            },
        ))
        .child(chrome_close_button(
            "chart_settings_close",
            theme,
            move |_, cx| {
                close_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.close_chart_settings_menu(terminal_cx);
                });
            },
        ))
}

fn chart_settings_centered_origin(
    viewport: gpui::Size<Pixels>,
    panel_size: gpui::Size<Pixels>,
) -> gpui::Point<Pixels> {
    point(
        (viewport.width - panel_size.width) / 2.0,
        (viewport.height - panel_size.height) / 2.0,
    )
}

fn chart_settings_panel_size(viewport: gpui::Size<Pixels>) -> gpui::Size<Pixels> {
    let horizontal_margin = OVERLAY_EDGE_MARGIN * 2.0;
    let vertical_margin = OVERLAY_EDGE_MARGIN * 2.0;
    size(
        px(CHART_SETTINGS_PANEL_WIDTH
            .min((f32::from(viewport.width) - horizontal_margin).max(0.0))),
        px(CHART_SETTINGS_PANEL_HEIGHT
            .min((f32::from(viewport.height) - vertical_margin).max(0.0))),
    )
}

fn chart_settings_sidebar(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    selected: ChartSettingsSection,
    templates: ChartSettingsTemplateView<'_>,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let inner_radius = px((f32::from(RadiusToken::Default.logical_pixels()) - 1.0).max(0.0));
    let mut sections = div().flex_1().min_h_0().flex().flex_col().gap_0p5();
    for section in ChartSettingsSection::ALL {
        let active = selected == section;
        let terminal = terminal.clone();
        sections = sections.child(
            div()
                .id(("chart_settings_section", section as usize))
                .w_full()
                .h(px(32.0))
                .px_2()
                .flex()
                .items_center()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .role(Role::Button)
                .cursor_pointer()
                .when(active, |item| {
                    item.bg(gpui_color(colors.active_bg.over(colors.surface)))
                })
                .when(!active, |item| {
                    item.hover(|item| item.bg(gpui_color(colors.hover_bg.over(colors.surface))))
                })
                .on_click(move |_, _, cx| {
                    terminal.update(cx, |terminal, terminal_cx| {
                        terminal.set_chart_settings_section(section, terminal_cx);
                    });
                    cx.stop_propagation();
                })
                .child(
                    div()
                        .text_sm()
                        .font_weight(if active {
                            platform_font_weight(TypographyRole::Emphasis)
                        } else {
                            platform_font_weight(TypographyRole::Normal)
                        })
                        .text_color(gpui_color(if active {
                            colors.text_primary
                        } else {
                            colors.text_secondary
                        }))
                        .child(section.label()),
                ),
        );
    }
    div()
        .id("chart_settings_sidebar")
        .w(px(CHART_SETTINGS_SIDEBAR_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .p_2()
        .rounded_tl(inner_radius)
        .rounded_bl(inner_radius)
        .bg(gpui_color(colors.surface))
        .child(sections)
        .child(chart_settings_template_control(
            terminal, menu, templates, theme,
        ))
}

fn chart_settings_template_control(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: ChartSettingsTemplateView<'_>,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let toggle = terminal.clone();
    let mut control = div()
        .relative()
        .w_full()
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
        .child(
            Button::new("chart_settings_templates")
                .theme(theme)
                .resting_fill(colors.surface)
                .w_full()
                .label("Template")
                .caret(header_icon(HugeIcon::ChevronDown))
                .on_click(move |_, _, cx| {
                    toggle.update(cx, |terminal, terminal_cx| {
                        terminal.toggle_chart_settings_template_menu(terminal_cx);
                    });
                }),
        );
    if state.overlay == ChartSettingsTemplateOverlay::Menu {
        let save = terminal.clone();
        let set_default = terminal.clone();
        let default_menu = menu.clone();
        let apply_all = terminal.clone();
        let apply_all_menu = menu.clone();
        let mut popup = div()
            .id("chart_settings_template_menu")
            .absolute()
            .bottom(px(36.0))
            .left_0()
            .w(px(240.0))
            .max_h(px(360.0))
            .overflow_y_scroll()
            .p_1()
            .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
            .border_1()
            .border_color(gpui_color(colors.border_secondary))
            .bg(gpui_color(colors.surface))
            .occlude()
            .child(
                MenuRow::compact_inset("chart_template_save", "Save…", theme).on_click(
                    move |_, window, cx| {
                        save.update(cx, |terminal, terminal_cx| {
                            terminal.open_chart_settings_template_save_dialog(window, terminal_cx);
                        });
                    },
                ),
            )
            .child(
                MenuRow::compact_inset("chart_template_default", "Set as default", theme).on_click(
                    move |_, _, cx| {
                        set_default.update(cx, |terminal, terminal_cx| {
                            terminal
                                .set_current_chart_settings_as_default(&default_menu, terminal_cx);
                        });
                    },
                ),
            );
        if state.apply_to_all {
            popup = popup.child(
                MenuRow::compact_inset("chart_template_apply_all", "Apply to all charts", theme)
                    .on_click(move |_, _, cx| {
                        apply_all.update(cx, |terminal, terminal_cx| {
                            terminal.apply_chart_settings_to_all(&apply_all_menu, terminal_cx);
                        });
                    }),
            );
        }
        for (index, template) in state.templates.iter().enumerate() {
            let apply = terminal.clone();
            let apply_menu = menu.clone();
            popup = popup.child(
                MenuRow::compact_inset(("chart_template", index), template.name.clone(), theme)
                    .on_click(move |_, _, cx| {
                        apply.update(cx, |terminal, terminal_cx| {
                            terminal.apply_named_chart_settings_template(
                                &apply_menu,
                                index,
                                terminal_cx,
                            );
                        });
                    }),
            );
        }
        control = control.child(gpui::deferred(popup));
    }
    control
}

fn chart_settings_template_save_dialog(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    input: Option<&Entity<InputState>>,
    error: Option<&str>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    let cancel = terminal.clone();
    let save = terminal.clone();
    let save_menu = menu.clone();
    div()
        .id("chart_template_save_scrim")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui_color(colors.surface.with_alpha(0.72)))
        .child(
            div()
                .w(px(420.0))
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                .border_1()
                .border_color(gpui_color(colors.border_secondary))
                .bg(gpui_color(colors.surface))
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .child("Save chart template"),
                )
                .children(input.map(|input| Input::new(input).platform(theme)))
                .children(error.map(|error| {
                    div()
                        .text_xs()
                        .text_color(gpui_color(colors.danger))
                        .child(error.to_string())
                }))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("chart_template_cancel")
                                .variant(theme, ButtonVariant::Secondary)
                                .label("Cancel")
                                .on_click(move |_, _, cx| {
                                    cancel.update(cx, |terminal, terminal_cx| {
                                        terminal.cancel_chart_settings_template_save(terminal_cx);
                                    });
                                }),
                        )
                        .child(
                            Button::new("chart_template_confirm")
                                .variant(theme, ButtonVariant::Filled)
                                .label("Save")
                                .on_click(move |_, _, cx| {
                                    save.update(cx, |terminal, terminal_cx| {
                                        terminal
                                            .save_chart_settings_template(&save_menu, terminal_cx);
                                    });
                                }),
                        ),
                ),
        )
        .into_any_element()
}

fn chart_series_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    snapshot: &ChartSettingsSnapshot,
    color_picker: Option<&ChartColorPickerState>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let body = match snapshot.chart_type {
        ChartType::Candles => {
            candle_series_settings(terminal, menu, &snapshot.appearance, color_picker, theme)
        }
        ChartType::Bars => {
            bar_series_settings(terminal, menu, &snapshot.appearance, color_picker, theme)
        }
        ChartType::Line | ChartType::BrushableArea => line_series_settings(
            terminal,
            menu,
            &snapshot.appearance,
            color_picker,
            snapshot.chart_type == ChartType::BrushableArea,
            theme,
        ),
        ChartType::Area => {
            area_series_settings(terminal, menu, &snapshot.appearance, color_picker, theme)
        }
        ChartType::Baseline => {
            baseline_series_settings(terminal, menu, &snapshot.appearance, color_picker, theme)
        }
    };
    settings_content_header(
        snapshot.chart_type.label(),
        "Appearance controls adapt to the active price-series type.",
        theme,
    )
    .child(body)
    .into_any_element()
}

fn candle_series_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    appearance: &ChartAppearanceSettings,
    color_picker: Option<&ChartColorPickerState>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    div()
        .child(settings_group_heading(
            "Body",
            "Directional candle colors",
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::Up,
            &appearance.up_color,
            color_picker,
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::Down,
            &appearance.down_color,
            color_picker,
            theme,
        ))
        .child(settings_group_heading(
            "Wicks",
            "Independent wick colors and visibility",
            theme,
        ))
        .child(settings_toggle_row(
            terminal,
            menu,
            "Show wicks",
            appearance.wick_visible,
            ChartSettingsAction::ToggleWicks,
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::WickUp,
            &appearance.wick_up_color,
            color_picker,
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::WickDown,
            &appearance.wick_down_color,
            color_picker,
            theme,
        ))
        .child(settings_group_heading(
            "Borders",
            "Candle outline styling",
            theme,
        ))
        .child(settings_toggle_row(
            terminal,
            menu,
            "Show borders",
            appearance.border_visible,
            ChartSettingsAction::ToggleBorders,
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::BorderUp,
            &appearance.border_up_color,
            color_picker,
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::BorderDown,
            &appearance.border_down_color,
            color_picker,
            theme,
        ))
        .into_any_element()
}

fn bar_series_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    appearance: &ChartAppearanceSettings,
    color_picker: Option<&ChartColorPickerState>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    div()
        .child(settings_group_heading(
            "OHLC bars",
            "Directional colors and bar geometry",
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::Up,
            &appearance.up_color,
            color_picker,
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::Down,
            &appearance.down_color,
            color_picker,
            theme,
        ))
        .child(settings_toggle_row(
            terminal,
            menu,
            "Show open tick",
            appearance.open_visible,
            ChartSettingsAction::ToggleOpen,
            theme,
        ))
        .child(settings_toggle_row(
            terminal,
            menu,
            "Thin bars",
            appearance.thin_bars,
            ChartSettingsAction::ToggleThinBars,
            theme,
        ))
        .into_any_element()
}

fn line_series_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    appearance: &ChartAppearanceSettings,
    color_picker: Option<&ChartColorPickerState>,
    brushable: bool,
    theme: &AxiusflowTheme,
) -> AnyElement {
    div()
        .child(settings_group_heading(
            "Line",
            "Stroke color, width and pattern",
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::Line,
            &appearance.line_color,
            color_picker,
            theme,
        ))
        .child(settings_line_controls(terminal, menu, appearance, theme))
        .when(brushable, |content| {
            content.child(
                div()
                    .mt_3()
                    .text_xs()
                    .text_color(gpui_color(theme.colors.text_muted))
                    .child("Brush selections retain their own transient range styling."),
            )
        })
        .into_any_element()
}

fn area_series_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    appearance: &ChartAppearanceSettings,
    color_picker: Option<&ChartColorPickerState>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    div()
        .child(settings_group_heading(
            "Line",
            "Area boundary stroke",
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::Line,
            &appearance.line_color,
            color_picker,
            theme,
        ))
        .child(settings_line_controls(terminal, menu, appearance, theme))
        .child(settings_group_heading("Fill", "Area fill color", theme))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::AreaTop,
            &appearance.area_top_color,
            color_picker,
            theme,
        ))
        .into_any_element()
}

fn baseline_series_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    appearance: &ChartAppearanceSettings,
    color_picker: Option<&ChartColorPickerState>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    div()
        .child(settings_group_heading(
            "Baseline",
            "Independent colors above and below the base",
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::BaselineTop,
            &appearance.baseline_top_color,
            color_picker,
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::BaselineBottom,
            &appearance.baseline_bottom_color,
            color_picker,
            theme,
        ))
        .child(settings_group_heading(
            "Stroke",
            "Shared line width and pattern",
            theme,
        ))
        .child(settings_line_controls(terminal, menu, appearance, theme))
        .into_any_element()
}

fn chart_canvas_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    snapshot: &ChartSettingsSnapshot,
    color_picker: Option<&ChartColorPickerState>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    settings_content_header(
        "Canvas",
        "Customize chart guides without changing market data or scale state.",
        theme,
    )
    .child(canvas_grid_settings(
        terminal,
        menu,
        &snapshot.appearance,
        color_picker,
        theme,
    ))
    .child(canvas_crosshair_settings(
        terminal,
        menu,
        snapshot,
        color_picker,
        theme,
    ))
    .into_any_element()
}

fn canvas_grid_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    appearance: &ChartAppearanceSettings,
    color_picker: Option<&ChartColorPickerState>,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    div()
        .child(settings_group_heading("Grid", "Pane grid lines", theme))
        .child(settings_toggle_row(
            terminal,
            menu,
            "Show grid lines",
            appearance.grid_visible,
            ChartSettingsAction::ToggleGrid,
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::Grid,
            &appearance.grid_color,
            color_picker,
            theme,
        ))
        .child(settings_choice_row(
            terminal,
            menu,
            "Grid style",
            &[
                (
                    "Solid",
                    appearance.grid_style == 0,
                    ChartSettingsAction::GridStyle(0),
                ),
                (
                    "Dotted",
                    appearance.grid_style == 1,
                    ChartSettingsAction::GridStyle(1),
                ),
                (
                    "Dashed",
                    appearance.grid_style == 2,
                    ChartSettingsAction::GridStyle(2),
                ),
            ],
            theme,
        ))
}

fn canvas_crosshair_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    snapshot: &ChartSettingsSnapshot,
    color_picker: Option<&ChartColorPickerState>,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let appearance = &snapshot.appearance;
    div()
        .child(settings_group_heading(
            "Crosshair",
            "Pointer guides and snapping",
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::Crosshair,
            &appearance.crosshair_color,
            color_picker,
            theme,
        ))
        .child(settings_choice_row(
            terminal,
            menu,
            "Mode",
            &[
                (
                    "Normal",
                    snapshot.crosshair_mode == 0,
                    ChartSettingsAction::CrosshairMode(0),
                ),
                (
                    "Magnet",
                    snapshot.crosshair_mode == 1,
                    ChartSettingsAction::CrosshairMode(1),
                ),
                (
                    "Hidden",
                    snapshot.crosshair_mode == 2,
                    ChartSettingsAction::CrosshairMode(2),
                ),
                (
                    "OHLC",
                    snapshot.crosshair_mode == 3,
                    ChartSettingsAction::CrosshairMode(3),
                ),
            ],
            theme,
        ))
        .child(settings_choice_row(
            terminal,
            menu,
            "Width",
            &[
                (
                    "1",
                    appearance.crosshair_width == 1,
                    ChartSettingsAction::CrosshairWidth(1),
                ),
                (
                    "2",
                    appearance.crosshair_width == 2,
                    ChartSettingsAction::CrosshairWidth(2),
                ),
                (
                    "3",
                    appearance.crosshair_width == 3,
                    ChartSettingsAction::CrosshairWidth(3),
                ),
            ],
            theme,
        ))
        .child(settings_choice_row(
            terminal,
            menu,
            "Style",
            &[
                (
                    "Solid",
                    appearance.crosshair_style == 0,
                    ChartSettingsAction::CrosshairStyle(0),
                ),
                (
                    "Dotted",
                    appearance.crosshair_style == 1,
                    ChartSettingsAction::CrosshairStyle(1),
                ),
                (
                    "Dashed",
                    appearance.crosshair_style == 2,
                    ChartSettingsAction::CrosshairStyle(2),
                ),
            ],
            theme,
        ))
}

fn settings_content_header(
    title: &'static str,
    description: &'static str,
    theme: &AxiusflowTheme,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .pr(px(72.0))
        .child(
            div()
                .text_base()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(theme.colors.text_primary))
                .child(title),
        )
        .child(
            div()
                .mb_2()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child(description),
        )
}

fn settings_group_heading(
    title: &'static str,
    description: &'static str,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    div()
        .mt_3()
        .mb_1()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_sm()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(theme.colors.text_primary))
                .child(title),
        )
        .child(
            div()
                .text_xs()
                .text_color(gpui_color(theme.colors.text_muted))
                .child(description),
        )
}

fn settings_toggle_row(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    label: &'static str,
    enabled: bool,
    action: ChartSettingsAction,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let terminal = terminal.clone();
    let menu = menu.clone();
    div()
        .h(px(38.0))
        .flex()
        .items_center()
        .justify_between()
        .border_b_1()
        .border_color(gpui_color(colors.border_secondary))
        .child(
            div()
                .text_sm()
                .text_color(gpui_color(colors.text_secondary))
                .child(label),
        )
        .child(
            div()
                .id(label)
                .w(px(34.0))
                .h(px(19.0))
                .p(px(2.0))
                .flex()
                .items_center()
                .when(enabled, |track| {
                    track.justify_end().bg(gpui_color(colors.primary))
                })
                .when(!enabled, |track| {
                    track.justify_start().bg(gpui_color(colors.input_border))
                })
                .rounded_full()
                .cursor_pointer()
                .on_click(move |_, _, cx| {
                    terminal.update(cx, |terminal, terminal_cx| {
                        terminal.apply_chart_settings_action(&menu, action, terminal_cx);
                    });
                    cx.stop_propagation();
                })
                .child(
                    div()
                        .size(px(15.0))
                        .rounded_full()
                        .bg(gpui_color(colors.primary_foreground)),
                ),
        )
}

fn settings_color_row(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    setting: ChartColorSetting,
    value: &str,
    open_picker: Option<&ChartColorPickerState>,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let terminal_for_toggle = terminal.clone();
    let current_for_toggle = value.to_string();
    let color = chart_css_color(value, colors.text_secondary);
    let open = open_picker.is_some_and(|picker| picker.setting == setting);
    let mut row = div()
        .relative()
        .w_full()
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(gpui_color(colors.border_secondary))
        .child(
            div()
                .h(px(38.0))
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .text_color(gpui_color(colors.text_secondary))
                        .child(setting.label()),
                )
                .child(
                    div()
                        .id(("chart_color_picker", setting as usize))
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                        .on_click(move |_, window, cx| {
                            terminal_for_toggle.update(cx, |terminal, terminal_cx| {
                                terminal.toggle_chart_color_picker(
                                    setting,
                                    &current_for_toggle,
                                    window,
                                    terminal_cx,
                                );
                            });
                            cx.stop_propagation();
                        })
                        .child(
                            div()
                                .size(px(22.0))
                                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                                .border_1()
                                .border_color(gpui_color(colors.input_border))
                                .bg(color),
                        )
                        .child(
                            div()
                                .w(px(62.0))
                                .font_family(axiusflow_design_system::platform_font_family())
                                .text_xs()
                                .text_color(gpui_color(colors.text_muted))
                                .child(value.to_ascii_uppercase()),
                        ),
                ),
        );
    if open && let Some(picker) = open_picker {
        let apply = terminal.clone();
        let apply_menu = menu.clone();
        row = row.child(gpui::deferred(
            ColorPicker::new(
                ("chart_color_popover", setting as usize),
                value,
                &picker.input,
                theme,
            )
            .error(picker.error.as_deref())
            .on_select(move |value, _, cx| {
                apply.update(cx, |terminal, terminal_cx| {
                    terminal.apply_chart_color(&apply_menu, setting, &value, terminal_cx);
                });
            }),
        ));
    }
    row
}

fn settings_line_controls(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    appearance: &ChartAppearanceSettings,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .child(settings_choice_row(
            terminal,
            menu,
            "Width",
            &[
                (
                    "1",
                    appearance.line_width == 1,
                    ChartSettingsAction::LineWidth(1),
                ),
                (
                    "2",
                    appearance.line_width == 2,
                    ChartSettingsAction::LineWidth(2),
                ),
                (
                    "3",
                    appearance.line_width == 3,
                    ChartSettingsAction::LineWidth(3),
                ),
                (
                    "4",
                    appearance.line_width == 4,
                    ChartSettingsAction::LineWidth(4),
                ),
            ],
            theme,
        ))
        .child(settings_choice_row(
            terminal,
            menu,
            "Style",
            &[
                (
                    "Solid",
                    appearance.line_style == 0,
                    ChartSettingsAction::LineStyle(0),
                ),
                (
                    "Dotted",
                    appearance.line_style == 1,
                    ChartSettingsAction::LineStyle(1),
                ),
                (
                    "Dashed",
                    appearance.line_style == 2,
                    ChartSettingsAction::LineStyle(2),
                ),
            ],
            theme,
        ))
}

fn settings_choice_row(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    label: &'static str,
    choices: &[(&'static str, bool, ChartSettingsAction)],
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let mut controls = div()
        .id(label)
        .flex()
        .items_center()
        .gap_1()
        .p(px(2.0))
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .border_1()
        .border_color(gpui_color(colors.input_border))
        .bg(gpui_color(colors.input_fill))
        .role(Role::TabList)
        .aria_label(label);
    for (index, (choice, selected, action)) in choices.iter().copied().enumerate() {
        let terminal = terminal.clone();
        let menu = menu.clone();
        controls = controls.child(
            Tab::new((label, index), theme)
                .segmented()
                .selected(selected)
                .aria_label(choice)
                .px_2()
                .h(px(24.0))
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .on_click(move |_, _, cx| {
                    terminal.update(cx, |terminal, terminal_cx| {
                        terminal.apply_chart_settings_action(&menu, action, terminal_cx);
                    });
                    cx.stop_propagation();
                })
                .child(choice),
        );
    }
    div()
        .min_h(px(42.0))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .border_b_1()
        .border_color(gpui_color(colors.border_secondary))
        .child(
            div()
                .text_sm()
                .text_color(gpui_color(colors.text_secondary))
                .child(label),
        )
        .child(controls)
}

fn chart_css_color(value: &str, fallback: ThemeColor) -> Hsla {
    value
        .strip_prefix('#')
        .filter(|hex| hex.len() == 6)
        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        .map_or_else(|| gpui_color(fallback), |hex| gpui::rgb(hex).into())
}
/// Circular account avatar for the header toolbar. Signed-out sessions keep
/// the asset-free muted person glyph; signed-in sessions render verified
/// initials with the profile photo overlaid when the user record has one.
/// The photo loads asynchronously through GPUI's image cache: while loading
/// or on failure the element paints nothing and the initials underneath
/// stay visible, so image faults never break authentication or the avatar.
/// Stale loads cannot win: the source derives from the current photo URL
/// every frame, and the cache keys by URI.
pub(super) fn account_avatar_button(
    terminal: &Entity<TerminalApp>,
    account: &axiusflow_desktop::account::AccountMenuState,
    theme: &AxiusflowTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let presentation = &account.presentation;
    let tooltip = if account.signed_in() && !presentation.display_name.is_empty() {
        format!("Account — {}", presentation.display_name)
    } else if account.signed_in() {
        "Account".to_string()
    } else {
        "Account — Sign in".to_string()
    };
    let toggle_terminal = terminal.clone();
    chrome_tooltip(
        "account_avatar",
        tooltip,
        Button::new("account_avatar")
            .theme(theme)
            .h(px(32.0))
            .w(px(56.0))
            .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
            .bg(gpui_color(colors.surface_secondary))
            .cursor_pointer()
            .aria_label("Profile menu")
            .hover(|button| button.bg(gpui_color(colors.hover_bg.over(colors.surface_secondary))))
            .on_click(move |event, _, cx| {
                let anchor = event.position();
                toggle_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.toggle_account_menu_at(anchor, terminal_cx);
                });
                cx.stop_propagation();
            })
            .leading(
                div()
                    .size(px(26.0))
                    .flex_none()
                    .rounded_full()
                    .overflow_hidden()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(account_avatar_face(account, theme)),
            )
            .child(header_icon(HugeIcon::ChevronDown).with_size(px(14.0))),
        theme,
    )
}

fn account_avatar_face(
    account: &axiusflow_desktop::account::AccountMenuState,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    if !account.signed_in() {
        let glyph = colors.text_muted;
        return div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(2.0))
            .child(div().size(px(7.0)).rounded_full().bg(gpui_color(glyph)))
            .child(
                div()
                    .w(px(14.0))
                    .h(px(6.0))
                    .rounded_t(px(7.0))
                    .bg(gpui_color(glyph)),
            )
            .into_any_element();
    }
    let presentation = &account.presentation;
    div()
        .relative()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_xs()
                .font_weight(platform_font_weight(TypographyRole::Normal))
                .text_color(gpui_color(colors.text_primary))
                .child(axiusflow_desktop::account::profile_initials(
                    &presentation.display_name,
                    &presentation.email,
                )),
        )
        .children(
            axiusflow_desktop::account::has_profile_photo(&presentation.photo_url).then(|| {
                img(presentation.photo_url.as_str())
                    .absolute()
                    .inset_0()
                    .size_full()
                    .rounded_full()
                    .object_fit(ObjectFit::Cover)
            }),
        )
        .into_any_element()
}

/// Account dropdown anchored under the header avatar. The panel opens below
/// the avatar's actual click point with a small gap and clamps into the
/// viewport, so resize, scaling, and fullscreen never push it off-screen.
/// Authorizing keeps both recovery exits; authenticated sessions show the
/// verified profile name without billing or email metadata. Global
/// profile/About actions use the same compact menu primitives as chart menus.
pub(super) fn account_menu_layer(
    terminal: &Entity<TerminalApp>,
    account: &axiusflow_desktop::account::AccountMenuState,
    anchor: Option<gpui::Point<Pixels>>,
    viewport: gpui::Size<Pixels>,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let colors = theme.colors;
    let dismiss = terminal.clone();
    let action_terminal = terminal.clone();
    let header = account_menu_header(account, theme);
    let has_header = header.is_some();
    let header_bottom = WORKSPACE_TITLE_BAR_HEIGHT + ACCOUNT_MENU_GAP;
    let anchor = anchor.unwrap_or(point(px(OVERLAY_EDGE_MARGIN), px(header_bottom)));
    let header_rows = if has_header { 2.0 } else { 0.0 };
    let action_rows = if account.signed_in() || account.authorizing() || account.retry_sign_out {
        3.0
    } else {
        2.0
    };
    let separators = 1.0 + if has_header { 1.0 } else { 0.0 };
    let origin = clamp_overlay_origin(
        point(
            anchor.x,
            anchor.y.max(px(header_bottom)) + px(ACCOUNT_MENU_GAP),
        ),
        viewport,
        CHART_SETTINGS_MENU_WIDTH,
        action_rows + header_rows,
        separators,
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
                .children(header)
                .children(has_header.then(|| menu_separator(theme)))
                .children(account_menu_actions(&action_terminal, account, theme))
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

/// Dropdown header for the account panel. Signed-in sessions show the
/// verified name and avatar; other visible states name themselves with their detail. A plain
/// signed-out session shows no identity header.
fn account_menu_header(
    account: &axiusflow_desktop::account::AccountMenuState,
    theme: &AxiusflowTheme,
) -> Option<AnyElement> {
    let colors = theme.colors;
    let presentation = &account.presentation;
    if account.signed_in() {
        let name = if presentation.display_name.is_empty() {
            presentation.state.to_string()
        } else {
            presentation.display_name.clone()
        };
        return Some(
            div()
                .flex()
                .flex_col()
                .px_3()
                .py_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .size(px(28.0))
                                .flex_none()
                                .rounded_full()
                                .overflow_hidden()
                                .child(account_avatar_face(account, theme)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .font_weight(platform_font_weight(TypographyRole::Normal))
                                .text_color(gpui_color(colors.text_primary))
                                .child(name),
                        ),
                )
                .into_any_element(),
        );
    }
    if account.hides_identity() {
        return None;
    }
    // The state names the session; the action rows below own the verbs.
    // Showing the action twice read as two sign-in buttons.
    let subtitle = presentation.detail.clone();
    Some(
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
                    .font_weight(platform_font_weight(TypographyRole::Normal))
                    .text_color(gpui_color(colors.text_primary))
                    .child(presentation.state),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(gpui_color(colors.text_muted))
                    .child(subtitle),
            )
            .into_any_element(),
    )
}

#[derive(Clone, Copy)]
enum AccountMenuClick {
    SignIn,
    Cancel,
    SignOut,
    Reopen,
    ManageProfile,
    About,
}

#[derive(Clone, Copy)]
struct AccountMenuRowSpec {
    id: &'static str,
    label: &'static str,
    destructive: bool,
    enabled: bool,
    click: AccountMenuClick,
    edges: AccountMenuRowEdges,
}

#[derive(Clone, Copy)]
struct AccountMenuRowEdges {
    first: bool,
    last: bool,
}

type AccountMenuActionRow = Option<(&'static str, &'static str, bool, AccountMenuClick)>;

fn account_menu_action_rows(
    account: &axiusflow_desktop::account::AccountMenuState,
) -> Vec<AccountMenuActionRow> {
    if account.authorizing() {
        return vec![
            Some((
                "account_menu_reopen",
                "Open browser page again",
                false,
                AccountMenuClick::Reopen,
            )),
            Some((
                "account_menu_cancel",
                "Cancel sign-in",
                false,
                AccountMenuClick::Cancel,
            )),
            None,
            Some((
                "account_menu_about",
                "About Axiusflow",
                false,
                AccountMenuClick::About,
            )),
        ];
    }
    if account.signed_in() {
        return vec![
            Some((
                "account_menu_manage_profile",
                "Manage Profile",
                false,
                AccountMenuClick::ManageProfile,
            )),
            Some((
                "account_menu_about",
                "About Axiusflow",
                false,
                AccountMenuClick::About,
            )),
            None,
            Some((
                "account_menu_sign_out",
                "Sign out",
                true,
                AccountMenuClick::SignOut,
            )),
        ];
    }
    if account.retry_sign_out {
        return vec![
            Some((
                "account_menu_retry_sign_out",
                "Retry sign-out",
                true,
                AccountMenuClick::SignOut,
            )),
            Some((
                "account_menu_sign_in",
                "Sign in",
                false,
                AccountMenuClick::SignIn,
            )),
            None,
            Some((
                "account_menu_about",
                "About Axiusflow",
                false,
                AccountMenuClick::About,
            )),
        ];
    }
    vec![
        Some((
            "account_menu_sign_in",
            "Sign in",
            false,
            AccountMenuClick::SignIn,
        )),
        None,
        Some((
            "account_menu_about",
            "About Axiusflow",
            false,
            AccountMenuClick::About,
        )),
    ]
}

fn account_menu_actions(
    action_terminal: &Entity<TerminalApp>,
    account: &axiusflow_desktop::account::AccountMenuState,
    theme: &AxiusflowTheme,
) -> Vec<AnyElement> {
    // While the browser holds the transaction there are two recovery exits.
    // Global profile/About actions share the same compact row geometry, while
    // authentication actions alone respect the account request pending flag.
    let rows = account_menu_action_rows(account);
    let authentication_enabled = !account.presentation.pending;
    let first_row = rows.iter().position(Option::is_some);
    let last_row = rows.iter().rposition(Option::is_some);
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| {
            let Some((id, label, danger, click)) = row else {
                return menu_separator(theme).into_any_element();
            };
            let enabled = match click {
                AccountMenuClick::ManageProfile | AccountMenuClick::About => true,
                AccountMenuClick::SignIn
                | AccountMenuClick::Cancel
                | AccountMenuClick::SignOut
                | AccountMenuClick::Reopen => authentication_enabled,
            };
            account_menu_row(
                action_terminal.clone(),
                AccountMenuRowSpec {
                    id,
                    label,
                    destructive: danger,
                    enabled,
                    click,
                    edges: AccountMenuRowEdges {
                        first: account.hides_identity() && first_row == Some(index),
                        last: last_row == Some(index),
                    },
                },
                theme,
            )
        })
        .collect()
}

fn account_menu_row(
    action_terminal: Entity<TerminalApp>,
    spec: AccountMenuRowSpec,
    theme: &AxiusflowTheme,
) -> AnyElement {
    let icon = match spec.click {
        AccountMenuClick::ManageProfile => HugeIcon::User,
        AccountMenuClick::About => HugeIcon::Info,
        AccountMenuClick::SignOut => HugeIcon::SignOut,
        AccountMenuClick::SignIn => HugeIcon::ArrowRightIcon01,
        AccountMenuClick::Reopen => HugeIcon::ArrowRightDouble,
        AccountMenuClick::Cancel => HugeIcon::CancelIcon01,
    };
    let icon_color = gpui_color(if spec.destructive {
        if spec.enabled {
            theme.colors.danger
        } else {
            theme.colors.danger.with_alpha(0.55)
        }
    } else if spec.enabled {
        theme.colors.icon
    } else {
        theme.colors.text_muted
    });
    MenuRow::compact(spec.id, spec.label, theme)
        .leading(header_icon(icon).with_size(px(16.0)).color(icon_color))
        .disabled(!spec.enabled)
        .destructive(spec.destructive)
        .flush_in_panel(spec.edges.first, spec.edges.last)
        .on_click(move |_, _, cx| {
            if spec.enabled {
                action_terminal.update(cx, |terminal, terminal_cx| match spec.click {
                    AccountMenuClick::SignIn => {
                        TerminalApp::request_sign_in(terminal_cx);
                        terminal.close_account_menu(terminal_cx);
                    }
                    AccountMenuClick::Cancel => {
                        TerminalApp::cancel_sign_in(terminal_cx);
                        terminal.close_account_menu(terminal_cx);
                    }
                    AccountMenuClick::SignOut => {
                        TerminalApp::sign_out(terminal_cx);
                        terminal.close_account_menu(terminal_cx);
                    }
                    AccountMenuClick::Reopen => {
                        TerminalApp::reopen_browser_page(terminal_cx);
                        terminal.close_account_menu(terminal_cx);
                    }
                    AccountMenuClick::ManageProfile => {
                        if let Err(error) = axiusflow_desktop::account::open_manage_profile() {
                            eprintln!("Axiusflow profile browser open degraded: {error}");
                        } else {
                            terminal.arm_profile_refresh_after_browser();
                        }
                        terminal.close_account_menu(terminal_cx);
                    }
                    AccountMenuClick::About => terminal.open_about_dialog(terminal_cx),
                });
            }
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chart_settings_header_actions_have_equal_optical_glyph_bounds() {
        let close_artwork = WORKSPACE_TAB_ICON_GLYPH * 14.0;
        let reset_artwork = CHART_SETTINGS_RESET_ICON_GLYPH * 18.0;
        assert!((close_artwork - reset_artwork).abs() < f32::EPSILON);
    }

    #[test]
    fn chart_settings_panel_is_centered_at_its_preferred_size() {
        let viewport = size(px(1_400.0), px(1_000.0));
        let panel_size = chart_settings_panel_size(viewport);
        assert_eq!(
            panel_size,
            size(
                px(CHART_SETTINGS_PANEL_WIDTH),
                px(CHART_SETTINGS_PANEL_HEIGHT)
            )
        );
        assert_eq!(
            chart_settings_centered_origin(viewport, panel_size),
            point(px(280.0), px(190.0))
        );
    }

    #[test]
    fn chart_settings_panel_stays_centered_and_inset_in_small_viewports() {
        let viewport = size(px(480.0), px(320.0));
        let panel_size = chart_settings_panel_size(viewport);
        assert_eq!(panel_size, size(px(464.0), px(304.0)));
        assert_eq!(
            chart_settings_centered_origin(viewport, panel_size),
            point(px(OVERLAY_EDGE_MARGIN), px(OVERLAY_EDGE_MARGIN))
        );
    }

    #[test]
    fn failed_durable_sign_out_exposes_a_retry_action() {
        let mut account = axiusflow_desktop::account::unavailable_menu_state();
        account.presentation.state = "Signed out";
        account.retry_sign_out = true;
        account.error = Some("sign-out cleanup failed".to_string());

        let rows = account_menu_action_rows(&account);
        let Some((id, label, destructive, click)) = rows[0] else {
            panic!("retry sign-out row must be present");
        };
        assert_eq!(id, "account_menu_retry_sign_out");
        assert_eq!(label, "Retry sign-out");
        assert!(destructive);
        assert!(matches!(click, AccountMenuClick::SignOut));
    }
}
