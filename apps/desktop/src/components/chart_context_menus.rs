use super::*;
use crate::desktop::native_ui::theme::platform_border_width;

pub(super) fn overlay_height(rows: f32, separators: f32) -> f32 {
    scaled_overlay_height(rows, separators, MenuScale::BASE)
}

pub(super) fn scaled_overlay_height(rows: f32, separators: f32, scale: MenuScale) -> f32 {
    // 1px border on each side. Compact dropdowns have no extra panel padding.
    2.0 + scale.len(CHART_CONTEXT_MENU_ROW_HEIGHT) * rows
        + CHART_CONTEXT_MENU_SEPARATOR_HEIGHT * separators
}

/// Screen-aware scale for the chart and price-axis context menus. They follow the
/// shared menu scale but keep only part of its growth, so a cursor popup stays
/// compact on large screens where the symbol and indicator panels grow fully.
pub(super) fn chart_context_menu_scale(viewport: gpui::Size<Pixels>) -> MenuScale {
    MenuScale::for_viewport(viewport).with_growth_share(CHART_CONTEXT_MENU_GROWTH_SHARE)
}

pub(super) fn clamp_overlay_origin(
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    width: f32,
    rows: f32,
    separators: f32,
) -> gpui::Point<Pixels> {
    clamp_scaled_overlay_origin(origin, viewport, width, rows, separators, MenuScale::BASE)
}

fn clamp_scaled_overlay_origin(
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    width: f32,
    rows: f32,
    separators: f32,
    scale: MenuScale,
) -> gpui::Point<Pixels> {
    let width = scale.px(width);
    let height = px(scaled_overlay_height(rows, separators, scale));
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
    clamp_scaled_overlay_origin(
        origin,
        viewport,
        CHART_CONTEXT_MENU_WIDTH,
        8.0,
        5.0,
        chart_context_menu_scale(viewport),
    )
}

/// Prefer opening the Y-axis menu into the chart, then keep an edge margin so it
/// never sits flush against the window.
pub(super) fn clamp_price_axis_menu_origin(
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    axis_on_left: bool,
) -> gpui::Point<Pixels> {
    let scale = chart_context_menu_scale(viewport);
    let width = scale.px(CHART_CONTEXT_MENU_WIDTH);
    let height = px(scaled_overlay_height(7.0, 2.0, scale));
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
    let scale = chart_context_menu_scale(viewport);
    let width = scale.px(PRICE_AXIS_FLYOUT_WIDTH);
    let height = px(scaled_overlay_height(rows, separators, scale));
    let margin = px(OVERLAY_EDGE_MARGIN);
    let gap = px(PRICE_AXIS_FLYOUT_GAP);
    let parent_y = root.y
        + scale.px(CHART_CONTEXT_MENU_ROW_HEIGHT) * row
        + px(CHART_CONTEXT_MENU_SEPARATOR_HEIGHT * separators_before);
    let left_x = root.x - width - gap;
    let right_x = root.x + scale.px(CHART_CONTEXT_MENU_WIDTH) + gap;
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
    theme: &AerisTheme,
) -> AnyElement {
    let scale = chart_context_menu_scale(viewport);
    let origin = clamp_chart_context_menu_origin(menu.position, viewport);
    let popup_bounds = Bounds::new(
        origin,
        size(
            scale.px(CHART_CONTEXT_MENU_WIDTH),
            px(scaled_overlay_height(8.0, 5.0, scale)),
        ),
    );
    let animation_origin = PopupAnimationOrigin::from_trigger(menu.position, popup_bounds);
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
        .child(animate_popup_from_origin(
            chart_context_menu_panel(terminal, menu, state, origin, scale, theme),
            "chart_context_menu_enter",
            animation_origin,
        ))
        .into_any_element()
}

pub(super) fn chart_context_menu_panel(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: ChartContextMenuState,
    origin: gpui::Point<Pixels>,
    scale: MenuScale,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let mut panel = flat_compact_menu_panel(
        "chart_context_menu",
        origin,
        scale.px(CHART_CONTEXT_MENU_WIDTH),
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
            scale,
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
            icon: HugeIcon::RefreshV2,
            label: "Reset view",
            enabled: state.enabled(ChartContextMenuState::READY),
            action: ChartContextAction::Reset,
        },
        ChartContextMenuItem {
            id: "chart_context_copy_price",
            icon: HugeIcon::Copy,
            label: "Copy price",
            enabled: state.enabled(ChartContextMenuState::COPY_PRICE),
            action: ChartContextAction::CopyPrice,
        },
        ChartContextMenuItem {
            id: "chart_context_remove_drawings",
            icon: HugeIcon::Trash,
            label: "Remove drawings",
            enabled: state.enabled(ChartContextMenuState::DRAWINGS),
            action: ChartContextAction::ClearDrawings,
        },
        ChartContextMenuItem {
            id: "chart_context_remove_indicators",
            icon: HugeIcon::Trash,
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
            icon: HugeIcon::CloseBold,
            label: "Close chart",
            enabled: state.pane_count > 1,
            action: ChartContextAction::Close,
        },
        ChartContextMenuItem {
            id: "chart_context_settings",
            icon: HugeIcon::Settings,
            label: "Settings",
            enabled: true,
            action: ChartContextAction::Settings,
        },
    ]
}

pub(super) fn chart_context_menu_item(
    terminal: &Entity<TerminalApp>,
    item: ChartContextMenuItem,
    theme: &AerisTheme,
    menu: ChartContextMenu,
    scale: MenuScale,
    first: bool,
    last: bool,
) -> impl IntoElement {
    let icon_size = scale.px(16.0);
    let action_terminal = terminal.clone();
    let destructive = item.action.is_destructive();
    let copy_feedback_generation = (item.action == ChartContextAction::CopyPrice)
        .then_some(menu.copy_feedback_generation)
        .flatten();
    let icon_color = gpui_color(if destructive {
        if item.enabled {
            theme.colors.danger
        } else {
            theme.colors.danger.with_alpha(0.55)
        }
    } else if copy_feedback_generation.is_some() {
        theme.colors.primary
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
    let leading = if let Some(generation) = copy_feedback_generation {
        div()
            .size(icon_size)
            .flex()
            .items_center()
            .justify_center()
            .child(
                header_icon(HugeIcon::CopySuccess)
                    .with_size(icon_size)
                    .color(icon_color),
            )
            .with_animation(
                ("copy_price_success", generation),
                Animation::new(COPY_PRICE_SUCCESS_ANIMATION_DURATION).with_easing(ease_out_quint()),
                |icon, delta| icon.opacity(delta).mt(px((1.0 - delta) * 2.0)),
            )
            .into_any_element()
    } else {
        header_icon(icon)
            .with_size(icon_size)
            .color(icon_color)
            .into_any_element()
    };
    let row_label = if copy_feedback_generation.is_some() {
        "Copied"
    } else {
        label
    };
    let mut row = MenuRow::compact(id, row_label, theme)
        .scale(scale)
        .leading(leading)
        .disabled(!enabled)
        .destructive(destructive)
        .flush_in_panel(first, last);
    if action == ChartContextAction::CopyPrice
        && let Some(price) = menu.copy_price.clone()
    {
        row = row.trailing(copy_price_chip(price, enabled, scale, theme));
    }
    row.on_click(move |event, window, cx| {
        if enabled {
            let mut activated_menu = menu.clone();
            if action == ChartContextAction::Settings {
                activated_menu.position = event.position();
            }
            action_terminal.update(cx, |terminal, terminal_cx| {
                terminal.finish_chart_context_menu(activated_menu, action, window, terminal_cx);
            });
        }
    })
}

pub(super) fn copy_price_chip(
    price: SharedString,
    enabled: bool,
    scale: MenuScale,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let ink = if enabled {
        colors.text_muted
    } else {
        colors.text_muted.with_alpha(0.55)
    };
    div()
        .flex_none()
        .h(scale.px(18.0))
        .px(scale.px(6.0))
        .flex()
        .items_center()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .border_1()
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
        .font_family(aeris_design_system::platform_font_family())
        .font_features(platform_tabular_numerals())
        .text_size(scale.rems(0.75))
        .text_color(gpui_color(ink))
        .child(price)
}

pub(super) fn price_axis_menu_layer(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: PriceAxisMenuState,
    viewport: gpui::Size<Pixels>,
    theme: &AerisTheme,
) -> AnyElement {
    let scale = chart_context_menu_scale(viewport);
    let origin = clamp_price_axis_menu_origin(menu.position, viewport, state.left);
    let root_bounds = Bounds::new(
        origin,
        size(
            scale.px(CHART_CONTEXT_MENU_WIDTH),
            px(scaled_overlay_height(7.0, 2.0, scale)),
        ),
    );
    let root_animation_origin = PopupAnimationOrigin::from_trigger(menu.position, root_bounds);
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
        .child(animate_popup_from_origin(
            price_axis_menu_panel(terminal, menu, state, origin, scale, theme),
            "price_axis_menu_enter",
            root_animation_origin,
        ));
    if menu.flyout != PriceAxisMenuFlyout::None {
        let flyout_origin = clamp_price_axis_flyout_origin(origin, viewport, menu.flyout);
        let (rows, separators, row, separators_before) = menu.flyout.geometry();
        let flyout_bounds = Bounds::new(
            flyout_origin,
            size(
                scale.px(PRICE_AXIS_FLYOUT_WIDTH),
                px(scaled_overlay_height(rows, separators, scale)),
            ),
        );
        let row_height = scale.px(CHART_CONTEXT_MENU_ROW_HEIGHT);
        let parent_y = origin.y
            + row_height * row
            + px(CHART_CONTEXT_MENU_SEPARATOR_HEIGHT * separators_before)
            + row_height / 2.0;
        let parent_x = if flyout_origin.x < origin.x {
            origin.x
        } else {
            origin.x + scale.px(CHART_CONTEXT_MENU_WIDTH)
        };
        let flyout_animation_origin =
            PopupAnimationOrigin::from_trigger(point(parent_x, parent_y), flyout_bounds);
        layer = layer.child(animate_popup_from_origin(
            price_axis_flyout_panel(terminal, menu, state, flyout_origin, viewport, scale, theme),
            ("price_axis_flyout_enter", menu.flyout as usize),
            flyout_animation_origin,
        ));
    }
    layer.into_any_element()
}

pub(super) fn price_axis_menu_panel(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: PriceAxisMenuState,
    origin: gpui::Point<Pixels>,
    scale: MenuScale,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let mut panel = flat_compact_menu_panel(
        "price_axis_menu",
        origin,
        scale.px(CHART_CONTEXT_MENU_WIDTH),
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
            scale,
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
    scale: MenuScale,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let mut panel = flat_compact_menu_panel(
        "price_axis_flyout",
        origin,
        scale.px(PRICE_AXIS_FLYOUT_WIDTH),
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
            scale,
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
    theme: &AerisTheme,
    scale: MenuScale,
    first: bool,
    last: bool,
) -> impl IntoElement {
    let colors = theme.colors;
    let icon_size = scale.px(16.0);
    let action_terminal = terminal.clone();
    let enabled = row.enabled();
    let checked = matches!(row, PriceAxisMenuRow::Toggle { checked: true, .. });
    let open = matches!(row, PriceAxisMenuRow::Flyout { open: true, .. });
    let chevron = matches!(row, PriceAxisMenuRow::Flyout { .. });
    let label = row.label();
    let menu = menu.clone();
    let mut item = MenuRow::compact(label, label, theme)
        .scale(scale)
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
                .with_size(icon_size)
                .color(gpui_color(colors.icon)),
        );
    }
    if chevron {
        item = item.trailing(
            header_icon(HugeIcon::ArrowRight)
                .with_size(icon_size)
                .color(gpui_color(if enabled {
                    colors.icon
                } else {
                    colors.text_muted
                })),
        );
    }
    item
}

/// Where the viewer dragged the chart settings panel. `None` keeps it centered; every fresh
/// open starts centered again.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ChartSettingsPlacement {
    pub(super) origin: Option<gpui::Point<Pixels>>,
    /// Pointer offset from the panel origin captured when a title-bar drag begins.
    grab: gpui::Point<Pixels>,
}

/// Cursor of the settings title bar, kept for the whole move because GPUI shows the drag
/// source's cursor while a drag is active. GPUI's Windows backend maps the open/closed hand
/// styles to the arrow (Windows has no grab cursor), so Windows uses its hand cursor instead.
#[cfg(target_os = "windows")]
const CHART_SETTINGS_MOVE_CURSOR: gpui::CursorStyle = gpui::CursorStyle::PointingHand;
#[cfg(not(target_os = "windows"))]
const CHART_SETTINGS_MOVE_CURSOR: gpui::CursorStyle = gpui::CursorStyle::OpenHand;

/// Drag payload for moving the chart settings panel by its title bar.
#[derive(Clone)]
struct ChartSettingsMoveDrag;

impl Render for ChartSettingsMoveDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

impl TerminalApp {
    fn begin_chart_settings_move(
        &mut self,
        pointer: gpui::Point<Pixels>,
        origin: gpui::Point<Pixels>,
    ) {
        self.chart_settings_placement.grab = pointer - origin;
    }

    fn move_chart_settings(&mut self, pointer: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let origin = pointer - self.chart_settings_placement.grab;
        if self.chart_settings_placement.origin != Some(origin) {
            self.chart_settings_placement.origin = Some(origin);
            cx.notify();
        }
    }
}

pub(super) fn chart_settings_menu_layer(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    view: ChartSettingsView<'_>,
    placement: Option<gpui::Point<Pixels>>,
    viewport: gpui::Size<Pixels>,
    theme: &AerisTheme,
) -> AnyElement {
    let scale = chart_settings_scale(viewport);
    let panel_size = chart_settings_panel_size(viewport);
    let origin = clamp_chart_settings_origin(
        placement.unwrap_or_else(|| chart_settings_centered_origin(viewport, panel_size)),
        viewport,
        panel_size,
    );
    let bounds = Bounds::new(origin, panel_size);
    let animation_origin = PopupAnimationOrigin::from_trigger(menu.position, bounds);
    let dismiss = terminal.clone();
    let move_terminal = terminal.clone();
    let content = match view.section {
        ChartSettingsSection::Series => {
            chart_series_settings(terminal, menu, view.snapshot, view.color_picker, theme)
        }
        ChartSettingsSection::Canvas => {
            chart_canvas_settings(terminal, menu, view.snapshot, view.color_picker, theme)
        }
        ChartSettingsSection::Trading => {
            chart_trading_settings(terminal, menu, view.snapshot, theme)
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
        .on_drag_move::<ChartSettingsMoveDrag>(move |event, _, cx| {
            move_terminal.update(cx, |terminal, terminal_cx| {
                terminal.move_chart_settings(event.event.position, terminal_cx);
            });
        })
        .child(animate_popup_from_origin(
            chart_settings_panel(terminal, menu, bounds, scale, content, view, theme),
            "chart_settings_menu_enter",
            animation_origin,
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
    bounds: Bounds<Pixels>,
    scale: MenuScale,
    content: AnyElement,
    view: ChartSettingsView<'_>,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let dismiss_overlays = terminal.clone();
    div()
        .id("chart_settings_menu")
        .absolute()
        .left(bounds.origin.x)
        .top(bounds.origin.y)
        .w(bounds.size.width)
        .h(bounds.size.height)
        .rounded(px(f32::from(RadiusToken::Medium.logical_pixels())))
        .border_1()
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface))
        .font_family(aeris_design_system::platform_font_family())
        .font_weight(platform_font_weight(TypographyRole::Normal))
        .text_color(gpui_color(colors.text_primary))
        .occlude()
        .on_any_mouse_down(move |_, _, cx| {
            dismiss_overlays.update(cx, |terminal, terminal_cx| {
                terminal.dismiss_chart_settings_overlays(terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(rem_scaled(
            scale,
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(chart_settings_title_bar(
                    terminal,
                    menu,
                    bounds.origin,
                    theme,
                ))
                .child(div().flex_1().min_h_0().child(chart_settings_panel_body(
                    terminal, menu, content, view, theme,
                ))),
        ))
}

/// Title bar of the settings panel. Dragging it moves the panel anywhere in the window.
fn chart_settings_title_bar(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    origin: gpui::Point<Pixels>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let grab_terminal = terminal.clone();
    div()
        .id("chart_settings_title_bar")
        .h(design_rems(CHART_SETTINGS_TITLE_BAR_HEIGHT))
        .flex_none()
        .pl_4()
        .pr_3()
        .flex()
        .items_center()
        .justify_between()
        .border_b(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border_secondary))
        .cursor(CHART_SETTINGS_MOVE_CURSOR)
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            grab_terminal.update(cx, |terminal, _| {
                terminal.begin_chart_settings_move(event.position, origin);
            });
        })
        .on_drag(ChartSettingsMoveDrag, |drag, _, _, cx| {
            cx.new(|_| drag.clone())
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(chart_settings_drag_grip(theme))
                .child(
                    div()
                        .text_sm()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(gpui_color(colors.text_primary))
                        .child("Chart settings"),
                ),
        )
        .child(chart_settings_actions(terminal, menu, theme))
}

fn chart_settings_drag_grip(theme: &AerisTheme) -> Div {
    let mut grip = div().flex().flex_col().gap_0p5();
    for _ in 0..3 {
        let mut row = div().flex().gap_0p5();
        for _ in 0..2 {
            row = row.child(
                div()
                    .size(design_rems(3.0))
                    .rounded_full()
                    .bg(gpui_color(theme.colors.text_muted)),
            );
        }
        grip = grip.child(row);
    }
    grip
}

/// Sidebar, content and template dialog, laid out at the panel's scaled rem.
fn chart_settings_panel_body(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    content: AnyElement,
    view: ChartSettingsView<'_>,
    theme: &AerisTheme,
) -> Div {
    div()
        .relative()
        .size_full()
        .flex()
        .child(chart_settings_sidebar(
            terminal,
            menu,
            view.section,
            view.templates,
            theme,
        ))
        .child(
            div()
                .id("chart_settings_content_scroll")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_y_scroll()
                .px_6()
                .py_4()
                .child(content),
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
    theme: &AerisTheme,
) -> impl IntoElement {
    let close_terminal = terminal.clone();
    let reset_terminal = terminal.clone();
    let reset_menu = menu.clone();
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(chrome_icon_button(
            "chart_settings_reset",
            HugeIcon::Refresh,
            "Reset settings",
            ChromeIconButtonTone::Neutral,
            theme,
            move |_, cx| {
                reset_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.reset_chart_settings(&reset_menu, terminal_cx);
                });
            },
        ))
        .child(chrome_icon_button(
            "chart_settings_close",
            HugeIcon::Close,
            "Close",
            ChromeIconButtonTone::Destructive,
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

/// Screen-aware scale of the settings panel: part of the shared menu growth, so it stays
/// compact on large screens.
fn chart_settings_scale(viewport: gpui::Size<Pixels>) -> MenuScale {
    MenuScale::for_viewport(viewport).with_growth_share(CHART_SETTINGS_GROWTH_SHARE)
}

/// Keeps a dragged settings panel fully inside the window, even after the window shrinks.
fn clamp_chart_settings_origin(
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    panel_size: gpui::Size<Pixels>,
) -> gpui::Point<Pixels> {
    let margin = px(OVERLAY_EDGE_MARGIN);
    let max_x = (viewport.width - panel_size.width - margin).max(margin);
    let max_y = (viewport.height - panel_size.height - margin).max(margin);
    point(
        origin.x.max(margin).min(max_x),
        origin.y.max(margin).min(max_y),
    )
}

/// Grows the 840x600 design with the compact settings scale and insets it inside smaller
/// viewports.
fn chart_settings_panel_size(viewport: gpui::Size<Pixels>) -> gpui::Size<Pixels> {
    let scale = chart_settings_scale(viewport);
    let horizontal_margin = OVERLAY_EDGE_MARGIN * 2.0;
    let vertical_margin = OVERLAY_EDGE_MARGIN * 2.0;
    size(
        px(scale
            .len(CHART_SETTINGS_PANEL_WIDTH)
            .min((f32::from(viewport.width) - horizontal_margin).max(0.0))),
        px(scale
            .len(CHART_SETTINGS_PANEL_HEIGHT)
            .min((f32::from(viewport.height) - vertical_margin).max(0.0))),
    )
}

fn chart_settings_sidebar(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    selected: ChartSettingsSection,
    templates: ChartSettingsTemplateView<'_>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let mut sections = div().flex_1().min_h_0().flex().flex_col().gap_0p5();
    for section in ChartSettingsSection::ALL {
        let active = selected == section;
        let terminal = terminal.clone();
        sections = sections.child(
            div()
                .id(("chart_settings_section", section as usize))
                .w_full()
                .h(design_rems(36.0))
                .px_3()
                .flex()
                .items_center()
                .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
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
                        .font_weight(platform_font_weight(TypographyRole::Normal))
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
        .w(design_rems(CHART_SETTINGS_SIDEBAR_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .border_r_1()
        .border_color(gpui_color(colors.border_secondary))
        .child(sections)
        .child(chart_settings_template_control(
            terminal, menu, templates, theme,
        ))
}

fn chart_settings_template_control(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: ChartSettingsTemplateView<'_>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let toggle = terminal.clone();
    let mut control = div()
        .relative()
        .w_full()
        .pt_3()
        .border_t_1()
        .border_color(gpui_color(colors.border_secondary))
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
        .child(
            Button::new("chart_settings_templates")
                .theme(theme)
                .resting_fill(colors.surface)
                .w_full()
                .h(design_rems(32.0))
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
        let apply_all = terminal.clone();
        let apply_all_menu = menu.clone();
        let template_count = state.templates.len();
        let last = template_count + usize::from(state.apply_to_all);
        let mut popup = div()
            .id("chart_settings_template_menu")
            .absolute()
            .bottom(design_rems(36.0))
            .left_0()
            .w(design_rems(240.0))
            .max_h(design_rems(360.0))
            .overflow_y_scroll()
            .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
            .border_1()
            .border_color(gpui_color(colors.border_secondary))
            .bg(gpui_color(colors.surface))
            .occlude()
            .child(
                MenuRow::compact("chart_template_save", "Save…", theme)
                    .resting_fill(colors.surface)
                    .flush_in_panel(true, last == 0)
                    .on_click(move |_, window, cx| {
                        save.update(cx, |terminal, terminal_cx| {
                            terminal.open_chart_settings_template_save_dialog(window, terminal_cx);
                        });
                    }),
            );
        if state.apply_to_all {
            popup = popup.child(
                MenuRow::compact("chart_template_apply_all", "Apply to all charts", theme)
                    .resting_fill(colors.surface)
                    .flush_in_panel(false, template_count == 0)
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
                MenuRow::compact(("chart_template", index), template.name.clone(), theme)
                    .resting_fill(colors.surface)
                    .flush_in_panel(false, index + 1 == template_count)
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
        control = control.child(gpui::deferred(animate_popup_from_origin(
            popup,
            "chart_settings_template_menu_enter",
            PopupAnimationOrigin::BOTTOM_LEFT,
        )));
    }
    control
}

fn chart_settings_template_save_dialog(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    input: Option<&Entity<InputState>>,
    error: Option<&str>,
    theme: &AerisTheme,
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
                .w(design_rems(420.0))
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
                                .h(design_rems(32.0))
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
                                .h(design_rems(32.0))
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
    theme: &AerisTheme,
) -> AnyElement {
    let body = match snapshot.chart_type {
        ChartType::Candles => {
            candle_series_settings(terminal, menu, &snapshot.appearance, color_picker, theme)
        }
        ChartType::Footprint => {
            footprint_series_settings(terminal, menu, snapshot.order_flow, theme)
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

fn footprint_series_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    settings: OrderFlowSettings,
    theme: &AerisTheme,
) -> AnyElement {
    div()
        .child(settings_group_heading(
            "Numbers bars",
            "Tick-derived cell presentation",
            theme,
        ))
        .child(settings_choice_row(
            terminal,
            menu,
            "Cells",
            &[
                (
                    "Bid × Ask",
                    settings.display_mode == FootprintDisplayMode::BidAsk,
                    ChartSettingsAction::FootprintMode(FootprintDisplayMode::BidAsk),
                ),
                (
                    "Total",
                    settings.display_mode == FootprintDisplayMode::Total,
                    ChartSettingsAction::FootprintMode(FootprintDisplayMode::Total),
                ),
                (
                    "Delta",
                    settings.display_mode == FootprintDisplayMode::Delta,
                    ChartSettingsAction::FootprintMode(FootprintDisplayMode::Delta),
                ),
            ],
            theme,
        ))
        .child(settings_choice_row(
            terminal,
            menu,
            "Profile",
            &[
                (
                    "Profile",
                    settings.display_mode == FootprintDisplayMode::ProfileInBar,
                    ChartSettingsAction::FootprintMode(FootprintDisplayMode::ProfileInBar),
                ),
                (
                    "Ladder",
                    settings.display_mode == FootprintDisplayMode::VolumeLadder,
                    ChartSettingsAction::FootprintMode(FootprintDisplayMode::VolumeLadder),
                ),
                (
                    "Imbalance",
                    settings.display_mode == FootprintDisplayMode::HorizontalImbalance,
                    ChartSettingsAction::FootprintMode(FootprintDisplayMode::HorizontalImbalance),
                ),
                (
                    "Histogram",
                    settings.display_mode == FootprintDisplayMode::BidAskHistogram,
                    ChartSettingsAction::FootprintMode(FootprintDisplayMode::BidAskHistogram),
                ),
            ],
            theme,
        ))
        .child(footprint_row_size_settings(terminal, menu, settings, theme))
        .child(settings_group_heading(
            "Order-flow studies",
            "One shared retained trade stream",
            theme,
        ))
        .child(settings_toggle_row(
            terminal,
            menu,
            "Cumulative delta",
            settings.show_cumulative_delta,
            ChartSettingsAction::ToggleCumulativeDelta,
            theme,
        ))
        .child(settings_toggle_row(
            terminal,
            menu,
            "Delta histogram",
            settings.show_delta_histogram,
            ChartSettingsAction::ToggleDeltaHistogram,
            theme,
        ))
        .child(settings_toggle_row(
            terminal,
            menu,
            "Large trades and sweeps",
            settings.show_trade_bubbles,
            ChartSettingsAction::ToggleTradeBubbles,
            theme,
        ))
        .child(footprint_threshold_settings(
            terminal, menu, settings, theme,
        ))
        .into_any_element()
}

fn footprint_row_size_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    settings: OrderFlowSettings,
    theme: &AerisTheme,
) -> AnyElement {
    let choice = |label: &'static str, ticks: u32| {
        (
            label,
            settings.ticks_per_row == ticks,
            ChartSettingsAction::FootprintTicksPerRow(ticks),
        )
    };
    div()
        .child(settings_choice_row(
            terminal,
            menu,
            "Row size",
            &[
                choice("Auto", 0),
                choice("1 tick", 1),
                choice("2", 2),
                choice("5", 5),
            ],
            theme,
        ))
        .child(settings_choice_row(
            terminal,
            menu,
            "",
            &[
                choice("10", 10),
                choice("25", 25),
                choice("50", 50),
                choice("100 ticks", 100),
            ],
            theme,
        ))
        .into_any_element()
}

fn footprint_threshold_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    settings: OrderFlowSettings,
    theme: &AerisTheme,
) -> AnyElement {
    let selected = settings.trade_bubble_minimum_volume.to_bits();
    settings_choice_row(
        terminal,
        menu,
        "Large threshold",
        &[
            (
                "Auto",
                selected == 0.0_f64.to_bits(),
                ChartSettingsAction::TradeBubbleMinimumVolumeBits(0.0_f64.to_bits()),
            ),
            (
                "10",
                selected == 10.0_f64.to_bits(),
                ChartSettingsAction::TradeBubbleMinimumVolumeBits(10.0_f64.to_bits()),
            ),
            (
                "50",
                selected == 50.0_f64.to_bits(),
                ChartSettingsAction::TradeBubbleMinimumVolumeBits(50.0_f64.to_bits()),
            ),
            (
                "100",
                selected == 100.0_f64.to_bits(),
                ChartSettingsAction::TradeBubbleMinimumVolumeBits(100.0_f64.to_bits()),
            ),
        ],
        theme,
    )
    .into_any_element()
}

fn candle_series_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    appearance: &ChartAppearanceSettings,
    color_picker: Option<&ChartColorPickerState>,
    theme: &AerisTheme,
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
            &effective_appearance_color(&appearance.up_color, |colors| colors.bullish, theme),
            color_picker,
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::Down,
            &effective_appearance_color(&appearance.down_color, |colors| colors.bearish, theme),
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
            &effective_appearance_color(&appearance.wick_up_color, |colors| colors.bullish, theme),
            color_picker,
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::WickDown,
            &effective_appearance_color(
                &appearance.wick_down_color,
                |colors| colors.bearish,
                theme,
            ),
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
            &effective_appearance_color(
                &appearance.border_up_color,
                |colors| colors.bullish,
                theme,
            ),
            color_picker,
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::BorderDown,
            &effective_appearance_color(
                &appearance.border_down_color,
                |colors| colors.bearish,
                theme,
            ),
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
    theme: &AerisTheme,
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
            &effective_appearance_color(&appearance.up_color, |colors| colors.bullish, theme),
            color_picker,
            theme,
        ))
        .child(settings_color_row(
            terminal,
            menu,
            ChartColorSetting::Down,
            &effective_appearance_color(&appearance.down_color, |colors| colors.bearish, theme),
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
    theme: &AerisTheme,
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
    theme: &AerisTheme,
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
    theme: &AerisTheme,
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
    theme: &AerisTheme,
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

fn chart_trading_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    snapshot: &ChartSettingsSnapshot,
    theme: &AerisTheme,
) -> AnyElement {
    settings_content_header(
        "Trading",
        "Control chart trading overlays without changing account or order state.",
        theme,
    )
    .child(settings_group_heading(
        "Chart overlays",
        "Practice and execution presentation",
        theme,
    ))
    .child(settings_toggle_row(
        terminal,
        menu,
        "Order management lines",
        snapshot.trading_visibility.show_order_management_lines,
        ChartSettingsAction::ToggleOrderManagementLines,
        theme,
    ))
    .child(settings_toggle_row(
        terminal,
        menu,
        "Execution marks",
        snapshot.trading_visibility.show_execution_marks,
        ChartSettingsAction::ToggleExecutionMarks,
        theme,
    ))
    .into_any_element()
}

fn canvas_grid_settings(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    appearance: &ChartAppearanceSettings,
    color_picker: Option<&ChartColorPickerState>,
    theme: &AerisTheme,
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
            &effective_appearance_color(&appearance.grid_color, |colors| colors.grid, theme),
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
    theme: &AerisTheme,
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
            &effective_appearance_color(
                &appearance.crosshair_color,
                |colors| colors.crosshair,
                theme,
            ),
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
    theme: &AerisTheme,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .pb_3()
        .border_b_1()
        .border_color(gpui_color(theme.colors.border_secondary))
        .child(
            div()
                .text_base()
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

fn settings_group_heading(
    title: &'static str,
    description: &'static str,
    theme: &AerisTheme,
) -> impl IntoElement {
    div()
        .mt_3()
        .mb_2()
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
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let terminal = terminal.clone();
    let menu = menu.clone();
    div()
        .h(design_rems(38.0))
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
                .w(design_rems(34.0))
                .h(design_rems(19.0))
                .p(design_rems(2.0))
                .flex()
                .items_center()
                .when(enabled, |track| {
                    track.justify_end().bg(gpui_color(colors.primary))
                })
                .when(!enabled, |track| {
                    track
                        .justify_start()
                        .bg(gpui_color(colors.border_secondary))
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
                        .size(design_rems(15.0))
                        .rounded_full()
                        .bg(gpui_color(colors.primary_foreground)),
                ),
        )
}

/// Effective CSS color of a theme-following appearance field under the active theme.
fn effective_appearance_color(
    color: &ChartAppearanceColor,
    role: fn(ChartThemeColors) -> &'static str,
    theme: &AerisTheme,
) -> String {
    match color {
        ChartAppearanceColor::Theme => {
            role(ChartThemeColors::for_theme(aeris_chart_theme(theme.mode))).to_string()
        }
        ChartAppearanceColor::Custom(color) => color.clone(),
    }
}

fn settings_color_row(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    setting: ChartColorSetting,
    value: &str,
    open_picker: Option<&ChartColorPickerState>,
    theme: &AerisTheme,
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
                .h(design_rems(38.0))
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
                        .h(design_rems(30.0))
                        .px_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                        .border(platform_border_width(theme))
                        .border_color(gpui_color(colors.border_secondary))
                        .bg(gpui_color(colors.surface_secondary))
                        .cursor_pointer()
                        .hover(|button| button.bg(gpui_color(colors.hover_bg)))
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
                        .child(div().size(design_rems(16.0)).rounded_full().bg(color))
                        .child(
                            div()
                                .w(design_rems(62.0))
                                .font_family(aeris_design_system::platform_font_family())
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
    theme: &AerisTheme,
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
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let mut controls = div()
        .id(label)
        .flex()
        .items_center()
        .gap_1()
        .p(design_rems(2.0))
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border_1()
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
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
                .h(design_rems(24.0))
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
        .min_h(design_rems(42.0))
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
    let Some(hex) = value.strip_prefix('#') else {
        return gpui_color(fallback);
    };
    let parsed = match hex.len() {
        6 => u32::from_str_radix(hex, 16)
            .ok()
            .map(|rgb| gpui::rgba((rgb << 8) | 0xFF).into()),
        8 => u32::from_str_radix(hex, 16)
            .ok()
            .map(|rgba| gpui::rgba(rgba).into()),
        _ => None,
    };
    parsed.unwrap_or_else(|| gpui_color(fallback))
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
    account: &aeris_desktop::account::AccountMenuState,
    theme: &AerisTheme,
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
    account: &aeris_desktop::account::AccountMenuState,
    theme: &AerisTheme,
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
                .child(aeris_desktop::account::profile_initials(
                    &presentation.display_name,
                    &presentation.email,
                )),
        )
        .children(
            aeris_desktop::account::has_profile_photo(&presentation.photo_url).then(|| {
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
    account: &aeris_desktop::account::AccountMenuState,
    anchor: Option<gpui::Point<Pixels>>,
    viewport: gpui::Size<Pixels>,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let dismiss = terminal.clone();
    let action_terminal = terminal.clone();
    let header = account_menu_header(account, theme);
    let has_header = header.is_some();
    let header_bottom = WORKSPACE_TITLE_BAR_HEIGHT + ACCOUNT_MENU_GAP;
    let anchor = anchor.unwrap_or(point(px(OVERLAY_EDGE_MARGIN), px(header_bottom)));
    let header_rows = if has_header { 2.0 } else { 0.0 };
    let menu_rows = account_menu_action_rows(account);
    let action_rows = menu_rows
        .iter()
        .filter(|row| row.is_some())
        .fold(0.0, |count, _| count + 1.0);
    let separators = menu_rows
        .iter()
        .filter(|row| row.is_none())
        .fold(0.0, |count, _| count + 1.0)
        + if has_header { 1.0 } else { 0.0 };
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
    let popup_bounds = Bounds::new(
        origin,
        size(
            px(CHART_SETTINGS_MENU_WIDTH),
            px(overlay_height(action_rows + header_rows, separators)),
        ),
    );
    let animation_origin = PopupAnimationOrigin::from_trigger(anchor, popup_bounds);
    let panel =
        flat_compact_menu_panel("account_menu", origin, px(CHART_SETTINGS_MENU_WIDTH), theme)
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
            }));
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
        .child(animate_popup_from_origin(
            panel,
            "account_menu_enter",
            animation_origin,
        ))
        .into_any_element()
}

/// Dropdown header for the account panel. Signed-in sessions show the
/// verified name and avatar; other visible states name themselves with their detail. A plain
/// signed-out session shows no identity header.
fn account_menu_header(
    account: &aeris_desktop::account::AccountMenuState,
    theme: &AerisTheme,
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
    ToggleTheme,
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
    account: &aeris_desktop::account::AccountMenuState,
) -> Vec<AccountMenuActionRow> {
    let mut rows = vec![
        Some((
            "account_menu_theme",
            "Switch theme",
            false,
            AccountMenuClick::ToggleTheme,
        )),
        None,
    ];
    rows.extend(authentication_account_menu_action_rows(account));
    rows
}

fn authentication_account_menu_action_rows(
    account: &aeris_desktop::account::AccountMenuState,
) -> Vec<AccountMenuActionRow> {
    if !aeris_desktop::account::AUTH_BACKEND_CONFIGURED {
        vec![Some((
            "account_menu_about",
            "About Aeris Terminal",
            false,
            AccountMenuClick::About,
        ))]
    } else if account.authorizing() {
        vec![
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
                "About Aeris Terminal",
                false,
                AccountMenuClick::About,
            )),
        ]
    } else if account.signed_in() {
        vec![
            Some((
                "account_menu_manage_profile",
                "Manage Profile",
                false,
                AccountMenuClick::ManageProfile,
            )),
            Some((
                "account_menu_about",
                "About Aeris Terminal",
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
        ]
    } else if account.retry_sign_out {
        vec![
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
                "About Aeris Terminal",
                false,
                AccountMenuClick::About,
            )),
        ]
    } else {
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
                "About Aeris Terminal",
                false,
                AccountMenuClick::About,
            )),
        ]
    }
}

fn account_menu_actions(
    action_terminal: &Entity<TerminalApp>,
    account: &aeris_desktop::account::AccountMenuState,
    theme: &AerisTheme,
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
                AccountMenuClick::ToggleTheme
                | AccountMenuClick::ManageProfile
                | AccountMenuClick::About => true,
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
    theme: &AerisTheme,
) -> AnyElement {
    let icon = match spec.click {
        AccountMenuClick::ToggleTheme => match theme.mode {
            aeris_design_system::ThemeMode::Light => HugeIcon::Moon,
            aeris_design_system::ThemeMode::Dark => HugeIcon::Sun,
        },
        AccountMenuClick::ManageProfile => HugeIcon::User,
        AccountMenuClick::About => HugeIcon::Info,
        AccountMenuClick::SignOut => HugeIcon::SignOut,
        AccountMenuClick::SignIn => HugeIcon::ArrowRight,
        AccountMenuClick::Reopen => HugeIcon::ArrowRightDouble,
        AccountMenuClick::Cancel => HugeIcon::Close,
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
    let label = if matches!(spec.click, AccountMenuClick::ToggleTheme) {
        format!("Switch to {} theme", theme.mode.toggled().label())
    } else {
        spec.label.to_string()
    };
    MenuRow::compact(spec.id, label, theme)
        .leading(header_icon(icon).with_size(px(16.0)).color(icon_color))
        .disabled(!spec.enabled)
        .destructive(spec.destructive)
        .flush_in_panel(spec.edges.first, spec.edges.last)
        .on_click(move |_, window, cx| {
            if spec.enabled {
                action_terminal.update(cx, |terminal, terminal_cx| match spec.click {
                    AccountMenuClick::ToggleTheme => {
                        terminal.toggle_theme(window, terminal_cx);
                        terminal.close_account_menu(terminal_cx);
                    }
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
                        if let Err(error) = aeris_desktop::account::open_manage_profile() {
                            eprintln!("Aeris profile browser open degraded: {error}");
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
            point(px(280.0), px(200.0))
        );
    }

    #[test]
    fn dragged_chart_settings_panel_stays_inside_the_window() {
        let viewport = size(px(1_400.0), px(1_000.0));
        let panel_size = chart_settings_panel_size(viewport);
        assert_eq!(
            clamp_chart_settings_origin(point(px(120.0), px(90.0)), viewport, panel_size),
            point(px(120.0), px(90.0)),
            "a drag inside the window is kept exactly"
        );
        assert_eq!(
            clamp_chart_settings_origin(point(px(-500.0), px(-500.0)), viewport, panel_size),
            point(px(OVERLAY_EDGE_MARGIN), px(OVERLAY_EDGE_MARGIN))
        );
        assert_eq!(
            clamp_chart_settings_origin(point(px(5_000.0), px(5_000.0)), viewport, panel_size),
            point(
                viewport.width - panel_size.width - px(OVERLAY_EDGE_MARGIN),
                viewport.height - panel_size.height - px(OVERLAY_EDGE_MARGIN)
            )
        );
    }

    #[test]
    fn chart_settings_panel_grows_on_large_screens() {
        let viewport = size(px(3_840.0), px(2_160.0));
        let factor = chart_settings_scale(viewport).factor();
        assert!(factor > 1.0);
        assert!(
            factor < MenuScale::for_viewport(viewport).factor(),
            "settings grow less than the full-screen pickers"
        );
        let panel_size = chart_settings_panel_size(viewport);
        assert_eq!(
            panel_size,
            size(
                px(CHART_SETTINGS_PANEL_WIDTH * factor),
                px(CHART_SETTINGS_PANEL_HEIGHT * factor)
            )
        );
        let origin = chart_settings_centered_origin(viewport, panel_size);
        assert!(origin.x >= px(OVERLAY_EDGE_MARGIN) && origin.y >= px(OVERLAY_EDGE_MARGIN));
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
    fn chart_color_trigger_preserves_picker_alpha() {
        let rgba = chart_css_color("#335CFF80", AerisTheme::dark().colors.text_secondary).to_rgb();
        assert!((rgba.r - f32::from(0x33_u8) / 255.0).abs() < 0.001);
        assert!((rgba.g - f32::from(0x5C_u8) / 255.0).abs() < 0.001);
        assert!((rgba.b - 1.0).abs() < 0.001);
        assert!((rgba.a - f32::from(0x80_u8) / 255.0).abs() < 0.001);
    }

    #[test]
    fn development_menu_has_no_authentication_actions() {
        let account = aeris_desktop::account::unavailable_menu_state();
        let rows = account_menu_action_rows(&account);
        assert_eq!(rows.len(), 3);
        let Some((theme_id, theme_label, theme_destructive, theme_click)) = rows[0] else {
            panic!("Theme row must be present");
        };
        assert_eq!(theme_id, "account_menu_theme");
        assert_eq!(theme_label, "Switch theme");
        assert!(!theme_destructive);
        assert!(matches!(theme_click, AccountMenuClick::ToggleTheme));
        assert!(rows[1].is_none());
        let Some((id, label, destructive, click)) = rows[2] else {
            panic!("About row must be present");
        };
        assert_eq!(id, "account_menu_about");
        assert_eq!(label, "About Aeris Terminal");
        assert!(!destructive);
        assert!(matches!(click, AccountMenuClick::About));
    }
}
