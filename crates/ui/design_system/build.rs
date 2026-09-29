#[path = "src/token_compiler.rs"]
mod token_compiler;

use std::{env, fmt::Write as _, fs, path::PathBuf};
use token_compiler::{cascade, parse_color, parse_pixels, parse_theme_blocks, resolve, source};

const COLORS: [&str; 43] = [
    "surface",
    "surface-secondary",
    "border",
    "border-secondary",
    "text-primary",
    "text-secondary",
    "text-muted",
    "text-positive",
    "text-negative",
    "hover-bg",
    "active-bg",
    "icon",
    "icon-active",
    "primary",
    "primary-foreground",
    "danger",
    "danger-foreground",
    "warning",
    "positive",
    "positive-subtle",
    "negative-subtle",
    "button-fill",
    "button-fill-hover",
    "button-fill-active",
    "button-fill-foreground",
    "button-fill-subtle",
    "buy",
    "buy-hover",
    "buy-active",
    "buy-disabled",
    "buy-disabled-foreground",
    "buy-ring",
    "buy-foreground",
    "sell",
    "sell-hover",
    "sell-active",
    "sell-disabled",
    "sell-disabled-foreground",
    "sell-ring",
    "sell-foreground",
    "ring",
    "bullish",
    "bearish",
];

fn main() {
    println!("cargo:rerun-if-changed=platform.css");
    println!("cargo:rerun-if-changed=src/token_compiler.rs");
    if let Err(error) = compile_tokens() {
        panic!("invalid platform.css contract: {error}");
    }
}

fn compile_tokens() -> Result<(), String> {
    let css = fs::read_to_string("platform.css").map_err(|error| error.to_string())?;
    let (root, dark_overrides) = parse_theme_blocks(&css)?;
    let dark = cascade(&root, &dark_overrides);
    let mut output = String::new();

    emit_string(&mut output, "FONT_STACK", &resolve(&root, "font-sans")?);
    let stack = resolve(&root, "font-sans")?;
    let family = stack
        .strip_prefix('"')
        .and_then(|value| value.split_once('"').map(|(family, _)| family))
        .ok_or_else(|| "--font-sans must start with a quoted family".to_owned())?;
    emit_string(&mut output, "FONT_FAMILY", family);
    emit_u16(
        &mut output,
        "FONT_WEIGHT_NORMAL",
        &resolve(&root, "font-weight-normal")?,
    )?;
    emit_u16(
        &mut output,
        "FONT_WEIGHT_EMPHASIS",
        &resolve(&root, "font-weight-emphasis")?,
    )?;
    emit_u16(
        &mut output,
        "FONT_WEIGHT_STRONG",
        &resolve(&root, "font-weight-strong")?,
    )?;
    let feature = resolve(&root, "font-feature-tabular-numerals")?;
    emit_string(&mut output, "TABULAR_FEATURE", feature.trim_matches('"'));

    emit_colors(&mut output, "LIGHT_COLORS", &root)?;
    emit_colors(&mut output, "DARK_COLORS", &dark)?;
    emit_sources(&mut output, "LIGHT_COLOR_SOURCES", &root)?;
    emit_sources(&mut output, "DARK_COLOR_SOURCES", &dark)?;
    emit_f32(
        &mut output,
        "BORDER_WIDTH",
        parse_pixels(&resolve(&root, "border-width")?)?,
    );
    emit_pixel_u16(
        &mut output,
        "RADIUS_SMALL",
        &resolve(&root, "radius-small")?,
    )?;
    emit_pixel_u16(
        &mut output,
        "RADIUS_DEFAULT",
        &resolve(&root, "radius-default")?,
    )?;
    emit_pixel_u16(
        &mut output,
        "RADIUS_MEDIUM",
        &resolve(&root, "radius-medium")?,
    )?;
    emit_pixel_u16(
        &mut output,
        "RADIUS_LARGE",
        &resolve(&root, "radius-large")?,
    )?;
    emit_pixel_u16(
        &mut output,
        "RADIUS_BUTTON",
        &resolve(&root, "radius-button")?,
    )?;

    let path = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is unavailable")?)
        .join("platform_tokens.rs");
    fs::write(path, output).map_err(|error| error.to_string())
}

fn emit_string(output: &mut String, name: &str, value: &str) {
    writeln!(output, "pub(crate) const {name}: &str = {value:?};")
        .expect("writing generated tokens to a String cannot fail");
}

fn emit_u16(output: &mut String, name: &str, value: &str) -> Result<(), String> {
    let value = value
        .parse::<u16>()
        .map_err(|_| format!("{name} must be an integer"))?;
    writeln!(output, "pub(crate) const {name}: u16 = {value};")
        .expect("writing generated tokens to a String cannot fail");
    Ok(())
}

fn emit_f32(output: &mut String, name: &str, value: f32) {
    writeln!(output, "pub(crate) const {name}: f32 = {value:?};")
        .expect("writing generated tokens to a String cannot fail");
}

fn emit_pixel_u16(output: &mut String, name: &str, value: &str) -> Result<(), String> {
    let pixels = value
        .strip_suffix("px")
        .ok_or_else(|| format!("{name} must be a px dimension"))?;
    emit_u16(output, name, pixels)
}

fn emit_colors(
    output: &mut String,
    name: &str,
    declarations: &token_compiler::Declarations,
) -> Result<(), String> {
    writeln!(
        output,
        "pub(crate) const {name}: [[u8; 4]; {}] = [",
        COLORS.len()
    )
    .expect("writing generated tokens to a String cannot fail");
    for token in COLORS {
        let [r, g, b, a] = parse_color(&resolve(declarations, token)?)?;
        writeln!(output, "    [{r}, {g}, {b}, {a}],")
            .expect("writing generated tokens to a String cannot fail");
    }
    output.push_str("];\n");
    Ok(())
}

fn emit_sources(
    output: &mut String,
    name: &str,
    declarations: &token_compiler::Declarations,
) -> Result<(), String> {
    writeln!(
        output,
        "pub(crate) const {name}: [&str; {}] = [",
        COLORS.len()
    )
    .expect("writing generated tokens to a String cannot fail");
    for token in COLORS {
        writeln!(output, "    {:?},", source(declarations, token)?)
            .expect("writing generated tokens to a String cannot fail");
    }
    output.push_str("];\n");
    Ok(())
}
