use super::*;

pub(super) fn overlay_height(rows: f32, separators: f32) -> f32 {
    scaled_overlay_height(rows, separators, MenuScale::BASE)
}

pub(super) fn scaled_overlay_height(rows: f32, separators: f32, scale: MenuScale) -> f32 {
    // The `MenuPanel` inset plus a 1px border allowance on each side, around the rows.
    2.0 * (scale.len(MENU_PANEL_INSET) + 1.0)
        + scale.len(CHART_CONTEXT_MENU_ROW_HEIGHT) * rows
        + CHART_CONTEXT_MENU_SEPARATOR_HEIGHT * separators
}

/// Top of a panel's first row below its origin: the 1px border allowance and the scaled inset.
fn first_row_offset(scale: MenuScale) -> Pixels {
    px(1.0 + scale.len(MENU_PANEL_INSET))
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
    clamp_overlay_origin(
        origin,
        viewport,
        CHART_CONTEXT_MENU_WIDTH,
        CHART_CONTEXT_MENU_ROWS,
        CHART_CONTEXT_MENU_SEPARATORS,
    )
}

const CHART_CONTEXT_MENU_ROWS: f32 = 9.0;
const CHART_CONTEXT_MENU_SEPARATORS: f32 = 5.0;
/// The "Capture chart" row: third row, below one separator.
const CAPTURE_CHART_ROW: f32 = 2.0;
const CAPTURE_CHART_ROW_SEPARATORS_BEFORE: f32 = 1.0;

/// The capture chart submenu opens beside its row, to the right when it fits and otherwise
/// to the left, and stays inside the window.
pub(super) fn clamp_chart_capture_flyout_origin(
    root: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
) -> gpui::Point<Pixels> {
    let scale = MenuScale::BASE;
    let width = scale.px(CHART_CAPTURE_FLYOUT_WIDTH);
    let height = px(scaled_overlay_height(2.0, 0.0, scale));
    let margin = px(OVERLAY_EDGE_MARGIN);
    let gap = px(PRICE_AXIS_FLYOUT_GAP);
    let right_x = root.x + scale.px(CHART_CONTEXT_MENU_WIDTH) + gap;
    let left_x = root.x - width - gap;
    let max_x = (viewport.width - width - margin).max(margin);
    let x = if right_x <= max_x || left_x < margin {
        right_x.min(max_x)
    } else {
        left_x
    };
    let row_y = root.y
        + scale.px(CHART_CONTEXT_MENU_ROW_HEIGHT) * CAPTURE_CHART_ROW
        + px(CHART_CONTEXT_MENU_SEPARATOR_HEIGHT * CAPTURE_CHART_ROW_SEPARATORS_BEFORE);
    point(
        x.max(margin),
        row_y
            .max(margin)
            .min((viewport.height - height - margin).max(margin)),
    )
}

/// Prefer opening the Y-axis menu into the chart, then keep an edge margin so it
/// never sits flush against the window.
pub(super) fn clamp_price_axis_menu_origin(
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    axis_on_left: bool,
) -> gpui::Point<Pixels> {
    let scale = MenuScale::BASE;
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
    let scale = MenuScale::BASE;
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
    let scale = MenuScale::BASE;
    let origin = clamp_chart_context_menu_origin(menu.position, viewport);
    let popup_bounds = Bounds::new(
        origin,
        size(
            scale.px(CHART_CONTEXT_MENU_WIDTH),
            px(scaled_overlay_height(
                CHART_CONTEXT_MENU_ROWS,
                CHART_CONTEXT_MENU_SEPARATORS,
                scale,
            )),
        ),
    );
    let animation_origin = PopupAnimationOrigin::from_trigger(menu.position, popup_bounds);
    let dismiss = terminal.clone();
    let dismiss_right = terminal.clone();
    let mut layer = div()
        .id("chart_context_menu_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .on_click(move |_, _, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_chart_context_menu(terminal_cx);
            });
            cx.stop_propagation();
        })
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            dismiss_right.update(cx, |terminal, terminal_cx| {
                terminal.close_chart_context_menu(terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(
            chart_context_menu_panel(terminal, menu, state, origin, scale, theme)
                .animate_from(animation_origin),
        );
    if menu.capture_flyout_open {
        let flyout_origin = clamp_chart_capture_flyout_origin(origin, viewport);
        let flyout_bounds = Bounds::new(
            flyout_origin,
            size(
                scale.px(CHART_CAPTURE_FLYOUT_WIDTH),
                px(scaled_overlay_height(2.0, 0.0, scale)),
            ),
        );
        let row_height = scale.px(CHART_CONTEXT_MENU_ROW_HEIGHT);
        let parent_y = origin.y
            + first_row_offset(scale)
            + row_height * CAPTURE_CHART_ROW
            + px(CHART_CONTEXT_MENU_SEPARATOR_HEIGHT * CAPTURE_CHART_ROW_SEPARATORS_BEFORE)
            + row_height / 2.0;
        let parent_x = if flyout_origin.x < origin.x {
            origin.x
        } else {
            origin.x + scale.px(CHART_CONTEXT_MENU_WIDTH)
        };
        let flyout = MenuPanel::new(
            "capture_chart_menu",
            MenuPlacement::At(flyout_origin),
            theme,
        )
        .scale(scale)
        .width(scale.px(CHART_CAPTURE_FLYOUT_WIDTH))
        .animate_from(PopupAnimationOrigin::from_trigger(
            point(parent_x, parent_y),
            flyout_bounds,
        ))
        .children(
            chart_capture_menu_items(state)
                .into_iter()
                .map(|item| chart_context_menu_item(terminal, item, theme, menu.clone(), scale)),
        );
        layer = layer.child(flyout);
    }
    layer.into_any_element()
}

pub(super) fn chart_context_menu_panel(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: ChartContextMenuState,
    origin: gpui::Point<Pixels>,
    scale: MenuScale,
    theme: &AerisTheme,
) -> MenuPanel {
    let mut panel = MenuPanel::new("chart_context_menu", MenuPlacement::At(origin), theme)
        .scale(scale)
        .width(scale.px(CHART_CONTEXT_MENU_WIDTH));
    for (index, item) in chart_context_menu_items(state).into_iter().enumerate() {
        if matches!(index, 1 | 3 | 5 | 7 | 8) {
            panel = panel.separator();
        }
        panel = panel.child(chart_context_menu_item(
            terminal,
            item,
            theme,
            menu.clone(),
            scale,
        ));
    }
    panel
}

pub(super) fn chart_context_menu_items(state: ChartContextMenuState) -> [ChartContextMenuItem; 9] {
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
            id: "chart_context_capture",
            icon: HugeIcon::Camera,
            label: "Capture chart",
            enabled: state.enabled(ChartContextMenuState::READY),
            action: ChartContextAction::CaptureMenu,
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

pub(super) fn chart_capture_menu_items(state: ChartContextMenuState) -> [ChartContextMenuItem; 2] {
    let enabled = state.enabled(ChartContextMenuState::READY);
    [
        ChartContextMenuItem {
            id: "capture_chart_copy",
            icon: HugeIcon::Copy,
            label: "Copy image",
            enabled,
            action: ChartContextAction::CopyCapture,
        },
        ChartContextMenuItem {
            id: "capture_chart_save",
            icon: HugeIcon::Download,
            label: "Save image…",
            enabled,
            action: ChartContextAction::SaveCapture,
        },
    ]
}

pub(super) fn chart_context_menu_item(
    terminal: &Entity<TerminalApp>,
    item: ChartContextMenuItem,
    theme: &AerisTheme,
    menu: ChartContextMenu,
    scale: MenuScale,
) -> impl IntoElement {
    let icon_size = scale.px(16.0);
    let action_terminal = terminal.clone();
    let destructive = item.action.is_destructive();
    let copy_feedback_generation = menu
        .copy_feedback
        .filter(|feedback| feedback.action == item.action)
        .map(|feedback| feedback.generation);
    let icon_color = gpui_color(if copy_feedback_generation.is_some() {
        theme.colors.primary
    } else {
        MenuRow::leading_icon_color(theme, destructive, item.enabled)
    });
    let ChartContextMenuItem {
        id,
        icon,
        label,
        enabled,
        action,
    } = item;
    let row_label = if copy_feedback_generation.is_some() {
        "Copied"
    } else {
        label
    };
    let opens_capture_menu = action == ChartContextAction::CaptureMenu;
    let row = MenuRow::compact(id, row_label, theme).scale(scale);
    let row = if let Some(generation) = copy_feedback_generation {
        row.leading(
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
                    ("chart_copy_success", generation),
                    Animation::new(CHART_COPY_SUCCESS_ANIMATION_DURATION)
                        .with_easing(ease_out_quint()),
                    |icon, delta| icon.opacity(delta).mt(px((1.0 - delta) * 2.0)),
                ),
        )
    } else {
        row.leading_icon(header_icon(icon).with_size(icon_size))
    };
    let mut row = row
        .disabled(!enabled)
        .destructive(destructive)
        .highlighted(opens_capture_menu && menu.capture_flyout_open);
    if action == ChartContextAction::CopyPrice
        && let Some(price) = menu.copy_price.clone()
    {
        row = row.trailing(copy_price_chip(price, enabled, scale, theme));
    }
    if opens_capture_menu {
        row = row.trailing(
            header_icon(HugeIcon::ArrowRight)
                .with_size(icon_size)
                .color(icon_color),
        );
    }
    if !matches!(
        action,
        ChartContextAction::CopyCapture | ChartContextAction::SaveCapture
    ) {
        let hover_terminal = terminal.clone();
        row = row.on_hover(move |hovered, _, cx| {
            if *hovered {
                hover_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.set_chart_capture_flyout(opens_capture_menu && enabled, terminal_cx);
                });
            }
        });
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
    let scale = MenuScale::BASE;
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
    let dismiss_right = terminal.clone();
    let mut layer = div()
        .id("price_axis_menu_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .on_click(move |_, _, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_chart_context_menu(terminal_cx);
            });
            cx.stop_propagation();
        })
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            dismiss_right.update(cx, |terminal, terminal_cx| {
                terminal.close_chart_context_menu(terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(
            price_axis_menu_panel(terminal, menu, state, origin, scale, theme)
                .animate_from(root_animation_origin),
        );
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
            + first_row_offset(scale)
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
        layer = layer.child(
            price_axis_flyout_panel(terminal, menu, state, flyout_origin, viewport, scale, theme)
                .animate_from(flyout_animation_origin),
        );
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
) -> MenuPanel {
    let mut panel = MenuPanel::new("price_axis_menu", MenuPlacement::At(origin), theme)
        .scale(scale)
        .width(scale.px(CHART_CONTEXT_MENU_WIDTH));
    for (index, row) in price_axis_root_rows(menu.flyout, state)
        .into_iter()
        .enumerate()
    {
        if matches!(index, 2 | 4) {
            panel = panel.separator();
        }
        panel = panel.child(price_axis_menu_item(terminal, menu, row, theme, scale));
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
) -> MenuPanel {
    // Keyed by flyout so switching flyouts replays the entry motion.
    let mut panel = MenuPanel::new(
        ("price_axis_flyout", menu.flyout as usize),
        MenuPlacement::At(origin),
        theme,
    )
    .scale(scale)
    .width(scale.px(PRICE_AXIS_FLYOUT_WIDTH))
    .max_height((viewport.height - px(2.0 * OVERLAY_EDGE_MARGIN)).max(px(0.0)));
    for (index, row) in price_axis_flyout_rows(menu.flyout, state)
        .into_iter()
        .enumerate()
    {
        if menu.flyout == PriceAxisMenuFlyout::Labels && index == 9 {
            panel = panel.separator();
        }
        panel = panel.child(price_axis_menu_item(terminal, menu, row, theme, scale));
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
) -> impl IntoElement {
    let colors = theme.colors;
    let icon_size = scale.px(16.0);
    let action_terminal = terminal.clone();
    let enabled = row.enabled();
    let toggle = match row {
        PriceAxisMenuRow::Toggle { checked, .. } => Some(checked),
        _ => None,
    };
    let open = matches!(row, PriceAxisMenuRow::Flyout { open: true, .. });
    let chevron = matches!(row, PriceAxisMenuRow::Flyout { .. });
    let label = row.label();
    let menu = menu.clone();
    let mut item = MenuRow::compact(label, label, theme)
        .scale(scale)
        .highlighted(open)
        .disabled(!enabled)
        .when_some(toggle, MenuRow::checked)
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
    let origin = clamp_floating_panel_origin(
        placement.unwrap_or_else(|| chart_settings_centered_origin(viewport, panel_size)),
        viewport,
        panel_size,
    );
    let bounds = Bounds::new(origin, panel_size);
    let animation_origin = PopupAnimationOrigin::from_trigger(menu.position, bounds);
    let dismiss = terminal.clone();
    let dismiss_right = terminal.clone();
    let move_terminal = terminal.clone();
    let ui = SettingsUi {
        terminal,
        menu,
        color_picker: view.color_picker,
        theme,
    };
    let content = match view.section {
        ChartSettingsSection::Symbol => chart_symbol_settings(ui, view.snapshot),
        ChartSettingsSection::Canvas => chart_canvas_settings(ui, view.snapshot),
        ChartSettingsSection::Legend => chart_legend_settings(ui, view.snapshot),
        ChartSettingsSection::Trading => chart_trading_settings(ui, view.snapshot),
    };
    div()
        .id("chart_settings_menu_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .on_click(move |_, _, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_chart_settings_menu(terminal_cx);
            });
            cx.stop_propagation();
        })
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            dismiss_right.update(cx, |terminal, terminal_cx| {
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
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(rem_scaled(
            scale,
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(chart_settings_title_bar(terminal, bounds.origin, theme))
                .child(div().flex_1().min_h_0().child(chart_settings_panel_body(
                    terminal, menu, content, view, theme,
                ))),
        ))
}

/// Title bar of the settings panel. Dragging it moves the panel anywhere in the window.
fn chart_settings_title_bar(
    terminal: &Entity<TerminalApp>,
    origin: gpui::Point<Pixels>,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let grab_terminal = terminal.clone();
    let close_terminal = terminal.clone();
    div()
        .id("chart_settings_title_bar")
        .relative()
        .h(design_rems(CHART_SETTINGS_TITLE_BAR_HEIGHT))
        .flex_none()
        .pl_4()
        .pr_3()
        .flex()
        .items_center()
        .justify_between()
        .border_b(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border_secondary))
        .cursor(FLOATING_PANEL_MOVE_CURSOR)
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
                .text_sm()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(colors.text_primary))
                .child("Chart settings"),
        )
        .child(chart_settings_drag_grip(theme))
        .child(close_button("chart_settings_close", theme, move |_, cx| {
            close_terminal.update(cx, |terminal, terminal_cx| {
                terminal.close_chart_settings_menu(terminal_cx);
            });
        }))
}

fn chart_settings_drag_grip(theme: &AerisTheme) -> Div {
    let mut grip = div()
        .absolute()
        .left(relative(0.5))
        .top(relative(0.5))
        .ml(design_rems(-21.0))
        .mt(design_rems(-9.0))
        .w(design_rems(42.0))
        .h(design_rems(18.0))
        .flex()
        .items_center()
        .justify_center()
        .gap_1()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .border_1()
        .border_color(gpui_color(theme.colors.border_secondary))
        .bg(gpui_color(theme.colors.surface_secondary));
    for _ in 0..3 {
        grip = grip.child(
            div()
                .size(design_rems(3.0))
                .rounded_full()
                .bg(gpui_color(theme.colors.text_muted)),
        );
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
        .children(match view.templates.overlay {
            ChartSettingsTemplateOverlay::SaveDialog => Some(chart_settings_template_save_dialog(
                terminal,
                menu,
                view.templates.name_input,
                view.templates.error,
                theme,
            )),
            ChartSettingsTemplateOverlay::DeleteDialog(index) => {
                view.templates.templates.get(index).map(|template| {
                    chart_settings_template_delete_dialog(terminal, template, theme)
                })
            }
            ChartSettingsTemplateOverlay::Closed
            | ChartSettingsTemplateOverlay::Menu
            | ChartSettingsTemplateOverlay::TemplatesFlyout => None,
        })
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
/// Keeps a dragged floating panel inside the window, inset by the overlay edge margin.
pub(super) fn clamp_floating_panel_origin(
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    panel_size: gpui::Size<Pixels>,
) -> gpui::Point<Pixels> {
    clamp_panel_origin_within(origin, viewport, panel_size, px(OVERLAY_EDGE_MARGIN))
}

/// Keeps a panel inside the window, `margin` from every edge.
pub(super) fn clamp_panel_origin_within(
    origin: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    panel_size: gpui::Size<Pixels>,
    margin: Pixels,
) -> gpui::Point<Pixels> {
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
    let sections = TabList::sidebar("chart_settings_sections", "Chart settings sections")
        .flex_1()
        .min_h_0()
        .children(ChartSettingsSection::ALL.into_iter().map(|section| {
            let terminal = terminal.clone();
            Tab::new(("chart_settings_section", section as usize), theme)
                .sidebar()
                .selected(selected == section)
                .on_click(move |_, _, cx| {
                    terminal.update(cx, |terminal, terminal_cx| {
                        terminal.set_chart_settings_section(section, terminal_cx);
                    });
                    cx.stop_propagation();
                })
                .child(section.label())
        }));
    div()
        .id("chart_settings_sidebar")
        .w(design_rems(CHART_SETTINGS_SIDEBAR_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .flex_col()
        .gap_3()
        .p_2()
        .border_r(platform_border_width(theme))
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
    let open = state.overlay.menu_open();
    let button = Button::new("chart_settings_templates", theme)
        .variant(ButtonVariant::Outline)
        .button_size(ButtonSize::Lg)
        .trigger()
        .full_width()
        .open(open)
        .label("Template")
        .caret(header_icon(HugeIcon::ChevronDown))
        .on_click(move |_, _, cx| {
            toggle.update(cx, |terminal, terminal_cx| {
                terminal.toggle_chart_settings_template_menu(terminal_cx);
            });
        });
    // The flyout hangs off the trigger, beside the menu that opens above it, so its last row
    // lines up with the menu's last row, the one that opens it.
    let trigger = div().relative().w_full().child(button).children(
        (state.overlay == ChartSettingsTemplateOverlay::TemplatesFlyout).then(|| {
            gpui::deferred(
                div()
                    .absolute()
                    .bottom_full()
                    .mb(ANCHOR_GAP)
                    .left(design_rems(
                        CHART_SETTINGS_TEMPLATE_MENU_WIDTH + TIMEFRAME_FLYOUT_GAP,
                    ))
                    .child(chart_settings_templates_flyout(
                        terminal, menu, state, theme,
                    )),
            )
        }),
    );
    let menu = open.then(|| chart_settings_template_menu(terminal, menu, state, theme));
    // The settings panel closes its popovers on any press inside it; presses on this control
    // belong to the trigger and its menus.
    div()
        .w_full()
        .pt_3()
        .border_t(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
        .child(
            MenuAnchor::new("chart_settings_template_anchor", trigger)
                .full_width()
                .menu(menu),
        )
}

fn chart_settings_template_menu(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: ChartSettingsTemplateView<'_>,
    theme: &AerisTheme,
) -> MenuPanel {
    let reset = terminal.clone();
    let reset_menu = menu.clone();
    let save = terminal.clone();
    let apply_all = terminal.clone();
    let apply_all_menu = menu.clone();
    let mut actions = vec![
        MenuRow::compact("chart_template_reset", "Reset to defaults", theme).on_click(
            move |_, _, cx| {
                reset.update(cx, |terminal, terminal_cx| {
                    terminal.reset_chart_settings(&reset_menu, terminal_cx);
                });
            },
        ),
        MenuRow::compact("chart_template_save", "Save as template…", theme).on_click(
            move |_, window, cx| {
                save.update(cx, |terminal, terminal_cx| {
                    terminal.open_chart_settings_template_save_dialog(window, terminal_cx);
                });
            },
        ),
    ];
    if state.apply_to_all {
        actions.push(
            MenuRow::compact("chart_template_apply_all", "Apply to all charts", theme).on_click(
                move |_, _, cx| {
                    apply_all.update(cx, |terminal, terminal_cx| {
                        terminal.apply_chart_settings_to_all(&apply_all_menu, terminal_cx);
                    });
                },
            ),
        );
    }
    let actions = actions.into_iter().map(|row| {
        let hover = terminal.clone();
        row.on_hover(move |hovered, _, cx| {
            if *hovered {
                hover.update(cx, |terminal, terminal_cx| {
                    terminal.set_chart_settings_templates_flyout(false, terminal_cx);
                });
            }
        })
    });
    let flyout_open = state.overlay == ChartSettingsTemplateOverlay::TemplatesFlyout;
    let hover = terminal.clone();
    let open_flyout = terminal.clone();
    let templates_row = MenuRow::compact("chart_template_saved", "Templates", theme)
        .highlighted(flyout_open)
        .trailing(
            header_icon(HugeIcon::ArrowRight)
                .with_size(px(16.0))
                .color(gpui_color(theme.colors.icon)),
        )
        .on_hover(move |hovered, _, cx| {
            if *hovered {
                hover.update(cx, |terminal, terminal_cx| {
                    terminal.set_chart_settings_templates_flyout(true, terminal_cx);
                });
            }
        })
        .on_click(move |_, _, cx| {
            open_flyout.update(cx, |terminal, terminal_cx| {
                terminal.set_chart_settings_templates_flyout(true, terminal_cx);
            });
        });
    MenuPanel::new(
        "chart_settings_template_menu",
        MenuPlacement::Anchored {
            side: MenuSide::Above,
            align: MenuAlign::Start,
        },
        theme,
    )
    .width(design_rems(CHART_SETTINGS_TEMPLATE_MENU_WIDTH))
    .animate_from(PopupAnimationOrigin::BOTTOM_LEFT)
    .children(actions)
    .separator()
    .child(templates_row)
}

/// Width of the template menu and of its saved-templates flyout.
const CHART_SETTINGS_TEMPLATE_MENU_WIDTH: f32 = 240.0;

/// The saved templates: a row applies its template, its trash button asks to delete it.
fn chart_settings_templates_flyout(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    state: ChartSettingsTemplateView<'_>,
    theme: &AerisTheme,
) -> MenuPanel {
    let rows = state.templates.iter().enumerate().map(|(index, template)| {
        let apply = terminal.clone();
        let apply_menu = menu.clone();
        let delete = terminal.clone();
        MenuRow::compact(("chart_template", index), template.name.clone(), theme)
            .trailing(
                Button::new(("chart_template_delete", index), theme)
                    .variant(ButtonVariant::Ghost)
                    .button_size(ButtonSize::Sm)
                    .icon(header_icon(HugeIcon::Trash))
                    .aria_label(format!("Delete template {}", template.name))
                    .danger_on_hover()
                    .tab_stop(false)
                    .on_click(move |_, _, cx| {
                        delete.update(cx, |terminal, terminal_cx| {
                            terminal.confirm_chart_settings_template_delete(index, terminal_cx);
                        });
                        cx.stop_propagation();
                    }),
            )
            .on_click(move |_, _, cx| {
                apply.update(cx, |terminal, terminal_cx| {
                    terminal.apply_named_chart_settings_template(&apply_menu, index, terminal_cx);
                });
            })
    });
    let empty = state.templates.is_empty().then(|| {
        MenuRow::compact("chart_template_none", "No saved templates", theme).disabled(true)
    });
    MenuPanel::new(
        "chart_settings_templates_flyout",
        MenuPlacement::InFlow,
        theme,
    )
    .width(design_rems(CHART_SETTINGS_TEMPLATE_MENU_WIDTH))
    .max_height(design_rems(360.0))
    .animate_from(PopupAnimationOrigin::new(0.0, 1.0))
    .children(rows)
    .children(empty)
}

fn chart_settings_template_delete_dialog(
    terminal: &Entity<TerminalApp>,
    template: &WorkspaceChartSettingsTemplateState,
    theme: &AerisTheme,
) -> AnyElement {
    let cancel = terminal.clone();
    let confirm = terminal.clone();
    ConfirmationDialog::new(
        "chart_template_delete_dialog",
        "Delete chart template?",
        ConfirmationTone::Destructive,
        theme,
        move |_, cx| {
            cancel.update(cx, |terminal, terminal_cx| {
                terminal.cancel_chart_settings_template_delete(terminal_cx);
            });
        },
        move |_, cx| {
            confirm.update(cx, |terminal, terminal_cx| {
                terminal.delete_chart_settings_template(terminal_cx);
            });
        },
    )
    .confirm_label("Delete")
    .message(format!(
        "Delete the \u{201c}{}\u{201d} template? Charts that already use it keep their settings.",
        template.name
    ))
    .into_any_element()
}

fn chart_settings_template_save_dialog(
    terminal: &Entity<TerminalApp>,
    menu: &ChartContextMenu,
    input: Option<&Entity<InputState>>,
    error: Option<&str>,
    theme: &AerisTheme,
) -> AnyElement {
    let cancel = terminal.clone();
    let save = terminal.clone();
    let save_menu = menu.clone();
    ConfirmationDialog::new(
        "chart_template_save_dialog",
        "Save chart template",
        ConfirmationTone::Positive,
        theme,
        move |_, cx| {
            cancel.update(cx, |terminal, terminal_cx| {
                terminal.cancel_chart_settings_template_save(terminal_cx);
            });
        },
        move |_, cx| {
            save.update(cx, |terminal, terminal_cx| {
                terminal.save_chart_settings_template(&save_menu, terminal_cx);
            });
        },
    )
    .confirm_label("Save")
    .children(input.map(|input| Input::new(input).platform(theme)))
    .children(error.map(|error| {
        div()
            .text_xs()
            .text_color(gpui_color(theme.colors.danger))
            .child(error.to_string())
    }))
    .into_any_element()
}

/// Height of one settings row.
const SETTINGS_ROW_HEIGHT: f32 = 40.0;
/// Width of one color column in a paired row (up and down, above and below).
const SETTINGS_PAIR_COLUMN_WIDTH: f32 = 136.0;
/// Width of the trailing switch column of paired rows. Rows without a switch keep it empty so
/// every row's colors line up under their captions.
const SETTINGS_SWITCH_COLUMN_WIDTH: f32 = 44.0;
/// The engine's hidden crosshair mode.
const CROSSHAIR_HIDDEN: u8 = 2;

/// What every settings row needs: the app that applies changes, the chart they target, the
/// open color picker and the theme.
#[derive(Clone, Copy)]
struct SettingsUi<'a> {
    terminal: &'a Entity<TerminalApp>,
    menu: &'a ChartContextMenu,
    color_picker: Option<&'a ChartColorPickerState>,
    theme: &'a AerisTheme,
}

impl SettingsUi<'_> {
    fn apply_on_change(
        self,
        action: ChartSettingsAction,
    ) -> impl Fn(bool, &gpui::ClickEvent, &mut Window, &mut App) + 'static {
        let terminal = self.terminal.clone();
        let menu = self.menu.clone();
        move |_, _, _, cx| {
            terminal.update(cx, |terminal, terminal_cx| {
                terminal.apply_chart_settings_action(&menu, action, terminal_cx);
            });
        }
    }
}

/// The trailing switch of a paired color row.
#[derive(Clone, Copy)]
enum PairSwitch {
    /// The group has no switch column.
    Absent,
    /// The group has a switch column, but this part is always drawn.
    Empty,
    /// Shows or hides the part; its colors lock while it is hidden.
    Toggle(bool, ChartSettingsAction),
}

impl PairSwitch {
    const fn has_column(self) -> bool {
        !matches!(self, Self::Absent)
    }

    const fn part_visible(self) -> bool {
        match self {
            Self::Toggle(visible, _) => visible,
            Self::Absent | Self::Empty => true,
        }
    }
}

/// One color of a settings row: which setting it edits and the color the chart paints.
#[derive(Clone, Copy)]
struct ColorCell<'a> {
    setting: ChartColorSetting,
    value: &'a str,
}

/// A single-choice setting: the options, which is selected, and the action each applies.
type Choices<'a> = &'a [(&'static str, bool, ChartSettingsAction)];

/// Identity and text of a single-choice row. `id` keeps rows that share a label (two "Style"
/// rows on one page) distinct.
#[derive(Clone, Copy)]
struct ChoiceRow {
    id: &'static str,
    label: &'static str,
    hint: Option<&'static str>,
    enabled: bool,
}

impl ChoiceRow {
    const fn new(id: &'static str, label: &'static str) -> Self {
        Self {
            id,
            label,
            hint: None,
            enabled: true,
        }
    }

    const fn hint(mut self, hint: &'static str) -> Self {
        self.hint = Some(hint);
        self
    }

    const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// A switch and the one option that refines it.
#[derive(Clone, Copy)]
struct SwitchWithOption {
    label: &'static str,
    hint: Option<&'static str>,
    enabled: bool,
    action: ChartSettingsAction,
    option: &'static str,
    option_checked: bool,
    option_action: ChartSettingsAction,
}

fn chart_symbol_settings(ui: SettingsUi<'_>, snapshot: &ChartSettingsSnapshot) -> AnyElement {
    let title = snapshot.chart_type.label();
    let appearance = &snapshot.appearance;
    let colors = &snapshot.series_colors;
    let groups = match snapshot.chart_type {
        ChartType::Candles
        | ChartType::HollowCandles
        | ChartType::HollowCandlesBullish
        | ChartType::HollowCandlesBearish => vec![candle_group(ui, title, appearance)],
        ChartType::Footprint => footprint_groups(ui, title, snapshot.order_flow),
        ChartType::Bars => vec![bar_group(ui, title, appearance)],
        ChartType::Line | ChartType::LineWithMarkers | ChartType::BrushableArea => {
            vec![line_group(ui, title, appearance, colors)]
        }
        ChartType::Area => vec![
            line_group(ui, "Line", appearance, colors),
            area_fill_group(ui, colors),
        ],
        ChartType::Baseline => vec![baseline_group(ui, title, appearance, colors)],
    };
    settings_section(groups)
}

fn candle_group(
    ui: SettingsUi<'_>,
    title: &'static str,
    appearance: &ChartAppearanceSettings,
) -> AnyElement {
    let candle = effective_candle_colors(appearance, ui.theme);
    let cell = |setting, value| ColorCell { setting, value };
    settings_group(
        title,
        [
            settings_pair_captions("Up", "Down", true, ui.theme),
            settings_color_pair_row(
                ui,
                "Body",
                [
                    cell(ChartColorSetting::Up, &candle.up),
                    cell(ChartColorSetting::Down, &candle.down),
                ],
                PairSwitch::Empty,
            ),
            settings_color_pair_row(
                ui,
                "Borders",
                [
                    cell(ChartColorSetting::BorderUp, &candle.border_up),
                    cell(ChartColorSetting::BorderDown, &candle.border_down),
                ],
                PairSwitch::Toggle(
                    appearance.border_visible,
                    ChartSettingsAction::ToggleBorders,
                ),
            ),
            settings_color_pair_row(
                ui,
                "Wicks",
                [
                    cell(ChartColorSetting::WickUp, &candle.wick_up),
                    cell(ChartColorSetting::WickDown, &candle.wick_down),
                ],
                PairSwitch::Toggle(appearance.wick_visible, ChartSettingsAction::ToggleWicks),
            ),
        ],
        ui.theme,
    )
}

/// The candle colors Aeris Charts paints. Bodies use the engine's own theme resolution; an
/// unpinned wick or border follows its body color (the engine's reference-parity rule), not
/// the theme color, so a custom body shows the same wick and border the chart draws.
#[derive(Debug, PartialEq, Eq)]
struct CandleColors {
    up: String,
    down: String,
    wick_up: String,
    wick_down: String,
    border_up: String,
    border_down: String,
}

fn effective_candle_colors(
    appearance: &ChartAppearanceSettings,
    theme: &AerisTheme,
) -> CandleColors {
    let chart_theme = aeris_chart_theme(theme.mode);
    let up = appearance.effective_up_color(chart_theme);
    let down = appearance.effective_down_color(chart_theme);
    let follow = |part: &ChartAppearanceColor, body: &str| match part {
        ChartAppearanceColor::Custom(color) => color.clone(),
        ChartAppearanceColor::Theme => body.to_string(),
    };
    CandleColors {
        wick_up: follow(&appearance.wick_up_color, &up),
        wick_down: follow(&appearance.wick_down_color, &down),
        border_up: follow(&appearance.border_up_color, &up),
        border_down: follow(&appearance.border_down_color, &down),
        up,
        down,
    }
}

fn footprint_groups(
    ui: SettingsUi<'_>,
    title: &'static str,
    settings: OrderFlowSettings,
) -> Vec<AnyElement> {
    let mode = |label, mode| {
        (
            label,
            settings.display_mode == mode,
            ChartSettingsAction::FootprintMode(mode),
        )
    };
    let rows = |label, ticks| {
        (
            label,
            settings.ticks_per_row == ticks,
            ChartSettingsAction::FootprintTicksPerRow(ticks),
        )
    };
    vec![
        settings_group(
            title,
            [
                settings_stacked_choice_row(
                    ui,
                    ChoiceRow::new("footprint_display", "Display"),
                    &[
                        mode("Bid × Ask", FootprintDisplayMode::BidAsk),
                        mode("Total", FootprintDisplayMode::Total),
                        mode("Delta", FootprintDisplayMode::Delta),
                        mode("Profile", FootprintDisplayMode::ProfileInBar),
                        mode("Ladder", FootprintDisplayMode::VolumeLadder),
                        mode("Imbalance", FootprintDisplayMode::HorizontalImbalance),
                        mode("Histogram", FootprintDisplayMode::BidAskHistogram),
                    ],
                ),
                settings_choice_row(
                    ui,
                    ChoiceRow::new("footprint_row_size", "Row size").hint("Ticks per row"),
                    &[
                        rows("Auto", 0),
                        rows("1", 1),
                        rows("2", 2),
                        rows("5", 5),
                        rows("10", 10),
                        rows("25", 25),
                        rows("50", 50),
                        rows("100", 100),
                    ],
                ),
            ],
            ui.theme,
        ),
        settings_group(
            "Order flow",
            [
                settings_switch_row(
                    ui,
                    "Cumulative delta",
                    None,
                    settings.show_cumulative_delta,
                    ChartSettingsAction::ToggleCumulativeDelta,
                ),
                settings_switch_row(
                    ui,
                    "Delta histogram",
                    None,
                    settings.show_delta_histogram,
                    ChartSettingsAction::ToggleDeltaHistogram,
                ),
            ],
            ui.theme,
        ),
    ]
}

fn bar_group(
    ui: SettingsUi<'_>,
    title: &'static str,
    appearance: &ChartAppearanceSettings,
) -> AnyElement {
    let up = effective_appearance_color(&appearance.up_color, |colors| colors.bullish, ui.theme);
    let down =
        effective_appearance_color(&appearance.down_color, |colors| colors.bearish, ui.theme);
    settings_group(
        title,
        [
            settings_pair_captions("Up", "Down", false, ui.theme),
            settings_color_pair_row(
                ui,
                "Bars",
                [
                    ColorCell {
                        setting: ChartColorSetting::Up,
                        value: &up,
                    },
                    ColorCell {
                        setting: ChartColorSetting::Down,
                        value: &down,
                    },
                ],
                PairSwitch::Absent,
            ),
            settings_switch_row(
                ui,
                "Open tick",
                Some("The left tick at each bar's open"),
                appearance.open_visible,
                ChartSettingsAction::ToggleOpen,
            ),
            settings_switch_row(
                ui,
                "Thin bars",
                Some("One pixel wide at every zoom"),
                appearance.thin_bars,
                ChartSettingsAction::ToggleThinBars,
            ),
        ],
        ui.theme,
    )
}

fn line_group(
    ui: SettingsUi<'_>,
    title: &'static str,
    appearance: &ChartAppearanceSettings,
    colors: &ChartSeriesColors,
) -> AnyElement {
    settings_group(
        title,
        [
            settings_color_row(
                ui,
                "Color",
                ColorCell {
                    setting: ChartColorSetting::Line,
                    value: &colors.line,
                },
                true,
            ),
            line_width_row(ui, appearance),
            line_style_row(ui, appearance),
        ],
        ui.theme,
    )
}

/// The area's gradient: top at the series line, bottom at the plot floor. An unpinned stop
/// shows the translucent default Aeris Charts derives from the line color.
fn area_fill_group(ui: SettingsUi<'_>, colors: &ChartSeriesColors) -> AnyElement {
    settings_group(
        "Fill",
        [
            settings_pair_captions("Top", "Bottom", false, ui.theme),
            settings_color_pair_row(
                ui,
                "Colors",
                [
                    ColorCell {
                        setting: ChartColorSetting::AreaTop,
                        value: &colors.area_top,
                    },
                    ColorCell {
                        setting: ChartColorSetting::AreaBottom,
                        value: &colors.area_bottom,
                    },
                ],
                PairSwitch::Absent,
            ),
        ],
        ui.theme,
    )
}

fn baseline_group(
    ui: SettingsUi<'_>,
    title: &'static str,
    appearance: &ChartAppearanceSettings,
    colors: &ChartSeriesColors,
) -> AnyElement {
    settings_group(
        title,
        [
            settings_pair_captions("Above", "Below", false, ui.theme),
            settings_color_pair_row(
                ui,
                "Colors",
                [
                    ColorCell {
                        setting: ChartColorSetting::BaselineTop,
                        value: &colors.baseline_top,
                    },
                    ColorCell {
                        setting: ChartColorSetting::BaselineBottom,
                        value: &colors.baseline_bottom,
                    },
                ],
                PairSwitch::Absent,
            ),
            line_width_row(ui, appearance),
            line_style_row(ui, appearance),
        ],
        ui.theme,
    )
}

fn line_width_row(ui: SettingsUi<'_>, appearance: &ChartAppearanceSettings) -> AnyElement {
    let width = |label, width| {
        (
            label,
            appearance.line_width == width,
            ChartSettingsAction::LineWidth(width),
        )
    };
    settings_choice_row(
        ui,
        ChoiceRow::new("line_width", "Width"),
        &[width("1", 1), width("2", 2), width("3", 3), width("4", 4)],
    )
}

fn line_style_row(ui: SettingsUi<'_>, appearance: &ChartAppearanceSettings) -> AnyElement {
    let style = |label, style| {
        (
            label,
            appearance.line_style == style,
            ChartSettingsAction::LineStyle(style),
        )
    };
    settings_choice_row(
        ui,
        ChoiceRow::new("line_style", "Style"),
        &[style("Solid", 0), style("Dotted", 1), style("Dashed", 2)],
    )
}

fn chart_canvas_settings(ui: SettingsUi<'_>, snapshot: &ChartSettingsSnapshot) -> AnyElement {
    settings_section(vec![
        canvas_surface_group(ui, &snapshot.appearance),
        canvas_grid_group(ui, &snapshot.appearance),
        canvas_crosshair_group(ui, snapshot),
    ])
}

fn canvas_surface_group(ui: SettingsUi<'_>, appearance: &ChartAppearanceSettings) -> AnyElement {
    let background = effective_appearance_color(
        &appearance.background_color,
        |colors| colors.background,
        ui.theme,
    );
    let text = effective_appearance_color(&appearance.text_color, |colors| colors.text, ui.theme);
    settings_group(
        "Chart",
        [
            settings_color_row(
                ui,
                "Background",
                ColorCell {
                    setting: ChartColorSetting::Background,
                    value: &background,
                },
                true,
            ),
            settings_color_row(
                ui,
                "Text",
                ColorCell {
                    setting: ChartColorSetting::Text,
                    value: &text,
                },
                true,
            ),
        ],
        ui.theme,
    )
}

fn canvas_grid_group(ui: SettingsUi<'_>, appearance: &ChartAppearanceSettings) -> AnyElement {
    let visible = appearance.grid_visible;
    let color = effective_appearance_color(&appearance.grid_color, |colors| colors.grid, ui.theme);
    let style = |label, style| {
        (
            label,
            appearance.grid_style == style,
            ChartSettingsAction::GridStyle(style),
        )
    };
    settings_group(
        "Grid lines",
        [
            settings_switch_row(
                ui,
                "Show grid lines",
                None,
                visible,
                ChartSettingsAction::ToggleGrid,
            ),
            settings_color_row(
                ui,
                "Color",
                ColorCell {
                    setting: ChartColorSetting::Grid,
                    value: &color,
                },
                visible,
            ),
            settings_choice_row(
                ui,
                ChoiceRow::new("grid_style", "Style").enabled(visible),
                &[style("Solid", 0), style("Dotted", 1), style("Dashed", 2)],
            ),
        ],
        ui.theme,
    )
}

fn crosshair_mode_hint(mode: u8) -> &'static str {
    match mode {
        1 => "Snaps to each bar's close",
        CROSSHAIR_HIDDEN => "No crosshair lines or labels",
        3 => "Snaps to the nearest open, high, low or close",
        _ => "Follows the pointer freely",
    }
}

fn canvas_crosshair_group(ui: SettingsUi<'_>, snapshot: &ChartSettingsSnapshot) -> AnyElement {
    let appearance = &snapshot.appearance;
    let visible = snapshot.crosshair_mode != CROSSHAIR_HIDDEN;
    let color = effective_appearance_color(
        &appearance.crosshair_color,
        |colors| colors.crosshair,
        ui.theme,
    );
    let mode = |label, mode| {
        (
            label,
            snapshot.crosshair_mode == mode,
            ChartSettingsAction::CrosshairMode(mode),
        )
    };
    let width = |label, width| {
        (
            label,
            appearance.crosshair_width == width,
            ChartSettingsAction::CrosshairWidth(width),
        )
    };
    let style = |label, style| {
        (
            label,
            appearance.crosshair_style == style,
            ChartSettingsAction::CrosshairStyle(style),
        )
    };
    settings_group(
        "Crosshair",
        [
            settings_choice_row(
                ui,
                ChoiceRow::new("crosshair_mode", "Mode")
                    .hint(crosshair_mode_hint(snapshot.crosshair_mode)),
                &[
                    mode("Normal", 0),
                    mode("Magnet", 1),
                    mode("OHLC", 3),
                    mode("Hidden", CROSSHAIR_HIDDEN),
                ],
            ),
            settings_color_row(
                ui,
                "Color",
                ColorCell {
                    setting: ChartColorSetting::Crosshair,
                    value: &color,
                },
                visible,
            ),
            settings_choice_row(
                ui,
                ChoiceRow::new("crosshair_width", "Width").enabled(visible),
                &[width("1", 1), width("2", 2), width("3", 3)],
            ),
            settings_choice_row(
                ui,
                ChoiceRow::new("crosshair_style", "Style").enabled(visible),
                &[style("Solid", 0), style("Dotted", 1), style("Dashed", 2)],
            ),
        ],
        ui.theme,
    )
}

fn chart_legend_settings(ui: SettingsUi<'_>, snapshot: &ChartSettingsSnapshot) -> AnyElement {
    let legend = snapshot.legend;
    settings_section(vec![
        settings_group(
            "Symbol",
            [settings_switch_with_option(
                ui,
                SwitchWithOption {
                    label: "Symbol legend",
                    hint: None,
                    enabled: legend.symbol,
                    action: ChartSettingsAction::ToggleSymbolLegend,
                    option: "OHLC values",
                    option_checked: legend.symbol_values,
                    option_action: ChartSettingsAction::ToggleSymbolLegendValues,
                },
            )],
            ui.theme,
        ),
        settings_group(
            "Indicators",
            [settings_switch_with_option(
                ui,
                SwitchWithOption {
                    label: "Indicator legend",
                    hint: None,
                    enabled: legend.indicators,
                    action: ChartSettingsAction::ToggleIndicatorLegend,
                    option: "Indicator values",
                    option_checked: legend.indicator_values,
                    option_action: ChartSettingsAction::ToggleIndicatorLegendValues,
                },
            )],
            ui.theme,
        ),
    ])
}

fn chart_trading_settings(ui: SettingsUi<'_>, snapshot: &ChartSettingsSnapshot) -> AnyElement {
    let visibility = snapshot.trading_visibility;
    settings_section(vec![
        settings_group(
            "Orders and positions",
            [settings_switch_with_option(
                ui,
                SwitchWithOption {
                    label: "Order and position lines",
                    hint: Some("Working orders, open positions and their TP/SL"),
                    enabled: visibility.show_order_management_lines,
                    action: ChartSettingsAction::ToggleOrderManagementLines,
                    option: "Extend lines to the left edge",
                    option_checked: visibility.extend_order_lines_left,
                    option_action: ChartSettingsAction::ToggleExtendOrderLinesLeft,
                },
            )],
            ui.theme,
        ),
        settings_group(
            "Executions",
            [settings_switch_row(
                ui,
                "Execution marks",
                Some("Marks where your orders filled"),
                visibility.show_execution_marks,
                ChartSettingsAction::ToggleExecutionMarks,
            )],
            ui.theme,
        ),
    ])
}

/// A settings section: its groups stacked with even spacing.
fn settings_section(groups: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_5()
        .children(groups)
        .into_any_element()
}

/// A titled group of related rows in one bordered block, rows divided by hairlines.
fn settings_group(
    title: &'static str,
    rows: impl IntoIterator<Item = AnyElement>,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let mut block = div()
        .flex()
        .flex_col()
        .px_3()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary));
    for (index, row) in rows.into_iter().enumerate() {
        block = block.child(
            div()
                .when(index > 0, |divided| {
                    divided
                        .border_t(platform_border_width(theme))
                        .border_color(gpui_color(colors.border_subtle))
                })
                .child(row),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_xs()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(colors.text_secondary))
                .child(title),
        )
        .child(block)
        .into_any_element()
}

/// A row's label, with an optional muted hint below it. A locked row mutes its label too.
fn settings_label(
    label: &'static str,
    hint: Option<&'static str>,
    enabled: bool,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .text_sm()
                .text_color(gpui_color(if enabled {
                    colors.text_primary
                } else {
                    colors.text_muted
                }))
                .child(label),
        )
        .children(hint.map(|hint| {
            div()
                .truncate()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(hint)
        }))
}

/// One setting: label and hint on the left, its control on the right.
fn settings_row(
    label: &'static str,
    hint: Option<&'static str>,
    enabled: bool,
    control: impl IntoElement,
    theme: &AerisTheme,
) -> AnyElement {
    div()
        .min_h(design_rems(SETTINGS_ROW_HEIGHT))
        .py_1()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .child(settings_label(label, hint, enabled, theme))
        .child(control)
        .into_any_element()
}

fn settings_switch(
    ui: SettingsUi<'_>,
    label: &'static str,
    enabled: bool,
    action: ChartSettingsAction,
) -> Switch {
    Switch::new(label, label, enabled, ui.theme).on_change(ui.apply_on_change(action))
}

fn settings_switch_row(
    ui: SettingsUi<'_>,
    label: &'static str,
    hint: Option<&'static str>,
    enabled: bool,
    action: ChartSettingsAction,
) -> AnyElement {
    settings_row(
        label,
        hint,
        true,
        settings_switch(ui, label, enabled, action),
        ui.theme,
    )
}

/// A switch with its refining option as a checkbox right below it, in the same row block. The
/// option stays visible but locked while the switch is off, so turning the switch back on
/// restores the earlier choice.
fn settings_switch_with_option(ui: SettingsUi<'_>, row: SwitchWithOption) -> AnyElement {
    div()
        .pb_2()
        .child(settings_switch_row(
            ui,
            row.label,
            row.hint,
            row.enabled,
            row.action,
        ))
        .child(
            CheckboxRow::new(row.option, row.option, row.option_checked, ui.theme)
                .disabled(!row.enabled)
                .on_change(ui.apply_on_change(row.option_action)),
        )
        .into_any_element()
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

/// The outline button that shows a color's swatch and hex code and opens its picker.
/// A color as a round swatch over the transparency backdrop, so translucent colors read.
pub(super) fn color_swatch_dot(value: &str, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    div()
        .size(design_rems(16.0))
        .flex_none()
        .rounded_full()
        .border(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .bg(transparency_backdrop(theme))
        .child(
            div()
                .size_full()
                .rounded_full()
                .bg(chart_css_color(value, colors.text_secondary)),
        )
}

pub(super) fn color_swatch_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    value: &str,
    open: bool,
    theme: &AerisTheme,
) -> Button {
    let colors = theme.colors;
    Button::new(id, theme)
        .variant(ButtonVariant::Outline)
        .open(open)
        .aria_label(label)
        .leading(color_swatch_dot(value, theme))
        .child(
            // Sized for `#RRGGBBAA` so translucent colors fit on one line like opaque ones.
            div()
                .w(design_rems(72.0))
                .whitespace_nowrap()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(value.to_ascii_uppercase()),
        )
}

/// A color's swatch button and, while open, its picker. A locked color cannot open.
fn settings_color_control(ui: SettingsUi<'_>, cell: ColorCell<'_>, enabled: bool) -> Div {
    let setting = cell.setting;
    let theme = ui.theme;
    let toggle = ui.terminal.clone();
    let current = cell.value.to_string();
    let open_picker = ui
        .color_picker
        .filter(|picker| enabled && picker.setting == setting);
    let mut control = div().relative().flex().child(
        color_swatch_button(
            ("chart_color_picker", setting as usize),
            setting.label(),
            cell.value,
            open_picker.is_some(),
            theme,
        )
        .disabled(!enabled)
        .on_press(move |_, window, cx| {
            toggle.update(cx, |terminal, terminal_cx| {
                terminal.toggle_chart_color_picker(setting, &current, window, terminal_cx);
            });
        }),
    );
    if let Some(picker) = open_picker {
        let apply = ui.terminal.clone();
        let apply_menu = ui.menu.clone();
        let dismiss = ui.terminal.clone();
        control = control.child(gpui::deferred(
            ColorPicker::new(
                ("chart_color_popover", setting as usize),
                cell.value,
                &picker.input,
                theme,
            )
            .error(picker.error.as_deref())
            .on_select(move |value, _, cx| {
                apply.update(cx, |terminal, terminal_cx| {
                    terminal.apply_chart_color(&apply_menu, setting, &value, terminal_cx);
                });
            })
            .on_dismiss(move |_, cx| {
                dismiss.update(cx, TerminalApp::close_chart_color_picker);
            }),
        ));
    }
    control
}

fn settings_color_row(
    ui: SettingsUi<'_>,
    label: &'static str,
    cell: ColorCell<'_>,
    enabled: bool,
) -> AnyElement {
    settings_row(
        label,
        None,
        enabled,
        settings_color_control(ui, cell, enabled),
        ui.theme,
    )
}

/// Column captions above paired color rows, aligned with their color columns.
fn settings_pair_captions(
    first: &'static str,
    second: &'static str,
    switch_column: bool,
    theme: &AerisTheme,
) -> AnyElement {
    let caption = |text: &'static str| {
        div()
            .w(design_rems(SETTINGS_PAIR_COLUMN_WIDTH))
            .flex_none()
            .text_center()
            .text_xs()
            .text_color(gpui_color(theme.colors.text_muted))
            .child(text)
    };
    div()
        .h(design_rems(28.0))
        .flex()
        .items_center()
        .gap_3()
        .child(div().flex_1())
        .child(caption(first))
        .child(caption(second))
        .when(switch_column, |captions| {
            captions.child(
                div()
                    .w(design_rems(SETTINGS_SWITCH_COLUMN_WIDTH))
                    .flex_none(),
            )
        })
        .into_any_element()
}

/// Two related colors side by side (up and down, above and below), with an optional switch that
/// shows or hides the part they paint.
fn settings_color_pair_row(
    ui: SettingsUi<'_>,
    label: &'static str,
    cells: [ColorCell<'_>; 2],
    switch: PairSwitch,
) -> AnyElement {
    let visible = switch.part_visible();
    let column = |cell| {
        div()
            .w(design_rems(SETTINGS_PAIR_COLUMN_WIDTH))
            .flex_none()
            .flex()
            .justify_center()
            .child(settings_color_control(ui, cell, visible))
    };
    let [first, second] = cells;
    div()
        .min_h(design_rems(SETTINGS_ROW_HEIGHT))
        .py_1()
        .flex()
        .items_center()
        .gap_3()
        .child(settings_label(label, None, true, ui.theme))
        .child(column(first))
        .child(column(second))
        .when(switch.has_column(), |row| {
            row.child(
                div()
                    .w(design_rems(SETTINGS_SWITCH_COLUMN_WIDTH))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .children(match switch {
                        PairSwitch::Toggle(enabled, action) => {
                            Some(settings_switch(ui, label, enabled, action))
                        }
                        PairSwitch::Absent | PairSwitch::Empty => None,
                    }),
            )
        })
        .into_any_element()
}

/// The single-choice tabs of one setting.
fn settings_choice_tabs(ui: SettingsUi<'_>, row: ChoiceRow, choices: Choices<'_>) -> TabList {
    let mut tabs = TabList::new(row.id, row.label, ui.theme);
    for (index, (choice, selected, action)) in choices.iter().copied().enumerate() {
        let terminal = ui.terminal.clone();
        let menu = ui.menu.clone();
        tabs = tabs.child(
            Tab::new((row.id, index), ui.theme)
                .selected(selected)
                .disabled(!row.enabled)
                .aria_label(choice)
                .on_click(move |_, _, cx| {
                    terminal.update(cx, |terminal, terminal_cx| {
                        terminal.apply_chart_settings_action(&menu, action, terminal_cx);
                    });
                    cx.stop_propagation();
                })
                .child(choice),
        );
    }
    tabs
}

fn settings_choice_row(ui: SettingsUi<'_>, row: ChoiceRow, choices: Choices<'_>) -> AnyElement {
    settings_row(
        row.label,
        row.hint,
        row.enabled,
        settings_choice_tabs(ui, row, choices),
        ui.theme,
    )
}

/// A choice with too many options to share a line with its label: the label above, the options
/// below, wrapping when the panel is narrow.
fn settings_stacked_choice_row(
    ui: SettingsUi<'_>,
    row: ChoiceRow,
    choices: Choices<'_>,
) -> AnyElement {
    div()
        .py_3()
        .flex()
        .flex_col()
        .gap_2()
        .child(settings_label(row.label, row.hint, row.enabled, ui.theme))
        .child(
            div()
                .flex()
                .child(settings_choice_tabs(ui, row, choices).wrap()),
        )
        .into_any_element()
}

pub(super) fn chart_css_color(value: &str, fallback: ThemeColor) -> Hsla {
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
            clamp_floating_panel_origin(point(px(120.0), px(90.0)), viewport, panel_size),
            point(px(120.0), px(90.0)),
            "a drag inside the window is kept exactly"
        );
        assert_eq!(
            clamp_floating_panel_origin(point(px(-500.0), px(-500.0)), viewport, panel_size),
            point(px(OVERLAY_EDGE_MARGIN), px(OVERLAY_EDGE_MARGIN))
        );
        assert_eq!(
            clamp_floating_panel_origin(point(px(5_000.0), px(5_000.0)), viewport, panel_size),
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
    fn candle_settings_show_the_colors_the_chart_paints() {
        let theme = AerisTheme::dark();
        let mut appearance = ChartAppearanceSettings {
            up_color: ChartAppearanceColor::Custom("#123456".into()),
            ..ChartAppearanceSettings::default()
        };
        let colors = effective_candle_colors(&appearance, &theme);
        assert_eq!(colors.up, "#123456");
        assert_eq!(
            (colors.wick_up.as_str(), colors.border_up.as_str()),
            ("#123456", "#123456"),
            "an unpinned wick and border follow the custom body, as Aeris Charts paints them"
        );
        let bearish = appearance.effective_down_color(aeris_chart_theme(theme.mode));
        assert_eq!(colors.wick_down, bearish);
        appearance.wick_up_color = ChartAppearanceColor::Custom("#ABCDEF".into());
        assert_eq!(
            effective_candle_colors(&appearance, &theme).wick_up,
            "#ABCDEF"
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
}
