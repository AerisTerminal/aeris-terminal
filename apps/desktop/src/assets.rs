use aeris_chart_integration::ChartDrawingStamp;
use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

const DRAWING_ASSET_PREFIX: &str = "aeris/icons/drawing/";
const STAMP_ASSET_PREFIX: &str = "aeris/icons/stamp/";
const UI_ASSET_PREFIX: &str = "aeris/icons/ui/";
const SERIES_ASSET_PREFIX: &str = "aeris/icons/series/";
const BRAND_ASSET_PREFIX: &str = "aeris/brand/";
const EXCHANGE_ASSET_PREFIX: &str = "aeris/exchange_logo/";
const PROVIDER_ASSET_PREFIX: &str = "aeris/provider_logo/";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiIcon {
    Add,
    ArrowLeft,
    ArrowRightDouble,
    ArrowRight,
    Camera,
    Close,
    CloseBold,
    Chart,
    CheckIcon,
    ChevronDown,
    Copy,
    CopySuccess,
    DataPanel,
    Download,
    Trash,
    EraserIcon,
    Failure,
    Lock,
    Moon,
    Redo,
    Refresh,
    RefreshV2,
    Search,
    Settings,
    SidebarLeft,
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
    pub const ALL: [Self; 39] = [
        Self::Add,
        Self::ArrowLeft,
        Self::ArrowRightDouble,
        Self::ArrowRight,
        Self::Camera,
        Self::Close,
        Self::CloseBold,
        Self::Chart,
        Self::CheckIcon,
        Self::ChevronDown,
        Self::Copy,
        Self::CopySuccess,
        Self::DataPanel,
        Self::Download,
        Self::Trash,
        Self::EraserIcon,
        Self::Failure,
        Self::Lock,
        Self::Moon,
        Self::Redo,
        Self::Refresh,
        Self::RefreshV2,
        Self::Search,
        Self::Settings,
        Self::SidebarLeft,
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
            Self::Camera => "camera.svg",
            Self::Close => "close.svg",
            Self::CloseBold => "close-bold.svg",
            Self::Chart => "chart.svg",
            Self::CheckIcon => "check.svg",
            Self::ChevronDown => "chevron-down.svg",
            Self::Copy => "copy.svg",
            Self::CopySuccess => "copy-success.svg",
            Self::DataPanel => "data-panel.svg",
            Self::Download => "download.svg",
            Self::Trash => "trash.svg",
            Self::EraserIcon => "eraser.svg",
            Self::Failure => "failure.svg",
            Self::Lock => "lock.svg",
            Self::Moon => "moon.svg",
            Self::Redo => "redo.svg",
            Self::Refresh => "refresh.svg",
            Self::RefreshV2 => "refresh-2.svg",
            Self::Search => "search.svg",
            Self::Settings => "settings.svg",
            Self::SidebarLeft => "sidebar-left.svg",
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

/// Declares the drawing-toolbar glyphs once: the enum, its inventory, its asset path, and the
/// embedded bytes all come from the same `Variant => "file.svg"` list.
macro_rules! drawing_icons {
    ($($variant:ident => $file:literal,)+) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum DrawingIcon {
            $($variant,)+
        }

        impl DrawingIcon {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            #[must_use]
            pub fn path(self) -> SharedString {
                match self {
                    $(Self::$variant => concat!("aeris/icons/drawing/", $file),)+
                }
                .into()
            }
        }

        fn drawing_asset(path: &str) -> Option<&'static [u8]> {
            Some(match path.strip_prefix(DRAWING_ASSET_PREFIX)? {
                $($file => include_bytes!(concat!("../assets/icons/drawing/", $file)),)+
                _ => return None,
            })
        }
    };
}

drawing_icons! {
    Cursor => "cursor.svg",
    GroupArrow => "group-arrow.svg",
    TrendLine => "trend-line.svg",
    Ray => "ray.svg",
    InfoLine => "info-line.svg",
    ExtendedLine => "extended-line.svg",
    TrendAngle => "trend-angle.svg",
    HorizontalLine => "horizontal-line.svg",
    HorizontalRay => "horizontal-ray.svg",
    VerticalLine => "vertical-line.svg",
    CrossLine => "cross-line.svg",
    ArrowLine => "arrow-line.svg",
    ParallelChannel => "parallel-channel.svg",
    RegressionTrend => "regression-trend.svg",
    FlatTopChannel => "flat-top-channel.svg",
    FlatBottomChannel => "flat-bottom-channel.svg",
    DisjointChannel => "disjoint-channel.svg",
    AndrewsPitchfork => "andrews-pitchfork.svg",
    SchiffPitchfork => "schiff-pitchfork.svg",
    ModifiedSchiffPitchfork => "modified-schiff-pitchfork.svg",
    InsidePitchfork => "inside-pitchfork.svg",
    Pitchfan => "pitchfan.svg",
    FibonacciRetracement => "fibonacci.svg",
    FibonacciExtension => "fib-extension.svg",
    FibonacciChannel => "fib-channel.svg",
    FibonacciTimeZones => "fib-time-zones.svg",
    FibonacciTrendTime => "fib-trend-time.svg",
    FibonacciSpeedFan => "fib-speed-fan.svg",
    FibonacciSpeedArcs => "fib-speed-arcs.svg",
    FibonacciCircles => "fib-circles.svg",
    FibonacciSpiral => "fib-spiral.svg",
    FibonacciWedge => "fib-wedge.svg",
    GannBox => "gann-box.svg",
    GannSquareFixed => "gann-square-fixed.svg",
    GannSquare => "gann-square.svg",
    GannFan => "gann-fan.svg",
    PatternXabcd => "pattern-xabcd.svg",
    PatternCypher => "pattern-cypher.svg",
    PatternHeadShoulders => "pattern-head-shoulders.svg",
    PatternAbcd => "pattern-abcd.svg",
    PatternTriangle => "pattern-triangle.svg",
    PatternThreeDrives => "pattern-three-drives.svg",
    ElliottImpulse => "elliott-impulse.svg",
    ElliottCorrection => "elliott-correction.svg",
    ElliottTriangle => "elliott-triangle.svg",
    ElliottDoubleCombination => "elliott-double-combo.svg",
    ElliottTripleCombination => "elliott-triple-combo.svg",
    CyclicLines => "cyclic-lines.svg",
    TimeCycles => "time-cycles.svg",
    SineLine => "sine-line.svg",
    LongPosition => "long-position.svg",
    ShortPosition => "short-position.svg",
    Forecast => "forecast.svg",
    BarsPattern => "bars-pattern.svg",
    Projection => "projection.svg",
    AnchoredVwap => "anchored-vwap.svg",
    FixedRangeVolumeProfile => "fixed-range-volume-profile.svg",
    AnchoredVolumeProfile => "anchored-volume-profile.svg",
    PriceRange => "price-range.svg",
    DateRange => "date-range.svg",
    DatePriceRange => "ruler.svg",
    Brush => "brush.svg",
    Highlighter => "highlighter.svg",
    Rectangle => "rectangle.svg",
    RotatedRectangle => "rotated-rectangle.svg",
    Path => "path.svg",
    Circle => "circle.svg",
    Ellipse => "ellipse.svg",
    Polyline => "polyline.svg",
    Triangle => "triangle.svg",
    Arc => "arc.svg",
    Curve => "curve.svg",
    DoubleCurve => "double-curve.svg",
    Text => "text.svg",
    AnchoredText => "anchored-text.svg",
    Note => "note.svg",
    PriceNote => "price-note.svg",
    Callout => "callout.svg",
    Comment => "comment.svg",
    PriceLabel => "price-label.svg",
    Signpost => "signpost.svg",
    FlagMark => "flag-mark.svg",
    ArrowMarkerUp => "arrow-marker-up.svg",
    ArrowMarkerDown => "arrow-marker-down.svg",
    ArrowMarkerLeft => "arrow-marker-left.svg",
    ArrowMarkerRight => "arrow-marker-right.svg",
}

/// Toolbar glyph of a built-in chart stamp; the artwork is the one the chart rasterizes.
#[must_use]
pub fn stamp_icon_path(stamp: ChartDrawingStamp) -> SharedString {
    format!("{STAMP_ASSET_PREFIX}{}.svg", stamp.key()).into()
}

fn stamp_asset(path: &str) -> Option<&'static [u8]> {
    let key = path
        .strip_prefix(STAMP_ASSET_PREFIX)?
        .strip_suffix(".svg")?;
    ChartDrawingStamp::ALL
        .into_iter()
        .find(|stamp| stamp.key() == key)
        .map(ChartDrawingStamp::svg)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeriesIcon {
    Candlestick,
    OhlcBar,
    Line,
    LineWithMarkers,
    Area,
    HeikinAshi,
    BrushableArea,
}

impl SeriesIcon {
    pub const ALL: [Self; 7] = [
        Self::Candlestick,
        Self::OhlcBar,
        Self::Line,
        Self::LineWithMarkers,
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
            Self::LineWithMarkers => "line-with-markers-chart.svg",
            Self::Area => "area-chart-type.svg",
            Self::HeikinAshi => "heikin-ashi-chart.svg",
            Self::BrushableArea => "brushable-area-chart.svg",
        };
        format!("{SERIES_ASSET_PREFIX}{name}").into()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrandAsset {
    MainLogo,
    LogomarkDark,
    LogomarkWhite,
}

impl BrandAsset {
    pub const ALL: [Self; 3] = [Self::MainLogo, Self::LogomarkDark, Self::LogomarkWhite];

    #[must_use]
    pub fn path(self) -> SharedString {
        let name = match self {
            Self::MainLogo => "logo.svg",
            Self::LogomarkDark => "logomark-dark.svg",
            Self::LogomarkWhite => "logomark-white.svg",
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderLogo {
    TastytradeLight,
    TastytradeDark,
}

impl ProviderLogo {
    pub const ALL: [Self; 2] = [Self::TastytradeLight, Self::TastytradeDark];

    #[must_use]
    pub fn for_theme(mode: aeris_design_system::ThemeMode) -> Self {
        match mode {
            aeris_design_system::ThemeMode::Light => Self::TastytradeDark,
            aeris_design_system::ThemeMode::Dark => Self::TastytradeLight,
        }
    }

    #[must_use]
    pub fn path(self) -> SharedString {
        let name = match self {
            Self::TastytradeLight => "tastytrades_light.png",
            Self::TastytradeDark => "tastytrades_dark.png",
        };
        format!("{PROVIDER_ASSET_PREFIX}{name}").into()
    }
}

/// Rithmic's required attribution artwork. Each mark ships pre-rendered at the exact
/// device pixels of both densities: GPUI samples images without mipmaps, so shrinking
/// the large original aliases the marks' fine lettering into noise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributionMark {
    MarketDataByRithmic,
    PoweredByOmne,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkDensity {
    Standard,
    High,
}

impl MarkDensity {
    pub const ALL: [Self; 2] = [Self::Standard, Self::High];

    #[must_use]
    pub fn for_scale(window_scale: f32) -> Self {
        if window_scale <= 1.0 {
            Self::Standard
        } else {
            Self::High
        }
    }

    const fn suffix(self) -> &'static str {
        match self {
            Self::Standard => "1x",
            Self::High => "2x",
        }
    }
}

impl AttributionMark {
    pub const ALL: [Self; 2] = [Self::MarketDataByRithmic, Self::PoweredByOmne];
    /// Logical height; the standard asset is exactly this many pixels tall.
    pub const HEIGHT: f32 = 18.0;

    /// Logical width, equal to the standard asset's pixel width so neither
    /// density is resampled at draw time.
    #[must_use]
    pub const fn width(self, mode: aeris_design_system::ThemeMode) -> f32 {
        match (self, mode) {
            (Self::MarketDataByRithmic, _) => 140.0,
            (Self::PoweredByOmne, aeris_design_system::ThemeMode::Light) => 134.0,
            (Self::PoweredByOmne, aeris_design_system::ThemeMode::Dark) => 118.0,
        }
    }

    #[must_use]
    pub fn path(self, mode: aeris_design_system::ThemeMode, density: MarkDensity) -> SharedString {
        let name = match self {
            Self::MarketDataByRithmic => "market_data_by_rithmic",
            Self::PoweredByOmne => "powered_by_omne",
        };
        // Light themes take the dark-ink artwork and dark themes the light-ink artwork.
        let ink = match mode {
            aeris_design_system::ThemeMode::Light => "dark",
            aeris_design_system::ThemeMode::Dark => "light",
        };
        format!(
            "{PROVIDER_ASSET_PREFIX}{name}_{ink}_{}.png",
            density.suffix()
        )
        .into()
    }

    fn all_paths() -> impl Iterator<Item = SharedString> {
        Self::ALL.into_iter().flat_map(|mark| {
            [
                aeris_design_system::ThemeMode::Light,
                aeris_design_system::ThemeMode::Dark,
            ]
            .into_iter()
            .flat_map(move |mode| {
                MarkDensity::ALL
                    .into_iter()
                    .map(move |density| mark.path(mode, density))
            })
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AerisAssets;

impl AssetSource for AerisAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(drawing_asset(path)
            .or_else(|| stamp_asset(path))
            .or_else(|| ui_asset(path))
            .or_else(|| series_asset(path))
            .or_else(|| brand_asset(path))
            .or_else(|| exchange_asset(path))
            .or_else(|| provider_asset(path))
            .map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(DrawingIcon::ALL
            .iter()
            .copied()
            .map(DrawingIcon::path)
            .chain(ChartDrawingStamp::ALL.into_iter().map(stamp_icon_path))
            .chain(UiIcon::ALL.into_iter().map(UiIcon::path))
            .chain(SeriesIcon::ALL.into_iter().map(SeriesIcon::path))
            .chain(BrandAsset::ALL.into_iter().map(BrandAsset::path))
            .chain(ExchangeLogo::ALL.into_iter().map(ExchangeLogo::path))
            .chain(ProviderLogo::ALL.into_iter().map(ProviderLogo::path))
            .chain(AttributionMark::all_paths())
            .filter(|asset| path.is_empty() || asset.starts_with(path))
            .collect())
    }
}

fn ui_asset(path: &str) -> Option<&'static [u8]> {
    Some(match path.strip_prefix(UI_ASSET_PREFIX)? {
        "add.svg" => include_bytes!("../assets/icons/ui/add.svg"),
        "arrow-left.svg" => include_bytes!("../assets/icons/ui/arrow-left.svg"),
        "arrow-right-double.svg" => {
            include_bytes!("../assets/icons/ui/arrow-right-double.svg")
        }
        "arrow-right.svg" => include_bytes!("../assets/icons/ui/arrow-right.svg"),
        "camera.svg" => include_bytes!("../assets/icons/ui/camera.svg"),
        "close.svg" => include_bytes!("../assets/icons/ui/close.svg"),
        "close-bold.svg" => include_bytes!("../assets/icons/ui/close-bold.svg"),
        "chart.svg" => include_bytes!("../assets/icons/ui/chart.svg"),
        "check.svg" => include_bytes!("../assets/icons/ui/check.svg"),
        "chevron-down.svg" => include_bytes!("../assets/icons/ui/chevron-down.svg"),
        "copy.svg" => include_bytes!("../assets/icons/ui/copy.svg"),
        "download.svg" => include_bytes!("../assets/icons/ui/download.svg"),
        "copy-success.svg" => include_bytes!("../assets/icons/ui/copy-success.svg"),
        "data-panel.svg" => include_bytes!("../assets/icons/ui/data-panel.svg"),
        "trash.svg" => include_bytes!("../assets/icons/ui/trash.svg"),
        "eraser.svg" => include_bytes!("../assets/icons/ui/eraser.svg"),
        "failure.svg" => include_bytes!("../assets/icons/ui/failure.svg"),
        "lock.svg" => include_bytes!("../assets/icons/ui/lock.svg"),
        "moon.svg" => include_bytes!("../assets/icons/ui/moon.svg"),
        "redo.svg" => include_bytes!("../assets/icons/ui/redo.svg"),
        "refresh.svg" => include_bytes!("../assets/icons/ui/refresh.svg"),
        "refresh-2.svg" => include_bytes!("../assets/icons/ui/refresh-2.svg"),
        "search.svg" => include_bytes!("../assets/icons/ui/search.svg"),
        "settings.svg" => include_bytes!("../assets/icons/ui/settings.svg"),
        "sidebar-left.svg" => include_bytes!("../assets/icons/ui/sidebar-left.svg"),
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
        "line-with-markers-chart.svg" => {
            include_bytes!("../assets/icons/series/line-with-markers-chart.svg")
        }
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
        "logomark-dark.svg" => include_bytes!("../assets/aeris_assets/logomark-dark.svg"),
        "logomark-white.svg" => include_bytes!("../assets/aeris_assets/logomark-white.svg"),
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

fn provider_asset(path: &str) -> Option<&'static [u8]> {
    Some(match path.strip_prefix(PROVIDER_ASSET_PREFIX)? {
        "tastytrades_light.png" => {
            include_bytes!("../assets/exchange_assets/provider_logo/tastytrades_light.png")
        }
        "tastytrades_dark.png" => {
            include_bytes!("../assets/exchange_assets/provider_logo/tastytrades_dark.png")
        }
        "market_data_by_rithmic_light_1x.png" => {
            include_bytes!(
                "../assets/exchange_assets/provider_logo/market_data_by_rithmic_light_1x.png"
            )
        }
        "market_data_by_rithmic_light_2x.png" => {
            include_bytes!(
                "../assets/exchange_assets/provider_logo/market_data_by_rithmic_light_2x.png"
            )
        }
        "market_data_by_rithmic_dark_1x.png" => {
            include_bytes!(
                "../assets/exchange_assets/provider_logo/market_data_by_rithmic_dark_1x.png"
            )
        }
        "market_data_by_rithmic_dark_2x.png" => {
            include_bytes!(
                "../assets/exchange_assets/provider_logo/market_data_by_rithmic_dark_2x.png"
            )
        }
        "powered_by_omne_light_1x.png" => {
            include_bytes!("../assets/exchange_assets/provider_logo/powered_by_omne_light_1x.png")
        }
        "powered_by_omne_light_2x.png" => {
            include_bytes!("../assets/exchange_assets/provider_logo/powered_by_omne_light_2x.png")
        }
        "powered_by_omne_dark_1x.png" => {
            include_bytes!("../assets/exchange_assets/provider_logo/powered_by_omne_dark_1x.png")
        }
        "powered_by_omne_dark_2x.png" => {
            include_bytes!("../assets/exchange_assets/provider_logo/powered_by_omne_dark_2x.png")
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

        let stamps = ChartDrawingStamp::ALL.into_iter().map(stamp_icon_path);
        for path in DrawingIcon::ALL
            .iter()
            .map(|icon| icon.path())
            .chain(stamps)
        {
            assert!(paths.insert(path.clone()), "duplicate asset path: {path}");
            let bytes = assets
                .load(path.as_ref())
                .expect("asset lookup")
                .expect("embedded drawing icon");
            assert!(!bytes.is_empty(), "empty asset: {path}");
        }
    }

    #[test]
    fn drawing_tool_icons_share_one_theme_neutral_canvas() {
        let assets = AerisAssets;
        // The cursor and text glyphs are drawn on their own canvases and the flyout arrow is a
        // 12 px mark; every other tool glyph shares the 28 px line-art canvas.
        let own_canvas = [
            DrawingIcon::Cursor,
            DrawingIcon::Text,
            DrawingIcon::GroupArrow,
        ];

        for icon in DrawingIcon::ALL
            .iter()
            .filter(|icon| !own_canvas.contains(icon))
        {
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

    // `Icon` paints only the SVG alpha mask in one tint, so any paint other than
    // `currentColor` collapses into a solid silhouette instead of showing its color.
    #[test]
    fn monochrome_icons_paint_only_current_color() {
        let assets = AerisAssets;
        let paths = DrawingIcon::ALL
            .iter()
            .map(|icon| icon.path())
            .chain(ChartDrawingStamp::ALL.into_iter().map(stamp_icon_path))
            .chain(UiIcon::ALL.into_iter().map(UiIcon::path));

        for path in paths {
            let bytes = assets
                .load(path.as_ref())
                .expect("asset lookup")
                .expect("embedded monochrome icon");
            let svg = std::str::from_utf8(&bytes)
                .expect("UTF-8 SVG")
                .to_ascii_lowercase();
            for paint in ["#", "rgb(", "white", "black", "opacity"] {
                assert!(!svg.contains(paint), "{path} paints with {paint}");
            }
        }
    }

    #[test]
    fn horizontal_line_uses_the_horizontal_line_glyph_not_the_diagonal_ray() {
        assert_eq!(
            DrawingIcon::HorizontalLine.path().as_ref(),
            "aeris/icons/drawing/horizontal-line.svg"
        );
        assert_eq!(
            DrawingIcon::HorizontalRay.path().as_ref(),
            "aeris/icons/drawing/horizontal-ray.svg"
        );
        assert_eq!(
            DrawingIcon::Ray.path().as_ref(),
            "aeris/icons/drawing/ray.svg"
        );
    }

    #[test]
    fn stamp_icons_serve_the_artwork_the_chart_rasterizes() {
        for stamp in ChartDrawingStamp::ALL {
            let bytes = AerisAssets
                .load(stamp_icon_path(stamp).as_ref())
                .expect("asset lookup")
                .expect("embedded stamp icon");
            assert_eq!(bytes.as_ref(), stamp.svg());
        }
    }

    #[test]
    fn stacked_split_icon_is_the_side_by_side_geometry_rotated_one_quarter_turn() {
        fn path_data(svg: &str) -> &str {
            svg.split_once("<path d=\"")
                .and_then(|(_, path)| path.split_once('\"'))
                .map(|(path, _)| path)
                .expect("SVG path data")
        }

        let side_by_side_bytes = AerisAssets
            .load(UiIcon::SplitSideBySide.path().as_ref())
            .expect("asset lookup")
            .expect("embedded side-by-side split icon");
        let stacked_bytes = AerisAssets
            .load(UiIcon::SplitStacked.path().as_ref())
            .expect("asset lookup")
            .expect("embedded stacked split icon");
        let side_by_side = std::str::from_utf8(&side_by_side_bytes).expect("UTF-8 SVG");
        let stacked = std::str::from_utf8(&stacked_bytes).expect("UTF-8 SVG");

        assert_eq!(path_data(stacked), path_data(side_by_side));
        assert!(stacked.contains("transform=\"rotate(90 12 12)\""));
    }

    #[test]
    fn list_filters_the_asset_namespace() {
        let assets = AerisAssets;
        assert_eq!(
            assets.list("").expect("all assets").len(),
            DrawingIcon::ALL.len()
                + ChartDrawingStamp::ALL.len()
                + UiIcon::ALL.len()
                + SeriesIcon::ALL.len()
                + BrandAsset::ALL.len()
                + ExchangeLogo::ALL.len()
                + ProviderLogo::ALL.len()
                + AttributionMark::all_paths().count()
        );
        assert_eq!(
            assets
                .list("aeris/icons/drawing/trend")
                .expect("filtered assets"),
            vec![
                DrawingIcon::TrendLine.path(),
                DrawingIcon::TrendAngle.path()
            ]
        );
        assert_eq!(
            assets.list("hugeicons/").expect("removed namespace"),
            [] as [SharedString; 0]
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
            // Glyphs keep the grid they were drawn on (24, or 18 for marks shown at 18px)
            // so their edges stay on whole pixels at that display size.
            assert!(
                svg.contains("viewBox=\"0 0 24 24\"") || svg.contains("viewBox=\"0 0 18 18\""),
                "{path}"
            );
            assert!(svg.contains("currentColor"));
            assert!(!svg.contains('#'));
        }
    }

    #[test]
    fn provider_wordmarks_are_embedded_as_png_assets() {
        let assets = AerisAssets;
        for logo in ProviderLogo::ALL {
            let bytes = assets
                .load(logo.path().as_ref())
                .unwrap()
                .expect("provider wordmark");
            assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "{}", logo.path());
        }
    }

    #[test]
    fn attribution_marks_match_their_drawn_device_pixels_exactly() {
        let assets = AerisAssets;
        for mark in AttributionMark::ALL {
            for mode in [
                aeris_design_system::ThemeMode::Light,
                aeris_design_system::ThemeMode::Dark,
            ] {
                for (density, scale) in [(MarkDensity::Standard, 1.0), (MarkDensity::High, 2.0)] {
                    let path = mark.path(mode, density);
                    let bytes = assets
                        .load(path.as_ref())
                        .unwrap()
                        .expect("attribution mark");
                    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "{path}");
                    let dimension = |offset: usize| {
                        f64::from(u32::from_be_bytes(
                            bytes[offset..offset + 4].try_into().unwrap(),
                        ))
                    };
                    let logical_width = f64::from(mark.width(mode));
                    let logical_height = f64::from(AttributionMark::HEIGHT);
                    // The high-density artwork may round its width by a pixel.
                    assert!(
                        (dimension(16) - logical_width * scale).abs() <= 1.0,
                        "{path} width"
                    );
                    assert!(
                        (dimension(20) - logical_height * scale).abs() < f64::EPSILON,
                        "{path} height"
                    );
                }
            }
        }
        assert_eq!(MarkDensity::for_scale(1.0), MarkDensity::Standard);
        assert_eq!(MarkDensity::for_scale(1.25), MarkDensity::High);
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
            // Series marks are drawn on the 18px grid they are displayed at, so every
            // body, wick and tick edge lands on a whole pixel.
            assert!(
                svg.contains("width=\"18\" height=\"18\" viewBox=\"0 0 18 18\""),
                "{}",
                icon.path()
            );
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

        for logo in BrandAsset::ALL {
            let bytes = assets
                .load(logo.path().as_ref())
                .unwrap()
                .expect("brand logo");
            let svg = std::str::from_utf8(&bytes).unwrap();
            assert!(svg.contains("<svg"), "{}", logo.path());
            assert!(!svg.contains("<image"), "{}", logo.path());
        }

        let main_logo = assets
            .load(BrandAsset::MainLogo.path().as_ref())
            .unwrap()
            .expect("main brand logo");
        assert!(
            std::str::from_utf8(&main_logo)
                .unwrap()
                .contains("viewBox=\"0 0 54 54\"")
        );
        for mark in [BrandAsset::LogomarkDark, BrandAsset::LogomarkWhite] {
            let bytes = assets
                .load(mark.path().as_ref())
                .unwrap()
                .expect("logomark");
            let svg = std::str::from_utf8(&bytes).unwrap();
            assert!(svg.contains("viewBox=\"0 0 40 48\""), "{}", mark.path());
        }
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
