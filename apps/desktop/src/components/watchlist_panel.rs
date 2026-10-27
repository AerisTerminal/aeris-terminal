//! Docked watchlist panel: symbol table, reordering, row flags and the add-symbol control.

use super::chart_context_menus::chart_css_color;
use super::side_panel_dock::side_panel_header;
use super::*;
use gpui::{AppContext, Stateful};

const WATCHLIST_COLUMNS_HEIGHT: f32 = 26.0;
pub(super) const WATCHLIST_ROW_HEIGHT: f32 = 30.0;
const WATCHLIST_LAST_WIDTH: f32 = 70.0;
const WATCHLIST_CHANGE_WIDTH: f32 = 66.0;
const WATCHLIST_CHANGE_PERCENT_WIDTH: f32 = 64.0;
const WATCHLIST_VOLUME_WIDTH: f32 = 60.0;
const WATCHLIST_FLAG_SLOT_WIDTH: f32 = 16.0;
const WATCHLIST_FLAG_GLYPH_SIZE: f32 = 16.0;
const WATCHLIST_FLAG_GROUP: &str = "watchlist_flag_slot";
const WATCHLIST_FLAG_FILTER_MENU_WIDTH: f32 = 180.0;

pub(super) struct WatchlistPanelState {
    pub(super) rows: Vec<WatchlistRow>,
    pub(super) drag: Option<WatchlistDragState>,
    pub(super) scroll: ScrollHandle,
    pub(super) flags: WatchlistFlagsView,
}

/// The flag picker and flag filter the watchlist panel shows.
pub(super) struct WatchlistFlagsView {
    pub(super) picker: Option<WatchlistFlagPicker>,
    pub(super) filter: Option<String>,
    pub(super) filter_menu_open: bool,
    /// The distinct flag colors in the list, which the filter offers.
    pub(super) colors: Vec<String>,
}

pub(super) fn watchlist_side_panel(
    app: Entity<WorkspaceSurface>,
    terminal: &Entity<TerminalApp>,
    watchlist: WatchlistPanelState,
    theme: &AerisTheme,
) -> Div {
    let WatchlistPanelState {
        rows,
        drag,
        scroll,
        flags,
    } = watchlist;
    div()
        .relative()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(gpui_color(theme.colors.surface))
        .size_full()
        .child(side_panel_header(
            SidePanel::Watchlist,
            app.clone(),
            [
                watchlist_flag_filter_control(terminal, &flags, theme).into_any_element(),
                watchlist_add_symbol_control(app, theme).into_any_element(),
            ],
            theme,
        ))
        .child(watchlist_table(
            terminal,
            rows,
            drag.as_ref(),
            &flags,
            &scroll,
            theme,
        ))
}

#[derive(Clone)]
struct WatchlistRowDrag {
    provider: String,
    instrument_id: String,
}

impl Render for WatchlistRowDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size(px(1.0)).opacity(0.0)
    }
}

fn watchlist_table(
    terminal: &Entity<TerminalApp>,
    rows: Vec<WatchlistRow>,
    watchlist_drag: Option<&WatchlistDragState>,
    flags: &WatchlistFlagsView,
    watchlist_scroll: &ScrollHandle,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let mut body = div()
        .flex_1()
        .min_h_0()
        .font_features(platform_tabular_numerals());
    if rows.is_empty() {
        body = body.child(
            div()
                .px_3()
                .py_4()
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child(if flags.filter.is_some() {
                    "No symbols carry this flag"
                } else {
                    "Add symbols with +"
                }),
        );
    } else {
        for (index, row) in rows.into_iter().enumerate() {
            let picker = flags.picker.as_ref().filter(|picker| {
                picker.provider == row.instrument.provider
                    && picker.instrument_id == row.instrument.instrument_id
            });
            body = body.child(watchlist_row(
                terminal,
                &row,
                index,
                watchlist_drag,
                picker,
                theme,
            ));
        }
    }
    let move_terminal = terminal.clone();
    let move_scroll = watchlist_scroll.clone();
    let end_terminal = terminal.clone();
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(gpui_color(colors.surface))
        .child(watchlist_columns(theme))
        .child(
            body.id("watchlist_body")
                .overflow_y_scroll()
                .track_scroll(watchlist_scroll)
                .on_drag_move::<WatchlistRowDrag>(move |event, _, cx| {
                    let drag = event.drag(cx).clone();
                    move_terminal.update(cx, |terminal, terminal_cx| {
                        terminal.move_watchlist_drag(
                            &drag.provider,
                            &drag.instrument_id,
                            f32::from(event.event.position.y),
                            f32::from(event.bounds.top()),
                            f32::from(move_scroll.offset().y),
                            terminal_cx,
                        );
                    });
                })
                .on_drop(move |_: &WatchlistRowDrag, _, cx| {
                    end_terminal.update(cx, TerminalApp::end_watchlist_drag);
                }),
        )
}

fn watchlist_columns(theme: &AerisTheme) -> impl IntoElement + use<> {
    let colors = theme.colors;
    div()
        .h(px(WATCHLIST_COLUMNS_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .text_color(gpui_color(colors.text_muted))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .pl(px(WATCHLIST_FLAG_SLOT_WIDTH))
                .pr_2()
                .whitespace_nowrap()
                .text_ellipsis()
                .child("ASSET"),
        )
        .child(watchlist_header_cell("LAST", WATCHLIST_LAST_WIDTH, theme))
        .child(watchlist_header_cell("CHG", WATCHLIST_CHANGE_WIDTH, theme))
        .child(watchlist_header_cell(
            "CHG %",
            WATCHLIST_CHANGE_PERCENT_WIDTH,
            theme,
        ))
        .child(watchlist_header_cell(
            "VOLUME",
            WATCHLIST_VOLUME_WIDTH,
            theme,
        ))
}

fn watchlist_header_cell(
    value: impl Into<gpui::SharedString>,
    width: f32,
    theme: &AerisTheme,
) -> Div {
    div()
        .w(px(width))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_end()
        .px_1()
        .border_l_1()
        .border_color(gpui_color(theme.colors.border))
        .text_right()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(value.into())
}

fn watchlist_value_frame(width: f32) -> Div {
    div()
        .w(px(width))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_end()
        .px_1()
        .text_right()
        .whitespace_nowrap()
        .text_ellipsis()
}

fn watchlist_value_cell(value: impl Into<gpui::SharedString>, width: f32) -> Div {
    watchlist_value_frame(width).child(value.into())
}

fn watchlist_asset_cell(row: &WatchlistRow, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let asset_tone = if row.message.is_some() {
        colors.text_muted
    } else {
        colors.text_default
    };
    let logo = aeris_market_runtime::built_in_provider_presentations()
        .iter()
        .find(|descriptor| descriptor.id == row.instrument.provider)
        .and_then(|descriptor| super::assets::ExchangeLogo::for_logo_key(descriptor.logo_key));
    div()
        .min_w_0()
        .flex_1()
        .flex()
        .items_center()
        .gap_1()
        .pr_2()
        .whitespace_nowrap()
        .text_color(gpui_color(asset_tone))
        .children(logo.map(|logo| exchange_mark(logo, px(16.0), false, &colors)))
        .child(
            div()
                .min_w_0()
                .text_ellipsis()
                .child(row.instrument.display_symbol.clone()),
        )
        .when(row.trading_activity, |cell| {
            cell.child(
                div()
                    .flex_none()
                    .size(px(6.0))
                    .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
                    .bg(gpui_color(colors.primary)),
            )
        })
}

/// The last price with its changed digits in the tick direction's color.
fn watchlist_last_cell(row: &WatchlistRow, last: Option<i64>, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let Some(last) = last else {
        return watchlist_value_cell("—", WATCHLIST_LAST_WIDTH);
    };
    let text = market_summary_price(last, &row.instrument);
    let tick = row
        .tick_reference
        .and_then(|reference| market_summary_tick(&text, last, reference, &row.instrument));
    let Some(tick) = tick else {
        return watchlist_value_cell(text, WATCHLIST_LAST_WIDTH);
    };
    let tone = if tick.direction == std::cmp::Ordering::Greater {
        colors.text_positive
    } else {
        colors.text_negative
    };
    let (unchanged, changed) = text.split_at(tick.changed_from.min(text.len()));
    let (unchanged, changed) = (unchanged.to_string(), changed.to_string());
    watchlist_value_frame(WATCHLIST_LAST_WIDTH)
        .child(unchanged)
        .child(div().text_color(gpui_color(tone)).child(changed))
}

fn watchlist_row_content(
    terminal: &Entity<TerminalApp>,
    row: &WatchlistRow,
    picker: Option<&WatchlistFlagPicker>,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let values = market_summary_values(row.last, row.previous_close);
    let tone = values
        .change
        .map_or(colors.text_muted, |value| match value.cmp(&0) {
            std::cmp::Ordering::Less => colors.text_negative,
            std::cmp::Ordering::Greater => colors.text_positive,
            std::cmp::Ordering::Equal => colors.text_secondary,
        });
    div()
        .id(gpui::SharedString::from(format!(
            "watchlist_row_{}_{}",
            row.instrument.provider, row.instrument.instrument_id
        )))
        .group("watchlist_asset_row")
        .h(px(WATCHLIST_ROW_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .text_xs()
        .bg(gpui_color(if row.active {
            colors.active_bg.over(colors.surface)
        } else {
            colors.surface
        }))
        .when(!row.active, |item| {
            item.hover(move |item| item.bg(gpui_color(colors.hover_bg.over(colors.surface))))
        })
        .child(watchlist_flag_slot(terminal, row, picker, theme))
        .child(watchlist_asset_cell(row, theme))
        .child(watchlist_last_cell(row, values.last, theme))
        .child(
            watchlist_value_cell(
                values.change.map_or_else(
                    || "—".to_string(),
                    |value| market_summary_change(value, &row.instrument),
                ),
                WATCHLIST_CHANGE_WIDTH,
            )
            .text_color(gpui_color(tone)),
        )
        .child(
            watchlist_value_cell(
                values
                    .change_percent
                    .map_or_else(|| "—".to_string(), |value| format!("{value:+.2}%")),
                WATCHLIST_CHANGE_PERCENT_WIDTH,
            )
            .text_color(gpui_color(tone)),
        )
        .child(watchlist_value_cell(
            row.last.map_or_else(
                || "—".to_string(),
                |bar| compact_watchlist_volume(bar.volume, row.instrument.quantity_scale),
            ),
            WATCHLIST_VOLUME_WIDTH,
        ))
}

fn watchlist_row(
    terminal: &Entity<TerminalApp>,
    row: &WatchlistRow,
    index: usize,
    drag: Option<&WatchlistDragState>,
    picker: Option<&WatchlistFlagPicker>,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let colors = theme.colors;
    let instrument = row.instrument.clone();
    let active = row.active;
    let dragging = drag.is_some_and(|drag| {
        drag.provider == row.instrument.provider
            && drag.instrument_id == row.instrument.instrument_id
    });
    let drag_translation = watchlist_drag_translation(
        drag,
        &row.instrument.provider,
        &row.instrument.instrument_id,
        index,
    );
    let content = watchlist_row_content(terminal, row, picker, theme);
    let row = interactive_watchlist_row(content, terminal, instrument, active)
        .when(dragging, gpui::Styled::shadow_md)
        .when_some(drag_translation, |row, translation| {
            row.relative().top(px(translation))
        });
    div()
        .relative()
        .h(px(WATCHLIST_ROW_HEIGHT))
        .flex_none()
        .child(row)
        .children(dragging.then(|| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .top_0()
                .h(px(2.0))
                .bg(gpui_color(colors.primary))
        }))
}

fn interactive_watchlist_row(
    row: Stateful<Div>,
    terminal: &Entity<TerminalApp>,
    instrument: InstallProviderInstrument,
    active: bool,
) -> Stateful<Div> {
    let provider = instrument.provider.clone();
    let instrument_id = instrument.instrument_id.clone();
    let remove_terminal = terminal.clone();
    let select_terminal = terminal.clone();
    let begin_terminal = terminal.clone();
    let drag = WatchlistRowDrag {
        provider: provider.clone(),
        instrument_id: instrument_id.clone(),
    };
    row.cursor_pointer()
        .role(Role::Button)
        .aria_selected(active)
        .aria_label(format!("Select {}", instrument.display_symbol))
        .on_click(move |_, _, cx| {
            select_terminal.update(cx, |terminal, terminal_cx| {
                terminal.select_watchlist_instrument(&instrument, terminal_cx);
            });
        })
        .on_drag(drag, move |drag, cursor_offset, _, cx| {
            begin_terminal.update(cx, |terminal, terminal_cx| {
                terminal.begin_watchlist_drag(
                    &drag.provider,
                    &drag.instrument_id,
                    f32::from(cursor_offset.y),
                    terminal_cx,
                );
            });
            cx.new(|_| drag.clone())
        })
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            remove_terminal.update(cx, |terminal, terminal_cx| {
                terminal.remove_watchlist_instrument(&provider, &instrument_id, terminal_cx);
            });
            cx.stop_propagation();
        })
}

/// The flag at a row's leading edge. An unflagged row shows nothing until the slot is
/// hovered, then previews the flag in the default color. A left press sets or clears the flag and a right press opens its
/// color picker; neither reaches the row, so they never select, drag or remove it.
fn watchlist_flag_slot(
    terminal: &Entity<TerminalApp>,
    row: &WatchlistRow,
    picker: Option<&WatchlistFlagPicker>,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let glyph = |icon: HugeIcon, color: Hsla| {
        div().absolute().inset_0().flex().items_center().child(
            Icon::new(icon.path())
                .with_size(px(WATCHLIST_FLAG_GLYPH_SIZE))
                .color(color),
        )
    };
    let glyphs = match &row.flag {
        Some(flag) => vec![glyph(
            HugeIcon::FlagFilled,
            chart_css_color(flag, colors.primary),
        )],
        None => vec![
            glyph(
                HugeIcon::FlagFilled,
                chart_css_color(DEFAULT_WATCHLIST_FLAG_COLOR, colors.primary),
            )
            .opacity(0.0)
            .group_hover(WATCHLIST_FLAG_GROUP, |style| style.opacity(1.0)),
        ],
    };
    let provider = row.instrument.provider.clone();
    let instrument_id = row.instrument.instrument_id.clone();
    let press_terminal = terminal.clone();
    let (press_provider, press_instrument) = (provider.clone(), instrument_id.clone());
    let picker_terminal = terminal.clone();
    let (picker_provider, picker_instrument) = (provider.clone(), instrument_id.clone());
    let slot = div()
        .id(gpui::SharedString::from(format!(
            "watchlist_flag_{provider}_{instrument_id}"
        )))
        .group(WATCHLIST_FLAG_GROUP)
        .relative()
        .flex_none()
        .w(px(WATCHLIST_FLAG_SLOT_WIDTH))
        .h_full()
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(if row.flag.is_some() {
            format!("Remove flag from {}", row.instrument.display_symbol)
        } else {
            format!("Flag {}", row.instrument.display_symbol)
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            press_terminal.update(cx, |terminal, terminal_cx| {
                terminal.press_watchlist_flag(
                    &press_provider,
                    &press_instrument,
                    window,
                    terminal_cx,
                );
            });
        })
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            cx.stop_propagation();
            picker_terminal.update(cx, |terminal, terminal_cx| {
                terminal.toggle_watchlist_flag_picker(
                    &picker_provider,
                    &picker_instrument,
                    window,
                    terminal_cx,
                );
            });
        })
        .children(glyphs);
    let Some(picker) = picker else {
        return slot;
    };
    let apply = terminal.clone();
    let dismiss = terminal.clone();
    slot.child(gpui::deferred(
        ColorPicker::new(
            "watchlist_flag_color_picker",
            row.flag
                .clone()
                .unwrap_or_else(|| DEFAULT_WATCHLIST_FLAG_COLOR.to_string()),
            &picker.input,
            theme,
        )
        .error(picker.error.as_deref())
        .on_select(move |value, _, cx| {
            apply.update(cx, |terminal, terminal_cx| {
                terminal.apply_watchlist_flag_color(&value, terminal_cx);
            });
        })
        .on_dismiss(move |_, cx| {
            dismiss.update(cx, TerminalApp::close_watchlist_flag_picker);
        }),
    ))
}

/// The header's flag filter: every row, or only the rows carrying one flag color.
fn watchlist_flag_filter_control(
    terminal: &Entity<TerminalApp>,
    flags: &WatchlistFlagsView,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let icon = match &flags.filter {
        Some(color) => {
            header_icon(HugeIcon::FlagFilled).color(chart_css_color(color, theme.colors.primary))
        }
        None => header_icon(HugeIcon::Flag),
    };
    let toggle = terminal.clone();
    let trigger = chrome_tooltip(
        "watchlist_flag_filter",
        "Filter by flag",
        Button::new("watchlist_flag_filter", theme)
            .variant(ButtonVariant::Ghost)
            .button_size(ButtonSize::Sm)
            .round()
            .icon(icon)
            .open(flags.filter_menu_open)
            .aria_label("Filter by flag")
            .on_press(move |_, _, cx| {
                toggle.update(cx, TerminalApp::toggle_watchlist_flag_filter_menu);
            }),
        theme,
    );
    let dismiss = terminal.clone();
    MenuAnchor::new("watchlist_flag_filter_anchor", trigger)
        .menu(
            flags
                .filter_menu_open
                .then(|| watchlist_flag_filter_menu(terminal, flags, theme)),
        )
        .on_dismiss(move |_, cx| {
            dismiss.update(cx, TerminalApp::close_watchlist_flag_filter_menu);
        })
}

fn watchlist_flag_filter_menu(
    terminal: &Entity<TerminalApp>,
    flags: &WatchlistFlagsView,
    theme: &AerisTheme,
) -> MenuPanel {
    let show_all = terminal.clone();
    let mut menu = MenuPanel::new(
        "watchlist_flag_filter_menu",
        MenuPlacement::Anchored {
            side: MenuSide::Below,
            align: MenuAlign::End,
        },
        theme,
    )
    .width(px(WATCHLIST_FLAG_FILTER_MENU_WIDTH))
    .animate_from(PopupAnimationOrigin::TOP_RIGHT)
    .child(
        MenuRow::compact("watchlist_flag_filter_all", "All symbols", theme)
            .checked(flags.filter.is_none())
            .on_click(move |_, _, cx| {
                show_all.update(cx, |terminal, terminal_cx| {
                    terminal.set_watchlist_flag_filter(None, terminal_cx);
                });
            }),
    )
    .separator();
    if flags.colors.is_empty() {
        return menu.child(
            MenuRow::compact("watchlist_flag_filter_none", "No flagged symbols", theme)
                .disabled(true),
        );
    }
    for (index, color) in flags.colors.iter().enumerate() {
        let select = terminal.clone();
        let value = color.clone();
        menu = menu.child(
            MenuRow::compact(("watchlist_flag_filter_color", index), "Flagged", theme)
                .leading(
                    Icon::new(HugeIcon::FlagFilled.path())
                        .small()
                        .color(chart_css_color(color, theme.colors.primary)),
                )
                .detail(color.clone())
                .checked(flags.filter.as_ref() == Some(color))
                .on_click(move |_, _, cx| {
                    select.update(cx, |terminal, terminal_cx| {
                        terminal.set_watchlist_flag_filter(Some(value.clone()), terminal_cx);
                    });
                }),
        );
    }
    menu
}
fn compact_watchlist_volume(value: i64, scale: u32) -> String {
    let exponent = i32::try_from(scale).unwrap_or(i32::MAX);
    let divisor = 10_f64.powi(exponent);
    let value = value.to_f64().unwrap_or(0.0) / divisor;
    for (threshold, suffix) in [(1_000_000_000.0, "B"), (1_000_000.0, "M"), (1_000.0, "K")] {
        if value.abs() >= threshold {
            return format!("{:.2}{suffix}", value / threshold);
        }
    }
    format!("{value:.2}")
}

fn watchlist_add_symbol_control(
    app: Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    chrome_tooltip(
        "watchlist_add_symbol",
        "Add symbol to watchlist",
        Button::new("watchlist_add_symbol", theme)
            .variant(ButtonVariant::Ghost)
            .button_size(ButtonSize::Sm)
            .round()
            .icon(header_icon(HugeIcon::Add))
            .aria_label("Add symbol to watchlist")
            .on_press(move |position, window, cx| {
                app.update(cx, |surface, surface_cx| {
                    surface.open_watchlist_symbol_menu_at(position, window, surface_cx);
                });
            }),
        theme,
    )
}
