//! Offline HTML trading reports of closed trades. A report reads the trading owner's
//! closed-trade pages on a background thread, groups the trades into a simulated and a broker
//! section per currency, and saves one self-contained HTML file in the reports folder (Aeris
//! Reports in the user's Downloads) before opening it in the default browser. Nothing leaves
//! the machine.

mod charts;
mod collect;
mod dialog;
mod html;
mod output;
mod period;
mod statistics;

use super::bottom_panel::InstrumentLabel;
use super::*;
use aeris_localization::{panels, tr};

pub(super) use dialog::ReportDialog;

/// Why a report could not be produced. The user sees a composed notice for each kind; the
/// underlying detail goes to diagnostics only.
#[derive(Debug)]
enum ReportError {
    /// The read-only history worker refused or failed a closed-trade page.
    History(String),
    /// A P&L total does not fit the report's exact arithmetic.
    AmountOverflow,
    /// A value's formatting failed while the document was composed.
    Render,
    /// The report file could not be written to the reports folder.
    Write(std::io::Error),
}

impl ReportError {
    fn notice_detail(&self) -> &'static str {
        match self {
            Self::History(_) => tr!(panels::REPORT_ERROR_HISTORY),
            Self::AmountOverflow => tr!(panels::REPORT_ERROR_OVERFLOW),
            Self::Render => tr!(panels::REPORT_ERROR_RENDER),
            Self::Write(_) => tr!(panels::REPORT_ERROR_WRITE),
        }
    }
}

/// Everything a background report run needs, captured on the UI thread.
struct ReportRequest {
    accounts: Vec<aeris_trading::TradingAccount>,
    /// The working set's instruments; each page read adds the instruments it references.
    instruments: std::sync::Arc<BTreeMap<aeris_instruments::InstrumentId, InstrumentLabel>>,
    zone: period::ReportZone,
    span: period::ReportSpan,
    period: period::ReportPeriod,
    scope: String,
    generated_unix_nanos: i64,
    directory: std::path::PathBuf,
    /// The workspace theme when the report was requested; the document uses its colors.
    theme: aeris_design_system::AerisTheme,
}

/// A saved report and whether the default browser accepted it.
struct GeneratedReport {
    path: std::path::PathBuf,
    trades: usize,
    opened: bool,
}

/// Reads, computes, renders, saves and opens one report. Runs on a background thread only.
fn generate(
    service: &aeris_trading_runtime::TradingService,
    request: &ReportRequest,
) -> Result<GeneratedReport, ReportError> {
    let account_ids = request
        .accounts
        .iter()
        .map(|account| account.id.clone())
        .collect::<Vec<_>>();
    let collected = collect::collect_closed_trades(service, &account_ids, request.span)?;
    let mut instruments = (*request.instruments).clone();
    for (id, label) in collected.instruments {
        instruments.entry(id).or_insert(label);
    }
    let summary = statistics::build_report(
        &collected.trades,
        &request.accounts,
        &instruments,
        request.zone,
    )?;
    let document = html::ReportDocument {
        version: env!("CARGO_PKG_VERSION"),
        generated_unix_nanos: request.generated_unix_nanos,
        zone: request.zone,
        span: request.span,
        period: request.period,
        scope: &request.scope,
        truncated: collected.truncated,
        summary: &summary,
        theme: request.theme,
    };
    let html = html::render(&document).map_err(|_| ReportError::Render)?;
    let path = output::save_report(
        &request.directory,
        request.generated_unix_nanos,
        html.as_bytes(),
    )
    .map_err(ReportError::Write)?;
    let opened = match aeris_platform_runtime::open_file_in_default_app(&path) {
        Ok(()) => true,
        Err(error) => {
            diagnostic!("Aeris could not open the trading report in a browser: {error}");
            false
        }
    };
    Ok(GeneratedReport {
        path,
        trades: collected.trades.len(),
        opened,
    })
}
