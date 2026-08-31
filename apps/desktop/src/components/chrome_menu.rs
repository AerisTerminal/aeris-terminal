use super::*;

pub(super) const CHROME_MENU_WIDTH: f32 = 896.0;
pub(super) const CHROME_MENU_MIN_WIDTH: f32 = 320.0;
pub(super) const CHROME_MENU_SEARCH_HEIGHT: f32 = 44.0;
pub(super) const CHROME_MENU_INDICATOR_SEARCH_HEIGHT: f32 = 40.0;
pub(super) const CHROME_MENU_LIST_HEIGHT: f32 = 480.0;
pub(super) const CHROME_MENU_FOOTER_HEIGHT: f32 = 40.0;
pub(super) const CHROME_MENU_MAX_HEIGHT: f32 = 704.0;
pub(super) const CHROME_MENU_ROW_ICON_WELL: f32 = 24.0;
pub(super) const CHROME_MENU_SEARCH_ICON_SIZE: f32 = 16.0;

/// Outer width and scrolling-list height the symbol and indicator menus are drawn at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ChromeMenuExtent {
    pub(super) width: f32,
    pub(super) list_height: f32,
}

/// Size the symbol and indicator menus to the surface they open over rather than to a fixed
/// design width. Both keep the 896x704 ceiling on a roomy window and shrink from there, so a
/// narrow or short viewport gets a smaller menu instead of a clipped one: the search header and
/// the footer are fixed chrome, so the scrolling list absorbs whatever height is left.
pub(super) fn chrome_menu_extent(
    viewport: gpui::Size<Pixels>,
    chrome_height: f32,
    search_height: f32,
) -> ChromeMenuExtent {
    let viewport_width = f32::from(viewport.width).max(0.0);
    let width = (viewport_width - OVERLAY_EDGE_MARGIN * 2.0)
        .clamp(CHROME_MENU_MIN_WIDTH, CHROME_MENU_WIDTH)
        .min(viewport_width);
    let available =
        (f32::from(viewport.height) - chrome_height - OVERLAY_EDGE_MARGIN * 2.0).max(0.0);
    let list_height =
        (available.min(CHROME_MENU_MAX_HEIGHT) - search_height - CHROME_MENU_FOOTER_HEIGHT)
            .clamp(0.0, CHROME_MENU_LIST_HEIGHT);
    ChromeMenuExtent { width, list_height }
}

pub(super) fn chrome_menu_surface(
    colors: &axiusflow_design_system::ThemeColors,
    extent: ChromeMenuExtent,
) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .w(px(extent.width))
        .max_h(px(CHROME_MENU_MAX_HEIGHT))
        .max_h_full()
        .overflow_hidden()
        .bg(gpui_color(colors.surface))
        .text_color(gpui_color(colors.text_secondary))
}

pub(super) fn chrome_menu_scroll_body() -> Div {
    div().flex().flex_col().gap(px(2.0)).px(px(6.0)).py(px(6.0))
}

pub(super) fn chrome_menu_group_heading(
    label: &'static str,
    colors: &axiusflow_design_system::ThemeColors,
) -> Div {
    div()
        .flex_none()
        .px(px(10.0))
        .pt(px(2.0))
        .pb(px(4.0))
        .text_size(px(10.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(gpui_color(colors.text_muted))
        .child(label.to_ascii_uppercase())
}

pub(super) fn chrome_menu_empty(
    title: &'static str,
    detail: &'static str,
    colors: &axiusflow_design_system::ThemeColors,
) -> Div {
    div()
        .h(px(144.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .px_6()
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(gpui_color(colors.text_primary))
                .child(title),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(gpui_color(colors.text_muted))
                .child(detail),
        )
}

pub(super) fn chrome_menu_search_header(
    input: &Entity<InputState>,
    theme: &AxiusflowTheme,
    app: &Entity<WorkspaceSurface>,
    hint: impl Into<gpui::SharedString>,
    search_height: f32,
) -> Div {
    let colors = theme.colors;
    div()
        .h(px(search_height))
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px(px(if search_height <= 40.0 { 16.0 } else { 12.0 }))
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_sm()
        .text_color(gpui_color(colors.text_primary))
        .child(header_icon(HugeIcon::SearchIcon01).with_size(px(CHROME_MENU_SEARCH_ICON_SIZE)))
        .child(
            Input::new(input)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                .flex_1(),
        )
        .child(
            div()
                .text_size(px(11.0))
                .text_color(gpui_color(colors.text_muted))
                .child(hint.into()),
        )
        .child(chrome_menu_close_button(app, theme))
}

/// Compact 24px close control matching the workspace tab add button: a circular
/// hit with a 13px glyph, flex-centered. The standard chrome `Button` is a 32px
/// control whose 75% icon scaling and inner wrapper throw the X off-center.
///
/// Every close control in the chrome shares this so they stay identical.
pub(super) fn chrome_close_button<F: Fn(&mut Window, &mut App) + 'static>(
    id: &'static str,
    theme: &AxiusflowTheme,
    on_close: F,
) -> impl IntoElement + use<F> {
    let colors = theme.colors;
    div()
        .id(id)
        .occlude()
        .size(px(WORKSPACE_TAB_ICON_HIT))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(f32::from(RadiusToken::Full.logical_pixels())))
        .text_color(gpui_color(colors.icon))
        .cursor_pointer()
        .role(Role::Button)
        .aria_label("Close")
        .hover(move |close| {
            close
                .bg(gpui_color(colors.hover_bg.over(colors.surface)))
                .text_color(gpui_color(colors.text_primary))
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            on_close(window, cx);
            cx.stop_propagation();
        })
        .child(header_icon(HugeIcon::CancelIcon01).with_size(px(WORKSPACE_TAB_ICON_GLYPH)))
}

pub(super) fn chrome_menu_close_button(
    app: &Entity<WorkspaceSurface>,
    theme: &AxiusflowTheme,
) -> impl IntoElement + use<> {
    let close_app = app.clone();
    chrome_close_button("chrome_menu_close", theme, move |window, cx| {
        close_app.update(cx, |app, app_cx| {
            app.close_chrome_overlay(window, app_cx);
        });
    })
}

pub(super) fn chrome_menu_shortcut(
    colors: &axiusflow_design_system::ThemeColors,
    keys: &'static [&'static str],
    caption: &'static str,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .children(keys.iter().copied().map(|key| {
            div()
                .h(px(20.0))
                .min_w(px(20.0))
                .px_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
                .border_1()
                .border_color(gpui_color(colors.border))
                .child(key)
        }))
        .child(caption)
}

pub(super) fn scrollable_menu_body(
    body: Div,
    scroll: &ScrollHandle,
    color: ThemeColor,
    extent: ChromeMenuExtent,
) -> impl IntoElement + use<> {
    div()
        .relative()
        .flex_none()
        .w_full()
        .max_h(px(extent.list_height))
        .overflow_hidden()
        .child(
            tracked_overflow_y_scrollbar(body, scroll)
                .w_full()
                .max_h(px(extent.list_height)),
        )
        .child(ThinScrollbar::new(scroll, gpui_color(color)))
}

pub(super) fn chrome_menu_footer(
    colors: &axiusflow_design_system::ThemeColors,
    enter: &'static str,
    trailing: impl Into<gpui::SharedString>,
) -> Div {
    div()
        .h(px(CHROME_MENU_FOOTER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .border_t_1()
        .border_color(gpui_color(colors.border))
        .text_xs()
        .text_color(gpui_color(colors.text_muted))
        .child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .child(chrome_menu_shortcut(colors, &["↑", "↓"], "Navigate"))
                .child(chrome_menu_shortcut(colors, &["Enter"], enter))
                .child(chrome_menu_shortcut(colors, &["Esc"], "Close")),
        )
        .child(trailing.into())
}
