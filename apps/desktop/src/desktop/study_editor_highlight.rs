//! Zero-dependency Rust syntax highlighter and Study SDK snippet catalog for the
//! in-app Study Editor `TextArea`.
//!
//! All token colors are derived exclusively from `AerisTheme` semantic tokens so
//! syntax highlighting adapts automatically to Dark and Light themes without
//! hardcoded hex values.

use super::native_ui::theme::gpui_color;
use aeris_design_system::AerisTheme;
use gpui::{Hsla, TextRun};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

/// One-click Rust Study SDK snippet insertable at the editor cursor.
#[derive(Clone, Copy, Debug)]
pub(super) struct StudySnippet {
    pub(super) label: &'static str,
    pub(super) doc: &'static str,
    pub(super) body: &'static str,
}

/// Context-aware API autocomplete item for the Study Editor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StudyCompletion {
    pub(super) trigger: &'static str,
    pub(super) insert: &'static str,
    pub(super) signature: &'static str,
    pub(super) doc: &'static str,
}

/// Complete autocomplete catalog for Aeris Study SDK directives, `ta::*`, `of::*`, `strategy::*`,
/// and scene drawing helpers.
pub(super) const STUDY_COMPLETIONS: &[StudyCompletion] = &[
    StudyCompletion {
        trigger: "ta::sma",
        insert: "super::ta::sma(close, 20)",
        signature: "ta::sma(src: &[f64], period: usize) -> Vec<f64>",
        doc: "Simple Moving Average",
    },
    StudyCompletion {
        trigger: "ta::ema",
        insert: "super::ta::ema(close, 20)",
        signature: "ta::ema(src: &[f64], period: usize) -> Vec<f64>",
        doc: "Exponential Moving Average",
    },
    StudyCompletion {
        trigger: "ta::rma",
        insert: "super::ta::rma(close, 14)",
        signature: "ta::rma(src: &[f64], period: usize) -> Vec<f64>",
        doc: "Wilder's Moving Average",
    },
    StudyCompletion {
        trigger: "ta::rsi",
        insert: "super::ta::rsi(close, 14)",
        signature: "ta::rsi(src: &[f64], period: usize) -> Vec<f64>",
        doc: "Relative Strength Index (0..100)",
    },
    StudyCompletion {
        trigger: "ta::atr",
        insert: "super::ta::atr(high, low, close, 14)",
        signature: "ta::atr(high, low, close, period) -> Vec<f64>",
        doc: "Average True Range",
    },
    StudyCompletion {
        trigger: "ta::bb",
        insert: "super::ta::bb(close, 20, 2.0)",
        signature: "ta::bb(src, period, mult) -> (Vec<f64>, Vec<f64>, Vec<f64>)",
        doc: "Bollinger Bands (upper, middle, lower)",
    },
    StudyCompletion {
        trigger: "ta::macd",
        insert: "super::ta::macd(close, 12, 26, 9)",
        signature: "ta::macd(src, fast, slow, signal) -> (Vec<f64>, Vec<f64>, Vec<f64>)",
        doc: "MACD (line, signal, histogram)",
    },
    StudyCompletion {
        trigger: "ta::session_vwap",
        insert: "super::ta::session_vwap(high, low, close, volume)",
        signature: "ta::session_vwap(high, low, close, volume) -> Vec<f64>",
        doc: "Daily-resetting Session VWAP",
    },
    StudyCompletion {
        trigger: "ta::pivothigh",
        insert: "super::ta::pivothigh(high, i, 5, 5)",
        signature: "ta::pivothigh(src, bar_idx, left, right) -> Option<f64>",
        doc: "Confirmed swing pivot high",
    },
    StudyCompletion {
        trigger: "ta::pivotlow",
        insert: "super::ta::pivotlow(low, i, 5, 5)",
        signature: "ta::pivotlow(src, bar_idx, left, right) -> Option<f64>",
        doc: "Confirmed swing pivot low",
    },
    StudyCompletion {
        trigger: "ta::crossover",
        insert: "super::ta::crossover(&a, &b, i)",
        signature: "ta::crossover(a, b, i) -> bool",
        doc: "True when series `a` crosses above `b` at bar `i`",
    },
    StudyCompletion {
        trigger: "ta::crossunder",
        insert: "super::ta::crossunder(&a, &b, i)",
        signature: "ta::crossunder(a, b, i) -> bool",
        doc: "True when series `a` crosses below `b` at bar `i`",
    },
    StudyCompletion {
        trigger: "of::cvd",
        insert: "super::of::cvd(close.len())",
        signature: "of::cvd(len: usize) -> Vec<f64>",
        doc: "Cumulative Volume Delta (requires `// @require: order_flow`)",
    },
    StudyCompletion {
        trigger: "of::delta",
        insert: "super::of::delta(i)",
        signature: "of::delta(bar_index: usize) -> f64",
        doc: "Per-bar aggressor delta (buy_vol - sell_vol)",
    },
    StudyCompletion {
        trigger: "strategy::entry",
        insert: "super::strategy::entry(i, \"Long\", true, 1.0);",
        signature: "strategy::entry(bar_idx, id, is_long, qty)",
        doc: "Open or reverse a backtest position",
    },
    StudyCompletion {
        trigger: "strategy::exit",
        insert: "super::strategy::exit(i, \"Exit\", \"Long\", tp, sl);",
        signature: "strategy::exit(bar_idx, id, from_entry, tp, sl)",
        doc: "Attach take-profit / stop-loss bracket",
    },
    StudyCompletion {
        trigger: "draw_box_text",
        insert: "super::draw_box_text(x1, top, x2, bot, border, fill, 1, super::LINE_SOLID, text_col, \"FVG\");",
        signature: "draw_box_text(x1, top, x2, bot, border, fill, w, style, text_col, text)",
        doc: "Draw rectangular zone with label text",
    },
    StudyCompletion {
        trigger: "draw_line_fill",
        insert: "super::draw_line_fill(line_a, line_b, fill_rgba);",
        signature: "draw_line_fill(line_a, line_b, color_rgba)",
        doc: "Shade region between two drawn lines",
    },
    StudyCompletion {
        trigger: "plot_shape",
        insert: "super::plot_shape(i, super::LOC_BELOW_BAR, 0.0, super::LABEL_TRIANGLE_UP, col, white, \"BUY\");",
        signature: "plot_shape(bar_idx, location, price, shape, color, text_color, text)",
        doc: "Plot marker above/below bar or at price",
    },
    StudyCompletion {
        trigger: "set_plot_color",
        insert: "super::set_plot_color(0, i, color_rgba);",
        signature: "set_plot_color(output_idx, bar_idx, color_rgba)",
        doc: "Dynamic per-bar plot color override",
    },
    StudyCompletion {
        trigger: "emit_alert",
        insert: "super::emit_alert(i, \"signal\", \"Alert triggered\", super::ALERT_ONCE_PER_BAR_CLOSE);",
        signature: "emit_alert(bar_idx, condition_id, message, frequency)",
        doc: "Emit deduplicated alert to Aeris notification center",
    },
    StudyCompletion {
        trigger: "log_info",
        insert: "super::log_info(&format!(\"val={}\", close[i]));",
        signature: "log_info(message: &str)",
        doc: "Print debug message to Study Editor Console",
    },
    StudyCompletion {
        trigger: "@input:",
        insert: "// @input: len | int | 20 | Length | min=1 | max=200 | group=Core",
        signature: "// @input: id | type | default | label | ...",
        doc: "Declare typed Settings UI input",
    },
    StudyCompletion {
        trigger: "@request_tf:",
        insert: "// @request_tf: htf | 15m | confirmed | carry",
        signature: "// @request_tf: alias | tf | confirmed|forming | carry|gaps",
        doc: "Request non-repainting higher-timeframe series",
    },
    StudyCompletion {
        trigger: "@use:",
        insert: "// @use: user.my_lib",
        signature: "// @use: <library_project_id>",
        doc: "Import reusable helper functions from another study project",
    },
];

/// Returns up to `limit` API completions matching `query` (case-insensitive substring match).
#[must_use]
pub(super) fn matching_completions(query: &str, limit: usize) -> Vec<&'static StudyCompletion> {
    let q = query.trim().to_ascii_lowercase();
    STUDY_COMPLETIONS
        .iter()
        .filter(|c| {
            q.is_empty()
                || c.trigger.to_ascii_lowercase().contains(&q)
                || c.doc.to_ascii_lowercase().contains(&q)
        })
        .take(limit)
        .collect()
}

/// Curated Study SDK snippets for rapid indicator authoring.
pub(super) const STUDY_SNIPPETS: &[StudySnippet] = &[
    StudySnippet {
        label: "+ ta::ema",
        doc: "Built-in Exponential Moving Average via `super::ta::ema`",
        body: "    let fast = super::ta::ema(close, 20);\n    out.copy_from_slice(&fast);\n",
    },
    StudySnippet {
        label: "+ ta::rsi",
        doc: "Built-in Wilder RSI via `super::ta::rsi`",
        body: "    let rsi = super::ta::rsi(close, 14);\n    out.copy_from_slice(&rsi);\n",
    },
    StudySnippet {
        label: "+ Session VWAP",
        doc: "Daily-resetting Session VWAP using bar timestamps",
        body: "    let vwap = super::ta::session_vwap(high, low, close, volume);\n    out.copy_from_slice(&vwap);\n",
    },
    StudySnippet {
        label: "+ Alert",
        doc: "Emit a deduplicated alert to Aeris's notification center",
        body: "    let last = close.len().saturating_sub(1);\n    if super::ta::crossover(close, out, last) {\n        super::emit_alert(last, \"cross_up\", \"Price crossed above indicator\", super::ALERT_ONCE_PER_BAR_CLOSE);\n    }\n",
    },
    StudySnippet {
        label: "+ Log",
        doc: "Print a debug message to the Study Editor Console",
        body: "    super::log_info(&format!(\"{} @ {}s bars={}\", super::syminfo::symbol(), super::syminfo::timeframe_seconds(), close.len()));\n",
    },
];

/// Compact API quick-reference line displayed beneath the snippet bar.
pub(super) const STUDY_API_QUICK_REF: &str = "API: super::ta::{sma,ema,rma,rsi,atr,bb,macd,session_vwap,pivothigh,crossover} · super::{time,is_new_day,syminfo,emit_alert,log_info}";

#[derive(Clone, Copy)]
struct RustPalette {
    keyword: Hsla,
    ty: Hsla,
    number: Hsla,
    string: Hsla,
    comment: Hsla,
}

impl RustPalette {
    fn from_theme(theme: &AerisTheme) -> Self {
        let colors = theme.colors;
        Self {
            keyword: gpui_color(colors.ring_primary),
            ty: gpui_color(colors.positive),
            number: gpui_color(colors.warning),
            string: gpui_color(colors.positive),
            comment: gpui_color(colors.text_secondary),
        }
    }
}

/// Builds a theme-aware, cached Rust syntax highlighter callback for `gpui_base::input::TextareaState`.
///
/// Caches the most recent tokenized run vector keyed by `(text_len, text_hash, base_color)` so
/// live chart redraws never re-lex unchanged editor text.
#[must_use]
pub(super) fn rust_syntax_highlighter(
    theme: &AerisTheme,
) -> Arc<dyn Fn(&str, &TextRun) -> Vec<TextRun> + Send + Sync> {
    let palette = RustPalette::from_theme(theme);
    let cache: Mutex<Option<(usize, u64, Hsla, Vec<TextRun>)>> = Mutex::new(None);
    Arc::new(move |text: &str, base: &TextRun| {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let hash = hasher.finish();
        if let Ok(guard) = cache.lock()
            && let Some((cached_len, cached_hash, cached_color, runs)) = guard.as_ref()
            && *cached_len == text.len()
            && *cached_hash == hash
            && *cached_color == base.color
        {
            return runs.clone();
        }
        let runs = tokenize_rust_runs(text, base, palette);
        if let Ok(mut guard) = cache.lock() {
            *guard = Some((text.len(), hash, base.color, runs.clone()));
        }
        runs
    })
}

fn is_rust_keyword(word: &str) -> bool {
    matches!(
        word,
        "as" | "break"
            | "const"
            | "continue"
            | "else"
            | "enum"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "struct"
            | "trait"
            | "true"
            | "type"
            | "use"
            | "where"
            | "while"
    )
}

fn is_rust_type(word: &str) -> bool {
    matches!(
        word,
        "bool"
            | "char"
            | "f32"
            | "f64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "str"
            | "Option"
            | "Result"
            | "Some"
            | "None"
            | "Ok"
            | "Err"
            | "Vec"
            | "String"
            | "NAN"
            | "INFINITY"
            | "NEG_INFINITY"
    )
}

fn push_run(runs: &mut Vec<TextRun>, len: usize, color: Hsla, base: &TextRun) {
    if len == 0 {
        return;
    }
    if let Some(last) = runs.last_mut()
        && last.color == color
    {
        last.len += len;
        return;
    }
    runs.push(TextRun {
        len,
        color,
        ..base.clone()
    });
}

fn tokenize_rust_runs(text: &str, base: &TextRun, palette: RustPalette) -> Vec<TextRun> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut runs = Vec::with_capacity(32);
    let mut i = 0;

    while i < len {
        // Line comment: // ... \n
        if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            let start = i;
            i += 2;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            push_run(&mut runs, i - start, palette.comment, base);
            continue;
        }
        // Block comment: /* ... */
        if bytes[i] == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            let start = i;
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(len);
            push_run(&mut runs, i - start, palette.comment, base);
            continue;
        }
        // String literal: "..."
        if bytes[i] == b'"' {
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' && i + 1 < len {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            push_run(&mut runs, i - start, palette.string, base);
            continue;
        }
        // Number literal
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < len
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_')
            {
                i += 1;
            }
            push_run(&mut runs, i - start, palette.number, base);
            continue;
        }
        // Identifier / keyword / type
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &text[start..i];
            let color = if is_rust_keyword(word) {
                palette.keyword
            } else if is_rust_type(word) {
                palette.ty
            } else {
                base.color
            };
            push_run(&mut runs, i - start, color, base);
            continue;
        }
        // Any other UTF-8 character (punctuation, whitespace, non-ASCII).
        let ch_len = text[i..].chars().next().map_or(1, char::len_utf8);
        push_run(&mut runs, ch_len, base.color, base);
        i += ch_len;
    }

    runs
}

/// Converts a 1-based `(line, column)` diagnostic coordinate into a UTF-8 byte range
/// covering that token/line for cursor navigation in `TextareaState`.
#[must_use]
pub(super) fn line_column_to_byte_range(source: &str, line: u32, column: u32) -> (usize, usize) {
    let target_line = line.saturating_sub(1) as usize;
    let target_col = column.saturating_sub(1) as usize;
    let mut offset = 0usize;
    for (idx, line_str) in source.split('\n').enumerate() {
        if idx == target_line {
            let clean_line = line_str.strip_suffix('\r').unwrap_or(line_str);
            let col_byte = clean_line
                .char_indices()
                .nth(target_col)
                .map_or(clean_line.len(), |(b, _)| b);
            let start = offset + col_byte;
            let end = offset + clean_line.len();
            return (start, end.max(start));
        }
        offset += line_str.len() + 1;
    }
    (source.len(), source.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_highlighter_preserves_exact_utf8_byte_length_and_colors_tokens() {
        let theme = AerisTheme::dark();
        let palette = RustPalette::from_theme(&theme);
        let base = TextRun {
            len: 0,
            font: gpui::font("Inter"),
            color: gpui_color(theme.colors.text_secondary),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let src = "// comment — café\npub fn calculate(close: &[f64], out: &mut [f64]) {\n    let x = \"hi\";\n    out[0] = 42.5;\n}\n";
        let runs = tokenize_rust_runs(src, &base, palette);
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), src.len());
        assert!(runs.iter().any(|r| r.color == palette.comment));
        assert!(runs.iter().any(|r| r.color == palette.keyword));
        assert!(runs.iter().any(|r| r.color == palette.ty));
        assert!(runs.iter().any(|r| r.color == palette.string));
        assert!(runs.iter().any(|r| r.color == palette.number));
    }

    #[test]
    fn line_column_to_byte_range_maps_diagnostic_coordinates() {
        let src = "fn a() {\n    out[0] = \"bad\";\n}\n";
        let (start, end) = line_column_to_byte_range(src, 2, 5);
        assert_eq!(&src[start..end], "out[0] = \"bad\";");
    }
}
