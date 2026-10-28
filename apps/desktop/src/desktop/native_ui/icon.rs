use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

use gpui::{
    App, AssetSource, Hsla, ImageSource, IntoElement, ObjectFit, Pixels, RenderImage, RenderOnce,
    SharedString, StyleRefinement, Styled, Transformation, Window, div, img, prelude::*, svg,
};

/// Exact-alpha rasters per (path, device size, color). Icons come from a fixed asset set and
/// colors from theme tokens, so the set is small; the cap only guards against a runaway.
const MAXIMUM_EXACT_RASTERS: usize = 512;

/// Icon path, device size bits, and the RGB channel bits it was painted with.
type ExactRasterKey = (SharedString, u32, [u32; 3]);

static EXACT_RASTERS: LazyLock<Mutex<HashMap<ExactRasterKey, Arc<RenderImage>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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
    group_hover_color: Option<(SharedString, Hsla)>,
    size: Option<Pixels>,
    rotation_turns: Option<f32>,
    exact_alpha: bool,
}

impl Clone for Icon {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            style: self.style.clone(),
            color: self.color,
            group_hover_color: self.group_hover_color.clone(),
            size: self.size,
            rotation_turns: self.rotation_turns,
            exact_alpha: self.exact_alpha,
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

    /// Repaints the glyph while the pointer is over `group`, such as a menu row whose hover
    /// fill needs a contrasting glyph. Exact-alpha rasters keep their resting colour.
    pub(crate) fn group_hover_color(mut self, group: impl Into<SharedString>, color: Hsla) -> Self {
        self.group_hover_color = Some((group.into(), color));
        self
    }

    pub(crate) fn with_size(mut self, size: Pixels) -> Self {
        self.size = Some(size);
        self
    }

    /// Applies `size` unless the glyph already carries its own optical size.
    pub(crate) fn or_size(self, size: Pixels) -> Self {
        if self.size.is_some() {
            self
        } else {
            self.with_size(size)
        }
    }

    pub(crate) fn small(self) -> Self {
        self.with_size(gpui::px(14.0))
    }

    pub(crate) fn rotate(mut self, turns: f32) -> Self {
        self.rotation_turns = Some(turns);
        self
    }

    /// Paints the icon from an exact-color raster instead of GPUI's monochrome mask. GPUI runs
    /// monochrome masks through the text contrast and gamma correction, which brightens
    /// partial alpha, so an icon whose design uses a translucent fill opts into this.
    pub(crate) fn exact_alpha(mut self, exact_alpha: bool) -> Self {
        self.exact_alpha = exact_alpha;
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

/// The SVG root's `width` attribute, the unit its raster scale is measured against.
pub(crate) fn svg_intrinsic_width(bytes: &[u8]) -> Option<f32> {
    let header = std::str::from_utf8(bytes.get(..bytes.len().min(768))?).ok()?;
    let svg = header.find("<svg")?;
    let width = header[svg..].find("width=\"")? + svg + 7;
    let rest = &header[width..];
    let end = rest.find('"')?;
    rest[..end].parse().ok()
}

/// The icon's SVG with `currentColor` resolved, rasterized at its device-pixel size. The
/// color's alpha is left to the caller, so one raster serves every opacity.
fn exact_raster(
    path: &SharedString,
    size: Pixels,
    scale_factor: f32,
    color: Hsla,
    cx: &App,
) -> Option<Arc<RenderImage>> {
    let rgba = color.to_rgb();
    let [red, green, blue] = [rgba.r, rgba.g, rgba.b].map(|channel| channel.clamp(0.0, 1.0));
    let device = f32::from(size) * scale_factor.max(1.0);
    let key = (
        path.clone(),
        device.to_bits(),
        [red.to_bits(), green.to_bits(), blue.to_bits()],
    );
    if let Some(image) = EXACT_RASTERS
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).cloned())
    {
        return Some(image);
    }
    let bytes = crate::desktop::assets::AerisAssets
        .load(path.as_ref())
        .ok()??;
    let intrinsic = svg_intrinsic_width(&bytes)?;
    let source = std::str::from_utf8(&bytes).ok()?.replace(
        "currentColor",
        &format!(
            "rgb({:.3}%, {:.3}%, {:.3}%)",
            red * 100.0,
            green * 100.0,
            blue * 100.0
        ),
    );
    let image = cx
        .svg_renderer()
        .render_single_frame(source.as_bytes(), device / intrinsic)
        .ok()?;
    if let Ok(mut cache) = EXACT_RASTERS.lock() {
        if cache.len() >= MAXIMUM_EXACT_RASTERS {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&image));
    }
    Some(image)
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let size = device_pixel_size(
            self.size
                .unwrap_or_else(|| window.text_style().font_size.to_pixels(window.rem_size())),
            window.scale_factor(),
        );
        let color = self.color.unwrap_or_else(|| window.text_style().color);
        if self.exact_alpha
            && let Some(image) = exact_raster(&self.path, size, window.scale_factor(), color, cx)
        {
            return div()
                .size(size)
                .flex_none()
                .child(
                    img(ImageSource::Render(image))
                        .size(size)
                        .object_fit(ObjectFit::Fill)
                        .opacity(color.a),
                )
                .into_any_element();
        }
        let mut glyph = svg().path(self.path).flex_none().flex_shrink_0();
        *glyph.style() = self.style;
        let glyph = glyph.size(size).text_color(color).when_some(
            self.group_hover_color,
            |glyph, (group, hover_color)| {
                glyph.group_hover(group, move |style| style.text_color(hover_color))
            },
        );
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
            .into_any_element()
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
