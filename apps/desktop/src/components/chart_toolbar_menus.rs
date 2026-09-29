use super::*;

pub(super) fn chrome_overlay_layer(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
    chrome_height: f32,
    viewport: gpui::Size<Pixels>,
    cx: &App,
) -> Option<AnyElement> {
    let overlay = app_state.chrome_overlay?;
    let compact_panel = matches!(
        overlay,
        ChromeOverlay::Timeframe
            | ChromeOverlay::QuickTimeframe
            | ChromeOverlay::ChartType
            | ChromeOverlay::TimeZone
    );
    let anchored_menu = matches!(
        overlay,
        ChromeOverlay::Timeframe | ChromeOverlay::ChartType | ChromeOverlay::TimeZone
    );
    let quick_timeframe = overlay == ChromeOverlay::QuickTimeframe;
    let dual_container = overlay == ChromeOverlay::Timeframe;
    let menu_left = compact_menu_left(overlay, app_state, viewport);
    let animation_origin = chrome_overlay_animation_origin(overlay, app_state, menu_left, viewport);
    let phase = app_state.chrome_overlay_phase;
    let generation = app_state.chrome_overlay_generation;
    let closing = phase == ChromeOverlayPhase::Closing;
    let panel = chrome_overlay_content(app_state, app, overlay, chrome_height, viewport, theme, cx);
    let close_app = app.clone();
    Some(
        div()
            .id("chrome_overlay_scrim")
            .absolute()
            .top(px(chrome_height))
            .left_0()
            .right_0()
            .bottom_0()
            .occlude()
            .flex()
            .when(anchored_menu, |scrim| {
                scrim.items_start().justify_start().pl(menu_left)
            })
            .when(quick_timeframe, |scrim| {
                scrim
                    .items_start()
                    .justify_center()
                    .pt(px(QUICK_TIMEFRAME_POPUP_TOP))
            })
            .when(!anchored_menu && !quick_timeframe, |scrim| {
                scrim.items_center().justify_center()
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                close_app.update(cx, |app, app_cx| {
                    app.close_chrome_overlay(window, app_cx);
                });
                cx.stop_propagation();
            })
            .child(chrome_overlay_panel(
                panel,
                theme,
                ChromeOverlayPanelStyle {
                    interval_popup: compact_panel,
                    dual_container,
                    primary_surface: quick_timeframe,
                    elevation: chrome_overlay_elevation(overlay),
                    radius: if matches!(
                        overlay,
                        ChromeOverlay::Instrument | ChromeOverlay::Indicator
                    ) {
                        RadiusToken::Medium
                    } else {
                        RadiusToken::Default
                    },
                },
                closing,
                generation,
                phase,
                animation_origin,
            ))
            .into_any_element(),
    )
}

fn chrome_overlay_animation_origin(
    overlay: ChromeOverlay,
    app_state: &WorkspaceSurface,
    menu_left: Pixels,
    viewport: gpui::Size<Pixels>,
) -> PopupAnimationOrigin {
    let trigger = app_state
        .chrome_overlay_trigger_position
        .or_else(|| match overlay {
            ChromeOverlay::Timeframe | ChromeOverlay::QuickTimeframe => app_state
                .timeframe_trigger_bounds
                .map(|bounds| bounds.center()),
            ChromeOverlay::ChartType => app_state
                .chart_type_trigger_bounds
                .map(|bounds| bounds.center()),
            ChromeOverlay::TimeZone => app_state
                .time_zone_trigger_bounds
                .map(|bounds| bounds.center()),
            ChromeOverlay::Instrument | ChromeOverlay::Indicator => None,
        });
    let Some(trigger) = trigger else {
        return PopupAnimationOrigin::new(0.5, 0.0);
    };

    let (left, width) = match overlay {
        ChromeOverlay::Timeframe => {
            let groups = timeframe_menu_groups(app_state.available_intervals());
            let flyout = app_state.timeframe_menu_flyout.and_then(|group| {
                let index = groups.iter().position(|item| *item == group)?;
                Some((
                    index,
                    timeframe_group_intervals(group, app_state.available_intervals()).len(),
                ))
            });
            (menu_left, timeframe_overlay_extent(groups.len(), flyout).0)
        }
        ChromeOverlay::ChartType => (menu_left, TIMEFRAME_MENU_WIDTH),
        ChromeOverlay::TimeZone => (menu_left, TIME_ZONE_MENU_WIDTH),
        ChromeOverlay::QuickTimeframe => (
            ((viewport.width - px(QUICK_TIMEFRAME_POPUP_WIDTH)) / 2.0).max(px(0.0)),
            QUICK_TIMEFRAME_POPUP_WIDTH,
        ),
        ChromeOverlay::Instrument | ChromeOverlay::Indicator => {
            let viewport_width: f32 = viewport.width.into();
            let trigger_x: f32 = trigger.x.into();
            return PopupAnimationOrigin::new(
                if viewport_width > 0.0 {
                    trigger_x / viewport_width
                } else {
                    0.5
                },
                0.0,
            );
        }
    };
    let left: f32 = left.into();
    let trigger_x: f32 = trigger.x.into();
    PopupAnimationOrigin::new((trigger_x - left) / width.max(1.0), 0.0)
}

pub(super) fn compact_menu_left(
    overlay: ChromeOverlay,
    app_state: &WorkspaceSurface,
    viewport: gpui::Size<Pixels>,
) -> Pixels {
    let (trigger, width) = match overlay {
        ChromeOverlay::ChartType => (app_state.chart_type_trigger_bounds, TIMEFRAME_MENU_WIDTH),
        ChromeOverlay::TimeZone => (app_state.time_zone_trigger_bounds, TIME_ZONE_MENU_WIDTH),
        ChromeOverlay::Timeframe => {
            let intervals = app_state.available_intervals();
            let groups = timeframe_menu_groups(intervals);
            let flyout = app_state.timeframe_menu_flyout.and_then(|group| {
                let index = groups.iter().position(|item| *item == group)?;
                Some((index, timeframe_group_intervals(group, intervals).len()))
            });
            (
                app_state.timeframe_trigger_bounds,
                timeframe_overlay_extent(groups.len(), flyout).0,
            )
        }
        ChromeOverlay::QuickTimeframe => (
            app_state.timeframe_trigger_bounds,
            QUICK_TIMEFRAME_POPUP_WIDTH,
        ),
        ChromeOverlay::Instrument | ChromeOverlay::Indicator => return px(0.0),
    };
    clamp_anchored_menu_left(timeframe_overlay_left(trigger), viewport, width)
}

/// Anchored menus open under their header trigger, so a trigger near the right edge would run a
/// wide panel off-screen. Slide the panel back inside the window instead of clipping it.
pub(super) fn clamp_anchored_menu_left(
    left: Pixels,
    viewport: gpui::Size<Pixels>,
    width: f32,
) -> Pixels {
    let margin = px(OVERLAY_EDGE_MARGIN);
    let max_left = (viewport.width - px(width) - margin).max(px(0.0));
    left.min(max_left).max(px(0.0))
}

pub(super) fn chrome_overlay_content(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    overlay: ChromeOverlay,
    chrome_height: f32,
    viewport: gpui::Size<Pixels>,
    theme: &AerisTheme,
    cx: &App,
) -> AnyElement {
    let pending = app_state.rithmic_switch.in_progress();
    match overlay {
        ChromeOverlay::Instrument => instrument_dialog_content(
            app,
            chrome_menu_extent(viewport, chrome_height),
            &InstrumentSelectorState {
                label: terminal_instrument_label(app_state),
                instruments: app_state.instrument_entries(cx),
                input: app_state.symbol_input.clone(),
                availability: InstrumentSelectorAvailability {
                    selection_pending: app_state.market_state.symbol_selection_pending,
                    enabled: true,
                },
                provider: app_state.provider,
                menu: InstrumentSelectorMenu {
                    keyboard_selection: app_state.chrome_selection,
                    keyboard_active: app_state.menu_state.chrome_list_keyboard,
                },
                scroll: app_state.scrolls.instrument.clone(),
                target: app_state.symbol_selection_target,
            },
            theme,
        )
        .into_any_element(),
        ChromeOverlay::Indicator => indicator_dialog_content(
            app,
            IndicatorDialogState {
                extent: chrome_menu_extent(viewport, chrome_height),
                input: &app_state.indicator_input,
                message: app_state.indicator_message.as_deref(),
                keyboard_selection: app_state.chrome_selection,
                scroll: &app_state.scrolls.indicator,
            },
            theme,
            cx,
        )
        .into_any_element(),
        ChromeOverlay::Timeframe => timeframe_overlay_content(
            app,
            app_state.available_intervals(),
            TimeframeOverlayState {
                selected: app_state.selected_interval(),
                flyout: app_state.timeframe_menu_flyout,
                keyboard_selection: app_state.chrome_selection,
                keyboard_active: app_state.menu_state.timeframe_flyout_keyboard,
                pending,
            },
            theme,
        )
        .into_any_element(),
        ChromeOverlay::QuickTimeframe => {
            quick_timeframe_overlay_content(&app_state.timeframe_input, theme).into_any_element()
        }
        ChromeOverlay::ChartType => chart_type_overlay_content(
            app,
            app_state.chart_type(cx),
            app_state.chrome_selection,
            theme,
        )
        .into_any_element(),
        ChromeOverlay::TimeZone => {
            time_zone_overlay_content(app_state, app, theme, cx).into_any_element()
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChromeOverlayElevation {
    Flat,
    Elevated,
}

const fn chrome_overlay_elevation(overlay: ChromeOverlay) -> ChromeOverlayElevation {
    match overlay {
        ChromeOverlay::QuickTimeframe => ChromeOverlayElevation::Elevated,
        ChromeOverlay::Instrument
        | ChromeOverlay::Indicator
        | ChromeOverlay::Timeframe
        | ChromeOverlay::ChartType
        | ChromeOverlay::TimeZone => ChromeOverlayElevation::Flat,
    }
}

#[derive(Clone, Copy)]
pub(super) struct ChromeOverlayPanelStyle {
    interval_popup: bool,
    dual_container: bool,
    primary_surface: bool,
    elevation: ChromeOverlayElevation,
    radius: RadiusToken,
}

pub(super) fn chrome_overlay_panel(
    content: AnyElement,
    theme: &AerisTheme,
    style: ChromeOverlayPanelStyle,
    closing: bool,
    generation: u64,
    phase: ChromeOverlayPhase,
    animation_origin: PopupAnimationOrigin,
) -> impl IntoElement {
    let colors = theme.colors;
    let enter_offset = animation_origin.enter_offset();
    div()
        .id("chrome_overlay_panel")
        .relative()
        .flex_none()
        .when(!style.dual_container, |panel| {
            panel
                .rounded(px(f32::from(style.radius.logical_pixels())))
                .border_1()
                .border_color(gpui_color(if style.primary_surface {
                    colors.border
                } else if style.interval_popup {
                    colors.border_secondary
                } else {
                    colors.border
                }))
                .bg(gpui_color(if style.primary_surface {
                    colors.surface
                } else if style.interval_popup {
                    colors.surface_secondary
                } else {
                    colors.surface
                }))
        })
        .when(
            !style.dual_container && style.elevation == ChromeOverlayElevation::Elevated,
            gpui::Styled::shadow_md,
        )
        .when(style.interval_popup && !style.dual_container, |panel| {
            panel.max_h_full().overflow_y_scroll()
        })
        .when(!style.interval_popup, |panel| {
            panel.max_h_full().overflow_hidden()
        })
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(content)
        .children(closing.then(|| {
            div()
                .id("chrome_overlay_closing_blocker")
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .left_0()
                .occlude()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        }))
        .with_animation(
            ("chrome_overlay_transition", generation),
            Animation::new(if closing {
                CHROME_OVERLAY_EXIT_DURATION
            } else {
                CHROME_OVERLAY_TRANSITION_DURATION
            })
            .with_easing(ease_out_quint()),
            move |panel, delta| {
                let progress = chrome_overlay_progress(phase, delta);
                panel
                    .opacity(0.3 + 0.7 * progress)
                    .top(px(enter_offset.y * (1.0 - progress)))
            },
        )
}

pub(super) fn timeframe_overlay_left(trigger_bounds: Option<Bounds<Pixels>>) -> Pixels {
    trigger_bounds.map_or(px(0.0), |bounds| bounds.origin.x.max(px(0.0)))
}

pub(super) fn timeframe_flyout_offset(group_index: usize) -> f32 {
    CHART_CONTEXT_MENU_ROW_HEIGHT * bounded_menu_count(group_index)
}

pub(super) fn timeframe_flyout_height(interval_count: usize) -> f32 {
    CHART_CONTEXT_MENU_ROW_HEIGHT * bounded_menu_count(interval_count) + 2.0
}

fn bounded_menu_count(count: usize) -> f32 {
    f32::from(u16::try_from(count).unwrap_or(u16::MAX))
}

pub(super) fn timeframe_overlay_extent(
    group_count: usize,
    flyout: Option<(usize, usize)>,
) -> (f32, f32) {
    let root_height = overlay_height(bounded_menu_count(group_count), 0.0);
    let Some((group_index, interval_count)) = flyout else {
        return (TIMEFRAME_MENU_WIDTH, root_height);
    };
    (
        TIMEFRAME_MENU_WIDTH + TIMEFRAME_FLYOUT_GAP + TIMEFRAME_FLYOUT_WIDTH,
        root_height
            .max(timeframe_flyout_offset(group_index) + timeframe_flyout_height(interval_count)),
    )
}

#[derive(Clone, Copy)]
pub(super) struct TimeframeOverlayState {
    selected: ChartInterval,
    flyout: Option<TimeframeMenuGroup>,
    keyboard_selection: usize,
    keyboard_active: bool,
    pending: bool,
}

pub(super) fn timeframe_overlay_content(
    app: &Entity<WorkspaceSurface>,
    intervals: &[ChartInterval],
    state: TimeframeOverlayState,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let groups = timeframe_menu_groups(intervals);
    let flyout_layout = state.flyout.and_then(|group| {
        groups.iter().position(|item| *item == group).map(|index| {
            (
                group,
                index,
                timeframe_group_intervals(group, intervals).len(),
            )
        })
    });
    let (width, height) = timeframe_overlay_extent(
        groups.len(),
        flyout_layout.map(|(_, index, count)| (index, count)),
    );
    let hover_root = app.clone();
    let mut root = timeframe_menu_surface(
        "timeframe_overlay_root",
        TIMEFRAME_MENU_WIDTH,
        colors.surface,
        colors.border,
        0.0,
        0.0,
    )
    .on_hover(move |hovered, window, cx| {
        hover_root.update(cx, |app, app_cx| {
            app.hover_timeframe_menu_region(*hovered, window, app_cx);
        });
    });
    let group_count = groups.len();
    let last_group = group_count.saturating_sub(1);
    for (index, group) in groups.into_iter().enumerate() {
        let active_in_group = timeframe_group_intervals(group, intervals)
            .into_iter()
            .find(|interval| *interval == state.selected)
            .map(ChartInterval::label);
        let highlighted = state.flyout == Some(group)
            || (state.flyout.is_none() && state.keyboard_selection == index);
        root = root.child(timeframe_group_row(
            app,
            group,
            TimeframeGroupRowState {
                active_label: active_in_group,
                open: highlighted,
                pending: state.pending,
                position: MenuRowPosition::new(index, last_group),
            },
            theme,
        ));
    }

    let mut overlay = div()
        .id("timeframe_overlay")
        .relative()
        .flex_none()
        .w(px(width))
        .h(px(height))
        .occlude()
        .text_color(gpui_color(colors.text_primary))
        .child(root);
    if let Some((group, index, count)) = flyout_layout {
        let flyout_height = timeframe_flyout_height(count);
        let bridge_app = app.clone();
        overlay = overlay
            .child(
                div()
                    .id("timeframe_overlay_hover_bridge")
                    .absolute()
                    .left(px(TIMEFRAME_MENU_WIDTH))
                    .top_0()
                    .bottom_0()
                    .w(px(TIMEFRAME_FLYOUT_GAP))
                    .occlude()
                    .on_hover(move |hovered, window, cx| {
                        bridge_app.update(cx, |app, app_cx| {
                            app.hover_timeframe_menu_region(*hovered, window, app_cx);
                        });
                    }),
            )
            .child(timeframe_flyout_host(
                app,
                intervals,
                state,
                group,
                index,
                flyout_height,
                theme,
            ));
    }
    overlay
}

fn timeframe_flyout_host(
    app: &Entity<WorkspaceSurface>,
    intervals: &[ChartInterval],
    state: TimeframeOverlayState,
    group: TimeframeMenuGroup,
    index: usize,
    flyout_height: f32,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let animation_origin = PopupAnimationOrigin::new(
        0.0,
        (CHART_CONTEXT_MENU_ROW_HEIGHT / 2.0 / flyout_height).clamp(0.0, 1.0),
    );
    div()
        .id("timeframe_overlay_flyout_host")
        .absolute()
        .left(px(TIMEFRAME_MENU_WIDTH + TIMEFRAME_FLYOUT_GAP))
        .top(px(timeframe_flyout_offset(index)))
        .w(px(TIMEFRAME_FLYOUT_WIDTH))
        .h(px(flyout_height))
        .flex_none()
        .child(animate_popup_from_origin(
            timeframe_flyout_panel(
                app,
                intervals,
                state.selected,
                group,
                state.keyboard_active.then_some(state.keyboard_selection),
                state.pending,
                theme,
            ),
            ("timeframe_flyout_enter", group as usize),
            animation_origin,
        ))
}

pub(super) fn timeframe_flyout_panel(
    app: &Entity<WorkspaceSurface>,
    intervals: &[ChartInterval],
    selected: ChartInterval,
    group: TimeframeMenuGroup,
    keyboard_index: Option<usize>,
    pending: bool,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let hover_panel = app.clone();
    let mut panel = timeframe_menu_surface(
        "timeframe_overlay_flyout",
        TIMEFRAME_FLYOUT_WIDTH,
        colors.surface_secondary,
        colors.border_secondary,
        0.0,
        0.0,
    )
    .on_hover(move |hovered, window, cx| {
        hover_panel.update(cx, |app, app_cx| {
            app.hover_timeframe_menu_region(*hovered, window, app_cx);
        });
    });
    let rows = timeframe_group_intervals(group, intervals);
    let last = rows.len().saturating_sub(1);
    for (index, interval) in rows.into_iter().enumerate() {
        panel = panel.child(timeframe_overlay_row(
            app,
            interval,
            index,
            TimeframeRowState {
                active: timeframe_flyout_row_is_active(interval, selected, keyboard_index, index),
                selected: interval == selected,
                pending,
            },
            TimeframeRowStyle {
                fill: colors.surface_secondary,
                track_menu_hover: true,
                position: MenuRowPosition::new(index, last),
            },
            theme,
        ));
    }
    panel
}

pub(super) fn timeframe_flyout_row_is_active(
    interval: ChartInterval,
    selected: ChartInterval,
    keyboard_index: Option<usize>,
    row_index: usize,
) -> bool {
    interval == selected || keyboard_index == Some(row_index)
}

pub(super) fn timeframe_menu_surface(
    id: &'static str,
    width: f32,
    fill: ThemeColor,
    border: ThemeColor,
    vertical_padding: f32,
    horizontal_padding: f32,
) -> Stateful<Div> {
    // Compact dropdown lists pass 0, 0. Do not restore panel padding here.
    div()
        .id(id)
        .w(px(width))
        .flex()
        .flex_col()
        .flex_none()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border_1()
        .border_color(gpui_color(border))
        .bg(gpui_color(fill))
        .px(px(horizontal_padding))
        .py(px(vertical_padding))
        .overflow_hidden()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

pub(super) fn chart_type_overlay_content(
    app: &Entity<WorkspaceSurface>,
    selected: ChartType,
    keyboard_selection: usize,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let mut panel = div()
        .w(px(TIMEFRAME_MENU_WIDTH))
        .flex()
        .flex_col()
        .text_color(gpui_color(colors.text_primary));
    let last = ChartType::ALL.len().saturating_sub(1);
    for (index, chart_type) in ChartType::ALL.into_iter().enumerate() {
        panel = panel.child(chart_type_overlay_row(
            app,
            chart_type,
            index,
            ChartTypeRowState {
                selected: selected == chart_type,
                keyboard: keyboard_selection == index,
                position: MenuRowPosition::new(index, last),
            },
            theme,
        ));
    }
    panel
}

pub(super) fn time_zone_overlay_content(
    app_state: &WorkspaceSurface,
    app: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
    cx: &App,
) -> impl IntoElement {
    let colors = theme.colors;
    let matches = app_state.time_zone_matches(cx);
    let selected = app_state.chart_time_zone_id(cx);
    let utc_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0);
    let mut rows = div()
        .id("time_zone_rows")
        .flex()
        .flex_col()
        .max_h(px(TIME_ZONE_MENU_MAX_HEIGHT - 58.0))
        .overflow_y_scroll()
        .track_scroll(&app_state.scrolls.time_zone);
    for (index, time_zone) in matches.into_iter().enumerate() {
        let row_app = app.clone();
        let display_name = time_zone.replace('_', " ");
        let badge = AerisChartView::time_zone_badge_label(time_zone, utc_seconds)
            .unwrap_or_else(|| "UTC".to_string());
        let mut row = MenuRow::compact(("time_zone_row", index), display_name, theme)
            .highlighted(app_state.chrome_selection == index)
            .fill_width()
            .on_click(move |_, window, cx| {
                row_app.update(cx, |app, app_cx| {
                    if app.set_chart_time_zone(time_zone, app_cx) {
                        app.close_chrome_overlay(window, app_cx);
                    }
                });
            });
        row = row.trailing(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(time_zone_badge(badge, theme))
                .when(time_zone == selected, |trailing| {
                    trailing.child(
                        header_icon(HugeIcon::CheckIcon)
                            .with_size(px(16.0))
                            .color(gpui_color(colors.icon)),
                    )
                }),
        );
        rows = rows.child(row);
    }
    div()
        .w(px(TIME_ZONE_MENU_WIDTH))
        .max_h(px(TIME_ZONE_MENU_MAX_HEIGHT))
        .flex()
        .flex_col()
        .bg(gpui_color(colors.surface))
        .child(
            div()
                .p_2()
                .border_b_1()
                .border_color(gpui_color(colors.border_secondary))
                .child(Input::new(&app_state.time_zone_input).platform(theme)),
        )
        .child(rows)
}

fn time_zone_badge(label: String, theme: &AerisTheme) -> impl IntoElement {
    let colors = theme.colors;
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
        .font_family(aeris_design_system::platform_font_family())
        .font_features(platform_tabular_numerals())
        .text_xs()
        .text_color(gpui_color(colors.text_muted))
        .child(label)
}

pub(super) fn quick_timeframe_overlay_content(
    input: &Entity<InputState>,
    theme: &AerisTheme,
) -> impl IntoElement {
    div()
        .w(px(QUICK_TIMEFRAME_POPUP_WIDTH))
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .child(
            div()
                .w_full()
                .text_center()
                .text_sm()
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .child("Time frame"),
        )
        .child(
            div()
                .w_full()
                .h(px(58.0))
                .text_size(px(18.0))
                .font_weight(platform_font_weight(TypographyRole::Normal))
                .child(
                    Input::new(input)
                        .appearance(true)
                        .bordered(true)
                        .focus_bordered(true)
                        .thick_border(true)
                        .platform(theme)
                        .flex_1(),
                ),
        )
}

pub(super) const fn timeframe_interval_group(interval: ChartInterval) -> TimeframeMenuGroup {
    match interval {
        ChartInterval::Tick100 => TimeframeMenuGroup::Ticks,
        ChartInterval::Minute1
        | ChartInterval::Minute3
        | ChartInterval::Minute5
        | ChartInterval::Minute15
        | ChartInterval::Minute30 => TimeframeMenuGroup::Minutes,
        ChartInterval::Hour1
        | ChartInterval::Hour2
        | ChartInterval::Hour4
        | ChartInterval::Hour8
        | ChartInterval::Hour12 => TimeframeMenuGroup::Hours,
        ChartInterval::Day1 | ChartInterval::Day3 => TimeframeMenuGroup::Days,
        ChartInterval::Week1 => TimeframeMenuGroup::Weeks,
        ChartInterval::Month1 => TimeframeMenuGroup::Months,
    }
}

pub(super) fn timeframe_menu_groups(intervals: &[ChartInterval]) -> Vec<TimeframeMenuGroup> {
    let mut groups = Vec::new();
    for interval in intervals.iter().copied() {
        let group = timeframe_interval_group(interval);
        if groups.last() != Some(&group) {
            groups.push(group);
        }
    }
    groups
}

pub(super) fn timeframe_group_intervals(
    group: TimeframeMenuGroup,
    intervals: &[ChartInterval],
) -> Vec<ChartInterval> {
    intervals
        .iter()
        .copied()
        .filter(|interval| timeframe_interval_group(*interval) == group)
        .collect()
}

pub(super) const fn timeframe_menu_row_label(interval: ChartInterval) -> &'static str {
    match interval {
        ChartInterval::Tick100 => "100 Ticks",
        ChartInterval::Minute1 => "1 Minute",
        ChartInterval::Minute3 => "3 Minutes",
        ChartInterval::Minute5 => "5 Minutes",
        ChartInterval::Minute15 => "15 Minutes",
        ChartInterval::Minute30 => "30 Minutes",
        ChartInterval::Hour1 => "1 Hour",
        ChartInterval::Hour2 => "2 Hours",
        ChartInterval::Hour4 => "4 Hours",
        ChartInterval::Hour8 => "8 Hours",
        ChartInterval::Hour12 => "12 Hours",
        ChartInterval::Day1 => "1 Day",
        ChartInterval::Day3 => "3 Days",
        ChartInterval::Week1 => "1 Week",
        ChartInterval::Month1 => "1 Month",
    }
}

pub(super) fn chrome_typeahead_blocked(event: &KeyDownEvent) -> bool {
    let modifiers = event.keystroke.modifiers;
    modifiers.control || modifiers.alt || modifiers.platform || modifiers.function
}

pub(super) fn chrome_typeahead_char(event: &KeyDownEvent) -> Option<char> {
    chrome_typeahead_char_from(
        event.keystroke.key.as_str(),
        event.keystroke.key_char.as_deref(),
        event.keystroke.modifiers.shift,
    )
}

pub(super) fn chrome_typeahead_char_from(
    key: &str,
    key_char: Option<&str>,
    shift: bool,
) -> Option<char> {
    if let Some(text) = key_char {
        let mut chars = text.chars();
        let ch = chars.next()?;
        return (chars.next().is_none() && ch.is_ascii_alphanumeric()).then_some(ch);
    }
    let mut chars = key.chars();
    let ch = chars.next()?;
    if chars.next().is_some() || !ch.is_ascii_alphanumeric() {
        return None;
    }
    if ch.is_ascii_alphabetic() && shift {
        Some(ch.to_ascii_uppercase())
    } else {
        Some(ch)
    }
}

#[derive(Clone, Copy)]
pub(super) struct MenuRowPosition {
    first: bool,
    last: bool,
}

impl MenuRowPosition {
    const fn new(index: usize, last: usize) -> Self {
        Self {
            first: index == 0,
            last: index == last,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct TimeframeGroupRowState {
    active_label: Option<&'static str>,
    open: bool,
    pending: bool,
    position: MenuRowPosition,
}

pub(super) fn timeframe_group_row(
    app: &Entity<WorkspaceSurface>,
    group: TimeframeMenuGroup,
    state: TimeframeGroupRowState,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let hover_app = app.clone();
    let click_app = app.clone();
    let mut trailing = div().flex().items_center().gap_1();
    if let Some(label) = state.active_label {
        trailing = trailing.child(
            div()
                .text_xs()
                .text_color(gpui_color(colors.text_secondary))
                .child(label),
        );
    }
    trailing = trailing.child(
        header_icon(HugeIcon::ArrowRight)
            .with_size(px(16.0))
            .color(gpui_color(colors.icon)),
    );
    let row = MenuRow::compact(
        ("timeframe_overlay_group", group as u64),
        group.label(),
        theme,
    )
    .resting_fill(theme.colors.surface)
    .highlighted(state.open)
    .disabled(state.pending)
    .trailing(trailing)
    .flush_in_panel(state.position.first, state.position.last)
    .on_hover(move |hovered, window, cx| {
        hover_app.update(cx, |app, app_cx| {
            if *hovered {
                app.hover_timeframe_menu_region(true, window, app_cx);
                app.open_timeframe_group(group, false, app_cx);
            } else {
                app.hover_timeframe_menu_region(false, window, app_cx);
            }
        });
    })
    .on_click(move |_, _, cx| {
        click_app.update(cx, |app, app_cx| {
            app.open_timeframe_group(group, false, app_cx);
        });
    });
    div()
        .id(("timeframe_overlay_group_host", group as u64))
        .w_full()
        .child(row)
}

#[derive(Clone, Copy)]
pub(super) struct TimeframeRowState {
    active: bool,
    selected: bool,
    pending: bool,
}

#[derive(Clone, Copy)]
pub(super) struct TimeframeRowStyle {
    fill: ThemeColor,
    track_menu_hover: bool,
    position: MenuRowPosition,
}

pub(super) fn timeframe_overlay_row(
    app: &Entity<WorkspaceSurface>,
    interval: ChartInterval,
    index: usize,
    state: TimeframeRowState,
    style: TimeframeRowStyle,
    theme: &AerisTheme,
) -> impl IntoElement {
    let row_app = app.clone();
    let mut row = MenuRow::compact(
        ("timeframe_overlay_row", index),
        timeframe_menu_row_label(interval),
        theme,
    )
    .resting_fill(style.fill)
    .highlighted(state.active)
    .disabled(state.pending)
    .flush_in_panel(style.position.first, style.position.last);
    if state.selected {
        row = row.trailing(
            header_icon(HugeIcon::CheckIcon)
                .with_size(px(16.0))
                .color(gpui_color(theme.colors.icon)),
        );
    }
    if style.track_menu_hover {
        let hover_app = app.clone();
        row = row.on_hover(move |hovered, window, cx| {
            hover_app.update(cx, |app, app_cx| {
                app.hover_timeframe_menu_region(*hovered, window, app_cx);
            });
        });
    }
    row.on_click(move |_, window, cx| {
        row_app.update(cx, |app, app_cx| {
            if app.select_interval(interval, app_cx) {
                app.close_chrome_overlay(window, app_cx);
            }
        });
    })
}

#[derive(Clone, Copy)]
pub(super) struct ChartTypeRowState {
    selected: bool,
    keyboard: bool,
    position: MenuRowPosition,
}

pub(super) fn chart_type_overlay_row(
    app: &Entity<WorkspaceSurface>,
    chart_type: ChartType,
    index: usize,
    state: ChartTypeRowState,
    theme: &AerisTheme,
) -> impl IntoElement {
    let row_app = app.clone();
    let mut row = MenuRow::compact(("chart_type_overlay_row", index), chart_type.label(), theme)
        .leading(series_glyph(chart_type, px(chart_chrome::HEADER_ICON_SIZE)))
        .highlighted(state.keyboard)
        .flush_in_panel(state.position.first, state.position.last)
        .on_click(move |_, window, cx| {
            row_app.update(cx, |app, app_cx| {
                app.set_chart_type(chart_type, app_cx);
                app.close_chrome_overlay(window, app_cx);
            });
        });
    if state.selected {
        row = row.trailing(
            header_icon(HugeIcon::CheckIcon)
                .with_size(px(16.0))
                .color(gpui_color(theme.colors.icon)),
        );
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_chrome_menus_are_flat_surfaces() {
        assert_eq!(
            chrome_overlay_elevation(ChromeOverlay::Instrument),
            ChromeOverlayElevation::Flat
        );
        assert_eq!(
            chrome_overlay_elevation(ChromeOverlay::Indicator),
            ChromeOverlayElevation::Flat
        );
        assert_eq!(
            chrome_overlay_elevation(ChromeOverlay::QuickTimeframe),
            ChromeOverlayElevation::Elevated
        );
    }
}
