//! Zero-dependency Rust syntax highlighter and Study SDK snippet catalog for the
//! in-app Study Editor `TextArea`.
//!
//! All token colors are derived exclusively from `AerisTheme` semantic tokens so
//! syntax highlighting adapts automatically to Dark and Light themes without
//! hardcoded hex values.

use super::native_ui::theme::gpui_color;
use aeris_design_system::AerisTheme;
use gpui::{Hsla, TextRun};
use std::sync::Arc;

/// One-click Rust Study SDK snippet insertable at the editor cursor.
#[derive(Clone, Copy, Debug)]
pub(super) struct StudySnippet {
    pub(super) label: &'static str,
    pub(super) doc: &'static str,
    pub(super) body: &'static str,
}

/// Curated Study SDK snippets for rapid indicator authoring.
pub(super) const STUDY_SNIPPETS: &[StudySnippet] = &[
    StudySnippet {
        label: "+ SMA",
        doc: "Simple Moving Average loop over `close`",
        body: "    let period = 20usize;\n    for i in 0..out.len() {\n        if i + 1 < period {\n            out[i] = f64::NAN;\n            continue;\n        }\n        let mut sum = 0.0;\n        for j in (i + 1 - period)..=i {\n            sum += close[j];\n        }\n        out[i] = sum / (period as f64);\n    }\n",
    },
    StudySnippet {
        label: "+ EMA",
        doc: "Recursive Exponential Moving Average over `close`",
        body: "    let alpha = 2.0 / (20.0 + 1.0);\n    let mut ema = f64::NAN;\n    for i in 0..out.len() {\n        let c = close[i];\n        ema = if !c.is_finite() { f64::NAN } else if !ema.is_finite() { c } else { alpha * c + (1.0 - alpha) * ema };\n        out[i] = ema;\n    }\n",
    },
    StudySnippet {
        label: "+ VWAP",
        doc: "Cumulative typical-price volume-weighted average",
        body: "    let mut pv = 0.0;\n    let mut vol = 0.0;\n    for i in 0..out.len() {\n        let tp = (high[i] + low[i] + close[i]) / 3.0;\n        pv += tp * volume[i];\n        vol += volume[i];\n        out[i] = if vol > 0.0 { pv / vol } else { f64::NAN };\n    }\n",
    },
    StudySnippet {
        label: "+ Gap",
        doc: "Emit an explicit hard gap (`f64::NAN`)",
        body: "out[i] = f64::NAN;",
    },
    StudySnippet {
        label: "+ HL2",
        doc: "Bar midpoint `(high[i] + low[i]) * 0.5`",
        body: "(high[i] + low[i]) * 0.5",
    },
];

/// Compact API quick-reference line displayed beneath the snippet bar.
pub(super) const STUDY_API_QUICK_REF: &str = "API: pub fn calculate(open: &[f64], high: &[f64], low: &[f64], close: &[f64], volume: &[f64], out: &mut [f64]) · f64::NAN = gap";

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

/// Builds a theme-aware Rust syntax highlighter callback for `gpui_base::input::TextareaState`.
#[must_use]
pub(super) fn rust_syntax_highlighter(
    theme: &AerisTheme,
) -> Arc<dyn Fn(&str, &TextRun) -> Vec<TextRun> + Send + Sync> {
    let palette = RustPalette::from_theme(theme);
    Arc::new(move |text: &str, base: &TextRun| tokenize_rust_runs(text, base, palette))
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
