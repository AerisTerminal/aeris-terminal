//! Chart capture: the Aeris Charts frame with the product legend and the Aeris logo lockup,
//! copied to the clipboard or saved as PNG.
//!
//! Aeris Charts captures and rasterizes the chart (panes, axes and trading layer, never the
//! crosshair). Terminal adds only its own presentation on top: the legend rows it shows on
//! screen and the brand mark, both laid out with the same bundled faces the window uses.

use super::{
    AerisChartView, LEGEND_INSET, LEGEND_MAX_WIDTH, LEGEND_ROW_HEIGHT, LegendRow, LegendValueTone,
    format_utc_offset, platform_theme,
};
use aeris_charts_native::{
    ImageExportOptions, PreparedChartImage, measure_text, prepare_engine_image, register_font_data,
};
use aeris_charts_render::color::Color;
use aeris_charts_render::draw_list::{Prim, RasterImage, TextAlign};
use aeris_design_system::{
    BRAND_FONT_BYTES, PLATFORM_FONT_BYTES, ThemeColor, TypographyRole, brand_font_family,
    platform_font_stack, platform_typography,
};
use aeris_observability::diagnostic;
#[cfg(not(windows))]
use gpui::{ClipboardItem, Image, ImageFormat};
use gpui::{Context, Task};
use num_traits::ToPrimitive;
use resvg::{tiny_skia, usvg};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Device pixels per chart pixel: sharp on high-density screens and in shared posts.
const EXPORT_SCALE: f32 = 2.0;
/// The on-screen legend's `text_xs` size, gap between readouts, and row padding.
const LEGEND_TEXT_SIZE: f32 = 12.0;
const LEGEND_RUN_GAP: f32 = 8.0;
const LEGEND_PADDING_X: f32 = 4.0;
/// The brand lockup, sized to read clearly in a shared image without crowding the plot.
const BRAND_LOGO_SIZE: f32 = 32.0;
const BRAND_WORDMARK: &str = "Aeris";
const BRAND_WORDMARK_SIZE: f32 = 20.0;
const BRAND_GAP: f32 = 8.0;
const BRAND_INSET: f32 = 12.0;
const BRAND_LOGO_SVG: &[u8] =
    include_bytes!("../../../../../apps/desktop/assets/aeris_assets/logo.svg");
/// Raster identity for executors that cache images by key.
const BRAND_LOGO_IMAGE_KEY: u64 = 0xae15_b0a0_0000_0000;

/// A chart frame captured on the UI thread, rasterized later on a background thread.
struct ChartCapture {
    image: PreparedChartImage,
    /// "Created with Aeris" and the capture time in the chart's time zone.
    caption: String,
    file_name: String,
    legend: Vec<ExportLegendRow>,
    palette: ExportPalette,
}

/// The image caption and file name for a capture at `utc_seconds`, both read in the chart's
/// time zone like its time axis: "Created with Aeris, Oct 03, 2026 07:10 UTC-4" and
/// "BTC_2026-10-03_07-10-12.png".
fn capture_labels(symbol: &str, time_zone: &str, utc_seconds: i64) -> (String, String) {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let symbol: String = symbol
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.'))
        .collect();
    let symbol = if symbol.is_empty() { "Aeris" } else { &symbol };
    let Some(time) = aeris_charts_engine::ChartTimeZone::parse(time_zone)
        .and_then(|zone| zone.local_parts(utc_seconds))
    else {
        return ("Created with Aeris".to_owned(), format!("{symbol}.png"));
    };
    let month = time
        .month
        .checked_sub(1)
        .and_then(|index| MONTHS.get(usize::try_from(index).ok()?))
        .copied()
        .unwrap_or("---");
    let caption = format!(
        "Created with Aeris, {month} {:02}, {} {:02}:{:02} {}",
        time.day,
        time.year,
        time.hour,
        time.minute,
        format_utc_offset(time.offset_seconds)
    );
    let file_name = format!(
        "{symbol}_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.png",
        time.year, time.month, time.day, time.hour, time.minute, time.second
    );
    (caption, file_name)
}

#[derive(Debug)]
struct ExportLegendRow {
    pane: usize,
    runs: Vec<ExportTextRun>,
}

#[derive(Debug)]
struct ExportTextRun {
    text: String,
    color: Color,
    weight: u16,
}

#[derive(Clone, Copy, Debug)]
struct ExportPalette {
    text: Color,
    muted: Color,
    bullish: Color,
    bearish: Color,
}

impl ChartCapture {
    /// Rasterize the chart with its legend and brand lockup as PNG bytes. Runs off the UI thread.
    fn render_png(&self) -> Result<Vec<u8>, String> {
        self.image.render_png(&self.overlay()?)
    }

    /// Rasterize the capture and place it on the Windows clipboard. Runs off the UI thread.
    ///
    /// GPUI's Windows clipboard offers images only as the registered "PNG" format, which most
    /// Windows apps never paste. arboard also writes `CF_DIBV5`, from which Windows derives
    /// `CF_DIB` and `CF_BITMAP`, so the capture pastes wherever a browser-copied image does.
    #[cfg(windows)]
    fn copy_to_clipboard(&self) -> Result<(), String> {
        let image = self.image.render(&self.overlay()?)?;
        let dimension = |value: u32| usize::try_from(value).map_err(|error| error.to_string());
        let image = arboard::ImageData {
            width: dimension(image.width)?,
            height: dimension(image.height)?,
            bytes: image.pixels.into(),
        };
        arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_image(image))
            .map_err(|error| error.to_string())
    }

    fn overlay(&self) -> Result<Vec<Prim>, String> {
        register_export_fonts()?;
        let mut overlay = Vec::new();
        self.caption_overlay(&mut overlay);
        self.legend_overlay(&mut overlay);
        self.brand_overlay(&mut overlay)?;
        Ok(overlay)
    }

    /// The caption on the first legend row of the top plot, muted so the legend leads.
    fn caption_overlay(&self, overlay: &mut Vec<Prim>) {
        let Some([x, y, _, _]) = self.image.pane_rects().first().copied() else {
            return;
        };
        let [x, y] = [x, y].map(|value| value.to_f32().unwrap_or(0.0));
        let scale = self.image.pixel_ratio();
        overlay.push(Prim::Text {
            x: x + (LEGEND_INSET + LEGEND_PADDING_X) * scale,
            y: y + (LEGEND_INSET + LEGEND_ROW_HEIGHT / 2.0) * scale,
            text: self.caption.clone(),
            color: self.palette.muted,
            size: LEGEND_TEXT_SIZE * scale,
            family: platform_font_stack().to_owned(),
            align: TextAlign::Left,
            weight: platform_typography().weight(TypographyRole::Normal),
            italic: false,
        });
    }

    /// The legend rows laid out like the on-screen legend: inset from each pane's plot corner
    /// (below the caption in the top plot), wrapping readout by readout within the plot and
    /// stopping before it runs off the pane.
    fn legend_overlay(&self, overlay: &mut Vec<Prim>) {
        let scale = self.image.pixel_ratio();
        let family = platform_font_stack();
        let size = LEGEND_TEXT_SIZE * scale;
        let row_height = LEGEND_ROW_HEIGHT * scale;
        let inset = LEGEND_INSET * scale;
        let padding = LEGEND_PADDING_X * scale;
        let gap = LEGEND_RUN_GAP * scale;
        for (pane, rect) in self.image.pane_rects().into_iter().enumerate() {
            let [x, y, width, height] = rect.map(|value| value.to_f32().unwrap_or(0.0));
            let available = (width / scale - LEGEND_INSET * 2.0).clamp(0.0, LEGEND_MAX_WIDTH);
            let line_start = x + inset + padding;
            let line_end = x + inset + available * scale - padding;
            let bottom = y + height - inset;
            let mut row_top = y + inset + if pane == 0 { row_height } else { 0.0 };
            'rows: for row in self.legend.iter().filter(|row| row.pane == pane) {
                let mut pen = line_start;
                for run in &row.runs {
                    let advance = measure_text(&run.text, size, family, run.weight, false)
                        .unwrap_or_default();
                    if pen > line_start && pen + advance > line_end {
                        row_top += row_height;
                        pen = line_start;
                    }
                    if row_top + row_height > bottom {
                        break 'rows;
                    }
                    overlay.push(Prim::Text {
                        x: pen,
                        y: row_top + row_height / 2.0,
                        text: run.text.clone(),
                        color: run.color,
                        size,
                        family: family.to_owned(),
                        align: TextAlign::Left,
                        weight: run.weight,
                        italic: false,
                    });
                    pen += advance + gap;
                }
                row_top += row_height;
            }
        }
    }

    /// The Aeris lockup (logo mark beside the wordmark, as in the window header) in the
    /// bottom-left corner of the lowest plot.
    fn brand_overlay(&self, overlay: &mut Vec<Prim>) -> Result<(), String> {
        let Some([x, y, _, height]) = self.image.pane_rects().last().copied() else {
            return Ok(());
        };
        let [x, y, height] = [x, y, height].map(|value| value.to_f32().unwrap_or(0.0));
        let scale = self.image.pixel_ratio();
        let logo_size = (BRAND_LOGO_SIZE * scale).round();
        let left = x + (LEGEND_INSET + LEGEND_PADDING_X) * scale;
        let top = y + height - BRAND_INSET * scale - logo_size;
        let logo = brand_logo(logo_size.to_u32().unwrap_or(1).max(1))?;
        overlay.push(Prim::Image {
            image: logo,
            rect: [left, top, logo_size, logo_size],
            opacity: 1.0,
        });
        overlay.push(Prim::Text {
            x: left + logo_size + BRAND_GAP * scale,
            y: top + logo_size / 2.0,
            text: BRAND_WORDMARK.to_owned(),
            color: self.palette.text,
            size: BRAND_WORDMARK_SIZE * scale,
            family: brand_font_family().to_owned(),
            align: TextAlign::Left,
            weight: 400,
            italic: false,
        });
        Ok(())
    }
}

/// The bundled Aeris logo rasterized at `size` device pixels, keeping its colors and depth.
fn brand_logo(size: u32) -> Result<RasterImage, String> {
    let tree = usvg::Tree::from_data(BRAND_LOGO_SVG, &usvg::Options::default())
        .map_err(|error| format!("brand logo: {error}"))?;
    let mut pixmap =
        tiny_skia::Pixmap::new(size, size).ok_or_else(|| "brand logo: empty size".to_owned())?;
    let svg = tree.size();
    let fit = size.to_f32().unwrap_or(1.0) / svg.width().max(svg.height());
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(fit, fit),
        &mut pixmap.as_mut(),
    );
    let pixels: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    Ok(RasterImage {
        key: BRAND_LOGO_IMAGE_KEY ^ u64::from(size),
        width: size,
        height: size,
        pixels: pixels.into(),
    })
}

/// Native export paints with the faces the window bundles instead of whatever the OS installs.
fn register_export_fonts() -> Result<(), String> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            for face in PLATFORM_FONT_BYTES.iter().chain([&BRAND_FONT_BYTES]) {
                register_font_data(face.to_vec())?;
            }
            Ok(())
        })
        .clone()
}

fn chart_color(color: ThemeColor) -> Color {
    let channel = |value: f32| {
        (value * 255.0)
            .round()
            .clamp(0.0, 255.0)
            .to_u8()
            .unwrap_or(0)
    };
    Color::rgba(
        channel(color.red()),
        channel(color.green()),
        channel(color.blue()),
        channel(color.alpha()),
    )
}

fn export_legend_rows(rows: Vec<LegendRow>, palette: ExportPalette) -> Vec<ExportLegendRow> {
    let typography = platform_typography();
    rows.into_iter()
        .filter(|row| row.visible)
        .map(|row| {
            let tone = match row.values_tone {
                LegendValueTone::Neutral => palette.muted,
                LegendValueTone::Bullish => palette.bullish,
                LegendValueTone::Bearish => palette.bearish,
            };
            let mut runs = vec![ExportTextRun {
                text: row.title,
                color: palette.text,
                weight: typography.weight(TypographyRole::Normal),
            }];
            runs.extend(row.values.into_iter().map(|value| {
                ExportTextRun {
                    color: value
                        .color
                        .as_deref()
                        .and_then(Color::parse_css)
                        .unwrap_or(tone),
                    text: value.text,
                    weight: typography.weight(TypographyRole::Normal),
                }
            }));
            ExportLegendRow {
                pane: row.pane,
                runs,
            }
        })
        .collect()
}

fn export_directory() -> PathBuf {
    std::env::home_dir().map_or_else(PathBuf::new, |home| {
        let pictures = home.join("Pictures");
        if pictures.is_dir() { pictures } else { home }
    })
}

impl AerisChartView {
    /// Capture the chart as it is shown, with its latest-bar legend and no crosshair. The
    /// capture invalidates the engine's retained frame, so callers repaint the live chart.
    fn capture(&mut self, utc_seconds: i64) -> Result<ChartCapture, String> {
        let image = prepare_engine_image(
            &mut self.engine,
            ImageExportOptions {
                width: 0,
                height: 0,
                scale: EXPORT_SCALE,
                include_crosshair: false,
                include_trading: true,
            },
        );
        let colors = platform_theme(self.theme).colors;
        let appearance = self.appearance_settings();
        let tone = |value: String, fallback: ThemeColor| {
            Color::parse_css(&value).unwrap_or_else(|| chart_color(fallback))
        };
        let palette = ExportPalette {
            text: chart_color(colors.text_primary),
            muted: chart_color(colors.text_secondary),
            bullish: tone(
                appearance.effective_up_color(self.theme),
                colors.text_secondary,
            ),
            bearish: tone(
                appearance.effective_down_color(self.theme),
                colors.text_secondary,
            ),
        };
        let (caption, file_name) =
            capture_labels(&self.asset_symbol, self.time_zone_id(), utc_seconds);
        Ok(ChartCapture {
            image: image?,
            caption,
            file_name,
            legend: export_legend_rows(self.legend_rows_at(None), palette),
            palette,
        })
    }

    /// Copy the chart capture to the clipboard as PNG. The task resolves to whether the
    /// clipboard now holds the capture.
    pub fn copy_capture(&mut self, cx: &mut Context<Self>) -> Task<bool> {
        let captured = self.capture(unix_now());
        cx.notify();
        let export = match captured {
            Ok(export) => export,
            Err(error) => {
                diagnostic!("Chart capture copy failed: {error}");
                return Task::ready(false);
            }
        };
        #[cfg(windows)]
        {
            let copy = cx
                .background_executor()
                .spawn(async move { export.copy_to_clipboard() });
            cx.spawn(async move |_, _| match copy.await {
                Ok(()) => true,
                Err(error) => {
                    diagnostic!("Chart capture copy failed: {error}");
                    false
                }
            })
        }
        #[cfg(not(windows))]
        {
            let render = cx
                .background_executor()
                .spawn(async move { export.render_png() });
            cx.spawn(async move |_, cx| match render.await {
                Ok(png) => {
                    cx.update(|cx| {
                        cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(
                            ImageFormat::Png,
                            png,
                        )));
                    });
                    true
                }
                Err(error) => {
                    diagnostic!("Chart capture copy failed: {error}");
                    false
                }
            })
        }
    }

    /// Ask where to save the chart capture, then write it as PNG off the UI thread.
    pub fn save_capture(&mut self, cx: &mut Context<Self>) {
        let captured = self.capture(unix_now());
        cx.notify();
        let export = match captured {
            Ok(export) => export,
            Err(error) => {
                diagnostic!("Chart capture save failed: {error}");
                return;
            }
        };
        let destination = cx.prompt_for_new_path(&export_directory(), Some(&export.file_name));
        cx.spawn(async move |_, cx| {
            let path = match destination.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    diagnostic!("Chart capture save failed: {error}");
                    return;
                }
            };
            let written = cx
                .background_executor()
                .spawn(async move { write_png(&export, &path) })
                .await;
            if let Err(error) = written {
                diagnostic!("Chart capture save failed: {error}");
            }
        })
        .detach();
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

fn write_png(export: &ChartCapture, path: &Path) -> Result<(), String> {
    let png = export.render_png()?;
    std::fs::write(path, png).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{AerisChartView, Prim, capture_labels};

    #[test]
    fn exported_image_carries_the_latest_legend_and_brand_without_moving_the_live_chart() {
        let mut chart = AerisChartView::new();
        chart
            .engine
            .recompute_layout_with_measure(true, |_, _| 48.0, |_, _| 48.0);
        chart.engine.fit_content();
        let live = (
            chart.engine.css_width,
            chart.engine.pane_w,
            chart.engine.axis_w,
        );
        chart.engine.crosshair = Some((40.0, 40.0));

        let export = chart
            .capture(1_791_025_812)
            .expect("a laid-out chart exports");
        let mut overlay = Vec::new();
        export.caption_overlay(&mut overlay);
        export.legend_overlay(&mut overlay);
        export
            .brand_overlay(&mut overlay)
            .expect("the bundled logo rasterizes");
        let painted_logo = |prim: &Prim| {
            matches!(prim, Prim::Image { image, .. }
                if image.width > 0 && image.pixels.chunks(4).any(|pixel| pixel[3] > 0))
        };
        assert!(overlay.iter().any(painted_logo));
        let texts: Vec<&str> = overlay
            .iter()
            .filter_map(|prim| match prim {
                Prim::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let title = &chart.legend_rows_at(None)[0].title;
        assert!(texts[0].starts_with("Created with Aeris, "));
        assert_eq!(texts.get(1), Some(&title.as_str()));
        assert_eq!(texts.last(), Some(&"Aeris"));
        let [plot_x, plot_y, plot_w, _] = export.image.pane_rects()[0];
        let row_y = |prim: &Prim| match prim {
            Prim::Text { x, y, .. } => {
                assert!(f64::from(*x) > f64::from(plot_x));
                assert!(f64::from(*x) < f64::from(plot_x + plot_w));
                f64::from(*y)
            }
            _ => panic!("caption and legend are text"),
        };
        let caption_y = row_y(&overlay[0]);
        assert!(caption_y > f64::from(plot_y));
        assert!(
            row_y(&overlay[1]) > caption_y,
            "the legend sits below the caption"
        );

        let png = export
            .render_png()
            .expect("bundled fonts render the export");
        assert!(png.starts_with(b"\x89PNG"));
        assert_eq!(
            (
                chart.engine.css_width,
                chart.engine.pane_w,
                chart.engine.axis_w
            ),
            live
        );
        assert_eq!(chart.engine.crosshair, Some((40.0, 40.0)));
    }

    #[test]
    fn capture_labels_read_the_chart_time_zone_and_keep_portable_file_names() {
        // 2026-10-03 11:10:12 UTC, 07:10:12 in New York daylight time.
        let instant = 1_791_025_812;
        assert_eq!(
            capture_labels("NQZ2026", "America/New_York", instant),
            (
                "Created with Aeris, Oct 03, 2026 07:10 UTC-4".to_owned(),
                "NQZ2026_2026-10-03_07-10-12.png".to_owned()
            )
        );
        assert_eq!(
            capture_labels("BTC/USD", "Etc/UTC", instant).1,
            "BTCUSD_2026-10-03_11-10-12.png"
        );
        assert_eq!(
            capture_labels(" / ", "Etc/UTC", instant).1,
            "Aeris_2026-10-03_11-10-12.png"
        );
    }
}
