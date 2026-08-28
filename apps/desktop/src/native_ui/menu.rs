use std::rc::Rc;

use axiusflow_design_system::{AxiusflowTheme, RadiusToken, ThemeColor};
use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, Hsla, IntoElement, Pixels, Point, RenderOnce,
    SharedString, Stateful, Window, div, prelude::*, px,
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
    highlighted: bool,
    disabled: bool,
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
            highlighted: false,
            disabled: false,
            round_top: false,
            round_bottom: false,
            fill_width: false,
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
        self.highlighted = highlighted;
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(crate) fn round_panel_ends(mut self, top: bool, bottom: bool) -> Self {
        self.round_top = top;
        self.round_bottom = bottom;
        self
    }

    pub(crate) fn fill_width(mut self) -> Self {
        self.fill_width = true;
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
        let enabled = accepts_input(self.disabled, self.activation.is_some());
        let highlighted_fill = match self.kind {
            RowKind::Compact | RowKind::CompactInset => colors.hover_bg,
            RowKind::SearchResult => colors.active_bg,
        }
        .over(self.resting_fill);
        let (height, horizontal_padding, rounded) = row_geometry(self.kind);
        let hover = self.hover;
        // Inner path of a 6px panel with a 1px border. Matching the outer radius
        // on the content box pulls the hover off the corners and leaves gaps.
        let inner_radius = px((f32::from(RadiusToken::Default.logical_pixels()) - 1.0).max(0.0));
        let round_top = self.round_top;
        let round_bottom = self.round_bottom;
        let fill_width = self.fill_width;
        let label = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .child(self.label.clone());

        div()
            .id(self.id)
            .block_mouse_except_scroll()
            .when_some(hover, |row, hover| {
                row.on_hover(move |hovered, window, cx| hover(hovered, window, cx))
            })
            .when(fill_width, |row| row.w_full())
            .h(height)
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px(horizontal_padding)
            .text_sm()
            .when(rounded, |row| {
                row.rounded(px(f32::from(RadiusToken::Sm.logical_pixels())))
            })
            .when(round_top, |row| {
                row.rounded_tl(inner_radius).rounded_tr(inner_radius)
            })
            .when(round_bottom, |row| {
                row.rounded_bl(inner_radius).rounded_br(inner_radius)
            })
            .when(self.highlighted, |row| {
                row.bg(theme_color(highlighted_fill))
                    .text_color(theme_color(colors.text_primary))
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
                        .bg(theme_color(colors.hover_bg.over(self.resting_fill)))
                        .text_color(theme_color(colors.text_primary));
                    if round_top {
                        style = style.rounded_tl(inner_radius).rounded_tr(inner_radius);
                    }
                    if round_bottom {
                        style = style.rounded_bl(inner_radius).rounded_br(inner_radius);
                    }
                    style
                })
            })
            .when(!enabled, |row| {
                row.text_color(theme_color(colors.text_muted))
                    .cursor_not_allowed()
            })
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
pub(crate) fn compact_menu_panel(
    id: impl Into<ElementId>,
    origin: Point<Pixels>,
    width: Pixels,
    theme: &AxiusflowTheme,
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
        .border_1()
        .border_color(theme_color(colors.border_secondary))
        .bg(theme_color(colors.surface_secondary))
        .text_color(theme_color(colors.text_primary))
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
}

pub(crate) fn menu_separator(theme: &AxiusflowTheme) -> Div {
    div().h(SEPARATOR_HEIGHT).flex().items_center().child(
        div()
            .h_px()
            .w_full()
            .bg(theme_color(theme.colors.border_secondary)),
    )
}

fn theme_color(color: ThemeColor) -> Hsla {
    let (h, s, l, a) = color.hsla_components();
    Hsla { h, s, l, a }
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
