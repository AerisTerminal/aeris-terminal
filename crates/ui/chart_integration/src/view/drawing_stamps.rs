//! Built-in icon stamps for the Aeris Charts icon-stamp tool. Aeris Charts draws a chart-local
//! raster per icon name; Terminal owns the glyph artwork and tints it with platform tokens.

use super::platform_theme;
use aeris_charts_engine::{ChartEngine, ChartTheme, MAX_DRAWING_ICON_SIZE};
use aeris_design_system::{AerisTheme, ThemeColor};
use aeris_observability::diagnostic;
use num_traits::ToPrimitive;
use resvg::{tiny_skia, usvg};
use std::sync::{Arc, OnceLock};

/// Rasterized at the engine's largest accepted size so stamps stay sharp on dense displays.
const STAMP_RASTER_SIZE: u32 = MAX_DRAWING_ICON_SIZE;

/// One built-in glyph the icon-stamp tool can place on a chart.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChartDrawingStamp {
    Check,
    Cross,
    Star,
    Alert,
    Info,
    Question,
    Bolt,
    Target,
}

impl ChartDrawingStamp {
    pub const ALL: [Self; 8] = [
        Self::Check,
        Self::Cross,
        Self::Star,
        Self::Alert,
        Self::Info,
        Self::Question,
        Self::Bolt,
        Self::Target,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Check => "Check mark",
            Self::Cross => "Cross mark",
            Self::Star => "Star",
            Self::Alert => "Alert",
            Self::Info => "Info",
            Self::Question => "Question",
            Self::Bolt => "Lightning",
            Self::Target => "Target",
        }
    }

    /// Stable file stem of the stamp artwork.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Cross => "cross",
            Self::Star => "star",
            Self::Alert => "alert",
            Self::Info => "info",
            Self::Question => "question",
            Self::Bolt => "bolt",
            Self::Target => "target",
        }
    }

    /// The engine icon name saved with every stamped drawing. Renaming one orphans the stamps
    /// already saved in workspaces, so these strings are a persistence contract.
    #[must_use]
    pub const fn icon_name(self) -> &'static str {
        match self {
            Self::Check => "aeris.stamp.check",
            Self::Cross => "aeris.stamp.cross",
            Self::Star => "aeris.stamp.star",
            Self::Alert => "aeris.stamp.alert",
            Self::Info => "aeris.stamp.info",
            Self::Question => "aeris.stamp.question",
            Self::Bolt => "aeris.stamp.bolt",
            Self::Target => "aeris.stamp.target",
        }
    }

    /// Monochrome `currentColor` artwork, also served to the desktop's toolbar menu icons.
    #[must_use]
    pub const fn svg(self) -> &'static [u8] {
        match self {
            Self::Check => include_bytes!("../../assets/stamps/check.svg"),
            Self::Cross => include_bytes!("../../assets/stamps/cross.svg"),
            Self::Star => include_bytes!("../../assets/stamps/star.svg"),
            Self::Alert => include_bytes!("../../assets/stamps/alert.svg"),
            Self::Info => include_bytes!("../../assets/stamps/info.svg"),
            Self::Question => include_bytes!("../../assets/stamps/question.svg"),
            Self::Bolt => include_bytes!("../../assets/stamps/bolt.svg"),
            Self::Target => include_bytes!("../../assets/stamps/target.svg"),
        }
    }

    /// The platform token a stamp is painted with, on the chart and in the toolbar alike.
    #[must_use]
    pub fn color(self, theme: &AerisTheme) -> ThemeColor {
        let colors = theme.colors;
        match self {
            Self::Check => colors.positive,
            Self::Cross | Self::Target => colors.negative,
            Self::Star | Self::Alert => colors.warning,
            Self::Info => colors.primary,
            Self::Question => colors.indigo,
            Self::Bolt => colors.purple,
        }
    }
}

struct StampRaster {
    name: &'static str,
    pixels: Arc<[u8]>,
}

/// Registers every built-in stamp tinted for `theme`. Charts share one raster set per theme.
pub(super) fn register_drawing_stamps(engine: &mut ChartEngine, theme: ChartTheme) {
    for raster in stamp_rasters(theme) {
        let registered = engine.set_drawing_icon(
            raster.name,
            STAMP_RASTER_SIZE,
            STAMP_RASTER_SIZE,
            Arc::clone(&raster.pixels),
        );
        if !registered {
            diagnostic!("Aeris Charts rejected drawing stamp {}", raster.name);
        }
    }
}

fn stamp_rasters(theme: ChartTheme) -> &'static [StampRaster] {
    static LIGHT: OnceLock<Vec<StampRaster>> = OnceLock::new();
    static DARK: OnceLock<Vec<StampRaster>> = OnceLock::new();
    let rasters = match theme {
        ChartTheme::Light => &LIGHT,
        ChartTheme::Dark => &DARK,
    };
    rasters.get_or_init(|| {
        let platform = platform_theme(theme);
        ChartDrawingStamp::ALL
            .into_iter()
            .filter_map(
                |stamp| match rasterize_stamp(stamp, stamp.color(&platform)) {
                    Ok(pixels) => Some(StampRaster {
                        name: stamp.icon_name(),
                        pixels,
                    }),
                    Err(error) => {
                        diagnostic!("drawing stamp {} is unavailable: {error}", stamp.key());
                        None
                    }
                },
            )
            .collect()
    })
}

/// Straight-alpha RGBA8 of the stamp artwork. The artwork paints only `currentColor`, so its
/// coverage is the whole image and the token supplies the color.
fn rasterize_stamp(stamp: ChartDrawingStamp, tint: ThemeColor) -> Result<Arc<[u8]>, String> {
    let tree = usvg::Tree::from_data(stamp.svg(), &usvg::Options::default())
        .map_err(|error| error.to_string())?;
    let mut pixmap = tiny_skia::Pixmap::new(STAMP_RASTER_SIZE, STAMP_RASTER_SIZE)
        .ok_or_else(|| "empty raster size".to_owned())?;
    let svg = tree.size();
    let fit = STAMP_RASTER_SIZE.to_f32().unwrap_or(1.0) / svg.width().max(svg.height());
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(fit, fit),
        &mut pixmap.as_mut(),
    );
    let [_, red, green, blue] = tint.rgb_u32().to_be_bytes();
    Ok(pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| [red, green, blue, pixel.demultiply().alpha()])
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stamp_rasterizes_tinted_artwork_within_engine_limits() {
        for theme in [ChartTheme::Light, ChartTheme::Dark] {
            let rasters = stamp_rasters(theme);
            assert_eq!(rasters.len(), ChartDrawingStamp::ALL.len());
            let platform = platform_theme(theme);
            for (stamp, raster) in ChartDrawingStamp::ALL.into_iter().zip(rasters) {
                assert_eq!(raster.name, stamp.icon_name());
                assert!(raster.name.len() <= aeris_charts_engine::MAX_DRAWING_ICON_NAME_BYTES);
                let side = usize::try_from(STAMP_RASTER_SIZE).expect("raster side");
                assert_eq!(raster.pixels.len(), side * side * 4);
                let [_, red, green, blue] = stamp.color(&platform).rgb_u32().to_be_bytes();
                let painted = raster
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .filter(|pixel| pixel[3] > 0)
                    .inspect(|pixel| assert_eq!(pixel[..3], [red, green, blue]))
                    .count();
                assert!(
                    painted > side * side / 10,
                    "{} paints too little",
                    stamp.key()
                );
            }
        }
    }

    #[test]
    fn stamp_artwork_paints_only_current_color() {
        for stamp in ChartDrawingStamp::ALL {
            let svg = std::str::from_utf8(stamp.svg())
                .expect("UTF-8 SVG")
                .to_ascii_lowercase();
            assert!(svg.contains("viewbox=\"0 0 28 28\""), "{}", stamp.key());
            assert!(svg.contains("currentcolor"), "{}", stamp.key());
            for paint in ["#", "rgb(", "white", "black", "opacity"] {
                assert!(!svg.contains(paint), "{} paints with {paint}", stamp.key());
            }
        }
    }
}
