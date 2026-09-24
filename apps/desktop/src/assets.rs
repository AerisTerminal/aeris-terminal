use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

const DRAWING_ASSET_PREFIX: &str = "aeris/icons/drawing/";
const UI_ASSET_PREFIX: &str = "aeris/icons/ui/";
const SERIES_ASSET_PREFIX: &str = "aeris/icons/series/";
const BRAND_ASSET_PREFIX: &str = "aeris/brand/";
const EXCHANGE_ASSET_PREFIX: &str = "aeris/exchange_logo/";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiIcon {
    Add,
    ArrowLeft,
    ArrowRightDouble,
    ArrowRight,
    Close,
    Chart,
    CheckIcon,
    ChevronDown,
    Copy,
    CopySuccess,
    Delete,
    EraserIcon,
    Failure,
    LockKeyholeIcon,
    Moon,
    Redo,
    Refresh,
    Search,
    Settings,
    SidebarLeft,
    SidebarRight,
    SplitSideBySide,
    SplitStacked,
    Sun,
    Undo,
    View,
    ViewOff,
    WindowClose,
    WindowMaximize,
    WindowMinimize,
    WindowRestore,
    Loader,
    Info,
    SignOut,
    User,
}

impl UiIcon {
    pub const ALL: [Self; 35] = [
        Self::Add,
        Self::ArrowLeft,
        Self::ArrowRightDouble,
        Self::ArrowRight,
        Self::Close,
        Self::Chart,
        Self::CheckIcon,
        Self::ChevronDown,
        Self::Copy,
        Self::CopySuccess,
        Self::Delete,
        Self::EraserIcon,
        Self::Failure,
        Self::LockKeyholeIcon,
        Self::Moon,
        Self::Redo,
        Self::Refresh,
        Self::Search,
        Self::Settings,
        Self::SidebarLeft,
        Self::SidebarRight,
        Self::SplitSideBySide,
        Self::SplitStacked,
        Self::Sun,
        Self::Undo,
        Self::View,
        Self::ViewOff,
        Self::WindowClose,
        Self::WindowMaximize,
        Self::WindowMinimize,
        Self::WindowRestore,
        Self::Loader,
        Self::Info,
        Self::SignOut,
        Self::User,
    ];

    #[must_use]
    pub fn path(self) -> SharedString {
        let name = match self {
            Self::Add => "add.svg",
            Self::ArrowLeft => "arrow-left.svg",
            Self::ArrowRightDouble => "arrow-right-double.svg",
            Self::ArrowRight => "arrow-right.svg",
            Self::Close => "close.svg",
            Self::Chart => "chart.svg",
            Self::CheckIcon => "check.svg",
            Self::ChevronDown => "chevron-down.svg",
            Self::Copy => "copy.svg",
            Self::CopySuccess => "copy-success.svg",
            Self::Delete => "delete.svg",
            Self::EraserIcon => "eraser.svg",
            Self::Failure => "failure.svg",
            Self::LockKeyholeIcon => "lock-keyhole.svg",
            Self::Moon => "moon.svg",
            Self::Redo => "redo.svg",
            Self::Refresh => "refresh.svg",
            Self::Search => "search.svg",
            Self::Settings => "settings.svg",
            Self::SidebarLeft => "sidebar-left.svg",
            Self::SidebarRight => "sidebar-right.svg",
            Self::SplitSideBySide => "split-side-by-side.svg",
            Self::SplitStacked => "split-stacked.svg",
            Self::Sun => "sun.svg",
            Self::Undo => "undo.svg",
            Self::View => "view.svg",
            Self::ViewOff => "view-off.svg",
            Self::WindowClose => "window-close.svg",
            Self::WindowMaximize => "window-maximize.svg",
            Self::WindowMinimize => "window-minimize.svg",
            Self::WindowRestore => "window-restore.svg",
            Self::Loader => "loader.svg",
            Self::Info => "info.svg",
            Self::SignOut => "signout.svg",
            Self::User => "user.svg",
        };
        format!("{UI_ASSET_PREFIX}{name}").into()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DrawingIcon {
    TrendLine,
    HorizontalLine,
    VerticalLine,
    Ray,
    Rectangle,
    Path,
    Cursor,
    Brush,
    Text,
}

impl DrawingIcon {
    pub const ALL: [Self; 9] = [
        Self::TrendLine,
        Self::HorizontalLine,
        Self::VerticalLine,
        Self::Ray,
        Self::Rectangle,
        Self::Path,
        Self::Cursor,
        Self::Brush,
        Self::Text,
    ];
    #[cfg(test)]
    pub const GEOMETRIC: [Self; 6] = [
        Self::TrendLine,
        Self::HorizontalLine,
        Self::VerticalLine,
        Self::Ray,
        Self::Rectangle,
        Self::Path,
    ];

    #[must_use]
    pub fn path(self) -> SharedString {
        match self {
            Self::TrendLine => concat!("aeris/icons/drawing/", "trend-line.svg"),
            Self::HorizontalLine => {
                concat!("aeris/icons/drawing/", "horizontal-line.svg")
            }
            Self::VerticalLine => concat!("aeris/icons/drawing/", "vertical-line.svg"),
            Self::Ray => concat!("aeris/icons/drawing/", "horizontal-ray.svg"),
            Self::Rectangle => concat!("aeris/icons/drawing/", "rectangle.svg"),
            Self::Path => concat!("aeris/icons/drawing/", "path.svg"),
            Self::Cursor => concat!("aeris/icons/drawing/", "cursor.svg"),
            Self::Brush => concat!("aeris/icons/drawing/", "brush.svg"),
            Self::Text => concat!("aeris/icons/drawing/", "text.svg"),
        }
        .into()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeriesIcon {
    Candlestick,
    OhlcBar,
    Line,
    Area,
    HeikinAshi,
    BrushableArea,
}

impl SeriesIcon {
    pub const ALL: [Self; 6] = [
        Self::Candlestick,
        Self::OhlcBar,
        Self::Line,
        Self::Area,
        Self::HeikinAshi,
        Self::BrushableArea,
    ];

    #[must_use]
    pub fn path(self) -> SharedString {
        let name = match self {
            Self::Candlestick => "candlestick-chart.svg",
            Self::OhlcBar => "ohlc-bar-chart.svg",
            Self::Line => "line-chart-type.svg",
            Self::Area => "area-chart-type.svg",
            Self::HeikinAshi => "heikin-ashi-chart.svg",
            Self::BrushableArea => "brushable-area-chart.svg",
        };
        format!("{SERIES_ASSET_PREFIX}{name}").into()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrandIcon {
    Mark,
}

impl BrandIcon {
    pub const ALL: [Self; 1] = [Self::Mark];

    #[must_use]
    pub fn path(self) -> SharedString {
        let name = match self {
            Self::Mark => "logo.svg",
        };
        format!("{BRAND_ASSET_PREFIX}{name}").into()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExchangeLogo {
    Binance,
    #[default]
    Rithmic,
    Hyperliquid,
}

impl ExchangeLogo {
    pub const ALL: [Self; 3] = [Self::Rithmic, Self::Hyperliquid, Self::Binance];

    #[must_use]
    pub fn path(self) -> SharedString {
        let name = match self {
            Self::Binance => "binance.svg",
            Self::Rithmic => "rithmic.svg",
            Self::Hyperliquid => "hyperliquid.svg",
        };
        format!("{EXCHANGE_ASSET_PREFIX}{name}").into()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AerisAssets;

impl AssetSource for AerisAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(drawing_asset(path)
            .or_else(|| ui_asset(path))
            .or_else(|| series_asset(path))
            .or_else(|| brand_asset(path))
            .or_else(|| exchange_asset(path))
            .map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(DrawingIcon::ALL
            .into_iter()
            .map(DrawingIcon::path)
            .chain(UiIcon::ALL.into_iter().map(UiIcon::path))
            .chain(SeriesIcon::ALL.into_iter().map(SeriesIcon::path))
            .chain(BrandIcon::ALL.into_iter().map(BrandIcon::path))
            .chain(ExchangeLogo::ALL.into_iter().map(ExchangeLogo::path))
            .filter(|asset| path.is_empty() || asset.starts_with(path))
            .collect())
    }
}

fn drawing_asset(path: &str) -> Option<&'static [u8]> {
    Some(match path.strip_prefix(DRAWING_ASSET_PREFIX)? {
        "trend-line.svg" => include_bytes!("../assets/icons/drawing/trend-line.svg"),
        "horizontal-line.svg" => include_bytes!("../assets/icons/drawing/horizontal-line.svg"),
        "vertical-line.svg" => include_bytes!("../assets/icons/drawing/vertical-line.svg"),
        "horizontal-ray.svg" => include_bytes!("../assets/icons/drawing/horizontal-ray.svg"),
        "rectangle.svg" => include_bytes!("../assets/icons/drawing/rectangle.svg"),
        "path.svg" => include_bytes!("../assets/icons/drawing/path.svg"),
        "cursor.svg" => include_bytes!("../assets/icons/drawing/cursor.svg"),
        "brush.svg" => include_bytes!("../assets/icons/drawing/brush.svg"),
        "text.svg" => include_bytes!("../assets/icons/drawing/text.svg"),
        _ => return None,
    })
}

fn ui_asset(path: &str) -> Option<&'static [u8]> {
    Some(match path.strip_prefix(UI_ASSET_PREFIX)? {
        "add.svg" => include_bytes!("../assets/icons/ui/add.svg"),
        "arrow-left.svg" => include_bytes!("../assets/icons/ui/arrow-left.svg"),
        "arrow-right-double.svg" => {
            include_bytes!("../assets/icons/ui/arrow-right-double.svg")
        }
        "arrow-right.svg" => include_bytes!("../assets/icons/ui/arrow-right.svg"),
        "close.svg" => include_bytes!("../assets/icons/ui/close.svg"),
        "chart.svg" => include_bytes!("../assets/icons/ui/chart.svg"),
        "check.svg" => include_bytes!("../assets/icons/ui/check.svg"),
        "chevron-down.svg" => include_bytes!("../assets/icons/ui/chevron-down.svg"),
        "copy.svg" => include_bytes!("../assets/icons/ui/copy.svg"),
        "copy-success.svg" => include_bytes!("../assets/icons/ui/copy-success.svg"),
        "delete.svg" => include_bytes!("../assets/icons/ui/delete.svg"),
        "eraser.svg" => include_bytes!("../assets/icons/ui/eraser.svg"),
        "failure.svg" => include_bytes!("../assets/icons/ui/failure.svg"),
        "lock-keyhole.svg" => include_bytes!("../assets/icons/ui/lock-keyhole.svg"),
        "moon.svg" => include_bytes!("../assets/icons/ui/moon.svg"),
        "redo.svg" => include_bytes!("../assets/icons/ui/redo.svg"),
        "refresh.svg" => include_bytes!("../assets/icons/ui/refresh.svg"),
        "search.svg" => include_bytes!("../assets/icons/ui/search.svg"),
        "settings.svg" => include_bytes!("../assets/icons/ui/settings.svg"),
        "sidebar-left.svg" => include_bytes!("../assets/icons/ui/sidebar-left.svg"),
        "sidebar-right.svg" => include_bytes!("../assets/icons/ui/sidebar-right.svg"),
        "split-side-by-side.svg" => include_bytes!("../assets/icons/ui/split-side-by-side.svg"),
        "split-stacked.svg" => include_bytes!("../assets/icons/ui/split-stacked.svg"),
        "sun.svg" => include_bytes!("../assets/icons/ui/sun.svg"),
        "undo.svg" => include_bytes!("../assets/icons/ui/undo.svg"),
        "view.svg" => include_bytes!("../assets/icons/ui/view.svg"),
        "view-off.svg" => include_bytes!("../assets/icons/ui/view-off.svg"),
        "window-close.svg" => include_bytes!("../assets/icons/ui/window-close.svg"),
        "window-maximize.svg" => include_bytes!("../assets/icons/ui/window-maximize.svg"),
        "window-minimize.svg" => include_bytes!("../assets/icons/ui/window-minimize.svg"),
        "window-restore.svg" => include_bytes!("../assets/icons/ui/window-restore.svg"),
        "loader.svg" => include_bytes!("../assets/icons/ui/loader.svg"),
        "info.svg" => include_bytes!("../assets/icons/ui/info.svg"),
        "signout.svg" => include_bytes!("../assets/icons/ui/signout.svg"),
        "user.svg" => include_bytes!("../assets/icons/ui/user.svg"),
        _ => return None,
    })
}

fn series_asset(path: &str) -> Option<&'static [u8]> {
    Some(match path.strip_prefix(SERIES_ASSET_PREFIX)? {
        "candlestick-chart.svg" => include_bytes!("../assets/icons/series/candlestick-chart.svg"),
        "ohlc-bar-chart.svg" => include_bytes!("../assets/icons/series/ohlc-bar-chart.svg"),
        "line-chart-type.svg" => include_bytes!("../assets/icons/series/line-chart-type.svg"),
        "area-chart-type.svg" => include_bytes!("../assets/icons/series/area-chart-type.svg"),
        "heikin-ashi-chart.svg" => include_bytes!("../assets/icons/series/heikin-ashi-chart.svg"),
        "brushable-area-chart.svg" => {
            include_bytes!("../assets/icons/series/brushable-area-chart.svg")
        }
        _ => return None,
    })
}

fn brand_asset(path: &str) -> Option<&'static [u8]> {
    Some(match path.strip_prefix(BRAND_ASSET_PREFIX)? {
        "logo.svg" => include_bytes!("../assets/aeris_assets/logo.svg"),
        _ => return None,
    })
}

fn exchange_asset(path: &str) -> Option<&'static [u8]> {
    Some(match path.strip_prefix(EXCHANGE_ASSET_PREFIX)? {
        "binance.svg" => include_bytes!("../assets/exchange_assets/exchange_logo/binance.svg"),
        "rithmic.svg" => include_bytes!("../assets/exchange_assets/exchange_logo/rithmic.svg"),
        "hyperliquid.svg" => {
            include_bytes!("../assets/exchange_assets/exchange_logo/hyperliquid.svg")
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_drawing_icon_has_a_unique_embedded_asset() {
        let assets = AerisAssets;
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
        let assets = AerisAssets;

        for icon in DrawingIcon::GEOMETRIC {
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
    fn horizontal_line_uses_the_horizontal_line_glyph_not_the_diagonal_ray() {
        assert_eq!(
            DrawingIcon::HorizontalLine.path().as_ref(),
            "aeris/icons/drawing/horizontal-line.svg"
        );
        assert_eq!(
            DrawingIcon::Ray.path().as_ref(),
            "aeris/icons/drawing/horizontal-ray.svg"
        );
    }

    #[test]
    fn list_filters_the_asset_namespace() {
        let assets = AerisAssets;
        assert_eq!(
            assets.list("").expect("all assets").len(),
            DrawingIcon::ALL.len()
                + UiIcon::ALL.len()
                + SeriesIcon::ALL.len()
                + BrandIcon::ALL.len()
                + ExchangeLogo::ALL.len()
        );
        assert_eq!(
            assets
                .list("aeris/icons/drawing/trend")
                .expect("filtered assets"),
            vec![DrawingIcon::TrendLine.path()]
        );
        assert!(
            assets
                .list("hugeicons/")
                .expect("removed namespace")
                .is_empty()
        );
        assert!(
            assets
                .load("unknown.svg")
                .expect("unknown lookup")
                .is_none()
        );
    }

    #[test]
    fn ui_icon_inventory_is_embedded_and_theme_neutral() {
        let assets = AerisAssets;
        let mut paths = HashSet::new();
        for icon in UiIcon::ALL {
            let path = icon.path();
            assert!(paths.insert(path.clone()), "duplicate asset path: {path}");
            let bytes = assets.load(path.as_ref()).unwrap().expect("UI icon");
            let svg = std::str::from_utf8(&bytes).expect("UTF-8 SVG");
            assert!(svg.contains("viewBox=\"0 0 24 24\""));
            assert!(svg.contains("currentColor"));
            assert!(!svg.contains('#'));
        }
    }

    #[test]
    fn colored_marks_use_square_vector_masks() {
        let assets = AerisAssets;
        for icon in SeriesIcon::ALL {
            let bytes = assets
                .load(icon.path().as_ref())
                .unwrap()
                .expect("series icon");
            let svg = std::str::from_utf8(&bytes).unwrap();
            assert!(svg.contains("viewBox=\"0 0 24 24\""));
            assert!(!svg.contains("<image"));
            assert!(!svg.contains("<filter"));
        }
        for logo in ExchangeLogo::ALL {
            let bytes = assets
                .load(logo.path().as_ref())
                .unwrap()
                .expect("exchange logo");
            let svg = std::str::from_utf8(&bytes).unwrap();
            assert!(svg.contains("<svg"), "{}", logo.path());
            assert!(!svg.contains("<image"), "{}", logo.path());
        }

        for logo in BrandIcon::ALL {
            let bytes = assets
                .load(logo.path().as_ref())
                .unwrap()
                .expect("brand logo");
            let svg = std::str::from_utf8(&bytes).unwrap();
            assert!(svg.contains("<svg"), "{}", logo.path());
            assert!(!svg.contains("<image"), "{}", logo.path());
        }

        let mark = assets
            .load(BrandIcon::Mark.path().as_ref())
            .unwrap()
            .expect("brand mark");
        assert!(
            std::str::from_utf8(&mark)
                .unwrap()
                .contains("viewBox=\"0 0 54 54\"")
        );
    }

    #[test]
    fn colored_marks_rasterize_at_display_size() {
        use gpui::SvgRenderer;
        use std::sync::Arc;

        let assets = AerisAssets;
        let renderer = SvgRenderer::new(Arc::new(AerisAssets));
        let rasterize = |path: &SharedString, logical_size: f32, window_scale: f32| {
            let bytes = assets.load(path.as_ref()).unwrap().expect("asset bytes");
            let header = std::str::from_utf8(&bytes[..bytes.len().min(768)]).unwrap();
            let svg = header.find("<svg").unwrap();
            let width = header[svg..].find("width=\"").unwrap() + svg + 7;
            let rest = &header[width..];
            let end = rest.find('"').unwrap();
            let intrinsic: f32 = rest[..end].parse().unwrap();
            let scale_factor = (logical_size * window_scale / intrinsic).max(1.0 / intrinsic);
            renderer
                .render_single_frame(&bytes, scale_factor)
                .expect("rasterize colored mark")
        };

        for icon in SeriesIcon::ALL {
            let image = rasterize(&icon.path(), 18.0, 2.0);
            assert_eq!(image.frame_count(), 1);
            assert!(image.size(0).width.0 > 0);
            assert!(image.size(0).height.0 > 0);
        }
        for logo in ExchangeLogo::ALL {
            let image = rasterize(&logo.path(), 20.0, 2.0);
            assert_eq!(image.frame_count(), 1);
            assert!(image.size(0).width.0 > 0);
            assert!(image.size(0).height.0 > 0);
        }
    }
}
