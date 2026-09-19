use gpui::{
    App, Hsla, IntoElement, Pixels, RenderOnce, SharedString, StyleRefinement, Styled,
    Transformation, Window, div, prelude::*, svg,
};

/// An `TradingPlot`-owned SVG icon loaded through the application's `AssetSource`.
///
/// Keeping the path as data makes icon rendering independent of a UI toolkit
/// library and lets the asset source remain the single authority for bundled
/// glyphs.
#[derive(Default, IntoElement)]
pub(crate) struct Icon {
    path: SharedString,
    style: StyleRefinement,
    color: Option<Hsla>,
    size: Option<Pixels>,
    rotation_turns: Option<f32>,
}

impl Clone for Icon {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            style: self.style.clone(),
            color: self.color,
            size: self.size,
            rotation_turns: self.rotation_turns,
        }
    }
}

impl Icon {
    pub(crate) fn new(path: impl Into<SharedString>) -> Self {
        Self::default().path(path)
    }

    pub(crate) fn path(mut self, path: impl Into<SharedString>) -> Self {
        self.path = path.into();
        self
    }

    pub(crate) fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    pub(crate) fn with_size(mut self, size: Pixels) -> Self {
        self.size = Some(size);
        self
    }

    pub(crate) fn small(self) -> Self {
        self.with_size(gpui::px(14.0))
    }

    pub(crate) fn rotate(mut self, turns: f32) -> Self {
        self.rotation_turns = Some(turns);
        self
    }

    #[cfg(test)]
    pub(crate) fn path_ref(&self) -> &SharedString {
        &self.path
    }
}

impl Styled for Icon {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let size = self
            .size
            .unwrap_or_else(|| window.text_style().font_size.to_pixels(window.rem_size()));
        let color = self.color.unwrap_or_else(|| window.text_style().color);
        let mut glyph = svg().path(self.path).flex_none().flex_shrink_0();
        *glyph.style() = self.style;
        let glyph = glyph.size(size).text_color(color);
        let glyph = if let Some(turns) = self.rotation_turns {
            glyph
                .with_transformation(Transformation::rotate(gpui::percentage(turns)))
                .into_any_element()
        } else {
            glyph.into_any_element()
        };
        // GPUI SVG participates in text layout and can sit on the baseline with
        // extra descent, which makes compact close glyphs look high in the hit.
        div()
            .size(size)
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .child(glyph)
    }
}

#[cfg(test)]
mod tests {
    use super::Icon;

    #[test]
    fn icon_keeps_the_owned_asset_path() {
        let icon = Icon::new("tradingplot/icons/ui/window-close.svg");
        assert_eq!(
            icon.path_ref().as_ref(),
            "tradingplot/icons/ui/window-close.svg"
        );
    }
}
