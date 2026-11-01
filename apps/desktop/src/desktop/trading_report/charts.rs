//! The report's charts as inline SVG drawn from the summary: horizontal grid lines with value
//! labels and no axis frame. Colors come from the stylesheet's theme-token classes, so the
//! markup carries no color of its own.

use super::html::{amount, escape, grouped};
use super::period::date_text;
use super::statistics::{CurrencyGroup, to_f64};
use chrono::{Datelike, NaiveDate};
use std::fmt::{self, Write as _};

/// Sized to the figure's inner width at the report's widest layout, so text draws at 1:1.
const WIDE: Frame = Frame {
    width: 1062.0,
    height: 300.0,
};
const HALF: Frame = Frame {
    width: 506.0,
    height: 240.0,
};
const TOP: f64 = 10.0;
const BOTTOM: f64 = 26.0;
const RIGHT: f64 = 16.0;
const MAXIMUM_TICKS: u8 = 12;
/// Above this many points a line keeps each bucket's extremes, which preserves its shape.
const LINE_POINT_LIMIT: usize = 1_200;
/// Above this many trading days the period chart totals months instead.
const DAILY_BAR_LIMIT: usize = 120;
const MAXIMUM_BAR_WIDTH: f64 = 28.0;

#[derive(Clone, Copy)]
struct Frame {
    width: f64,
    height: f64,
}

/// The per-period column chart and whether its columns are months rather than days.
pub(super) struct PeriodChart {
    pub(super) monthly: bool,
    pub(super) svg: String,
}

/// Cumulative P&L after each closed trade, starting from zero.
pub(super) fn cumulative(group: &CurrencyGroup) -> Result<String, fmt::Error> {
    let values = std::iter::once(0.0)
        .chain(
            group
                .curve
                .iter()
                .map(|point| whole(point.cumulative, group.scale)),
        )
        .collect::<Vec<_>>();
    line_chart(
        &values,
        WIDE,
        "primary",
        "Cumulative P&amp;L by closed trade",
    )
}

/// Distance below the running peak of the cumulative P&L, drawn below zero.
pub(super) fn drawdown(group: &CurrencyGroup) -> Result<String, fmt::Error> {
    let values = std::iter::once(0.0)
        .chain(
            group
                .curve
                .iter()
                .map(|point| -whole(point.drawdown, group.scale)),
        )
        .collect::<Vec<_>>();
    line_chart(&values, HALF, "negative", "Drawdown by closed trade")
}

/// Net P&L per exit day with trades, or per month when there are too many days to read.
pub(super) fn period(group: &CurrencyGroup) -> Result<PeriodChart, fmt::Error> {
    let monthly = group.daily.len() > DAILY_BAR_LIMIT;
    let mut bars: Vec<Bar> = Vec::new();
    for (day, breakdown) in &group.daily {
        let start = if monthly {
            day.with_day(1).unwrap_or(*day)
        } else {
            *day
        };
        match bars.last_mut() {
            Some(bar) if bar.start == start => {
                bar.net = bar.net.saturating_add(breakdown.net);
                bar.trades += breakdown.trades;
            }
            _ => bars.push(Bar {
                start,
                net: breakdown.net,
                trades: breakdown.trades,
            }),
        }
    }
    Ok(PeriodChart {
        monthly,
        svg: column_chart(&bars, monthly, group)?,
    })
}

struct Bar {
    start: NaiveDate,
    net: i128,
    trades: usize,
}

fn whole(units: i128, scale: u8) -> f64 {
    to_f64(units) / 10_f64.powi(i32::from(scale))
}

fn float(count: usize) -> f64 {
    u32::try_from(count).map_or(f64::from(u32::MAX), f64::from)
}

/// A 1, 2, 2.5 or 5 multiple of a power of ten at or above `raw`.
fn nice_step(raw: f64) -> f64 {
    if !(raw.is_finite() && raw > 0.0) {
        return 1.0;
    }
    let magnitude = 10_f64.powf(raw.log10().floor());
    let residual = raw / magnitude;
    let nice = if residual <= 1.0 {
        1.0
    } else if residual <= 2.0 {
        2.0
    } else if residual <= 2.5 {
        2.5
    } else if residual <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

/// The vertical value scale: always includes zero, rounded out to whole steps.
struct ValueAxis {
    min: f64,
    max: f64,
    step: f64,
    decimals: usize,
}

impl ValueAxis {
    fn covering(values: impl Iterator<Item = f64>) -> Self {
        let (mut low, high) = values.fold((0.0_f64, 0.0_f64), |(low, high), value| {
            (low.min(value), high.max(value))
        });
        if high - low <= f64::EPSILON {
            low = high - 1.0;
        }
        let step = nice_step((high - low) / 5.0);
        // The tolerance keeps a value that is already a whole step from gaining an empty one.
        Self {
            min: (low / step + 1e-9).floor() * step,
            max: (high / step - 1e-9).ceil() * step,
            step,
            decimals: decimals(step),
        }
    }

    fn ticks(&self) -> impl Iterator<Item = f64> + '_ {
        (0..=MAXIMUM_TICKS)
            .map(|index| self.min + self.step * f64::from(index))
            .take_while(|value| *value <= self.max + self.step / 2.0)
    }

    fn is_zero(&self, value: f64) -> bool {
        value.abs() < self.step * 1e-6
    }

    fn label(&self, value: f64) -> String {
        if self.is_zero(value) {
            return "0".to_string();
        }
        let digits = format!("{:.*}", self.decimals, value.abs());
        let (integer, fraction) = digits.split_once('.').unwrap_or((&digits, ""));
        let sign = if value < 0.0 { "-" } else { "" };
        if fraction.is_empty() {
            format!("{sign}{}", grouped(integer))
        } else {
            format!("{sign}{}.{fraction}", grouped(integer))
        }
    }
}

/// The fewest decimals that show every multiple of `step` exactly.
fn decimals(step: f64) -> usize {
    for places in 0_u8..=6 {
        let scaled = step * 10_f64.powi(i32::from(places));
        if (scaled - scaled.round()).abs() < 1e-6 * scaled.max(1.0) {
            return usize::from(places);
        }
    }
    6
}

/// The drawing area inside a frame, leaving room for the value labels on the left.
struct Plot {
    frame: Frame,
    left: f64,
}

impl Plot {
    fn new(frame: Frame, axis: &ValueAxis) -> Self {
        let widest = axis
            .ticks()
            .map(|value| axis.label(value).chars().count())
            .max()
            .unwrap_or(1);
        Self {
            frame,
            left: 7.0 * float(widest) + 12.0,
        }
    }

    const fn right(&self) -> f64 {
        self.frame.width - RIGHT
    }

    const fn bottom(&self) -> f64 {
        self.frame.height - BOTTOM
    }

    fn width(&self) -> f64 {
        self.right() - self.left
    }

    fn y(&self, axis: &ValueAxis, value: f64) -> f64 {
        let span = axis.max - axis.min;
        self.bottom() - (value - axis.min) / span * (self.bottom() - TOP)
    }

    fn open(&self, out: &mut String, tone: &str, label: &str) -> fmt::Result {
        write!(
            out,
            "<svg class=\"chart {tone}\" viewBox=\"0 0 {:.0} {:.0}\" role=\"img\" aria-label=\"{label}\">",
            self.frame.width, self.frame.height
        )
    }

    /// Horizontal grid lines with their values; the zero line is the stronger one.
    fn value_grid(&self, out: &mut String, axis: &ValueAxis) -> fmt::Result {
        out.push_str("<g class=\"grid\">");
        for value in axis.ticks() {
            let y = self.y(axis, value);
            let class = if axis.is_zero(value) {
                " class=\"zero\""
            } else {
                ""
            };
            write!(
                out,
                "<line{class} x1=\"{:.1}\" y1=\"{y:.1}\" x2=\"{:.1}\" y2=\"{y:.1}\"/>",
                self.left,
                self.right()
            )?;
        }
        out.push_str("</g><g class=\"values\">");
        for value in axis.ticks() {
            write!(
                out,
                "<text x=\"{:.1}\" y=\"{:.1}\">{}</text>",
                self.left - 8.0,
                self.y(axis, value),
                axis.label(value)
            )?;
        }
        out.push_str("</g>");
        Ok(())
    }

    fn bottom_label(&self, out: &mut String, x: f64, text: &str) -> fmt::Result {
        write!(
            out,
            "<text class=\"x\" x=\"{x:.1}\" y=\"{:.1}\">{text}</text>",
            self.bottom() + 18.0
        )
    }
}

fn line_chart(values: &[f64], frame: Frame, tone: &str, label: &str) -> Result<String, fmt::Error> {
    let axis = ValueAxis::covering(values.iter().copied());
    let plot = Plot::new(frame, &axis);
    let last = float(values.len().saturating_sub(1).max(1));
    let x = |index: f64| plot.left + index / last * plot.width();
    let mut out = String::new();
    plot.open(&mut out, tone, label)?;
    plot.value_grid(&mut out, &axis)?;
    out.push_str("<g class=\"labels\">");
    let step = nice_step(last / 6.0).max(1.0).round();
    for index in (0..=MAXIMUM_TICKS)
        .map(|tick| step * f64::from(tick))
        .take_while(|trade| *trade <= last)
    {
        plot.bottom_label(&mut out, x(index), &format!("{index:.0}"))?;
    }
    out.push_str("</g>");
    let points = line_points(values);
    let mut line = String::new();
    for (position, (index, value)) in points.iter().enumerate() {
        write!(
            line,
            "{}{:.1} {:.1}",
            if position == 0 { "M" } else { " L" },
            x(float(*index)),
            plot.y(&axis, *value)
        )?;
    }
    let zero = plot.y(&axis, 0.0);
    let first = points.first().map_or(0, |point| point.0);
    let final_index = points.last().map_or(0, |point| point.0);
    write!(
        out,
        "<path class=\"area\" d=\"{line} L{:.1} {zero:.1} L{:.1} {zero:.1} Z\"/><path class=\"line\" d=\"{line}\"/></svg>",
        x(float(final_index)),
        x(float(first)),
    )?;
    Ok(out)
}

/// Every point when few; otherwise the first, each bucket's low and high in order, and the last.
fn line_points(values: &[f64]) -> Vec<(usize, f64)> {
    if values.len() <= LINE_POINT_LIMIT {
        return values.iter().copied().enumerate().collect();
    }
    let bucket = values.len().div_ceil(LINE_POINT_LIMIT / 2);
    let mut points = Vec::with_capacity(LINE_POINT_LIMIT + 2);
    points.push((0, values[0]));
    for (chunk_index, chunk) in values.chunks(bucket).enumerate() {
        let (mut low, mut high) = (0, 0);
        for (offset, value) in chunk.iter().enumerate() {
            if *value < chunk[low] {
                low = offset;
            }
            if *value > chunk[high] {
                high = offset;
            }
        }
        for offset in [low.min(high), low.max(high)] {
            let index = chunk_index * bucket + offset;
            if points
                .last()
                .is_none_or(|point: &(usize, f64)| point.0 < index)
            {
                points.push((index, chunk[offset]));
            }
        }
    }
    let last = values.len() - 1;
    if points.last().is_some_and(|point| point.0 < last) {
        points.push((last, values[last]));
    }
    points
}

fn column_chart(bars: &[Bar], monthly: bool, group: &CurrencyGroup) -> Result<String, fmt::Error> {
    let axis = ValueAxis::covering(bars.iter().map(|bar| whole(bar.net, group.scale)));
    let plot = Plot::new(HALF, &axis);
    let band = plot.width() / float(bars.len().max(1));
    let width = (band * 0.7).clamp(1.0, MAXIMUM_BAR_WIDTH);
    let mut out = String::new();
    plot.open(
        &mut out,
        "columns",
        if monthly {
            "Net P&amp;L by exit month"
        } else {
            "Net P&amp;L by exit day"
        },
    )?;
    plot.value_grid(&mut out, &axis)?;
    let zero = plot.y(&axis, 0.0);
    out.push_str("<g class=\"bars\">");
    for (index, bar) in bars.iter().enumerate() {
        let top = plot.y(&axis, whole(bar.net, group.scale));
        let class = match bar.net.cmp(&0) {
            std::cmp::Ordering::Greater => "gain",
            std::cmp::Ordering::Less => "loss",
            std::cmp::Ordering::Equal => "flat",
        };
        let when = if monthly {
            bar.start.format("%B %Y").to_string()
        } else {
            date_text(bar.start)
        };
        write!(
            out,
            "<rect class=\"{class}\" x=\"{:.1}\" y=\"{:.1}\" width=\"{width:.1}\" height=\"{:.1}\"><title>{when}: {} {}, {} {}</title></rect>",
            plot.left + band * float(index) + (band - width) / 2.0,
            top.min(zero),
            (top - zero).abs().max(1.0),
            amount(bar.net, group.scale),
            escape(&group.currency),
            grouped(&bar.trades.to_string()),
            if bar.trades == 1 { "trade" } else { "trades" },
        )?;
    }
    out.push_str("</g><g class=\"labels\">");
    let every = bars.len().div_ceil(6).max(1);
    for (index, bar) in bars.iter().enumerate().step_by(every) {
        let label = if monthly {
            bar.start.format("%b %Y").to_string()
        } else {
            bar.start.format("%b %-d").to_string()
        };
        plot.bottom_label(&mut out, plot.left + band * (float(index) + 0.5), &label)?;
    }
    out.push_str("</g></svg>");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_axes_include_zero_and_use_round_steps() {
        let axis = ValueAxis::covering([3.0, 47.5].into_iter());
        assert_eq!((axis.min, axis.max, axis.step), (0.0, 50.0, 10.0));
        assert_eq!(axis.ticks().count(), 6);
        let axis = ValueAxis::covering([0.3].into_iter());
        assert_eq!(axis.label(axis.max), "0.3");
        let axis = ValueAxis::covering([-7.59].into_iter());
        assert_eq!((axis.min, axis.max, axis.step), (-8.0, 0.0, 2.0));
        assert_eq!(axis.label(-8.0), "-8");
        let axis = ValueAxis::covering([0.0].into_iter());
        assert!(axis.min < 0.0 && axis.max >= 0.0);
        let axis = ValueAxis::covering([12_500.0].into_iter());
        assert_eq!(axis.label(axis.max), "12,500");
    }

    #[test]
    fn long_lines_keep_their_extremes_and_endpoints() {
        let values = (0..10_000)
            .map(|index| {
                if index == 4_321 {
                    900.0
                } else {
                    f64::from(index % 7)
                }
            })
            .collect::<Vec<_>>();
        let points = line_points(&values);
        assert!(points.len() <= LINE_POINT_LIMIT + 2);
        assert_eq!(points.first(), Some(&(0, 0.0)));
        assert_eq!(points.last().map(|point| point.0), Some(9_999));
        assert!(points.contains(&(4_321, 900.0)));
        assert!(points.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }
}
