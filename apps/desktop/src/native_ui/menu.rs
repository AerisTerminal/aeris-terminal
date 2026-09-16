use std::rc::Rc;

use axiusflow_design_system::{
    AxiusflowTheme, RadiusToken, ThemeColor, TypographyRole, platform_font_family,
};
use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, IntoElement, Pixels, Point, RenderOnce,
    SharedString, Stateful, Window, div, prelude::*, px,
};

use super::{
    platform_font_weight,
    theme::{gpui_color, platform_border_width},
};

type Activation = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type Hover = Rc<dyn Fn(&bool, &mut Window, &mut App)>;

const COMPACT_ROW_HEIGHT: Pixels = px(32.0);
const SEARCH_ROW_HEIGHT: Pixels = px(36.0);
const SEPARATOR_HEIGHT: Pixels = px(1.0);

#[derive(Clone, Copy)]
enum RowKind {
    Compact,
    CompactInset,
    SearchResult,
}

fn row_geometry(kind: RowKind) -> (Pixels, Pixels, bool) {
    match kind {
        RowKind::Compact => (COMPACT_ROW_HEIGHT, px(12.0), false),
        RowKind::CompactInset => (COMPACT_ROW_HEIGHT, px(8.0), true),
        RowKind::SearchResult => (SEARCH_ROW_HEIGHT, px(8.0), true),
    }
}

const fn accepts_input(disabled: bool, has_activation: bool) -> bool {
    !disabled && has_activation
}

/// Axiusflow's shared selectable row for compact menus and search results.
#[derive(IntoElement)]
pub(crate) struct MenuRow {
    id: ElementId,
    kind: RowKind,
    theme: AxiusflowTheme,
    resting_fill: ThemeColor,
    label: SharedString,
    leading: Option<AnyElement>,
    trailing: Option<AnyElement>,
    activation: Option<Activation>,
    hover: Option<Hover>,
    behavior: MenuRowBehavior,
    edges: MenuRowEdges,
}

#[derive(Default)]
struct MenuRowBehavior {
    highlighted: bool,
    disabled: bool,
    destructive: bool,
}

#[derive(Default)]
struct MenuRowEdges {
    round_top: bool,
    round_bottom: bool,
    fill_width: bool,
}

impl MenuRow {
    pub(crate) fn compact(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        theme: &AxiusflowTheme,
    ) -> Self {
        Self::new(id, label, theme, RowKind::Compact)
    }

    pub(crate) fn search_result(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        theme: &AxiusflowTheme,
    ) -> Self {
        let mut row = Self::new(id, label, theme, RowKind::SearchResult);
        row.resting_fill = theme.colors.surface;
        row
    }

    pub(crate) fn compact_inset(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        theme: &AxiusflowTheme,
    ) -> Self {
        Self::new(id, label, theme, RowKind::CompactInset)
    }

    fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        theme: &AxiusflowTheme,
        kind: RowKind,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            theme: *theme,
            resting_fill: theme.colors.surface_secondary,
            label: label.into(),
            leading: None,
            trailing: None,
            activation: None,
            hover: None,
            behavior: MenuRowBehavior::default(),
            edges: MenuRowEdges::default(),
        }
    }

    pub(crate) fn resting_fill(mut self, fill: ThemeColor) -> Self {
        self.resting_fill = fill;
        self
    }

    pub(crate) fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }

    pub(crate) fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }

    pub(crate) fn highlighted(mut self, highlighted: bool) -> Self {
        self.behavior.highlighted = highlighted;
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.behavior.disabled = disabled;
        self
    }

    pub(crate) fn destructive(mut self, destructive: bool) -> Self {
        self.behavior.destructive = destructive;
        self
    }

    pub(crate) fn round_panel_ends(mut self, top: bool, bottom: bool) -> Self {
        self.edges.round_top = top;
        self.edges.round_bottom = bottom;
        self
    }

    pub(crate) fn fill_width(mut self) -> Self {
        self.edges.fill_width = true;
        self
    }

    pub(crate) fn flush_in_panel(self, first: bool, last: bool) -> Self {
        self.fill_width().round_panel_ends(first, last)
    }

    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.activation = Some(Rc::new(handler));
        self
    }

    pub(crate) fn on_hover(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.hover = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for MenuRow {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let colors = self.theme.colors;
        let enabled = accepts_input(self.behavior.disabled, self.activation.is_some());
        let destructive = self.behavior.destructive;
        let label_color = if destructive {
            if enabled {
                colors.danger
            } else {
                colors.danger.with_alpha(0.55)
            }
        } else if enabled {
            colors.text_primary
        } else {
            colors.text_muted
        };
        let highlighted_fill = if destructive {
            colors.danger.with_alpha(0.10).over(self.resting_fill)
        } else {
            match self.kind {
                RowKind::Compact | RowKind::CompactInset => colors.hover_bg,
                RowKind::SearchResult => colors.active_bg,
            }
            .over(self.resting_fill)
        };
        let hover_fill = if destructive {
            colors.danger.with_alpha(0.10).over(self.resting_fill)
        } else {
            colors.hover_bg.over(self.resting_fill)
        };
        let (height, horizontal_padding, rounded) = row_geometry(self.kind);
        let hover = self.hover;
        // Inner path of a 6px panel with a 1px border. Matching the outer radius
        // on the content box pulls the hover off the corners and leaves gaps.
        let inner_radius = px((f32::from(RadiusToken::Default.logical_pixels()) - 1.0).max(0.0));
        let round_top = self.edges.round_top;
        let round_bottom = self.edges.round_bottom;
        let fill_width = self.edges.fill_width;
        let label = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(gpui_color(label_color))
            .child(self.label.clone());

        div()
            .id(self.id)
            .block_mouse_except_scroll()
            .when_some(hover, |row, hover| {
                row.on_hover(move |hovered, window, cx| hover(hovered, window, cx))
            })
            .when(fill_width, gpui::Styled::w_full)
            .h(height)
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px(horizontal_padding)
            .font_family(platform_font_family())
            .font_weight(platform_font_weight(TypographyRole::Normal))
            .text_sm()
            .text_color(gpui_color(label_color))
            .when(rounded, |row| {
                row.rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
            })
            .when(round_top, |row| {
                row.rounded_tl(inner_radius).rounded_tr(inner_radius)
            })
            .when(round_bottom, |row| {
                row.rounded_bl(inner_radius).rounded_br(inner_radius)
            })
            .when(self.behavior.highlighted, |row| {
                row.bg(gpui_color(highlighted_fill))
                    .text_color(gpui_color(label_color))
                    .when(round_top, |row| {
                        row.rounded_tl(inner_radius).rounded_tr(inner_radius)
                    })
                    .when(round_bottom, |row| {
                        row.rounded_bl(inner_radius).rounded_br(inner_radius)
                    })
            })
            .when(enabled, |row| {
                row.cursor_pointer().hover(|style| {
                    let mut style = style
                        .bg(gpui_color(hover_fill))
                        .text_color(gpui_color(label_color));
                    if round_top {
                        style = style.rounded_tl(inner_radius).rounded_tr(inner_radius);
                    }
                    if round_bottom {
                        style = style.rounded_bl(inner_radius).rounded_br(inner_radius);
                    }
                    style
                })
            })
            .when(!enabled, gpui::Styled::cursor_not_allowed)
            .when_some(self.activation.filter(|_| enabled), |row, activation| {
                row.on_click(move |event, window, cx| {
                    activation(event, window, cx);
                    cx.stop_propagation();
                })
            })
            .children(self.leading)
            .child(label)
            .children(self.trailing)
    }
}

/// Flush compact dropdown surface: 1px rounded border, no panel padding.
/// Rows use [`MenuRow::compact`] plus [`MenuRow::flush_in_panel`]; do not wrap
/// this panel in `py`/`px` or `overflow_hidden` (that clips the border).
fn compact_menu_panel_with_elevation(
    id: impl Into<ElementId>,
    origin: Point<Pixels>,
    width: Pixels,
    theme: &AxiusflowTheme,
    elevated: bool,
) -> Stateful<Div> {
    let colors = theme.colors;
    div()
        .id(id)
        .absolute()
        .left(origin.x)
        .top(origin.y)
        .w(width)
        .occlude()
        .rounded(px(f32::from(RadiusToken::Default.logical_pixels())))
        .border(platform_border_width(theme))
        .border_color(gpui_color(colors.border_secondary))
        .bg(gpui_color(colors.surface_secondary))
        .when(elevated, gpui::Styled::shadow_md)
        .font_family(platform_font_family())
        .font_weight(platform_font_weight(TypographyRole::Normal))
        .text_color(gpui_color(colors.text_primary))
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
}

pub(crate) fn compact_menu_panel(
    id: impl Into<ElementId>,
    origin: Point<Pixels>,
    width: Pixels,
    theme: &AxiusflowTheme,
) -> Stateful<Div> {
    compact_menu_panel_with_elevation(id, origin, width, theme, true)
}

/// Flat compact dropdown surface used by chart-native menus that sit directly
/// against chart chrome. The shared geometry remains identical to elevated
/// menus; only the box shadow is omitted.
pub(crate) fn flat_compact_menu_panel(
    id: impl Into<ElementId>,
    origin: Point<Pixels>,
    width: Pixels,
    theme: &AxiusflowTheme,
) -> Stateful<Div> {
    compact_menu_panel_with_elevation(id, origin, width, theme, false)
}

pub(crate) fn menu_separator(theme: &AxiusflowTheme) -> Div {
    div().h(SEPARATOR_HEIGHT).flex().items_center().child(
        div()
            .h_px()
            .w_full()
            .bg(gpui_color(theme.colors.border_secondary)),
    )
}

#[cfg(test)]
mod tests {
    use gpui::px;

    use super::{COMPACT_ROW_HEIGHT, RowKind, SEARCH_ROW_HEIGHT, accepts_input, row_geometry};

    #[test]
    fn disabled_rows_never_accept_activation() {
        assert!(accepts_input(false, true));
        assert!(!accepts_input(true, true));
        assert!(!accepts_input(false, false));
    }

    #[test]
    fn row_kinds_preserve_menu_geometry() {
        assert_eq!(
            row_geometry(RowKind::Compact),
            (COMPACT_ROW_HEIGHT, px(12.0), false)
        );
        assert_eq!(
            row_geometry(RowKind::CompactInset),
            (COMPACT_ROW_HEIGHT, px(8.0), true)
        );
        assert_eq!(
            row_geometry(RowKind::SearchResult),
            (SEARCH_ROW_HEIGHT, px(8.0), true)
        );
    }
}
