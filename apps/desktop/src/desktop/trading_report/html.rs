//! The report as one self-contained HTML document: inline CSS from the workspace theme's tokens,
//! inline SVG charts, the bundled fonts and logo as data URIs, no scripts and nothing loaded
//! from elsewhere. A content-security policy forbids scripts and remote
//! loads even after an edit.

use super::super::assets::BrandAsset;
use super::charts;
use super::output::fingerprint;
use super::period::{ReportPeriod, ReportSpan, ReportZone, date_text, offset_text};
use super::statistics::{
    AccountLine, Breakdown, CurrencyGroup, ReportSource, ReportSummary, ReportTrade, Summary,
};
use super::{grid_price_text, market_price_text};
use aeris_design_system::{
    AerisTheme, RadiusToken, ThemeMode, brand_font_family, platform_font_family,
    platform_font_stack, platform_numeric_font_family, platform_numeric_font_stack,
};
use aeris_localization::{Text, panels, tr, tr_count};
use aeris_trading::{AccountEnvironment, FixedPoint, OrderSide};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{Datelike, Timelike};
use std::fmt::{self, Write as _};

/// Brackets the fingerprinted content, so anyone can recompute the hash from the file.
const CONTENT_START: &str = "<!--aeris-report-content-->";
const CONTENT_END: &str = "<!--/aeris-report-content-->";
const WEEKDAYS: [Text; 7] = [
    panels::WEEKDAY_MONDAY,
    panels::WEEKDAY_TUESDAY,
    panels::WEEKDAY_WEDNESDAY,
    panels::WEEKDAY_THURSDAY,
    panels::WEEKDAY_FRIDAY,
    panels::WEEKDAY_SATURDAY,
    panels::WEEKDAY_SUNDAY,
];

/// Everything one rendered report shows.
pub(super) struct ReportDocument<'a> {
    pub(super) version: &'static str,
    pub(super) generated_unix_nanos: i64,
    pub(super) zone: ReportZone,
    pub(super) span: ReportSpan,
    pub(super) period: ReportPeriod,
    /// "All accounts" or the one account's name.
    pub(super) scope: &'a str,
    pub(super) truncated: bool,
    pub(super) summary: &'a ReportSummary,
    /// The workspace theme when the report was requested.
    pub(super) theme: AerisTheme,
}

/// How the trade list prints times: in the report zone, with each time's UTC offset when the
/// period crosses a daylight-saving change and one offset no longer describes every time.
#[derive(Clone, Copy)]
struct TradeTimes {
    zone: ReportZone,
    with_offset: bool,
}

impl TradeTimes {
    fn text(self, unix_nanos: i64) -> String {
        let local = time_text(self.zone, unix_nanos);
        if self.with_offset {
            format!("{local} {}", offset_text(self.zone.offset_at(unix_nanos)))
        } else {
            local
        }
    }
}

pub(super) fn render(document: &ReportDocument<'_>) -> Result<String, fmt::Error> {
    let mut content = String::new();
    write_content(&mut content, document)?;
    let fingerprint = fingerprint(&content);
    let mut html = String::with_capacity(content.len() + 8_192);
    html.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:\">\n",
    );
    html.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    write!(
        html,
        "<meta name=\"generator\" content=\"Aeris Terminal {}\">\n<title>{}</title>\n<style>\n{}</style>\n</head>\n<body>\n",
        escape(document.version),
        tr!(
            panels::REPORT_HTML_TITLE,
            from = date_text(document.span.first_day),
            to = date_text(document.span.last_day)
        ),
        stylesheet(&document.theme)?,
    )?;
    html.push_str(CONTENT_START);
    html.push_str(&content);
    html.push_str(CONTENT_END);
    write!(
        html,
        "\n<footer class=\"provenance\">\n<p>{} <code>{fingerprint}</code></p>\n<p>{}</p>\n</footer>\n</body>\n</html>\n",
        tr!(panels::REPORT_FINGERPRINT_LABEL),
        tr!(panels::REPORT_FINGERPRINT_EXPLANATION),
    )?;
    Ok(html)
}

fn write_content(out: &mut String, document: &ReportDocument<'_>) -> fmt::Result {
    let summary = document.summary;
    let offsets = document.zone.offsets_during(document.span);
    let times = TradeTimes {
        zone: document.zone,
        with_offset: offsets.len() > 1,
    };
    write!(
        out,
        "\n<header class=\"masthead\">\n<p class=\"brand\"><img src=\"{}\" width=\"32\" height=\"32\" alt=\"\"> <span class=\"wordmark\">Aeris Terminal</span></p>\n<h1>{}</h1>\n<dl class=\"facts\">\n{}{}{}{}</dl>\n<p class=\"stamp\">{}</p>\n</header>\n",
        data_uri("image/svg+xml", BrandAsset::MainLogo.bytes()),
        tr!(panels::REPORT_TITLE),
        fact(
            tr!(panels::REPORT_PERIOD),
            &tr!(
                panels::REPORT_PERIOD_VALUE,
                period = document.period.label(),
                from = date_text(document.span.first_day),
                to = date_text(document.span.last_day)
            )
        ),
        fact(tr!(panels::REPORT_ACCOUNTS), document.scope),
        fact(
            tr!(panels::REPORT_TIME_ZONE),
            &format!(
                "{} ({})",
                document.zone.id(),
                offsets
                    .iter()
                    .map(|offset| offset_text(*offset))
                    .collect::<Vec<_>>()
                    .join(" / ")
            )
        ),
        fact(
            tr!(panels::REPORT_TRADES),
            &count(summary.accounts.iter().map(|line| line.trades).sum())
        ),
        tr!(
            panels::REPORT_STAMP,
            version = escape(document.version),
            time = time_text(document.zone, document.generated_unix_nanos),
            zone = escape(document.zone.id()),
            offset = offset_text(document.zone.offset_at(document.generated_unix_nanos))
        ),
    )?;
    if document.truncated {
        out.push_str("<p class=\"note\">");
        out.push_str(tr!(panels::REPORT_TRUNCATED));
        out.push_str("</p>\n");
    }
    let unknown = summary
        .groups
        .iter()
        .map(|group| group.summary.unknown_pnl)
        .sum::<usize>();
    if unknown > 0 {
        writeln!(
            out,
            "<p class=\"note\">{}</p>",
            tr_count!(panels::REPORT_UNKNOWN_PNL, unknown),
        )?;
    }
    write_accounts(out, &summary.accounts)?;
    for source in [ReportSource::Simulated, ReportSource::Broker] {
        let in_scope = summary
            .accounts
            .iter()
            .any(|line| source_of(line.environment) == source);
        if in_scope {
            write_source(out, source, document, times)?;
        }
    }
    Ok(())
}

const fn source_of(environment: AccountEnvironment) -> ReportSource {
    match environment {
        AccountEnvironment::Simulated => ReportSource::Simulated,
        AccountEnvironment::Demo | AccountEnvironment::Live => ReportSource::Broker,
    }
}

fn source_label(environment: AccountEnvironment) -> &'static str {
    match environment {
        AccountEnvironment::Simulated => tr!(panels::REPORT_SOURCE_SIMULATED),
        AccountEnvironment::Demo => tr!(panels::REPORT_SOURCE_DEMO),
        AccountEnvironment::Live => tr!(panels::REPORT_SOURCE_LIVE),
    }
}

fn fact(label: &str, value: &str) -> String {
    format!(
        "<div><dt>{}</dt><dd>{}</dd></div>\n",
        escape(label),
        escape(value)
    )
}

fn write_accounts(out: &mut String, accounts: &[AccountLine]) -> fmt::Result {
    write!(
        out,
        "<section>\n<h2>{}</h2>\n<table>\n<thead><tr><th>{}</th><th>{}</th><th>{}</th><th class=\"number\">{}</th><th class=\"number\">{}</th></tr></thead>\n<tbody>\n",
        tr!(panels::REPORT_ACCOUNTS),
        tr!(panels::REPORT_COL_ACCOUNT),
        tr!(panels::REPORT_COL_SOURCE),
        tr!(panels::REPORT_COL_CURRENCY),
        tr!(panels::REPORT_TRADES),
        tr!(panels::REPORT_NET_PNL),
    )?;
    for line in accounts {
        writeln!(
            out,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"number\">{}</td><td class=\"number\">{}</td></tr>",
            escape(&line.name),
            source_label(line.environment),
            escape(&line.currency),
            count(line.trades),
            pnl_span(line.net, line.scale, &line.currency),
        )?;
    }
    out.push_str("</tbody>\n</table>\n</section>\n");
    Ok(())
}

fn write_source(
    out: &mut String,
    source: ReportSource,
    document: &ReportDocument<'_>,
    times: TradeTimes,
) -> fmt::Result {
    let (title, lead) = match source {
        ReportSource::Simulated => (
            tr!(panels::REPORT_SOURCE_SIMULATED),
            tr!(panels::REPORT_LEAD_SIMULATED),
        ),
        ReportSource::Broker => (
            tr!(panels::REPORT_SOURCE_BROKER),
            tr!(panels::REPORT_LEAD_BROKER),
        ),
    };
    write!(
        out,
        "<section class=\"source\">\n<h2>{title}</h2>\n<p class=\"lead\">{lead}</p>\n"
    )?;
    let mut any = false;
    for group in &document.summary.groups {
        if group.source != source {
            continue;
        }
        any = true;
        write_group(out, group, times)?;
    }
    if !any {
        out.push_str("<p class=\"empty\">");
        out.push_str(tr!(panels::REPORT_EMPTY));
        out.push_str("</p>\n");
    }
    out.push_str("</section>\n");
    Ok(())
}

fn write_group(out: &mut String, group: &CurrencyGroup, times: TradeTimes) -> fmt::Result {
    write!(
        out,
        "<div class=\"group\">\n<h3>{}</h3>\n",
        tr!(
            panels::REPORT_RESULTS_IN,
            currency = escape(&group.currency)
        )
    )?;
    write_summary(out, &group.summary, group.scale, &group.currency)?;
    if !group.curve.is_empty() {
        write_charts(out, group)?;
    }
    write_breakdowns(out, group)?;
    write_trade_list(out, group, times)?;
    out.push_str("</div>\n");
    Ok(())
}

/// Four headline cards, then the remaining statistics as one-line label and value rows.
fn write_summary(out: &mut String, summary: &Summary, scale: u8, currency: &str) -> fmt::Result {
    let money = |value: Option<i128>| {
        value.map_or_else(|| "—".to_string(), |value| pnl_span(value, scale, currency))
    };
    let headline = [
        (tr!(panels::REPORT_NET_PNL), money(Some(summary.net))),
        (tr!(panels::REPORT_WIN_RATE), percent(summary.win_rate())),
        (
            tr!(panels::REPORT_PROFIT_FACTOR),
            summary
                .profit_factor()
                .map_or_else(|| "—".to_string(), |factor| format!("{factor:.2}")),
        ),
        (
            tr!(panels::REPORT_MAX_DRAWDOWN),
            money(Some(-summary.max_drawdown)),
        ),
    ];
    let trades = if summary.unknown_pnl == 0 {
        count(summary.trades)
    } else {
        format!(
            "{} <span class=\"muted\">({})</span>",
            count(summary.trades),
            tr!(panels::REPORT_WITH_PNL, known = count(summary.known()))
        )
    };
    let details = [
        (tr!(panels::REPORT_TRADES), trades),
        (
            tr!(panels::REPORT_WINS_LOSSES),
            format!("{} / {}", count(summary.wins), count(summary.losses)),
        ),
        (
            tr!(panels::REPORT_AVERAGE_WIN),
            money(summary.average_win()),
        ),
        (
            tr!(panels::REPORT_AVERAGE_LOSS),
            money(summary.average_loss()),
        ),
        (
            tr!(panels::REPORT_GROSS_PROFIT),
            money(Some(summary.gross_profit)),
        ),
        (
            tr!(panels::REPORT_GROSS_LOSS),
            money(Some(-summary.gross_loss)),
        ),
        (
            tr!(panels::REPORT_LARGEST_WIN),
            money((summary.wins > 0).then_some(summary.largest_win)),
        ),
        (
            tr!(panels::REPORT_LARGEST_LOSS),
            money((summary.losses > 0).then_some(summary.largest_loss)),
        ),
        (tr!(panels::REPORT_EXPECTANCY), money(summary.expectancy())),
        (
            tr!(panels::REPORT_STREAKS),
            tr!(
                panels::REPORT_STREAKS_VALUE,
                won = count(summary.longest_win_streak),
                lost = count(summary.longest_loss_streak)
            ),
        ),
    ];
    out.push_str("<div class=\"headline\">\n");
    for (label, value) in headline {
        writeln!(
            out,
            "<div class=\"card\"><p class=\"label\">{label}</p><p class=\"value\">{value}</p></div>"
        )?;
    }
    out.push_str("</div>\n<dl class=\"stats\">\n");
    for (label, value) in details {
        writeln!(out, "<div><dt>{label}</dt><dd>{value}</dd></div>")?;
    }
    out.push_str("</dl>\n");
    Ok(())
}

/// The cumulative P&L across the full width, then drawdown and the per-period columns.
fn write_charts(out: &mut String, group: &CurrencyGroup) -> fmt::Result {
    let money = |units: i128| pnl_span(units, group.scale, &group.currency);
    let period = charts::period(group)?;
    out.push_str("<div class=\"charts\">\n");
    write_figure(
        out,
        "wide",
        tr!(panels::REPORT_CHART_CUMULATIVE),
        &tr!(
            panels::REPORT_CHART_ENDING,
            value = money(group.summary.net)
        ),
        &charts::cumulative(group)?,
    )?;
    write_figure(
        out,
        "",
        tr!(panels::REPORT_CHART_DRAWDOWN),
        &tr!(
            panels::REPORT_CHART_LARGEST,
            value = money(-group.summary.max_drawdown)
        ),
        &charts::drawdown(group)?,
    )?;
    write_figure(
        out,
        "",
        if period.monthly {
            tr!(panels::REPORT_CHART_MONTHLY)
        } else {
            tr!(panels::REPORT_CHART_DAILY)
        },
        if period.monthly {
            tr!(panels::REPORT_CHART_BY_MONTH)
        } else {
            tr!(panels::REPORT_CHART_BY_DAY)
        },
        &period.svg,
    )?;
    out.push_str("</div>\n");
    Ok(())
}

fn write_figure(
    out: &mut String,
    class: &str,
    title: &str,
    caption: &str,
    svg: &str,
) -> fmt::Result {
    writeln!(
        out,
        "<figure class=\"{class}\">\n<figcaption><strong>{title}</strong><span>{caption}</span></figcaption>\n{svg}\n</figure>"
    )
}
fn write_breakdowns(out: &mut String, group: &CurrencyGroup) -> fmt::Result {
    let instruments = group
        .instruments
        .iter()
        .map(|(symbol, breakdown)| (symbol.clone(), breakdown));
    let sides = [
        (tr!(panels::REPORT_LONG).to_string(), &group.long),
        (tr!(panels::REPORT_SHORT).to_string(), &group.short),
    ];
    let weekdays = WEEKDAYS
        .iter()
        .zip(&group.weekdays)
        .filter(|(_, breakdown)| breakdown.trades > 0)
        .map(|(day, breakdown)| (day.get().to_string(), breakdown));
    let hours = group
        .hours
        .iter()
        .enumerate()
        .filter(|(_, breakdown)| breakdown.trades > 0)
        .map(|(hour, breakdown)| (format!("{hour:02}:00–{:02}:00", (hour + 1) % 24), breakdown));
    // The hour table is the longest, so it gets a column of its own beside the other three.
    out.push_str("<div class=\"breakdowns\">\n<div class=\"column\">\n");
    write_breakdown_table(
        out,
        group,
        tr!(panels::REPORT_BY_INSTRUMENT),
        tr!(panels::REPORT_COL_INSTRUMENT),
        instruments,
    )?;
    write_breakdown_table(
        out,
        group,
        tr!(panels::REPORT_BY_SIDE),
        tr!(panels::REPORT_COL_SIDE),
        sides,
    )?;
    write_breakdown_table(
        out,
        group,
        tr!(panels::REPORT_BY_WEEKDAY),
        tr!(panels::REPORT_COL_DAY),
        weekdays,
    )?;
    out.push_str("</div>\n<div class=\"column\">\n");
    write_breakdown_table(
        out,
        group,
        tr!(panels::REPORT_BY_HOUR),
        tr!(panels::REPORT_COL_HOUR),
        hours,
    )?;
    out.push_str("</div>\n</div>\n");
    Ok(())
}

fn write_breakdown_table<'a>(
    out: &mut String,
    group: &CurrencyGroup,
    title: &str,
    label: &str,
    rows: impl IntoIterator<Item = (String, &'a Breakdown)>,
) -> fmt::Result {
    write!(
        out,
        "<div class=\"breakdown\">\n<h4>{title}</h4>\n<table>\n<thead><tr><th>{label}</th><th class=\"number\">{}</th><th class=\"number\">{}</th><th class=\"number\">{}</th></tr></thead>\n<tbody>\n",
        tr!(panels::REPORT_TRADES),
        tr!(panels::REPORT_WIN_RATE),
        tr!(panels::REPORT_NET_PNL),
    )?;
    for (name, breakdown) in rows {
        writeln!(
            out,
            "<tr><td>{}</td><td class=\"number\">{}</td><td class=\"number\">{}</td><td class=\"number\">{}</td></tr>",
            escape(&name),
            count(breakdown.trades),
            percent(breakdown.win_rate()),
            pnl_span(breakdown.net, group.scale, &group.currency),
        )?;
    }
    out.push_str("</tbody>\n</table>\n</div>\n");
    Ok(())
}

fn write_trade_list(out: &mut String, group: &CurrencyGroup, times: TradeTimes) -> fmt::Result {
    write!(
        out,
        "<details class=\"trades\" open>\n<summary>{}</summary>\n<table>\n<thead><tr><th class=\"number\">#</th><th>{}</th><th>{}</th><th>{}</th><th class=\"number\">{}</th><th>{}</th><th class=\"number\">{}</th><th>{}</th><th class=\"number\">{}</th><th class=\"number\">{}</th></tr></thead>\n<tbody>\n",
        tr_count!(panels::REPORT_ALL_CLOSED, group.trades.len()),
        tr!(panels::REPORT_COL_ACCOUNT),
        tr!(panels::REPORT_COL_INSTRUMENT),
        tr!(panels::REPORT_COL_SIDE),
        tr!(panels::REPORT_COL_QUANTITY),
        tr!(panels::REPORT_COL_OPENED),
        tr!(panels::REPORT_COL_ENTRY),
        tr!(panels::REPORT_COL_CLOSED),
        tr!(panels::REPORT_COL_EXIT),
        tr!(panels::REPORT_COL_PNL),
    )?;
    for (index, trade) in group.trades.iter().enumerate() {
        write_trade_row(out, index + 1, trade, group, times)?;
    }
    out.push_str("</tbody>\n</table>\n</details>\n");
    Ok(())
}

fn write_trade_row(
    out: &mut String,
    number: usize,
    trade: &ReportTrade,
    group: &CurrencyGroup,
    times: TradeTimes,
) -> fmt::Result {
    let price = |value: FixedPoint| grid_price_text(value, trade.price_grid);
    let quantity = |value: FixedPoint| market_price_text(value.units(), u32::from(value.scale()));
    writeln!(
        out,
        "<tr><td class=\"number\">{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"number\">{}</td><td>{}</td><td class=\"number\">{}</td><td>{}</td><td class=\"number\">{}</td><td class=\"number\">{}</td></tr>",
        count(number),
        escape(&trade.account),
        escape(&trade.symbol),
        match trade.side {
            OrderSide::Buy => tr!(panels::REPORT_LONG),
            OrderSide::Sell => tr!(panels::REPORT_SHORT),
        },
        quantity(trade.exit.quantity),
        trade.entry.map_or_else(
            || "—".to_string(),
            |entry| times.text(entry.first_unix_nanos)
        ),
        trade
            .entry
            .map_or_else(|| "—".to_string(), |entry| price(entry.average_price)),
        times.text(trade.exit.last_unix_nanos),
        price(trade.exit.average_price),
        trade.pnl.map_or_else(
            || format!(
                "<span class=\"muted\">{}</span>",
                tr!(panels::REPORT_UNKNOWN)
            ),
            |pnl| pnl_span(pnl, group.scale, &group.currency)
        ),
    )
}

fn time_text(zone: ReportZone, unix_nanos: i64) -> String {
    let local = zone.local(unix_nanos);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        local.year(),
        local.month(),
        local.day(),
        local.hour(),
        local.minute(),
        local.second()
    )
}

fn count(value: usize) -> String {
    grouped(&value.to_string())
}

fn percent(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_string(), |value| format!("{:.1}%", value * 100.0))
}

/// A signed amount with thousands separators at its exact scale.
pub(super) fn amount(units: i128, scale: u8) -> String {
    let factor = 10_u128.pow(u32::from(scale.min(18)));
    let magnitude = units.unsigned_abs();
    let whole = grouped(&(magnitude / factor).to_string());
    let sign = if units < 0 { "-" } else { "" };
    if scale == 0 {
        format!("{sign}{whole}")
    } else {
        format!(
            "{sign}{whole}.{:0width$}",
            magnitude % factor,
            width = usize::from(scale.min(18))
        )
    }
}

fn pnl_span(units: i128, scale: u8, currency: &str) -> String {
    let class = match units.cmp(&0) {
        std::cmp::Ordering::Greater => "gain",
        std::cmp::Ordering::Less => "loss",
        std::cmp::Ordering::Equal => "flat",
    };
    format!(
        "<span class=\"{class}\">{} {}</span>",
        amount(units, scale),
        escape(currency)
    )
}

pub(super) fn grouped(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn data_uri(media_type: &str, bytes: &[u8]) -> String {
    format!("data:{media_type};base64,{}", STANDARD.encode(bytes))
}

pub(super) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// The platform, numeric and brand faces, embedded so the report looks the same on any
/// computer, and the rule that sets every numeric readout in the numeric face.
fn font_faces() -> Result<String, fmt::Error> {
    let fonts = [
        (
            platform_font_family(),
            "100 900",
            aeris_design_system::PLATFORM_FONT_BYTES,
        ),
        (
            platform_numeric_font_family(),
            "200 800",
            aeris_design_system::NUMERIC_FONT_BYTES,
        ),
        (
            brand_font_family(),
            "400",
            aeris_design_system::BRAND_FONT_BYTES,
        ),
    ];
    let mut faces = String::new();
    for (family, weight, bytes) in fonts {
        writeln!(
            faces,
            "@font-face {{ font-family: \"{family}\"; font-weight: {weight}; font-style: normal; font-display: swap; src: url(\"{}\") format(\"truetype\"); }}",
            data_uri("font/ttf", bytes)
        )?;
    }
    writeln!(
        faces,
        ".number, .card .value, .facts dd, .stats dd, .chart text {{ font-family: {}; font-variant-numeric: tabular-nums; }}",
        platform_numeric_font_stack()
    )?;
    Ok(faces)
}

/// The report's styles, every color, radius and border width taken from the light theme tokens.
fn stylesheet(theme: &AerisTheme) -> Result<String, fmt::Error> {
    let colors = theme.colors;
    let border = format!(
        "{}px solid {}",
        theme.dimensions.border_width,
        colors.border.css_hex()
    );
    let subtle = format!(
        "{}px solid {}",
        theme.dimensions.border_width,
        colors.border_subtle.css_hex()
    );
    let radius = RadiusToken::Default.logical_pixels();
    let small_radius = RadiusToken::Compact.logical_pixels();
    let faces = font_faces()?;
    Ok(format!(
        "{faces}:root {{ color-scheme: {scheme}; print-color-adjust: exact; -webkit-print-color-adjust: exact; }}
body {{ margin: 0 auto; padding: 32px; max-width: 1160px; background: {surface}; color: {text}; font-family: {font}; font-size: 14px; line-height: 1.45; }}
h1 {{ margin: 4px 0 16px; font-size: 28px; }}
h2 {{ margin: 0 0 4px; font-size: 20px; }}
h3 {{ margin: 24px 0 12px; font-size: 16px; }}
h4 {{ margin: 0 0 8px; font-size: 14px; }}
section {{ margin: 0 0 32px; }}
.masthead {{ margin-bottom: 24px; padding-bottom: 16px; border-bottom: {border}; }}
.brand {{ display: flex; align-items: center; gap: 10px; margin: 0 0 12px; }}
.brand .wordmark {{ font-family: \"{brand}\"; font-size: 22px; color: {text}; }}
.facts {{ display: flex; flex-wrap: wrap; gap: 12px 48px; margin: 0 0 12px; }}
.facts dt {{ color: {secondary}; font-size: 12px; }}
.facts dd {{ margin: 0; font-weight: 700; white-space: nowrap; }}
.stamp, .lead, .provenance {{ color: {secondary}; }}
.note {{ padding: 8px 12px; border-radius: {small_radius}px; background: {warning_fill}; color: {warning_text}; }}
.empty {{ color: {muted}; }}
.headline {{ display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 12px; }}
.card {{ padding: 12px 16px; border: {border}; border-radius: {radius}px; background: {surface_secondary}; white-space: nowrap; overflow: hidden; }}
.card p {{ margin: 0; }}
.card .label {{ color: {secondary}; font-size: 12px; }}
.card .value {{ margin-top: 2px; font-size: 20px; font-weight: 700; }}
.stats {{ display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); column-gap: 32px; margin: 12px 0 0; }}
.stats div {{ display: flex; justify-content: space-between; gap: 16px; padding: 7px 0; border-bottom: {subtle}; white-space: nowrap; }}
.stats dt {{ color: {secondary}; }}
.stats dd {{ margin: 0; font-weight: 500; }}
.charts {{ display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 16px; margin: 20px 0; }}
.charts .wide {{ grid-column: 1 / -1; }}
figure {{ margin: 0; padding: 16px; border: {border}; border-radius: {radius}px; }}
figcaption {{ display: flex; justify-content: space-between; align-items: baseline; gap: 16px; margin-bottom: 12px; white-space: nowrap; }}
figcaption span {{ color: {secondary}; font-size: 13px; }}
.chart {{ display: block; width: 100%; height: auto; overflow: visible; }}
.chart text {{ fill: {secondary}; font-size: 12px; }}
.chart .values text {{ text-anchor: end; dominant-baseline: middle; }}
.chart .labels text {{ text-anchor: middle; }}
.chart .grid line {{ stroke: {border_subtle}; stroke-width: {border_width}; }}
.chart .grid .zero {{ stroke: {border_color}; }}
.chart .line {{ fill: none; stroke-width: 2; stroke-linejoin: round; stroke-linecap: round; }}
.chart.primary .line {{ stroke: {primary}; }}
.chart.primary .area {{ fill: {primary_subtle}; }}
.chart.negative .line {{ stroke: {negative}; }}
.chart.negative .area {{ fill: {negative_subtle}; }}
.chart .bars .gain {{ fill: {positive}; }}
.chart .bars .loss {{ fill: {negative}; }}
.chart .bars .flat {{ fill: {border_color}; }}
.breakdowns {{ display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 32px; align-items: start; margin: 20px 0; }}
.breakdowns .column {{ display: flex; flex-direction: column; gap: 24px; }}
table {{ width: 100%; border-collapse: collapse; }}
th {{ text-align: left; color: {secondary}; font-weight: 700; font-size: 12px; border-bottom: {border}; padding: 6px 8px; white-space: nowrap; }}
td {{ padding: 6px 8px; border-bottom: {subtle}; white-space: nowrap; }}
.number {{ text-align: right; }}
.gain {{ color: {gain}; }}
.loss {{ color: {loss}; }}
.flat, .muted {{ color: {secondary}; }}
details.trades {{ margin-top: 20px; }}
details.trades summary {{ cursor: pointer; font-weight: 700; margin-bottom: 8px; }}
.trades table {{ font-size: 12px; }}
.provenance {{ margin-top: 32px; padding-top: 16px; border-top: {border}; font-size: 12px; }}
.provenance code {{ color: {text}; word-break: break-all; }}
@media (max-width: 860px) {{ .headline {{ grid-template-columns: repeat(2, minmax(0, 1fr)); }} .stats, .charts, .breakdowns {{ grid-template-columns: minmax(0, 1fr); }} }}
@media print {{ body {{ padding: 0; }} figure, .card, .breakdown {{ break-inside: avoid; }} }}
",
        surface = colors.surface.css_hex(),
        surface_secondary = colors.surface_secondary.css_hex(),
        text = colors.text_default.css_hex(),
        secondary = colors.text_secondary.css_hex(),
        muted = colors.text_muted.css_hex(),
        gain = colors.text_positive.css_hex(),
        loss = colors.text_negative.css_hex(),
        warning_fill = colors.warning_subtle.css_hex(),
        warning_text = colors.text_warning.css_hex(),
        border_color = colors.border.css_hex(),
        border_subtle = colors.border_subtle.css_hex(),
        border_width = theme.dimensions.border_width,
        primary = colors.primary.css_hex(),
        primary_subtle = colors.primary_subtle.css_hex(),
        positive = colors.positive.css_hex(),
        negative = colors.negative.css_hex(),
        negative_subtle = colors.negative_subtle.css_hex(),
        font = platform_font_stack(),
        brand = brand_font_family(),
        scheme = match theme.mode {
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        },
    ))
}
#[cfg(test)]
mod tests {
    use super::super::collect::tests::closed_trade;
    use super::super::statistics::build_report;
    use super::*;

    fn document_html(trades: &[aeris_trading_runtime::TradeRoundTrip]) -> String {
        let accounts = [
            aeris_trading::TradingAccount {
                id: aeris_trading::TradingAccountId::try_new("practice").expect("account"),
                display_name: "Practice <one>".to_string(),
                environment: AccountEnvironment::Simulated,
                venue_id: "fixture-venue".to_string(),
                broker_ref: None,
                currency: "USD".to_string(),
                currency_scale: 2,
                starting_equity: None,
            },
            aeris_trading::TradingAccount {
                id: aeris_trading::TradingAccountId::try_new("demo").expect("account"),
                display_name: "Demo".to_string(),
                environment: AccountEnvironment::Demo,
                venue_id: "fixture-venue".to_string(),
                broker_ref: None,
                currency: "USD".to_string(),
                currency_scale: 2,
                starting_equity: None,
            },
        ];
        let zone = ReportZone::new("Etc/UTC");
        let summary = build_report(trades, &accounts, &std::collections::BTreeMap::new(), zone)
            .expect("summary");
        let day = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("day");
        render(&ReportDocument {
            version: "1.2.3",
            generated_unix_nanos: 90_000_000_000,
            zone,
            span: ReportSpan {
                start_unix_nanos: 0,
                end_unix_nanos: 100_000_000_000,
                first_day: day,
                last_day: day,
            },
            period: ReportPeriod::Custom,
            scope: "All accounts",
            truncated: false,
            summary: &summary,
            theme: AerisTheme::dark(),
        })
        .expect("rendered")
    }

    #[test]
    fn the_report_is_offline_escaped_and_fingerprints_its_content() {
        let html = document_html(&[
            closed_trade("practice", 10, Some(12_345)),
            closed_trade("practice", 20, Some(-2_345)),
            closed_trade("demo", 30, None),
        ]);
        for forbidden in ["<script", "http://", "https://", "href=", "@import"] {
            assert!(!html.contains(forbidden), "{forbidden}");
        }
        for (index, _) in html.match_indices("src=\"") {
            assert!(
                html[index..].starts_with("src=\"data:"),
                "remote source at {index}"
            );
        }
        assert!(html.contains("data:image/svg+xml;base64,"), "brand logo");
        assert!(
            html.contains("<svg class=\"chart primary\""),
            "cumulative chart"
        );
        assert!(html.contains("<svg class=\"chart columns\""), "daily chart");
        assert!(html.contains("font/ttf"), "embedded font");
        assert!(html.contains("Practice &lt;one&gt;"));
        assert!(!html.contains("Practice <one>"));
        assert!(html.contains(
            "Generated by Aeris Terminal 1.2.3 on 1970-01-01 00:01:30 (Etc/UTC, UTC+00:00)"
        ));
        assert!(html.contains("<dd>Etc/UTC (UTC+00:00)</dd>"));
        assert!(html.contains("color-scheme: dark"), "the requested theme");
        assert!(html.contains(&AerisTheme::dark().colors.surface.css_hex()));
        assert!(html.contains("Simulated by Aeris</h2>"));
        assert!(html.contains("Broker-reported fills</h2>"));
        assert!(html.contains("100.00 USD"), "net of the practice account");
        assert!(html.contains("1 trade has no recorded P&amp;L"));
        let start = html.find(CONTENT_START).expect("start") + CONTENT_START.len();
        let end = html.find(CONTENT_END).expect("end");
        let recorded = html
            .split("<code>")
            .nth(1)
            .and_then(|rest| rest.split("</code>").next())
            .expect("fingerprint");
        assert_eq!(recorded, fingerprint(&html[start..end]));
    }

    #[test]
    fn amounts_keep_their_exact_scale_with_thousands_separators() {
        assert_eq!(amount(123_456_789, 2), "1,234,567.89");
        assert_eq!(amount(-5, 2), "-0.05");
        assert_eq!(amount(1_000, 0), "1,000");
        assert_eq!(count(999), "999");
        assert_eq!(percent(Some(0.4)), "40.0%");
    }
}
