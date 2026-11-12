//! The screener page: every market the screening provider lists, with live statistics,
//! sortable columns, category filters and search. Selecting a row opens it in the terminal.

use super::chart_context_menus::clamp_panel_origin_within;
use super::market_scan_view::{market_scans_tab, screener_tabs};
use super::market_screener::{
    AppView, MarketScreener, ResolvedMarket, SCREENER_REFRESH_TICK, ScreenerCategory,
    ScreenerColumn, ScreenerRow, ScreenerRowMenu, ScreenerSort, ScreenerStatus, ScreenerTab,
    capture_time_label,
};
use super::*;
use aeris_localization::{Text, panels, tr, tr_count};
use gpui::{MouseDownEvent, uniform_list};
use std::cmp::Ordering as CmpOrdering;

const PAGE_PADDING_X: f32 = 24.0;
const PAGE_PADDING_TOP: f32 = 20.0;
const PAGE_SECTION_GAP: f32 = 16.0;
pub(super) const CONTROL_HEIGHT: f32 = 28.0;
const SEARCH_WIDTH: f32 = 260.0;
pub(super) const TABLE_HEADER_HEIGHT: f32 = 32.0;
pub(super) const ROW_HEIGHT: f32 = 36.0;
const CELL_PADDING_X: f32 = 12.0;
const RANK_WIDTH: f32 = 56.0;
pub(super) const MARKET_MIN_WIDTH: f32 = 200.0;
pub(super) const EXCHANGE_LOGO_SIZE: f32 = 16.0;
pub(super) const SORT_ICON_SIZE: f32 = 12.0;
pub(super) const FOOTER_HEIGHT: f32 = 36.0;
const ROW_MENU_WIDTH: f32 = 200.0;
const ROW_MENU_ICON_SIZE: f32 = 16.0;

#[derive(Clone, Copy)]
struct ColumnSpec {
    column: ScreenerColumn,
    label: Text,
    /// Fixed width of a numeric column; the market column takes the remaining space.
    width: Option<f32>,
}

const COLUMNS: [ColumnSpec; 6] = [
    ColumnSpec {
        column: ScreenerColumn::Market,
        label: panels::SCREENER_COLUMN_MARKET,
        width: None,
    },
    ColumnSpec {
        column: ScreenerColumn::Price,
        label: panels::SCREENER_COLUMN_MARK_PRICE,
        width: Some(140.0),
    },
    ColumnSpec {
        column: ScreenerColumn::Change,
        label: panels::SCREENER_COLUMN_CHANGE_24H,
        width: Some(112.0),
    },
    ColumnSpec {
        column: ScreenerColumn::Volume,
        label: panels::SCREENER_COLUMN_VOLUME_24H,
        width: Some(128.0),
    },
    ColumnSpec {
        column: ScreenerColumn::OpenInterest,
        label: panels::SCREENER_COLUMN_OPEN_INTEREST,
        width: Some(128.0),
    },
    ColumnSpec {
        column: ScreenerColumn::Funding,
        label: panels::SCREENER_COLUMN_FUNDING,
        width: Some(120.0),
    },
];

/// A padded cell; every cell but the row's last carries the vertical column divider.
pub(super) fn table_cell(theme: &AerisTheme, divided: bool) -> Div {
    div()
        .h_full()
        .flex()
        .items_center()
        .overflow_hidden()
        .px(px(CELL_PADDING_X))
        .when(divided, |cell| {
            cell.border_r(px(theme.dimensions.border_width))
                .border_color(gpui_color(theme.colors.border_subtle))
        })
}

fn column_cell(spec: ColumnSpec, theme: &AerisTheme) -> Div {
    let last = spec.column == COLUMNS[COLUMNS.len() - 1].column;
    let cell = table_cell(theme, !last);
    match spec.width {
        Some(width) => cell.w(px(width)).flex_none().justify_end(),
        None => cell.min_w(px(MARKET_MIN_WIDTH)).flex_1(),
    }
}

/// The table container's corner radius less its border, so square-cornered table chrome
/// can be rounded to sit inside the border instead of over it.
pub(super) fn table_inner_radius(theme: &AerisTheme) -> Pixels {
    px((f32::from(RadiusToken::Default.logical_pixels()) - theme.dimensions.border_width).max(0.0))
}

pub(super) fn rank_cell(theme: &AerisTheme) -> Div {
    table_cell(theme, true).w(px(RANK_WIDTH)).flex_none()
}

pub(super) fn table_row_frame(height: f32) -> Div {
    div()
        .w_full()
        .h(px(height))
        .flex_none()
        .flex()
        .items_center()
        .map(numeric_text)
}

pub(super) fn direction_color(direction: Option<CmpOrdering>, theme: &AerisTheme) -> Hsla {
    let colors = theme.colors;
    gpui_color(match direction {
        Some(CmpOrdering::Greater) => colors.text_positive,
        Some(CmpOrdering::Less) => colors.text_negative,
        _ => colors.text_default,
    })
}

/// The screener page body shown under the title bar in place of the terminal workspace.
pub(super) fn market_screener_page(
    terminal: &Entity<TerminalApp>,
    screener: &MarketScreener,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    div()
        .id("market_screener_page")
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(PAGE_SECTION_GAP))
        .pt(px(PAGE_PADDING_TOP))
        .px(px(PAGE_PADDING_X))
        .bg(gpui_color(colors.surface))
        .child(page_header(terminal, screener, theme))
        .child(screener_tabs(terminal, screener.tab(), theme))
        .map(|page| match screener.tab() {
            ScreenerTab::Markets => page
                .child(category_bar(terminal, screener, theme))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .flex()
                        .flex_col()
                        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
                        .border(px(theme.dimensions.border_width))
                        .border_color(gpui_color(colors.border_secondary))
                        .overflow_hidden()
                        .child(table_header(terminal, screener.sort(), theme))
                        .child(table_body(terminal, screener, theme)),
                )
                .child(page_footer(screener, theme)),
            ScreenerTab::Scans => page.child(market_scans_tab(terminal, screener, theme)),
        })
        .into_any_element()
}

fn page_header(
    terminal: &Entity<TerminalApp>,
    screener: &MarketScreener,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    let mut summary = vec![
        screener
            .provider_name()
            .unwrap_or(tr!(panels::SCREENER_NO_PROVIDER_NAME))
            .to_string(),
    ];
    let markets = screener.tab() == ScreenerTab::Markets;
    if markets && screener.status() == ScreenerStatus::Ready {
        summary.push(tr_count!(
            panels::SCREENER_MARKETS_COUNT,
            screener.counts().all
        ));
    }
    if let Some(captured) = screener.captured_at_unix_millis().filter(|_| markets) {
        summary.push(tr!(
            panels::SCREENER_UPDATED,
            time = capture_time_label(captured)
        ));
    }
    let refresh_terminal = terminal.clone();
    let refresh = Button::new("screener_refresh", theme)
        .variant(ButtonVariant::Outline)
        .icon(header_icon(HugeIcon::Refresh))
        .aria_label(tr!(panels::SCREENER_REFRESH_ARIA))
        .tooltip(
            TooltipSpec::new(tr!(panels::SCREENER_REFRESH), theme).show_delay(TOOLTIP_OPEN_DELAY),
        )
        .on_click(move |_, _, cx| {
            refresh_terminal.update(cx, TerminalApp::refresh_market_screener);
        });
    div()
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xl()
                        .font_weight(platform_font_weight(TypographyRole::Strong))
                        .text_color(gpui_color(colors.text_default))
                        .child(AppView::Screener.label()),
                )
                .child(
                    div()
                        .text_sm()
                        .truncate()
                        .text_color(gpui_color(colors.text_secondary))
                        .child(summary.join(" · ")),
                ),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .children(screener.search_input().filter(|_| markets).map(|input| {
                    div()
                        .w(px(SEARCH_WIDTH))
                        .h(px(CONTROL_HEIGHT))
                        .flex()
                        .child(Input::new(input).platform(theme).flex_1())
                }))
                .when(markets, |controls| controls.child(refresh)),
        )
}

fn category_bar(
    terminal: &Entity<TerminalApp>,
    screener: &MarketScreener,
    theme: &AerisTheme,
) -> Div {
    let colors = theme.colors;
    let counts = screener.counts();
    let selected = screener.category();
    let tabs = ScreenerCategory::ALL
        .into_iter()
        .enumerate()
        .map(|(index, category)| {
            let select_terminal = terminal.clone();
            let active = category == selected;
            Tab::new(("screener_category", index), theme)
                .selected(active)
                .gap_1()
                .aria_label(tr!(
                    panels::SCREENER_SHOW_CATEGORY,
                    category = category.label()
                ))
                .on_click(move |_, _, cx| {
                    select_terminal.update(cx, |terminal, terminal_cx| {
                        terminal.set_market_screener_category(category, terminal_cx);
                    });
                })
                .child(category.label())
                .child(
                    div()
                        .text_color(gpui_color(if active {
                            colors.text_secondary
                        } else {
                            colors.text_muted
                        }))
                        .child(counts.get(category).to_string()),
                )
        });
    div().w_full().flex_none().flex().items_center().child(
        TabList::new(
            "screener_categories",
            tr!(panels::SCREENER_CATEGORY_LABEL),
            theme,
        )
        .children(tabs),
    )
}

fn table_header(terminal: &Entity<TerminalApp>, sort: ScreenerSort, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    table_row_frame(TABLE_HEADER_HEIGHT)
        // GPUI clips children to a rectangle, so the filled header carries the container's
        // rounding itself rather than painting over the rounded border.
        .rounded_t(table_inner_radius(theme))
        .bg(gpui_color(colors.surface_secondary))
        .border_b(px(theme.dimensions.border_width))
        .border_color(gpui_color(colors.border_secondary))
        .text_xs()
        .text_color(gpui_color(colors.text_secondary))
        .child(rank_cell(theme).child("#"))
        .children(
            COLUMNS
                .into_iter()
                .map(|spec| header_cell(terminal, spec, sort, theme)),
        )
}

fn header_cell(
    terminal: &Entity<TerminalApp>,
    spec: ColumnSpec,
    sort: ScreenerSort,
    theme: &AerisTheme,
) -> impl IntoElement {
    let colors = theme.colors;
    let active = sort.column == spec.column;
    let indicator = active.then(|| {
        header_icon(HugeIcon::ChevronDown)
            .with_size(px(SORT_ICON_SIZE))
            .color(gpui_color(colors.text_default))
            .when(!sort.descending, |icon| icon.rotate(0.5))
    });
    let sort_terminal = terminal.clone();
    let direction = if active && sort.descending {
        tr!(panels::SCREENER_DIRECTION_DESCENDING)
    } else {
        tr!(panels::SCREENER_DIRECTION_ASCENDING)
    };
    let label = div().child(spec.label.get());
    let numeric = spec.width.is_some();
    column_cell(spec, theme).child(
        div()
            .id(SharedString::from(format!("screener_sort_{}", spec.label)))
            .h_full()
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .role(Role::Button)
            .aria_label(if active {
                tr!(
                    panels::SCREENER_SORTED_ARIA,
                    label = spec.label,
                    direction = direction
                )
            } else {
                tr!(panels::SCREENER_SORT_BY, label = spec.label)
            })
            .when(active, |cell| {
                cell.text_color(gpui_color(colors.text_default))
            })
            .hover(move |cell| cell.text_color(gpui_color(colors.text_default)))
            .on_click(move |_, _, cx| {
                sort_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.sort_market_screener(spec.column, terminal_cx);
                });
            })
            .map(|cell| {
                // Numeric columns are right-aligned, so their indicator leads the label.
                if numeric {
                    cell.children(indicator).child(label)
                } else {
                    cell.child(label).children(indicator)
                }
            }),
    )
}

fn table_body(
    terminal: &Entity<TerminalApp>,
    screener: &MarketScreener,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let rows = screener.rows();
    if rows.is_empty() {
        return table_placeholder(screener, theme);
    }
    let list_terminal = terminal.clone();
    let list_theme = *theme;
    let list_rows = Rc::clone(&rows);
    let logo = screener.exchange_logo();
    let scroll = screener.scroll.clone();
    let scrollbar = scroll.0.borrow().base_handle.clone();
    div()
        .relative()
        .flex_1()
        .min_h_0()
        .w_full()
        .child(
            uniform_list("market_screener_rows", rows.len(), move |range, _, _| {
                let last = list_rows.len().saturating_sub(1);
                range
                    .filter_map(|index| {
                        Some(table_row(
                            &list_terminal,
                            index,
                            index == last,
                            list_rows.get(index)?,
                            logo,
                            &list_theme,
                        ))
                    })
                    .collect::<Vec<_>>()
            })
            .track_scroll(&scroll)
            .size_full(),
        )
        .child(ThinScrollbar::new(
            &scrollbar,
            gpui_color(colors.text_secondary),
        ))
        .into_any_element()
}

fn table_row(
    terminal: &Entity<TerminalApp>,
    index: usize,
    last: bool,
    row: &ScreenerRow,
    logo: Option<assets::ExchangeLogo>,
    theme: &AerisTheme,
) -> Stateful<Div> {
    let colors = theme.colors;
    let open_terminal = terminal.clone();
    let open_row = row.clone();
    let kind = row.kind.map(|kind| {
        div()
            .flex_none()
            .px(px(6.0))
            .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
            .bg(gpui_color(colors.surface_secondary))
            .text_xs()
            .text_color(gpui_color(colors.text_secondary))
            .child(kind.label())
    });
    table_row_frame(ROW_HEIGHT)
        .id(("market_screener_row", index))
        // The final row meets the container's rounded border, which also separates it.
        .map(|frame| {
            if last {
                frame.rounded_b(table_inner_radius(theme))
            } else {
                frame
                    .border_b(px(theme.dimensions.border_width))
                    .border_color(gpui_color(colors.border_subtle))
            }
        })
        .text_sm()
        .text_color(gpui_color(colors.text_default))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(tr!(panels::SCREENER_OPEN_ROW, symbol = row.display_symbol))
        .hover(move |row| row.bg(gpui_color(colors.hover_bg.over(colors.surface))))
        .on_click(move |_, _, cx| {
            open_terminal.update(cx, |terminal, terminal_cx| {
                terminal.open_market_screener_row(&open_row, terminal_cx);
            });
        })
        .on_mouse_down(
            MouseButton::Right,
            row_menu_on_right_click(terminal, &row.symbol, &row.exchange, &row.display_symbol),
        )
        .child(
            rank_cell(theme)
                .text_xs()
                .text_color(gpui_color(colors.text_muted))
                .child((index + 1).to_string()),
        )
        .child(
            column_cell(COLUMNS[0], theme)
                .gap_2()
                .children(
                    logo.map(|logo| exchange_mark(logo, px(EXCHANGE_LOGO_SIZE), false, &colors)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(platform_font_weight(TypographyRole::Emphasis))
                        .text_color(gpui_color(colors.text_default))
                        .child(row.display_symbol.clone()),
                )
                .children(kind),
        )
        .child(column_cell(COLUMNS[1], theme).child(row.price.clone()))
        .child(
            column_cell(COLUMNS[2], theme)
                .text_color(direction_color(row.change_direction, theme))
                .child(row.change.clone()),
        )
        .child(column_cell(COLUMNS[3], theme).child(row.volume.clone()))
        .child(column_cell(COLUMNS[4], theme).child(row.open_interest.clone()))
        .child(
            column_cell(COLUMNS[5], theme)
                .text_color(direction_color(row.funding_direction, theme))
                .child(row.funding.clone()),
        )
}

/// Loading, failure and no-match states in place of the rows.
fn table_placeholder(screener: &MarketScreener, theme: &AerisTheme) -> AnyElement {
    let colors = theme.colors;
    let content = match (screener.status(), screener.message()) {
        (ScreenerStatus::Loading, None) => div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Loader::new("market_screener_loader", header_icon(HugeIcon::Loader))
                    .color(gpui_color(colors.text_secondary)),
            )
            .child(tr!(panels::SCREENER_LOADING)),
        (ScreenerStatus::Loading, Some(message)) => div().child(message.to_string()),
        (ScreenerStatus::Ready, _) => {
            div().child(if screener.counts().get(screener.category()) == 0 {
                match screener.category() {
                    ScreenerCategory::All => tr!(panels::SCREENER_NONE_LISTED_ALL),
                    ScreenerCategory::Perpetuals => tr!(panels::SCREENER_NONE_LISTED_PERPETUALS),
                    ScreenerCategory::Spot => tr!(panels::SCREENER_NONE_LISTED_SPOT),
                }
                .to_string()
            } else {
                tr!(panels::SCREENER_NO_MATCH).to_string()
            })
        }
    };
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(gpui_color(colors.text_secondary))
        .child(content)
        .into_any_element()
}

fn page_footer(screener: &MarketScreener, theme: &AerisTheme) -> Div {
    let colors = theme.colors;
    let shown = screener.rows().len();
    let total = screener.counts().all;
    let status = screener
        .pending_selection_status()
        .map(|text| (text, colors.text_secondary))
        .or_else(|| {
            screener
                .message()
                .filter(|_| screener.status() == ScreenerStatus::Ready)
                .map(|message| (message.to_string(), colors.text_warning))
        })
        .or_else(|| watchlist_status_line(screener, theme));
    div()
        .w_full()
        .h(px(FOOTER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .text_xs()
        .text_color(gpui_color(colors.text_muted))
        .map(numeric_text)
        .child(
            div()
                .flex_none()
                .child(if screener.status() == ScreenerStatus::Ready {
                    tr!(panels::SCREENER_SHOWING, shown = shown, total = total)
                } else {
                    String::new()
                }),
        )
        .children(status.map(|(text, color)| {
            div()
                .min_w_0()
                .truncate()
                .text_color(gpui_color(color))
                .child(text)
        }))
        .child(div().flex_none().child(tr!(panels::SCREENER_CAPTION)))
}

/// The latest watchlist addition from a row menu, for a page footer.
pub(super) fn watchlist_status_line(
    screener: &MarketScreener,
    theme: &AerisTheme,
) -> Option<(String, ThemeColor)> {
    screener.watchlist_status().map(|(text, attention)| {
        let color = if attention {
            theme.colors.text_warning
        } else {
            theme.colors.text_secondary
        };
        (text, color)
    })
}

/// The menu a right-clicked row opens at the pointer.
pub(super) fn screener_row_menu_layer(
    terminal: &Entity<TerminalApp>,
    menu: &ScreenerRowMenu,
    viewport: gpui::Size<Pixels>,
    theme: &AerisTheme,
) -> AnyElement {
    let panel_size = size(
        px(ROW_MENU_WIDTH),
        menu_panel_height(1, 0, theme, MenuScale::BASE, px(ROOT_REM_PX)),
    );
    let origin =
        clamp_panel_origin_within(menu.anchor, viewport, panel_size, px(OVERLAY_EDGE_MARGIN));
    let animation_origin =
        PopupAnimationOrigin::from_trigger(menu.anchor, Bounds::new(origin, panel_size));
    let add_terminal = terminal.clone();
    let add_menu = menu.clone();
    let add = MenuRow::compact(
        "market_screener_row_add_to_watchlist",
        tr!(panels::SCREENER_ADD_TO_WATCHLIST),
        theme,
    )
    .leading_icon(header_icon(HugeIcon::Watchlist).with_size(px(ROW_MENU_ICON_SIZE)))
    .on_click(move |_, _, cx| {
        add_terminal.update(cx, |terminal, terminal_cx| {
            terminal.add_screener_row_to_watchlist(&add_menu, terminal_cx);
        });
    });
    let dismiss = terminal.clone();
    let dismiss_right = terminal.clone();
    div()
        .id("market_screener_row_menu_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .on_click(move |_, _, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_screener_row_menu(terminal_cx);
            });
            cx.stop_propagation();
        })
        .on_mouse_down(MouseButton::Right, move |_, _, cx| {
            dismiss_right.update(cx, |terminal, terminal_cx| {
                terminal.close_screener_row_menu(terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(
            MenuPanel::new("market_screener_row_menu", MenuPlacement::At(origin), theme)
                .width(px(ROW_MENU_WIDTH))
                .animate_from(animation_origin)
                .child(add),
        )
        .into_any_element()
}

/// Opens a row's menu at a right click on it.
pub(super) fn row_menu_on_right_click(
    terminal: &Entity<TerminalApp>,
    symbol: &str,
    exchange: &str,
    display_symbol: &str,
) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
    let terminal = terminal.clone();
    let (symbol, exchange, display_symbol) = (
        symbol.to_string(),
        exchange.to_string(),
        display_symbol.to_string(),
    );
    move |event, _, cx| {
        let menu = ScreenerRowMenu {
            anchor: event.position,
            symbol: symbol.clone(),
            exchange: exchange.clone(),
            display_symbol: display_symbol.clone(),
        };
        terminal.update(cx, |terminal, terminal_cx| {
            terminal.open_screener_row_menu(menu, terminal_cx);
        });
        cx.stop_propagation();
    }
}

impl TerminalApp {
    /// The screener search field, created the first time the page opens.
    fn ensure_market_screener_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pages.screener.search_input().is_some() {
            return;
        }
        let input = cx.new(|input_cx| {
            InputState::new(window, input_cx).placeholder(tr!(panels::SCREENER_SEARCH_PLACEHOLDER))
        });
        cx.subscribe(&input, |terminal, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let query = input.read(cx).value();
                terminal.pages.screener.set_query(&query);
                terminal.pages.screener.scroll_to_top();
                cx.notify();
            }
        })
        .detach();
        self.pages.screener.set_search_input(input);
    }

    /// Starts the screen consumer if needed, requests fresh statistics and keeps them
    /// refreshing while the page stays visible.
    pub(super) fn activate_market_screener(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace_id = self.tabs.active().id;
        self.pages.screener.ensure_worker(
            self.services.market_factory(),
            workspace_id,
            self.frames.dirty(),
        );
        self.ensure_market_screener_search(window, cx);
        self.refresh_market_scan_inputs(window, cx);
        self.pages.screener.refresh_now();
        self.pages.screener.refresh_if_due(Instant::now());
        let refresh = cx.spawn_in(window, async move |terminal, cx| {
            loop {
                cx.background_executor().timer(SCREENER_REFRESH_TICK).await;
                let visible = terminal.update(cx, |terminal, terminal_cx| {
                    if terminal.pages.view != AppView::Screener {
                        return false;
                    }
                    if terminal.pages.screener.refresh_if_due(Instant::now()) {
                        terminal_cx.notify();
                    }
                    true
                });
                if !matches!(visible, Ok(true)) {
                    break;
                }
            }
        });
        self.pages.screener.keep_refreshing(refresh);
    }

    pub(super) fn refresh_market_screener(&mut self, cx: &mut Context<Self>) {
        self.pages.screener.refresh_now();
        self.pages.screener.refresh_if_due(Instant::now());
        cx.notify();
    }

    pub(super) fn sort_market_screener(&mut self, column: ScreenerColumn, cx: &mut Context<Self>) {
        self.pages.screener.set_sort_column(column);
        self.pages.screener.scroll_to_top();
        cx.notify();
    }

    pub(super) fn set_market_screener_category(
        &mut self,
        category: ScreenerCategory,
        cx: &mut Context<Self>,
    ) {
        self.pages.screener.set_category(category);
        self.pages.screener.scroll_to_top();
        cx.notify();
    }

    pub(super) fn open_screener_row_menu(&mut self, menu: ScreenerRowMenu, cx: &mut Context<Self>) {
        self.pages.screener.open_row_menu(menu);
        cx.notify();
    }

    pub(super) fn close_screener_row_menu(&mut self, cx: &mut Context<Self>) {
        if self.pages.screener.close_row_menu() {
            cx.notify();
        }
    }

    /// Lists a row's market in the watchlist once its instrument resolves; the page stays.
    pub(super) fn add_screener_row_to_watchlist(
        &mut self,
        menu: &ScreenerRowMenu,
        cx: &mut Context<Self>,
    ) {
        self.pages
            .screener
            .add_to_watchlist(&menu.symbol, &menu.exchange, &menu.display_symbol);
        cx.notify();
    }

    pub(super) fn open_market_screener_row(&mut self, row: &ScreenerRow, cx: &mut Context<Self>) {
        self.pages
            .screener
            .open_market(&row.symbol, &row.exchange, &row.display_symbol, None);
        cx.notify();
    }

    /// Applies the screen consumer's latest messages. A resolved row selection switches the
    /// window back to the terminal with that instrument on the active chart.
    pub(super) fn poll_market_screener(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let outcome = self.pages.screener.poll();
        match outcome.resolved {
            Some(ResolvedMarket::Open {
                instrument,
                interval,
            }) => {
                self.show_app_view(AppView::Terminal, window, cx);
                match interval {
                    Some(interval) => self.active_surface().update(cx, |surface, surface_cx| {
                        surface.select_installed_instrument_on(&instrument, interval, surface_cx);
                    }),
                    None => self.select_watchlist_instrument(&instrument, cx),
                }
            }
            Some(ResolvedMarket::Watchlist {
                instrument,
                display_symbol,
            }) => {
                let added = self.add_watchlist_instrument(instrument, cx);
                self.pages
                    .screener
                    .record_watchlist_outcome(display_symbol, added);
                cx.notify();
            }
            None => {}
        }
        if outcome.changed {
            cx.notify();
        }
    }

    pub(super) fn retire_market_screener(&mut self, cx: &App) {
        if let Some(retirement) = self.pages.screener.begin_retirement() {
            self.lifecycle.retire_market_worker(retirement, cx);
        }
    }
}
