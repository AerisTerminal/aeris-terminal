//! Bounded multi-symbol Market Scanner for sandboxed WebAssembly user studies.
//!
//! Evaluates one compiled zero-authority [`CompiledWasmStudy`] across up to
//! [`MAXIMUM_WASM_SCAN_SYMBOLS`] symbols with strict per-symbol instruction fuel and memory
//! budgets, returning ranked [`WasmScanMatch`] hits without opening provider sessions.

use crate::sandbox::{StudySandboxColumnarBars, StudySandboxLimits};
use crate::wasm_runtime::{CompiledWasmStudy, WasmStudyMeta};

/// Maximum symbols evaluated in one bounded WASM study scan pass.
pub const MAXIMUM_WASM_SCAN_SYMBOLS: usize = 256;
/// Default per-symbol instruction fuel ceiling during multi-symbol scans.
pub const DEFAULT_WASM_SCAN_FUEL_PER_SYMBOL: u64 = 5_000_000;

/// Filter condition applied to the latest computed bars of a WASM study output series.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WasmScanPredicate {
    /// Latest finite value of `output_index` is `> threshold`.
    GreaterThan { output_index: usize, threshold: f64 },
    /// Latest finite value of `output_index` is `< threshold`.
    LessThan { output_index: usize, threshold: f64 },
    /// `output_index` crossed above `threshold` on the final bar (`prev <= t && curr > t`).
    CrossesAbove { output_index: usize, threshold: f64 },
    /// `output_index` crossed below `threshold` on the final bar (`prev >= t && curr < t`).
    CrossesBelow { output_index: usize, threshold: f64 },
    /// Latest finite value of `output_index` is non-zero (useful for boolean signal outputs).
    NonZero { output_index: usize },
    /// Study emitted at least one semantic alert during the scan wave.
    AlertTriggered,
}

/// One symbol's input bars supplied to [`evaluate_wasm_scan`].
#[derive(Clone, Debug)]
pub struct WasmScanSymbolInput<'a> {
    pub symbol: &'a str,
    pub bars: &'a StudySandboxColumnarBars,
}

/// One symbol that matched a [`WasmScanPredicate`].
#[derive(Clone, Debug, PartialEq)]
pub struct WasmScanMatch {
    pub symbol: String,
    pub value: f64,
    pub alert_count: usize,
    pub fuel_consumed: u64,
}

/// Summary of one bounded multi-symbol WASM study scan pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WasmScanSummary {
    pub scanned_symbols: usize,
    pub failed_symbols: usize,
    pub total_fuel_consumed: u64,
    pub matches: Vec<WasmScanMatch>,
}

/// Evaluates `compiled` across `symbols` (capped at [`MAXIMUM_WASM_SCAN_SYMBOLS`]) under strict
/// per-symbol resource limits and returns matching symbols sorted descending by primary value.
#[must_use]
pub fn evaluate_wasm_scan(
    compiled: &CompiledWasmStudy,
    output_count: usize,
    params: &[f64],
    str_params: &[(usize, String)],
    predicate: WasmScanPredicate,
    symbols: &[WasmScanSymbolInput<'_>],
    per_symbol_limits: Option<StudySandboxLimits>,
) -> WasmScanSummary {
    let limits = per_symbol_limits.unwrap_or(StudySandboxLimits {
        max_instruction_fuel: DEFAULT_WASM_SCAN_FUEL_PER_SYMBOL,
        ..StudySandboxLimits::default()
    });
    let mut summary = WasmScanSummary::default();

    for entry in symbols.iter().take(MAXIMUM_WASM_SCAN_SYMBOLS) {
        summary.scanned_symbols += 1;
        let meta = WasmStudyMeta {
            price_scale: entry.bars.price_scale,
            quantity_scale: entry.bars.quantity_scale,
            is_realtime: false,
            is_confirmed_bar: true,
            is_new_bar: false,
            symbol: entry.symbol.to_string(),
            timeframe_seconds: 60,
        };
        let Ok(result) = compiled.execute_columnar_with_typed_params(
            entry.bars,
            output_count,
            params,
            str_params,
            None,
            &meta,
            limits,
        ) else {
            summary.failed_symbols += 1;
            continue;
        };
        summary.total_fuel_consumed = summary
            .total_fuel_consumed
            .saturating_add(result.fuel_consumed);

        let (matched, value) = match predicate {
            WasmScanPredicate::GreaterThan {
                output_index,
                threshold,
            } => {
                let curr = latest_value(&result.outputs, output_index);
                (curr.is_some_and(|v| v > threshold), curr.unwrap_or(f64::NAN))
            }
            WasmScanPredicate::LessThan {
                output_index,
                threshold,
            } => {
                let curr = latest_value(&result.outputs, output_index);
                (curr.is_some_and(|v| v < threshold), curr.unwrap_or(f64::NAN))
            }
            WasmScanPredicate::CrossesAbove {
                output_index,
                threshold,
            } => {
                let (prev, curr) = last_two_values(&result.outputs, output_index);
                let ok = prev.is_some_and(|p| p <= threshold)
                    && curr.is_some_and(|c| c > threshold);
                (ok, curr.unwrap_or(f64::NAN))
            }
            WasmScanPredicate::CrossesBelow {
                output_index,
                threshold,
            } => {
                let (prev, curr) = last_two_values(&result.outputs, output_index);
                let ok = prev.is_some_and(|p| p >= threshold)
                    && curr.is_some_and(|c| c < threshold);
                (ok, curr.unwrap_or(f64::NAN))
            }
            WasmScanPredicate::NonZero { output_index } => {
                let curr = latest_value(&result.outputs, output_index);
                (curr.is_some_and(|v| v != 0.0), curr.unwrap_or(f64::NAN))
            }
            WasmScanPredicate::AlertTriggered => {
                let curr = latest_value(&result.outputs, 0).unwrap_or(0.0);
                (!result.alerts.is_empty(), curr)
            }
        };

        if matched {
            summary.matches.push(WasmScanMatch {
                symbol: entry.symbol.to_string(),
                value,
                alert_count: result.alerts.len(),
                fuel_consumed: result.fuel_consumed,
            });
        }
    }

    summary.matches.sort_by(|a, b| {
        b.value
            .total_cmp(&a.value)
            .then_with(|| a.symbol.cmp(&b.symbol))
    });
    summary
}

fn latest_value(outputs: &[Vec<Option<f64>>], output_index: usize) -> Option<f64> {
    outputs.get(output_index)?.last().copied().flatten()
}

fn last_two_values(
    outputs: &[Vec<Option<f64>>],
    output_index: usize,
) -> (Option<f64>, Option<f64>) {
    let Some(series) = outputs.get(output_index) else {
        return (None, None);
    };
    let n = series.len();
    if n < 2 {
        return (None, series.last().copied().flatten());
    }
    (series[n - 2], series[n - 1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toolchain::{PreviewFixturePreset, StudyPreviewFixture};

    const LAST_CLOSE_WAT: &str = r#"
        (module
            (memory (export "memory") 4)
            (global $heap (mut i32) (i32.const 1024))
            (func (export "aeris_study_abi_version") (result i32) (i32.const 1))
            (func (export "aeris_study_alloc") (param $len i32) (result i32)
                (local $ptr i32)
                (local.set $ptr (global.get $heap))
                (global.set $heap (i32.add (local.get $ptr) (local.get $len)))
                (local.get $ptr)
            )
            (func (export "aeris_study_calculate")
                (param $bars i32) (param $n i32) (param $out i32) (result i32)
                (local $i i32)
                (local $close_base i32)
                (local.set $close_base (i32.add (local.get $bars) (i32.mul (i32.mul (local.get $n) (i32.const 3)) (i32.const 8))))
                (block $done
                    (loop $loop
                        (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
                        (f64.store
                            (i32.add (local.get $out) (i32.mul (local.get $i) (i32.const 8)))
                            (f64.load (i32.add (local.get $close_base) (i32.mul (local.get $i) (i32.const 8))))
                        )
                        (local.set $i (i32.add (local.get $i) (i32.const 1)))
                        (br $loop)
                    )
                )
                (i32.const 0)
            )
        )
    "#;

    #[test]
    fn wasm_study_scanner_filters_and_ranks_symbols_within_per_symbol_fuel_budget() {
        let compiled =
            CompiledWasmStudy::from_bytes(LAST_CLOSE_WAT.as_bytes(), StudySandboxLimits::default())
                .expect("valid WAT compiles");
        let up = StudyPreviewFixture::generate(PreviewFixturePreset::Uptrend, 30, 60);
        let flat = StudyPreviewFixture::generate(PreviewFixturePreset::RangeBound, 30, 60);

        let inputs = [
            WasmScanSymbolInput {
                symbol: "UP_SYM",
                bars: &up.bars,
            },
            WasmScanSymbolInput {
                symbol: "FLAT_SYM",
                bars: &flat.bars,
            },
        ];
        let up_last = crate::fixed_point_to_f64(*up.bars.close.last().unwrap(), up.bars.price_scale)
            .unwrap();
        let flat_last =
            crate::fixed_point_to_f64(*flat.bars.close.last().unwrap(), flat.bars.price_scale)
                .unwrap();
        let threshold = (up_last + flat_last) * 0.5;
        let expected_winner = if up_last > flat_last {
            "UP_SYM"
        } else {
            "FLAT_SYM"
        };
        let summary = evaluate_wasm_scan(
            &compiled,
            1,
            &[],
            &[],
            WasmScanPredicate::GreaterThan {
                output_index: 0,
                threshold,
            },
            &inputs,
            None,
        );
        assert_eq!(summary.scanned_symbols, 2);
        assert_eq!(summary.failed_symbols, 0);
        assert!(summary.total_fuel_consumed > 0);
        assert_eq!(summary.matches.len(), 1);
        assert_eq!(summary.matches[0].symbol, expected_winner);
        assert!(summary.matches[0].value > threshold);
    }
}
