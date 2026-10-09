#[path = "src/token_compiler.rs"]
mod token_compiler;

use std::{env, fmt::Write as _, fs, path::PathBuf};
use token_compiler::{cascade, parse_color, parse_pixels, parse_theme_blocks, resolve, source};

#[macro_use]
#[path = "src/color_registry.rs"]
mod color_registry;

macro_rules! color_identifiers {
    ($($(#[$meta:meta])* $field:ident => $identifier:literal,)*) => {
        const COLORS: &[&str] = &[$($identifier),*];
    };
}

platform_color_registry!(color_identifiers);

/// Non-color custom properties the token contract owns outside the color registry.
fn is_non_color_token(name: &str) -> bool {
    name == "border-width"
        || ["font-", "radius-", "shadow-"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

fn main() {
    println!("cargo:rerun-if-changed=platform.css");
    println!("cargo:rerun-if-changed=src/token_compiler.rs");
    println!("cargo:rerun-if-changed=src/color_registry.rs");
    if let Err(error) = compile_tokens() {
        panic!("invalid platform.css contract: {error}");
    }
}

fn compile_tokens() -> Result<(), String> {
    let css = fs::read_to_string("platform.css").map_err(|error| error.to_string())?;
    let (root, dark_overrides) = parse_theme_blocks(&css)?;
    let dark = cascade(&root, &dark_overrides);
    require_registered_colors(&root)?;
    require_registered_colors(&dark)?;
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

    writeln!(
        output,
        "/// Number of color tokens `platform.css` declares.
pub const PLATFORM_COLOR_COUNT: usize = {};",
        COLORS.len()
    )
    .expect("writing generated tokens to a String cannot fail");
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
        "RADIUS_COMPACT",
        &resolve(&root, "radius-compact")?,
    )?;

    let path = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is unavailable")?)
        .join("platform_tokens.rs");
    fs::write(path, output).map_err(|error| error.to_string())
}

/// Every color the stylesheet declares must be projected natively, and every
/// other custom property must belong to a known non-color token family.
fn require_registered_colors(declarations: &token_compiler::Declarations) -> Result<(), String> {
    for name in declarations.keys() {
        if COLORS.contains(&name.as_str()) {
            continue;
        }
        if parse_color(&resolve(declarations, name)?).is_ok() {
            return Err(format!(
                "color --{name} is missing from src/color_registry.rs"
            ));
        }
        if !is_non_color_token(name) {
            return Err(format!("--{name} is not a recognised platform token"));
        }
    }
    Ok(())
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
        "pub(crate) const {name}: [[u8; 4]; PLATFORM_COLOR_COUNT] = ["
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
        "pub(crate) const {name}: [&str; PLATFORM_COLOR_COUNT] = ["
    )
    .expect("writing generated tokens to a String cannot fail");
    for token in COLORS {
        writeln!(output, "    {:?},", source(declarations, token)?)
            .expect("writing generated tokens to a String cannot fail");
    }
    output.push_str("];\n");
    Ok(())
}
