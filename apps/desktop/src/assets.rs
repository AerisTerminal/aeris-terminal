use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

const DRAWING_ASSET_PREFIX: &str = "axiusflow/icons/drawing/";
const UI_ASSET_PREFIX: &str = "axiusflow/icons/ui/";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiIcon {
    ActivityIcon01,
    AddIcon01,
    AiEraser,
    AiLock,
    ArrowLeftIcon01,
    ArrowRightDouble,
    ArrowRightIcon01,
    Brush,
    CancelIcon01,
    ChartLineDataIcon02,
    CheckmarkCircleIcon01,
    CursorIcon01,
    DeleteIcon02,
    ExchangeIcon01,
    FitToScreen,
    Lock,
    MoonIcon02,
    SearchIcon01,
    SidebarRightIcon01,
    SunIcon03,
    Text,
    WindowClose,
    WindowMaximize,
    WindowMinimize,
    WindowRestore,
}

impl UiIcon {
    pub const ALL: [Self; 25] = [
        Self::ActivityIcon01,
        Self::AddIcon01,
        Self::AiEraser,
        Self::AiLock,
        Self::ArrowLeftIcon01,
        Self::ArrowRightDouble,
        Self::ArrowRightIcon01,
        Self::Brush,
        Self::CancelIcon01,
        Self::ChartLineDataIcon02,
        Self::CheckmarkCircleIcon01,
        Self::CursorIcon01,
        Self::DeleteIcon02,
        Self::ExchangeIcon01,
        Self::FitToScreen,
        Self::Lock,
        Self::MoonIcon02,
        Self::SearchIcon01,
        Self::SidebarRightIcon01,
        Self::SunIcon03,
        Self::Text,
        Self::WindowClose,
        Self::WindowMaximize,
        Self::WindowMinimize,
        Self::WindowRestore,
    ];

    #[must_use]
    pub fn path(self) -> SharedString {
        let name = match self {
            Self::ActivityIcon01 => "activity-01.svg",
            Self::AddIcon01 => "add-01.svg",
            Self::AiEraser => "ai-eraser.svg",
            Self::AiLock => "ai-lock.svg",
            Self::ArrowLeftIcon01 => "arrow-left-01.svg",
            Self::ArrowRightDouble => "arrow-right-double.svg",
            Self::ArrowRightIcon01 => "arrow-right-01.svg",
            Self::Brush => "brush.svg",
            Self::CancelIcon01 => "cancel-01.svg",
            Self::ChartLineDataIcon02 => "chart-line-data-02.svg",
            Self::CheckmarkCircleIcon01 => "checkmark-circle-01.svg",
            Self::CursorIcon01 => "cursor-01.svg",
            Self::DeleteIcon02 => "delete-02.svg",
            Self::ExchangeIcon01 => "exchange-01.svg",
            Self::FitToScreen => "fit-to-screen.svg",
            Self::Lock => "lock.svg",
            Self::MoonIcon02 => "moon-02.svg",
            Self::SearchIcon01 => "search-01.svg",
            Self::SidebarRightIcon01 => "sidebar-right-01.svg",
            Self::SunIcon03 => "sun-03.svg",
            Self::Text => "text.svg",
            Self::WindowClose => "window-close.svg",
            Self::WindowMaximize => "window-maximize.svg",
            Self::WindowMinimize => "window-minimize.svg",
            Self::WindowRestore => "window-restore.svg",
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
        Ok(drawing_asset(path)
            .or_else(|| ui_asset(path))
            .map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(DrawingIcon::ALL
            .into_iter()
            .map(DrawingIcon::path)
            .chain(UiIcon::ALL.into_iter().map(UiIcon::path))
            .filter(|asset| path.is_empty() || asset.starts_with(path))
            .collect())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DesktopAssets;

impl AssetSource for DesktopAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.starts_with(DRAWING_ASSET_PREFIX) || path.starts_with(UI_ASSET_PREFIX) {
            AxiusflowAssets.load(path)
        } else {
            gpui_component_assets::Assets.load(path)
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut assets = AxiusflowAssets.list(path)?;
        for asset in gpui_component_assets::Assets.list(path)? {
            if !assets.contains(&asset) {
                assets.push(asset);
            }
        }
        Ok(assets)
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

fn ui_asset(path: &str) -> Option<&'static [u8]> {
    Some(match path.strip_prefix(UI_ASSET_PREFIX)? {
        "activity-01.svg" => include_bytes!("../assets/icons/ui/activity-01.svg"),
        "add-01.svg" => include_bytes!("../assets/icons/ui/add-01.svg"),
        "ai-eraser.svg" => include_bytes!("../assets/icons/ui/ai-eraser.svg"),
        "ai-lock.svg" => include_bytes!("../assets/icons/ui/ai-lock.svg"),
        "arrow-left-01.svg" => include_bytes!("../assets/icons/ui/arrow-left-01.svg"),
        "arrow-right-double.svg" => {
            include_bytes!("../assets/icons/ui/arrow-right-double.svg")
        }
        "arrow-right-01.svg" => include_bytes!("../assets/icons/ui/arrow-right-01.svg"),
        "brush.svg" => include_bytes!("../assets/icons/ui/brush.svg"),
        "cancel-01.svg" => include_bytes!("../assets/icons/ui/cancel-01.svg"),
        "chart-line-data-02.svg" => {
            include_bytes!("../assets/icons/ui/chart-line-data-02.svg")
        }
        "checkmark-circle-01.svg" => {
            include_bytes!("../assets/icons/ui/checkmark-circle-01.svg")
        }
        "cursor-01.svg" => include_bytes!("../assets/icons/ui/cursor-01.svg"),
        "delete-02.svg" => include_bytes!("../assets/icons/ui/delete-02.svg"),
        "exchange-01.svg" => include_bytes!("../assets/icons/ui/exchange-01.svg"),
        "fit-to-screen.svg" => include_bytes!("../assets/icons/ui/fit-to-screen.svg"),
        "lock.svg" => include_bytes!("../assets/icons/ui/lock.svg"),
        "moon-02.svg" => include_bytes!("../assets/icons/ui/moon-02.svg"),
        "search-01.svg" => include_bytes!("../assets/icons/ui/search-01.svg"),
        "sidebar-right-01.svg" => include_bytes!("../assets/icons/ui/sidebar-right-01.svg"),
        "sun-03.svg" => include_bytes!("../assets/icons/ui/sun-03.svg"),
        "text.svg" => include_bytes!("../assets/icons/ui/text.svg"),
        "window-close.svg" => include_bytes!("../assets/icons/ui/window-close.svg"),
        "window-maximize.svg" => include_bytes!("../assets/icons/ui/window-maximize.svg"),
        "window-minimize.svg" => include_bytes!("../assets/icons/ui/window-minimize.svg"),
        "window-restore.svg" => include_bytes!("../assets/icons/ui/window-restore.svg"),
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
            DrawingIcon::ALL.len() + UiIcon::ALL.len()
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
    fn desktop_source_composes_drawing_and_window_control_assets() {
        let assets = DesktopAssets;
        assert!(
            assets
                .load(DrawingIcon::TrendLine.path().as_ref())
                .unwrap()
                .is_some()
        );
        for icon in [
            UiIcon::WindowMinimize,
            UiIcon::WindowMaximize,
            UiIcon::WindowRestore,
            UiIcon::WindowClose,
        ] {
            let path = icon.path();
            assert!(
                assets.load(path.as_ref()).unwrap().is_some(),
                "missing {path}"
            );
        }
    }
}
