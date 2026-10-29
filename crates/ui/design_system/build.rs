#[path = "src/token_compiler.rs"]
mod token_compiler;

use std::{env, fmt::Write as _, fs, path::PathBuf};
use token_compiler::{
    AERIS_ONLY_FALLBACKS, Declarations, cascade, parse_aeris_only_blocks, parse_color,
    parse_preset_blocks, parse_theme_blocks, preset_ids, reject_mode_invariant_tokens, resolve,
    source,
};

#[macro_use]
#[path = "src/color_registry.rs"]
mod color_registry;

macro_rules! color_identifiers {
    ($($(#[$meta:meta])* $field:ident => $identifier:literal,)*) => {
        const COLORS: &[&str] = &[$($identifier),*];
    };
}

platform_color_registry!(color_identifiers);

/// Shape tokens a preset may restate but never change: presets only recolor the platform.
const PRESET_INVARIANT_TOKENS: &[&str] = &[
    "border-width",
    "radius-small",
    "radius-compact",
    "radius-default",
    "radius-medium",
    "radius-large",
];

/// Non-color custom properties the token contract owns outside the color registry.
fn is_non_color_token(name: &str) -> bool {
    name == "border-width"
        || ["font-", "radius-"]
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

/// The first, quoted family of a CSS font stack such as `"DM Sans", sans-serif`.
fn primary_family<'a>(stack: &'a str, property: &str) -> Result<&'a str, String> {
    stack
        .strip_prefix('"')
        .and_then(|value| value.split_once('"').map(|(family, _)| family))
        .ok_or_else(|| format!("--{property} must start with a quoted family"))
}

fn compile_tokens() -> Result<(), String> {
    let css = fs::read_to_string("platform.css").map_err(|error| error.to_string())?;
    let (root, dark_overrides) = parse_theme_blocks(&css)?;
    let dark = cascade(&root, &dark_overrides);
    reject_aeris_only_tokens(&root, ":root")?;
    reject_aeris_only_tokens(&dark_overrides, ".dark")?;
    reject_mode_invariant_tokens(&dark_overrides, ".dark")?;
    let (aeris_light_only, aeris_dark_only) = parse_aeris_only_blocks(&css)?;
    let aeris_light = cascade(&root, &aeris_light_only);
    let aeris_dark = cascade(&dark, &aeris_dark_only);
    require_registered_colors(&aeris_light)?;
    require_registered_colors(&aeris_dark)?;
    let presets = compile_presets(&css, &root, &dark)?;
    let mut output = String::new();

    let stack = resolve(&root, "font-sans")?;
    emit_string(&mut output, "FONT_STACK", &stack);
    emit_string(
        &mut output,
        "FONT_FAMILY",
        primary_family(&stack, "font-sans")?,
    );
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
    emit_colors(&mut output, "LIGHT_COLORS", &aeris_light)?;
    emit_colors(&mut output, "DARK_COLORS", &aeris_dark)?;
    emit_sources(&mut output, "LIGHT_COLOR_SOURCES", &aeris_light)?;
    emit_sources(&mut output, "DARK_COLOR_SOURCES", &aeris_dark)?;
    emit_presets(&mut output, &presets)?;
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
    emit_pixel_u16(
        &mut output,
        "BORDER_WIDTH",
        &resolve(&root, "border-width")?,
    )?;
    if resolve(&dark, "border-width")? != resolve(&root, "border-width")? {
        return Err("--border-width must be the same in light and dark".into());
    }

    let path = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is unavailable")?)
        .join("platform_tokens.rs");
    fs::write(path, output).map_err(|error| error.to_string())
}

/// One `[data-theme]` preset resolved through the stylesheet cascade.
struct Preset {
    id: String,
    light: Declarations,
    dark: Declarations,
}

/// Resolves each preset the way the CSS cascade does: the preset's light block overrides
/// `:root` (and `.dark`, which has the same specificity but appears earlier), and its
/// `.dark` block overrides that. The Aeris-only `:not([data-theme])` blocks never match a
/// preset, so each Aeris-only token takes the fallback components read it with.
fn compile_presets(
    css: &str,
    root: &Declarations,
    dark: &Declarations,
) -> Result<Vec<Preset>, String> {
    preset_ids(css)?
        .into_iter()
        .map(|id| {
            let (light_overrides, dark_overrides) = parse_preset_blocks(css, &id)?;
            reject_aeris_only_tokens(&light_overrides, &id)?;
            reject_aeris_only_tokens(&dark_overrides, &id)?;
            reject_mode_invariant_tokens(&light_overrides, &id)?;
            reject_mode_invariant_tokens(&dark_overrides, &id)?;
            let light = with_aeris_only_fallbacks(cascade(root, &light_overrides));
            let dark = with_aeris_only_fallbacks(cascade(
                &cascade(dark, &light_overrides),
                &dark_overrides,
            ));
            for declarations in [&light, &dark] {
                require_registered_colors(declarations)?;
                for token in PRESET_INVARIANT_TOKENS {
                    if resolve(declarations, token)? != resolve(root, token)? {
                        return Err(format!("preset {id} must not change --{token}"));
                    }
                }
            }
            Ok(Preset { id, light, dark })
        })
        .collect()
}

/// Aeris-only tokens live solely in the `:not([data-theme])` blocks.
fn reject_aeris_only_tokens(declarations: &Declarations, block: &str) -> Result<(), String> {
    match AERIS_ONLY_FALLBACKS
        .iter()
        .find(|(name, _)| declarations.contains_key(*name))
    {
        Some((name, _)) => Err(format!(
            "--{name} is Aeris-only and must not be declared in {block}"
        )),
        None => Ok(()),
    }
}

fn with_aeris_only_fallbacks(mut declarations: Declarations) -> Declarations {
    for (name, fallback) in AERIS_ONLY_FALLBACKS {
        declarations.insert(name.to_owned(), fallback.to_owned());
    }
    declarations
}

/// Every color the stylesheet declares must be projected natively, and every
/// other custom property must belong to a known non-color token family.
fn require_registered_colors(declarations: &Declarations) -> Result<(), String> {
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

fn emit_pixel_u16(output: &mut String, name: &str, value: &str) -> Result<(), String> {
    let pixels = value
        .strip_suffix("px")
        .ok_or_else(|| format!("{name} must be a px dimension"))?;
    emit_u16(output, name, pixels)
}

const COLOR_ROW: &str = "[[u8; 4]; PLATFORM_COLOR_COUNT]";
const SOURCE_ROW: &str = "[&str; PLATFORM_COLOR_COUNT]";

/// The resolved colors of one palette as a registry-ordered array literal.
fn color_rows(declarations: &Declarations) -> Result<String, String> {
    let mut rows = String::from("[\n");
    for token in COLORS {
        let [r, g, b, a] = parse_color(&resolve(declarations, token)?)?;
        writeln!(rows, "    [{r}, {g}, {b}, {a}],")
            .expect("writing generated tokens to a String cannot fail");
    }
    rows.push(']');
    Ok(rows)
}

/// The source expressions of one palette as a registry-ordered array literal.
fn source_rows(declarations: &Declarations) -> Result<String, String> {
    let mut rows = String::from("[\n");
    for token in COLORS {
        writeln!(rows, "    {:?},", source(declarations, token)?)
            .expect("writing generated tokens to a String cannot fail");
    }
    rows.push(']');
    Ok(rows)
}

fn emit_colors(output: &mut String, name: &str, declarations: &Declarations) -> Result<(), String> {
    writeln!(
        output,
        "pub(crate) const {name}: {COLOR_ROW} = {};",
        color_rows(declarations)?
    )
    .expect("writing generated tokens to a String cannot fail");
    Ok(())
}

fn emit_sources(
    output: &mut String,
    name: &str,
    declarations: &Declarations,
) -> Result<(), String> {
    writeln!(
        output,
        "pub(crate) const {name}: {SOURCE_ROW} = {};",
        source_rows(declarations)?
    )
    .expect("writing generated tokens to a String cannot fail");
    Ok(())
}

fn emit_presets(output: &mut String, presets: &[Preset]) -> Result<(), String> {
    writeln!(
        output,
        "pub(crate) const PRESET_COUNT: usize = {};
pub(crate) const PRESET_IDS: [&str; PRESET_COUNT] = {:?};",
        presets.len(),
        presets
            .iter()
            .map(|preset| preset.id.as_str())
            .collect::<Vec<_>>()
    )
    .expect("writing generated tokens to a String cannot fail");
    emit_preset_rows(
        output,
        "PRESET_LIGHT_COLORS",
        COLOR_ROW,
        presets,
        |preset| color_rows(&preset.light),
    )?;
    emit_preset_rows(output, "PRESET_DARK_COLORS", COLOR_ROW, presets, |preset| {
        color_rows(&preset.dark)
    })?;
    emit_preset_rows(
        output,
        "PRESET_LIGHT_COLOR_SOURCES",
        SOURCE_ROW,
        presets,
        |preset| source_rows(&preset.light),
    )?;
    emit_preset_rows(
        output,
        "PRESET_DARK_COLOR_SOURCES",
        SOURCE_ROW,
        presets,
        |preset| source_rows(&preset.dark),
    )
}

/// One row per preset, in `PRESET_IDS` order.
fn emit_preset_rows(
    output: &mut String,
    name: &str,
    row_type: &str,
    presets: &[Preset],
    rows: impl Fn(&Preset) -> Result<String, String>,
) -> Result<(), String> {
    writeln!(
        output,
        "pub(crate) const {name}: [{row_type}; PRESET_COUNT] = ["
    )
    .expect("writing generated tokens to a String cannot fail");
    for preset in presets {
        writeln!(output, "{},", rows(preset)?)
            .expect("writing generated tokens to a String cannot fail");
    }
    output.push_str("];\n");
    Ok(())
}
