use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

const DRAWING_ASSET_PREFIX: &str = "axiusflow/icons/drawing/";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrawingIcon {
    TrendLine,
    HorizontalLine,
    VerticalLine,
    Ray,
    Rectangle,
    Ruler,
    Fibonacci,
    ExtendedLine,
}

impl DrawingIcon {
    pub const ALL: [Self; 8] = [
        Self::TrendLine,
        Self::HorizontalLine,
        Self::VerticalLine,
        Self::Ray,
        Self::Rectangle,
        Self::Ruler,
        Self::Fibonacci,
        Self::ExtendedLine,
    ];

    #[must_use]
    pub fn path(self) -> SharedString {
        match self {
            Self::TrendLine => concat!("axiusflow/icons/drawing/", "trend-line.svg"),
            Self::HorizontalLine => {
                concat!("axiusflow/icons/drawing/", "horizontal-line.svg")
            }
            Self::VerticalLine => concat!("axiusflow/icons/drawing/", "vertical-line.svg"),
            Self::Ray => concat!("axiusflow/icons/drawing/", "ray.svg"),
            Self::Rectangle => concat!("axiusflow/icons/drawing/", "rectangle.svg"),
            Self::Ruler => concat!("axiusflow/icons/drawing/", "ruler.svg"),
            Self::Fibonacci => concat!("axiusflow/icons/drawing/", "fibonacci.svg"),
            Self::ExtendedLine => concat!("axiusflow/icons/drawing/", "extended-line.svg"),
        }
        .into()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AxiusflowAssets;

impl AssetSource for AxiusflowAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(drawing_asset(path).map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(DrawingIcon::ALL
            .into_iter()
            .map(DrawingIcon::path)
            .filter(|asset| path.is_empty() || asset.starts_with(path))
            .collect())
    }
}

fn drawing_asset(path: &str) -> Option<&'static [u8]> {
    Some(match path.strip_prefix(DRAWING_ASSET_PREFIX)? {
        "trend-line.svg" => include_bytes!("../assets/icons/drawing/trend-line.svg"),
        "horizontal-line.svg" => include_bytes!("../assets/icons/drawing/horizontal-line.svg"),
        "vertical-line.svg" => include_bytes!("../assets/icons/drawing/vertical-line.svg"),
        "ray.svg" => include_bytes!("../assets/icons/drawing/ray.svg"),
        "rectangle.svg" => include_bytes!("../assets/icons/drawing/rectangle.svg"),
        "ruler.svg" => include_bytes!("../assets/icons/drawing/ruler.svg"),
        "fibonacci.svg" => include_bytes!("../assets/icons/drawing/fibonacci.svg"),
        "extended-line.svg" => include_bytes!("../assets/icons/drawing/extended-line.svg"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_drawing_icon_has_a_unique_embedded_asset() {
        let assets = AxiusflowAssets;
        let mut paths = HashSet::new();

        for icon in DrawingIcon::ALL {
            let path = icon.path();
            assert!(paths.insert(path.clone()), "duplicate asset path: {path}");
            let bytes = assets
                .load(path.as_ref())
                .expect("asset lookup")
                .expect("embedded drawing icon");
            assert!(!bytes.is_empty(), "empty asset: {path}");
        }
    }

    #[test]
    fn drawing_icons_are_theme_neutral_svg() {
        let assets = AxiusflowAssets;

        for icon in DrawingIcon::ALL {
            let path = icon.path();
            let bytes = assets
                .load(path.as_ref())
                .expect("asset lookup")
                .expect("embedded drawing icon");
            let svg = std::str::from_utf8(&bytes).expect("UTF-8 SVG");
            let normalized = svg.to_ascii_lowercase();

            assert!(
                svg.contains("viewBox=\"0 0 28 28\""),
                "invalid viewBox: {path}"
            );
            assert!(svg.contains("currentColor"), "missing currentColor: {path}");
            assert!(!normalized.contains('#'), "hard-coded hex color: {path}");
            assert!(!normalized.contains("rgb("), "hard-coded RGB color: {path}");
        }
    }

    #[test]
    fn list_filters_the_asset_namespace() {
        let assets = AxiusflowAssets;
        assert_eq!(
            assets.list("").expect("all assets").len(),
            DrawingIcon::ALL.len()
        );
        assert_eq!(
            assets
                .list("axiusflow/icons/drawing/trend")
                .expect("filtered assets"),
            vec![DrawingIcon::TrendLine.path()]
        );
        assert!(
            assets
                .list("hugeicons/")
                .expect("foreign namespace")
                .is_empty()
        );
        assert!(
            assets
                .load("unknown.svg")
                .expect("unknown lookup")
                .is_none()
        );
    }
}
