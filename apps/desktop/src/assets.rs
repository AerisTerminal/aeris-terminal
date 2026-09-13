use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

const DRAWING_ASSET_PREFIX: &str = "axiusflow/icons/drawing/";
const UI_ASSET_PREFIX: &str = "axiusflow/icons/ui/";
const SERIES_ASSET_PREFIX: &str = "axiusflow/icons/series/";
const BRAND_ASSET_PREFIX: &str = "axiusflow/brand/";
const EXCHANGE_ASSET_PREFIX: &str = "axiusflow/exchange_logo/";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiIcon {
    AddIcon01,
    AnalyticsUpIcon,
    ArrowLeftIcon01,
    ArrowRightDouble,
    ArrowRightIcon01,
    CancelIcon01,
    CheckIcon,
    ChevronDown,
    Copy01Icon,
    DeleteIcon02,
    EraserIcon,
    LayoutAlignLeftIcon,
    LockKeyholeIcon,
    MoonIcon02,
    Redo01,
    Refresh01Icon,
    SearchIcon01,
    Settings01,
    SidebarRightIcon01,
    SplitSideBySide,
    SplitStacked,
    SunIcon03,
    Undo03,
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
    pub const ALL: [Self; 33] = [
        Self::AddIcon01,
        Self::AnalyticsUpIcon,
        Self::ArrowLeftIcon01,
        Self::ArrowRightDouble,
        Self::ArrowRightIcon01,
        Self::CancelIcon01,
        Self::CheckIcon,
        Self::ChevronDown,
        Self::Copy01Icon,
        Self::DeleteIcon02,
        Self::EraserIcon,
        Self::LayoutAlignLeftIcon,
        Self::LockKeyholeIcon,
        Self::MoonIcon02,
        Self::Redo01,
        Self::Refresh01Icon,
        Self::SearchIcon01,
        Self::Settings01,
        Self::SidebarRightIcon01,
        Self::SplitSideBySide,
        Self::SplitStacked,
        Self::SunIcon03,
        Self::Undo03,
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
            Self::AddIcon01 => "add-01.svg",
            Self::AnalyticsUpIcon => "analytics-up.svg",
            Self::ArrowLeftIcon01 => "arrow-left-01.svg",
            Self::ArrowRightDouble => "arrow-right-double.svg",
            Self::ArrowRightIcon01 => "arrow-right-01.svg",
            Self::CancelIcon01 => "cancel-01.svg",
            Self::CheckIcon => "check.svg",
            Self::ChevronDown => "chevron-down.svg",
            Self::Copy01Icon => "copy-01.svg",
            Self::DeleteIcon02 => "delete-02.svg",
            Self::EraserIcon => "eraser.svg",
            Self::LayoutAlignLeftIcon => "layout-align-left.svg",
            Self::LockKeyholeIcon => "lock-keyhole.svg",
            Self::MoonIcon02 => "moon-02.svg",
            Self::Redo01 => "redo-01.svg",
            Self::Refresh01Icon => "refresh-01.svg",
            Self::SearchIcon01 => "search-01.svg",
            Self::Settings01 => "settings-01.svg",
            Self::SidebarRightIcon01 => "sidebar-right-01.svg",
            Self::SplitSideBySide => "split-side-by-side.svg",
            Self::SplitStacked => "split-stacked.svg",
            Self::SunIcon03 => "sun-03.svg",
            Self::Undo03 => "undo-03.svg",
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
    ExtendedLine,
    HorizontalLine,
    VerticalLine,
    Ray,
    DirectionalRay,
    Rectangle,
    Path,
    Cursor,
    Brush,
    Fibonacci,
    Ruler,
    Text,
}

impl DrawingIcon {
    pub const ALL: [Self; 13] = [
        Self::TrendLine,
        Self::ExtendedLine,
        Self::HorizontalLine,
        Self::VerticalLine,
        Self::Ray,
        Self::DirectionalRay,
        Self::Rectangle,
        Self::Path,
        Self::Cursor,
        Self::Brush,
        Self::Fibonacci,
        Self::Ruler,
        Self::Text,
    ];

    #[must_use]
    pub fn path(self) -> SharedString {
        match self {
            Self::TrendLine => concat!("axiusflow/icons/drawing/", "trend-line.svg"),
            Self::ExtendedLine => concat!("axiusflow/icons/drawing/", "extended-line.svg"),
            Self::HorizontalLine => {
                concat!("axiusflow/icons/drawing/", "horizontal-line.svg")
            }
            Self::VerticalLine => concat!("axiusflow/icons/drawing/", "vertical-line.svg"),
            Self::Ray => concat!("axiusflow/icons/drawing/", "horizontal-ray.svg"),
            Self::DirectionalRay => concat!("axiusflow/icons/drawing/", "ray.svg"),
            Self::Rectangle => concat!("axiusflow/icons/drawing/", "rectangle.svg"),
            Self::Path => concat!("axiusflow/icons/drawing/", "path.svg"),
            Self::Cursor => concat!("axiusflow/icons/drawing/", "cursor.svg"),
            Self::Brush => concat!("axiusflow/icons/drawing/", "brush.svg"),
            Self::Fibonacci => concat!("axiusflow/icons/drawing/", "fibonacci.svg"),
            Self::Ruler => concat!("axiusflow/icons/drawing/", "ruler.svg"),
            Self::Text => concat!("axiusflow/icons/drawing/", "text.svg"),
        }
        .into()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BrandIcon {
    Mark,
    WordmarkDarkText,
    WordmarkLightText,
}

impl BrandIcon {
    pub const ALL: [Self; 3] = [Self::Mark, Self::WordmarkDarkText, Self::WordmarkLightText];

    #[must_use]
    pub fn path(self) -> SharedString {
        let name = match self {
            Self::Mark => "axiusflow_logo.svg",
            Self::WordmarkDarkText => "axiusflow_logo_with_text_dark.svg",
            Self::WordmarkLightText => "axiusflow_logo_with_text_light.svg",
        };
        format!("{BRAND_ASSET_PREFIX}{name}").into()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ExchangeLogo {
    Binance,
    #[default]
    Rithmic,
    Hyperliquid,
}

impl ExchangeLogo {
    pub const ALL: [Self; 3] = [Self::Rithmic, Self::Hyperliquid, Self::Binance];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Binance => "Binance",
            Self::Rithmic => "Rithmic",
            Self::Hyperliquid => "Hyperliquid",
        }
    }

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

pub(crate) const MAX_VECTOR_ASSET_ENCODED_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum VectorAsset {
    Series(SeriesIcon),
    Brand(BrandIcon),
    Exchange(ExchangeLogo),
}

impl VectorAsset {
    pub(crate) const ALL: [Self; 12] = [
        Self::Series(SeriesIcon::Candlestick),
        Self::Series(SeriesIcon::OhlcBar),
        Self::Series(SeriesIcon::Line),
        Self::Series(SeriesIcon::Area),
        Self::Series(SeriesIcon::HeikinAshi),
        Self::Series(SeriesIcon::BrushableArea),
        Self::Brand(BrandIcon::Mark),
        Self::Brand(BrandIcon::WordmarkDarkText),
        Self::Brand(BrandIcon::WordmarkLightText),
        Self::Exchange(ExchangeLogo::Rithmic),
        Self::Exchange(ExchangeLogo::Hyperliquid),
        Self::Exchange(ExchangeLogo::Binance),
    ];

    pub(crate) const fn identity(self) -> &'static str {
        match self {
            Self::Series(SeriesIcon::Candlestick) => "series:candlestick",
            Self::Series(SeriesIcon::OhlcBar) => "series:ohlc_bar",
            Self::Series(SeriesIcon::Line) => "series:line",
            Self::Series(SeriesIcon::Area) => "series:area",
            Self::Series(SeriesIcon::HeikinAshi) => "series:heikin_ashi",
            Self::Series(SeriesIcon::BrushableArea) => "series:brushable_area",
            Self::Brand(BrandIcon::Mark) => "brand:mark",
            Self::Brand(BrandIcon::WordmarkDarkText) => "brand:wordmark_dark_text",
            Self::Brand(BrandIcon::WordmarkLightText) => "brand:wordmark_light_text",
            Self::Exchange(ExchangeLogo::Binance) => "exchange:binance",
            Self::Exchange(ExchangeLogo::Rithmic) => "exchange:rithmic",
            Self::Exchange(ExchangeLogo::Hyperliquid) => "exchange:hyperliquid",
        }
    }

    pub(crate) const fn spec(self) -> VectorAssetSpec {
        let (path, width, height, revision) = match self {
            Self::Series(SeriesIcon::Candlestick) => (
                "axiusflow/icons/series/candlestick-chart.svg",
                24,
                24,
                0x8ea9_d080_508a_fc57,
            ),
            Self::Series(SeriesIcon::OhlcBar) => (
                "axiusflow/icons/series/ohlc-bar-chart.svg",
                24,
                24,
                0x5c62_97b7_fccb_a7cb,
            ),
            Self::Series(SeriesIcon::Line) => (
                "axiusflow/icons/series/line-chart-type.svg",
                24,
                24,
                0x158e_c604_523b_1c73,
            ),
            Self::Series(SeriesIcon::Area) => (
                "axiusflow/icons/series/area-chart-type.svg",
                24,
                24,
                0x5460_94d4_bae1_1d60,
            ),
            Self::Series(SeriesIcon::HeikinAshi) => (
                "axiusflow/icons/series/heikin-ashi-chart.svg",
                24,
                24,
                0xe6f3_72b6_c033_daff,
            ),
            Self::Series(SeriesIcon::BrushableArea) => (
                "axiusflow/icons/series/brushable-area-chart.svg",
                24,
                24,
                0x0508_6250_3b1b_7a00,
            ),
            Self::Brand(BrandIcon::Mark) => (
                "axiusflow/brand/axiusflow_logo.svg",
                54,
                54,
                0x55ab_4a10_4e5c_749a,
            ),
            Self::Brand(BrandIcon::WordmarkDarkText) => (
                "axiusflow/brand/axiusflow_logo_with_text_dark.svg",
                188,
                54,
                0xb0e8_51a5_51a7_09d6,
            ),
            Self::Brand(BrandIcon::WordmarkLightText) => (
                "axiusflow/brand/axiusflow_logo_with_text_light.svg",
                188,
                54,
                0x8b4c_9672_0fdc_6482,
            ),
            Self::Exchange(ExchangeLogo::Rithmic) => (
                "axiusflow/exchange_logo/rithmic.svg",
                32,
                32,
                0x7c52_ef04_3d26_e396,
            ),
            Self::Exchange(ExchangeLogo::Hyperliquid) => (
                "axiusflow/exchange_logo/hyperliquid.svg",
                270,
                270,
                0x22fb_3cca_ae73_a11c,
            ),
            Self::Exchange(ExchangeLogo::Binance) => (
                "axiusflow/exchange_logo/binance.svg",
                800,
                800,
                0x6f19_aca5_8f1b_171a,
            ),
        };
        VectorAssetSpec {
            path,
            view_box_width: width,
            view_box_height: height,
            fit: VectorFit::Contain,
            content_revision: revision,
        }
    }
}

impl From<SeriesIcon> for VectorAsset {
    fn from(value: SeriesIcon) -> Self {
        Self::Series(value)
    }
}

impl From<BrandIcon> for VectorAsset {
    fn from(value: BrandIcon) -> Self {
        Self::Brand(value)
    }
}

impl From<ExchangeLogo> for VectorAsset {
    fn from(value: ExchangeLogo) -> Self {
        Self::Exchange(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum VectorFit {
    Contain,
    Cover,
    Stretch,
}

impl VectorFit {
    const ALL: [Self; 3] = [Self::Contain, Self::Cover, Self::Stretch];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VectorAssetSpec {
    pub(crate) path: &'static str,
    pub(crate) view_box_width: u32,
    pub(crate) view_box_height: u32,
    pub(crate) fit: VectorFit,
    pub(crate) content_revision: u64,
}

pub(crate) fn validate_vector_asset(asset: VectorAsset, bytes: &[u8]) -> Result<(), &'static str> {
    if bytes.len() > MAX_VECTOR_ASSET_ENCODED_BYTES {
        return Err("encoded asset exceeds limit");
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "asset is not UTF-8")?;
    let document = roxmltree::Document::parse(text).map_err(|_| "malformed SVG XML")?;
    let root = document.root_element();
    if root.tag_name().name() != "svg" {
        return Err("document root is not SVG");
    }
    let view_box = root.attribute("viewBox").ok_or("viewBox is missing")?;
    let mut values = view_box.split_ascii_whitespace();
    let x = values.next().ok_or("viewBox is incomplete")?;
    let y = values.next().ok_or("viewBox is incomplete")?;
    let width = values.next().ok_or("viewBox is incomplete")?;
    let height = values.next().ok_or("viewBox is incomplete")?;
    let x = x.parse::<f64>().map_err(|_| "viewBox x is invalid")?;
    let y = y.parse::<f64>().map_err(|_| "viewBox y is invalid")?;
    if values.next().is_some() || !x.is_finite() || !y.is_finite() {
        return Err("viewBox is invalid");
    }
    let width = width
        .parse::<f64>()
        .map_err(|_| "viewBox width is invalid")?;
    let height = height
        .parse::<f64>()
        .map_err(|_| "viewBox height is invalid")?;
    let spec = asset.spec();
    if !VectorFit::ALL.contains(&spec.fit) {
        return Err("unsupported vector fit");
    }
    if !width.is_finite()
        || !height.is_finite()
        || width <= 0.0
        || height <= 0.0
        || (width - f64::from(spec.view_box_width)).abs() > f64::EPSILON
        || (height - f64::from(spec.view_box_height)).abs() > f64::EPSILON
    {
        return Err("viewBox disagrees with typed metadata");
    }

    for node in document.descendants().filter(roxmltree::Node::is_element) {
        match node.tag_name().name() {
            "script" | "animate" | "animateMotion" | "animateTransform" | "set"
            | "foreignObject" | "image" | "text" | "style" => {
                return Err("SVG contains forbidden content");
            }
            _ => {}
        }
        for attribute in node.attributes() {
            let name = attribute.name();
            let value = attribute.value().trim();
            if name.starts_with("on") {
                return Err("SVG contains an event handler");
            }
            if matches!(name, "href" | "src") && !value.starts_with('#') {
                return Err("SVG contains an external reference");
            }
            let lower = value.to_ascii_lowercase();
            if lower.contains("currentcolor") {
                return Err("full-color SVG depends on currentColor");
            }
            for reference in lower
                .match_indices("url(")
                .map(|(index, _)| &value[index + 4..])
            {
                if !reference
                    .trim_start_matches([' ', '\'', '"'])
                    .starts_with('#')
                {
                    return Err("SVG contains an external URL");
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AxiusflowAssets;

impl AssetSource for AxiusflowAssets {
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
        "extended-line.svg" => include_bytes!("../assets/icons/drawing/extended-line.svg"),
        "horizontal-line.svg" => include_bytes!("../assets/icons/drawing/horizontal-line.svg"),
        "vertical-line.svg" => include_bytes!("../assets/icons/drawing/vertical-line.svg"),
        "horizontal-ray.svg" => include_bytes!("../assets/icons/drawing/horizontal-ray.svg"),
        "ray.svg" => include_bytes!("../assets/icons/drawing/ray.svg"),
        "rectangle.svg" => include_bytes!("../assets/icons/drawing/rectangle.svg"),
        "path.svg" => include_bytes!("../assets/icons/drawing/path.svg"),
        "cursor.svg" => include_bytes!("../assets/icons/drawing/cursor.svg"),
        "brush.svg" => include_bytes!("../assets/icons/drawing/brush.svg"),
        "fibonacci.svg" => include_bytes!("../assets/icons/drawing/fibonacci.svg"),
        "ruler.svg" => include_bytes!("../assets/icons/drawing/ruler.svg"),
        "text.svg" => include_bytes!("../assets/icons/drawing/text.svg"),
        _ => return None,
    })
}

fn ui_asset(path: &str) -> Option<&'static [u8]> {
    Some(match path.strip_prefix(UI_ASSET_PREFIX)? {
        "add-01.svg" => include_bytes!("../assets/icons/ui/add-01.svg"),
        "analytics-up.svg" => include_bytes!("../assets/icons/ui/analytics-up.svg"),
        "arrow-left-01.svg" => include_bytes!("../assets/icons/ui/arrow-left-01.svg"),
        "arrow-right-double.svg" => {
            include_bytes!("../assets/icons/ui/arrow-right-double.svg")
        }
        "arrow-right-01.svg" => include_bytes!("../assets/icons/ui/arrow-right-01.svg"),
        "cancel-01.svg" => include_bytes!("../assets/icons/ui/cancel-01.svg"),
        "check.svg" => include_bytes!("../assets/icons/ui/check.svg"),
        "chevron-down.svg" => include_bytes!("../assets/icons/ui/chevron-down.svg"),
        "copy-01.svg" => include_bytes!("../assets/icons/ui/copy-01.svg"),
        "delete-02.svg" => include_bytes!("../assets/icons/ui/delete-02.svg"),
        "eraser.svg" => include_bytes!("../assets/icons/ui/eraser.svg"),
        "layout-align-left.svg" => include_bytes!("../assets/icons/ui/layout-align-left.svg"),
        "lock-keyhole.svg" => include_bytes!("../assets/icons/ui/lock-keyhole.svg"),
        "moon-02.svg" => include_bytes!("../assets/icons/ui/moon-02.svg"),
        "redo-01.svg" => include_bytes!("../assets/icons/ui/redo-01.svg"),
        "refresh-01.svg" => include_bytes!("../assets/icons/ui/refresh-01.svg"),
        "search-01.svg" => include_bytes!("../assets/icons/ui/search-01.svg"),
        "settings-01.svg" => include_bytes!("../assets/icons/ui/settings-01.svg"),
        "sidebar-right-01.svg" => include_bytes!("../assets/icons/ui/sidebar-right-01.svg"),
        "split-side-by-side.svg" => include_bytes!("../assets/icons/ui/split-side-by-side.svg"),
        "split-stacked.svg" => include_bytes!("../assets/icons/ui/split-stacked.svg"),
        "sun-03.svg" => include_bytes!("../assets/icons/ui/sun-03.svg"),
        "undo-03.svg" => include_bytes!("../assets/icons/ui/undo-03.svg"),
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
        "axiusflow_logo.svg" => include_bytes!("../assets/brand_assets/axiusflow_logo.svg"),
        "axiusflow_logo_with_text_dark.svg" => {
            include_bytes!("../assets/brand_assets/axiusflow_logo_with_text_dark.svg")
        }
        "axiusflow_logo_with_text_light.svg" => {
            include_bytes!("../assets/brand_assets/axiusflow_logo_with_text_light.svg")
        }
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

            let document = roxmltree::Document::parse(svg).expect("valid drawing SVG");
            let view_box = document
                .root_element()
                .attribute("viewBox")
                .expect("drawing viewBox");
            let values = view_box
                .split_ascii_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()
                .expect("numeric drawing viewBox");
            assert_eq!(values.len(), 4, "invalid viewBox: {path}");
            assert!(
                values[2].is_finite() && values[2] > 0.0,
                "invalid width: {path}"
            );
            assert!(
                values[3].is_finite() && values[3] > 0.0,
                "invalid height: {path}"
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
            "axiusflow/icons/drawing/horizontal-line.svg"
        );
        assert_eq!(
            DrawingIcon::Ray.path().as_ref(),
            "axiusflow/icons/drawing/horizontal-ray.svg"
        );
    }

    #[test]
    fn list_filters_the_asset_namespace() {
        let assets = AxiusflowAssets;
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
                .list("axiusflow/icons/drawing/trend")
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
        let assets = AxiusflowAssets;
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
    fn every_full_color_vector_is_classified_unique_and_structurally_safe() {
        let assets = AxiusflowAssets;
        let mut identities = HashSet::new();
        let mut paths = HashSet::new();
        for asset in VectorAsset::ALL {
            let spec = asset.spec();
            assert!(identities.insert(asset.identity()), "duplicate identity");
            assert!(paths.insert(spec.path), "duplicate path: {}", spec.path);
            let bytes = assets.load(spec.path).unwrap().expect("full-color vector");
            validate_vector_asset(asset, &bytes)
                .unwrap_or_else(|error| panic!("{}: {error}", asset.identity()));
        }
        assert_eq!(
            VectorAsset::ALL.len(),
            SeriesIcon::ALL.len() + BrandIcon::ALL.len() + ExchangeLogo::ALL.len()
        );
    }

    #[test]
    fn vector_content_revisions_match_embedded_bytes() {
        use sha2::{Digest, Sha256};

        let assets = AxiusflowAssets;
        for asset in VectorAsset::ALL {
            let spec = asset.spec();
            let bytes = assets.load(spec.path).unwrap().expect("full-color vector");
            let digest = Sha256::digest(&bytes);
            assert_eq!(
                spec.content_revision,
                u64::from_be_bytes(digest[..8].try_into().unwrap()),
                "stale content revision: {}",
                asset.identity()
            );
        }
    }

    #[test]
    fn unsafe_or_malformed_vector_fixtures_are_rejected() {
        let asset = VectorAsset::Brand(BrandIcon::Mark);
        let fixture = |body: &str| {
            format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 54 54">{body}</svg>"#)
        };
        for body in [
            "<script/>",
            "<image href=\"https://example.com/a.png\"/>",
            "<foreignObject/>",
            "<animate/>",
            "<text>Axiusflow</text>",
            "<path onclick=\"alert(1)\"/>",
            "<path fill=\"url(https://example.com/a.svg#paint)\"/>",
        ] {
            assert!(validate_vector_asset(asset, fixture(body).as_bytes()).is_err());
        }
        assert!(validate_vector_asset(asset, b"<svg").is_err());
        assert!(
            validate_vector_asset(asset, b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_err()
        );
    }

    #[test]
    fn safe_svg_feature_fixture_parses_and_rasterizes() {
        use gpui::{DevicePixels, SvgRenderer, SvgSize, size};
        use std::sync::Arc;

        let fixture = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 54 54">
          <defs><linearGradient id="g"><stop stop-color="#f00" stop-opacity=".5"/><stop offset="1" stop-color="#00f"/></linearGradient><clipPath id="c"><circle cx="27" cy="27" r="24"/></clipPath><mask id="m"><rect width="54" height="54" fill="#fff"/></mask></defs>
          <rect width="54" height="54" fill="url(#g)" clip-path="url(#c)" mask="url(#m)"/>
        </svg>"##;
        validate_vector_asset(VectorAsset::Brand(BrandIcon::Mark), fixture).unwrap();
        let renderer = SvgRenderer::new(Arc::new(AxiusflowAssets));
        let parsed = renderer.parse_svg(fixture).expect("parse feature fixture");
        let image = renderer
            .render_parsed(
                &parsed,
                SvgSize::ExactSize(size(DevicePixels(81), DevicePixels(81))),
            )
            .expect("rasterize feature fixture");
        assert_eq!(image.size(0), size(DevicePixels(81), DevicePixels(81)));
        assert!(
            image
                .as_bytes(0)
                .unwrap()
                .iter()
                .any(|channel| *channel != 0)
        );
    }

    #[test]
    fn deterministic_vector_contact_sheet_covers_every_asset() {
        use gpui::{DevicePixels, SvgRenderer, SvgSize, size};
        use num_traits::ToPrimitive;
        use std::sync::Arc;

        let assets = AxiusflowAssets;
        let renderer = SvgRenderer::new(Arc::new(AxiusflowAssets));
        let mut rendered_pixels = 0_u64;
        for asset in VectorAsset::ALL {
            let spec = asset.spec();
            let bytes = assets.load(spec.path).unwrap().expect("asset bytes");
            let parsed = renderer
                .parse_svg(&bytes)
                .expect("parse contact-sheet asset");
            let width = 96_u32;
            let height = (f64::from(width) * f64::from(spec.view_box_height)
                / f64::from(spec.view_box_width))
            .round()
            .max(1.0)
            .to_u32()
            .expect("bounded contact-sheet height");
            let image = renderer
                .render_parsed(
                    &parsed,
                    SvgSize::ExactSize(size(DevicePixels::from(width), DevicePixels::from(height))),
                )
                .expect("rasterize contact-sheet asset");
            assert_eq!(image.size(0), size(width.into(), height.into()));
            rendered_pixels += u64::from(width) * u64::from(height);
        }
        assert!(rendered_pixels > 0);
    }
}
