use std::rc::Rc;

use axiusflow_design_system::{AxiusflowTheme, RadiusToken, ThemeColor};
use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, Hsla, IntoElement, Pixels, Point, RenderOnce,
    SharedString, Stateful, Window, div, prelude::*, px,
};

type Activation = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

const COMPACT_ROW_HEIGHT: Pixels = px(32.0);
const SEARCH_ROW_HEIGHT: Pixels = px(40.0);
const PANEL_PADDING: Pixels = px(4.0);
const SEPARATOR_HEIGHT: Pixels = px(9.0);

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
    detail: Option<SharedString>,
    leading: Option<AnyElement>,
    trailing: Option<AnyElement>,
    activation: Option<Activation>,
    highlighted: bool,
    disabled: bool,
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
        detail: impl Into<SharedString>,
        theme: &AxiusflowTheme,
    ) -> Self {
        let mut row = Self::new(id, label, theme, RowKind::SearchResult);
        row.detail = Some(detail.into());
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
            detail: None,
            leading: None,
            trailing: None,
            activation: None,
            highlighted: false,
            disabled: false,
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

    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.activation = Some(Rc::new(handler));
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
        let label = match self.detail {
            Some(detail) => div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme_color(colors.text_primary))
                        .truncate()
                        .child(self.label.clone()),
                )
                .child(
                    div()
                        .min_w_0()
                        .text_size(px(11.0))
                        .text_color(theme_color(colors.text_muted))
                        .truncate()
                        .child(detail),
                ),
            None => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(self.label.clone()),
        };

        div()
            .id(self.id)
            .occlude()
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
            .when(self.highlighted, |row| {
                row.bg(theme_color(highlighted_fill))
                    .text_color(theme_color(colors.text_primary))
            })
            .when(enabled, |row| {
                row.cursor_pointer().hover(|row| {
                    row.bg(theme_color(colors.hover_bg.over(self.resting_fill)))
                        .text_color(theme_color(colors.text_primary))
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
        .py(PANEL_PADDING)
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
