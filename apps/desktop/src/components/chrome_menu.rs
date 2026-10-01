use super::*;

pub(super) const CHROME_MENU_WIDTH: f32 = 960.0;
pub(super) const CHROME_MENU_MIN_WIDTH: f32 = 320.0;
pub(super) const CHROME_MENU_SEARCH_HEIGHT: f32 = 44.0;
pub(super) const CHROME_MENU_LIST_HEIGHT: f32 = 520.0;
pub(super) const CHROME_MENU_MAX_HEIGHT: f32 = 744.0;
pub(super) const CHROME_MENU_ROW_ICON_WELL: f32 = 24.0;
pub(super) const CHROME_MENU_SEARCH_ICON_SIZE: f32 = 16.0;

/// Outer size, search-header height and content scale the symbol and indicator menus are
/// drawn at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ChromeMenuExtent {
    pub(super) width: f32,
    pub(super) search_height: f32,
    pub(super) list_height: f32,
    pub(super) scale: MenuScale,
}

/// Size the symbol and indicator menus to the surface they open over rather than to a fixed
/// design width. On a large logical viewport the 960x564 design, its rows and its text grow by
/// the shared [`MenuScale`]; a narrow or short viewport still shrinks the panel instead of
/// clipping it.
pub(super) fn chrome_menu_extent(
    viewport: gpui::Size<Pixels>,
    chrome_height: f32,
) -> ChromeMenuExtent {
    let scale = MenuScale::for_viewport(viewport);
    let viewport_width = f32::from(viewport.width).max(0.0);
    let width = (viewport_width - OVERLAY_EDGE_MARGIN * 2.0)
        .clamp(CHROME_MENU_MIN_WIDTH, scale.len(CHROME_MENU_WIDTH))
        .min(viewport_width);
    let search_height = scale.len(CHROME_MENU_SEARCH_HEIGHT);
    let available =
        (f32::from(viewport.height) - chrome_height - OVERLAY_EDGE_MARGIN * 2.0).max(0.0);
    let list_height = (available.min(scale.len(CHROME_MENU_MAX_HEIGHT)) - search_height)
        .clamp(0.0, scale.len(CHROME_MENU_LIST_HEIGHT));
    ChromeMenuExtent {
        width,
        search_height,
        list_height,
        scale,
    }
}

pub(super) fn chrome_menu_surface(
    colors: &aeris_design_system::ThemeColors,
    extent: ChromeMenuExtent,
) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .w(px(extent.width))
        .h(px(extent.search_height + extent.list_height))
        .max_h(extent.scale.px(CHROME_MENU_MAX_HEIGHT))
        .max_h_full()
        .rounded(px(f32::from(RadiusToken::Medium.logical_pixels())))
        .overflow_hidden()
        .bg(gpui_color(colors.surface))
        .font_family(aeris_design_system::platform_font_family())
        .font_weight(platform_font_weight(TypographyRole::Normal))
        .text_color(gpui_color(colors.text_secondary))
}

pub(super) fn chrome_menu_scroll_body(scale: MenuScale) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(scale.px(2.0))
        .px(scale.px(6.0))
        .py(scale.px(6.0))
}

pub(super) fn chrome_menu_group_heading(
    label: &'static str,
    scale: MenuScale,
    colors: &aeris_design_system::ThemeColors,
) -> Div {
    div()
        .flex_none()
        .px(scale.px(10.0))
        .pt(scale.px(2.0))
        .pb(scale.px(4.0))
        .text_size(scale.px(10.0))
        .font_weight(platform_font_weight(TypographyRole::Normal))
        .text_color(gpui_color(colors.text_muted))
        .child(label.to_ascii_uppercase())
}

pub(super) fn chrome_menu_empty(
    title: &'static str,
    detail: impl Into<gpui::SharedString>,
    scale: MenuScale,
    colors: &aeris_design_system::ThemeColors,
) -> Div {
    div()
        .h(scale.px(144.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .px_6()
        .child(
            div()
                .text_size(scale.px(13.0))
                .font_weight(platform_font_weight(TypographyRole::Strong))
                .text_color(gpui_color(colors.text_primary))
                .child(title),
        )
        .child(
            div()
                .text_size(scale.px(11.0))
                .text_color(gpui_color(colors.text_muted))
                .child(detail.into()),
        )
}

pub(super) fn chrome_menu_search_header(
    input: &Entity<InputState>,
    theme: &AerisTheme,
    app: &Entity<WorkspaceSurface>,
    hint: impl Into<gpui::SharedString>,
    extent: ChromeMenuExtent,
) -> Div {
    let colors = theme.colors;
    let scale = extent.scale;
    div()
        .h(px(extent.search_height))
        .flex_none()
        .flex()
        .items_center()
        .gap(scale.rems(0.5))
        .px(scale.px(12.0))
        .border_b_1()
        .border_color(gpui_color(colors.border))
        .text_size(scale.rems(0.875))
        .text_color(gpui_color(colors.text_primary))
        .child(header_icon(HugeIcon::Search).with_size(scale.px(CHROME_MENU_SEARCH_ICON_SIZE)))
        .child(
            Input::new(input)
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                .flex_1(),
        )
        .child(
            div()
                .text_size(scale.px(11.0))
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
#[derive(Clone, Copy)]
pub(super) enum ChromeIconButtonTone {
    Neutral,
    Destructive,
}

pub(super) fn chrome_icon_button<F: Fn(&mut Window, &mut App) + 'static>(
    id: &'static str,
    icon: HugeIcon,
    label: &'static str,
    tone: ChromeIconButtonTone,
    theme: &AerisTheme,
    on_activate: F,
) -> Stateful<Div> {
    let colors = theme.colors;
    round_icon_button(id, icon, label)
        .text_color(gpui_color(colors.icon))
        .cursor_pointer()
        .hover(move |button| match tone {
            ChromeIconButtonTone::Neutral => button
                .bg(gpui_color(colors.hover_bg.over(colors.surface)))
                .text_color(gpui_color(colors.text_primary)),
            ChromeIconButtonTone::Destructive => button
                .bg(gpui_color(colors.danger))
                .text_color(gpui_color(colors.danger_foreground)),
        })
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            on_activate(window, cx);
            cx.stop_propagation();
        })
}

pub(super) fn chrome_close_button<F: Fn(&mut Window, &mut App) + 'static>(
    id: &'static str,
    theme: &AerisTheme,
    on_close: F,
) -> impl IntoElement + use<F> {
    chrome_icon_button(
        id,
        HugeIcon::Close,
        "Close",
        ChromeIconButtonTone::Destructive,
        theme,
        on_close,
    )
}

pub(super) fn chrome_menu_close_button(
    app: &Entity<WorkspaceSurface>,
    theme: &AerisTheme,
) -> impl IntoElement + use<> {
    let close_app = app.clone();
    chrome_close_button("chrome_menu_close", theme, move |window, cx| {
        close_app.update(cx, |app, app_cx| {
            app.close_chrome_overlay(window, app_cx);
        });
    })
}

/// Compact add action used at the trailing edge of searchable menu rows.
///
/// Keep this geometry shared between the symbol and indicator pickers: the
/// row action is a 24px (at 1x scale) square with the platform 8px radius, while
/// the larger watchlist header add control remains a circular/full-radius chrome action.
pub(super) fn compact_menu_add_button(
    id: impl Into<gpui::ElementId>,
    scale: MenuScale,
    theme: &AerisTheme,
) -> Button {
    Button::new(id)
        .icon(header_icon(HugeIcon::Add).with_size(scale.px(14.0)))
        .theme(theme)
        .resting_fill(theme.colors.surface)
        .w(scale.px(24.0))
        .h(scale.px(24.0))
        .compact()
        .border_1()
        .border_color(gpui_color(theme.colors.border))
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .cursor_pointer()
        .tab_stop(false)
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
        .h(px(extent.list_height))
        .max_h(px(extent.list_height))
        .rounded_bl(px(f32::from(RadiusToken::Medium.logical_pixels())))
        .rounded_br(px(f32::from(RadiusToken::Medium.logical_pixels())))
        .overflow_hidden()
        .child(
            tracked_overflow_y_scrollbar(body, scroll)
                .w_full()
                .max_h(px(extent.list_height)),
        )
        .child(ThinScrollbar::new(scroll, gpui_color(color)))
}
