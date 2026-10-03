use gpui::{
    App, Hsla, IntoElement, Pixels, RenderOnce, SharedString, StyleRefinement, Styled,
    Transformation, Window, div, prelude::*, svg,
};

/// An `Aeris`-owned SVG icon loaded through the application's `AssetSource`.
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

/// Rounds a glyph box to whole device pixels. A rasterized glyph whose box ends between
/// device pixels resamples every edge, so thin strokes and bars smear unevenly.
pub(crate) fn device_pixel_size(size: Pixels, scale_factor: f32) -> Pixels {
    let scale = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    gpui::px((f32::from(size) * scale).round().max(1.0) / scale)
}

impl Styled for Icon {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let size = device_pixel_size(
            self.size
                .unwrap_or_else(|| window.text_style().font_size.to_pixels(window.rem_size())),
            window.scale_factor(),
        );
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
    use super::{Icon, device_pixel_size};
    use gpui::px;

    #[test]
    fn glyph_sizes_snap_to_whole_device_pixels() {
        assert_eq!(device_pixel_size(px(18.0), 1.0), px(18.0));
        assert_eq!(device_pixel_size(px(14.4), 1.0), px(14.0));
        assert_eq!(device_pixel_size(px(26.67), 1.0), px(27.0));
        assert_eq!(device_pixel_size(px(12.2), 1.5), px(12.0));
        assert_eq!(device_pixel_size(px(14.4), 2.0), px(14.5));
        assert_eq!(device_pixel_size(px(0.2), 1.0), px(1.0));
        assert_eq!(device_pixel_size(px(18.0), 0.0), px(18.0));
    }

    #[test]
    fn icon_keeps_the_owned_asset_path() {
        let icon = Icon::new("aeris/icons/ui/window-close.svg");
        assert_eq!(icon.path_ref().as_ref(), "aeris/icons/ui/window-close.svg");
    }
}
