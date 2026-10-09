//! Docked watchlist panel: symbol table, reordering and the add-symbol control.

use super::side_panel_dock::side_panel_header;
use super::*;
use gpui::{AppContext, Stateful};

const WATCHLIST_COLUMNS_HEIGHT: f32 = 26.0;
pub(super) const WATCHLIST_ROW_HEIGHT: f32 = 30.0;
const WATCHLIST_LAST_WIDTH: f32 = 70.0;
const WATCHLIST_CHANGE_WIDTH: f32 = 66.0;
const WATCHLIST_CHANGE_PERCENT_WIDTH: f32 = 64.0;
const WATCHLIST_VOLUME_WIDTH: f32 = 60.0;

pub(super) struct WatchlistPanelState {
    pub(super) rows: Vec<WatchlistRow>,
    pub(super) drag: Option<WatchlistDragState>,
    pub(super) scroll: ScrollHandle,
}

pub(super) fn watchlist_side_panel(
    app: Entity<WorkspaceSurface>,
    terminal: &Entity<TerminalApp>,
    watchlist: WatchlistPanelState,
    theme: &AerisTheme,
) -> Div {
    let WatchlistPanelState { rows, drag, scroll } = watchlist;
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
            [watchlist_add_symbol_control(app, theme).into_any_element()],
            theme,
        ))
        .child(watchlist_table(
            terminal,
            rows,
            drag.as_ref(),
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
                .child("Add symbols with +"),
        );
    } else {
        for (index, row) in rows.into_iter().enumerate() {
            body = body.child(watchlist_row(terminal, &row, index, watchlist_drag, theme));
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
                .px_2()
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

fn watchlist_value_cell(value: impl Into<gpui::SharedString>, width: f32) -> Div {
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
        .child(value.into())
}

fn watchlist_asset_cell(row: &WatchlistRow, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let asset_tone = if row.message.is_some() {
        colors.text_muted
    } else {
        colors.text_primary
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
        .px_2()
        .whitespace_nowrap()
        .text_color(gpui_color(asset_tone))
        .children(logo.map(|logo| exchange_mark(logo, px(16.0), false, &colors)))
        .child(
            div()
                .min_w_0()
                .flex_1()
                .text_ellipsis()
                .child(row.instrument.display_symbol.clone()),
        )
}

fn watchlist_row_content(row: &WatchlistRow, theme: &AerisTheme) -> Stateful<Div> {
    let colors = theme.colors;
    let scale = row.instrument.price_scale;
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
        .child(watchlist_asset_cell(row, theme))
        .child(watchlist_value_cell(
            values.last.map_or_else(
                || "—".to_string(),
                |value| market_summary_price(value, scale),
            ),
            WATCHLIST_LAST_WIDTH,
        ))
        .child(
            watchlist_value_cell(
                values.change.map_or_else(
                    || "—".to_string(),
                    |value| market_summary_change(value, scale),
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
    let content = watchlist_row_content(row, theme);
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
