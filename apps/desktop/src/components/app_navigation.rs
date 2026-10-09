//! Header page navigation: the dropdown beside the profile avatar that switches the window
//! between the trading terminal and the market screener.

use super::market_screener::{AppView, MarketScreener};
use super::*;

const APP_NAVIGATION_MENU_WIDTH: f32 = 200.0;
const APP_NAVIGATION_MENU_GAP: f32 = 4.0;
const APP_NAVIGATION_ICON_SIZE: f32 = 16.0;

/// The page shown under the title bar, the open page menu, and the pages besides the terminal.
#[derive(Default)]
pub(super) struct AppPages {
    pub(super) view: AppView,
    /// Navigation button click point the open page menu is anchored under.
    navigation_anchor: Option<gpui::Point<Pixels>>,
    pub(super) screener: MarketScreener,
}

pub(super) fn app_navigation_button(
    terminal: &Entity<TerminalApp>,
    current: AppView,
    theme: &AerisTheme,
) -> impl IntoElement {
    let toggle_terminal = terminal.clone();
    chrome_tooltip(
        "app_navigation",
        "Switch page",
        Button::new("app_navigation", theme)
            .resting_fill(theme.colors.surface_secondary)
            .button_size(ButtonSize::Lg)
            .aria_label(format!("Page: {}", current.label()))
            .icon(header_icon(current.icon()))
            .label(current.label())
            .caret(header_icon(HugeIcon::ChevronDown))
            .on_click(move |event, _, cx| {
                let anchor = event.position();
                toggle_terminal.update(cx, |terminal, terminal_cx| {
                    terminal.toggle_app_navigation_at(anchor, terminal_cx);
                });
            }),
        theme,
    )
}

/// The page menu anchored under the navigation button's click point.
pub(super) fn app_navigation_layer(
    terminal: &Entity<TerminalApp>,
    current: AppView,
    anchor: gpui::Point<Pixels>,
    viewport: gpui::Size<Pixels>,
    theme: &AerisTheme,
) -> AnyElement {
    let colors = theme.colors;
    let rows = AppView::ALL.len().to_f32().unwrap_or_default();
    let panel_size = size(
        px(APP_NAVIGATION_MENU_WIDTH),
        px(rows * CHART_CONTEXT_MENU_ROW_HEIGHT + 2.0 * theme.dimensions.border_width),
    );
    let header_bottom = WORKSPACE_TITLE_BAR_HEIGHT + APP_NAVIGATION_MENU_GAP;
    let margin = px(OVERLAY_EDGE_MARGIN);
    let origin = point(
        anchor
            .x
            .max(margin)
            .min((viewport.width - panel_size.width - margin).max(margin)),
        anchor.y.max(px(header_bottom)) + px(APP_NAVIGATION_MENU_GAP),
    );
    let animation_origin =
        PopupAnimationOrigin::from_trigger(anchor, Bounds::new(origin, panel_size));
    let last = AppView::ALL.len() - 1;
    let rows = AppView::ALL.into_iter().enumerate().map(|(index, view)| {
        let select_terminal = terminal.clone();
        MenuRow::compact(
            SharedString::from(format!("app_navigation_{}", view.label())),
            view.label(),
            theme,
        )
        .flush_in_panel(index == 0, index == last)
        .highlighted(view == current)
        .leading(
            header_icon(view.icon())
                .with_size(px(APP_NAVIGATION_ICON_SIZE))
                .color(gpui_color(colors.icon)),
        )
        .trailing(
            div()
                .size(px(APP_NAVIGATION_ICON_SIZE))
                .flex_none()
                .children((view == current).then(|| {
                    header_icon(HugeIcon::CheckIcon)
                        .with_size(px(APP_NAVIGATION_ICON_SIZE))
                        .color(gpui_color(colors.icon_active))
                })),
        )
        .on_click(move |_, window, cx| {
            select_terminal.update(cx, |terminal, terminal_cx| {
                terminal.show_app_view(view, window, terminal_cx);
            });
        })
    });
    let dismiss = terminal.clone();
    let panel = flat_compact_menu_panel(
        "app_navigation_menu",
        origin,
        px(APP_NAVIGATION_MENU_WIDTH),
        theme,
    )
    .children(rows);
    div()
        .id("app_navigation_scrim")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .on_any_mouse_down(move |_, window, cx| {
            dismiss.update(cx, |terminal, terminal_cx| {
                terminal.close_app_navigation(window, terminal_cx);
            });
            cx.stop_propagation();
        })
        .child(animate_popup_from_origin(
            panel,
            "app_navigation_menu_enter",
            animation_origin,
        ))
        .into_any_element()
}

impl TerminalApp {
    /// Opens the page menu under the navigation button's click point, or closes it when open.
    pub(super) fn toggle_app_navigation_at(
        &mut self,
        anchor: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.pages.navigation_anchor = match self.pages.navigation_anchor {
            Some(_) => None,
            None => Some(anchor),
        };
        cx.notify();
    }

    pub(super) fn close_app_navigation(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.pages.navigation_anchor.take().is_some() {
            self.release_frameless_title_bar(window, cx);
            cx.notify();
        }
    }

    pub(super) const fn app_navigation_open(&self) -> bool {
        self.pages.navigation_anchor.is_some()
    }

    /// Switches the page under the title bar. Workspaces, charts and their market demand stay
    /// mounted while the screener is shown, so returning to the terminal is immediate.
    pub(super) fn show_app_view(
        &mut self,
        view: AppView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_app_navigation(window, cx);
        if self.pages.view == view {
            return;
        }
        match view {
            AppView::Screener => {
                self.pages.view = view;
                self.close_chart_context_menu(cx);
                self.close_chart_settings_menu(cx);
                self.close_drawing_tool_menu(cx);
                self.activate_market_screener(window, cx);
                cx.notify();
            }
            AppView::Terminal => {
                self.show_terminal_view(cx);
                self.chrome_focus.focus(window, cx);
            }
        }
    }

    /// Returns to the terminal page without moving focus, for workspace actions that focus
    /// their own target.
    pub(super) fn show_terminal_view(&mut self, cx: &mut Context<Self>) {
        if self.pages.view != AppView::Terminal {
            self.pages.view = AppView::Terminal;
            self.pages.screener.stop_refreshing();
            cx.notify();
        }
    }

    pub(super) fn app_navigation_overlay(
        &self,
        terminal: &Entity<Self>,
        viewport: gpui::Size<Pixels>,
    ) -> Option<AnyElement> {
        let anchor = self.pages.navigation_anchor?;
        Some(app_navigation_layer(
            terminal,
            self.pages.view,
            anchor,
            viewport,
            &self.theme,
        ))
    }
}
