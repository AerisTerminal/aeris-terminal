use std::collections::{BTreeMap, BTreeSet};

pub(crate) type Declarations = BTreeMap<String, String>;

pub(crate) fn parse_theme_blocks(css: &str) -> Result<(Declarations, Declarations), String> {
    let css = strip_comments(css)?;
    let root = parse_blocks(&css, ":root")?;
    let dark = parse_blocks(&css, ".dark")?;
    if root.is_empty() || dark.is_empty() {
        return Err("platform.css must contain :root and .dark token blocks".into());
    }
    Ok((root, dark))
}

fn strip_comments(css: &str) -> Result<String, String> {
    let mut output = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        output.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("*/") else {
            return Err("unterminated CSS comment".into());
        };
        rest = &after[end + 2..];
    }
    output.push_str(rest);
    Ok(output)
}

fn parse_blocks(css: &str, selector: &str) -> Result<Declarations, String> {
    let mut declarations = BTreeMap::new();
    let mut cursor = 0;
    while let Some(relative) = css[cursor..].find(selector) {
        let selector_start = cursor + relative;
        let after_selector = selector_start + selector.len();
        let Some(open_relative) = css[after_selector..].find('{') else {
            return Err(format!("{selector} block is missing an opening brace"));
        };
        let open = after_selector + open_relative;
        if !css[after_selector..open].trim().is_empty() {
            cursor = after_selector;
            continue;
        }
        let Some(close_relative) = css[open + 1..].find('}') else {
            return Err(format!("{selector} block is missing a closing brace"));
        };
        let close = open + 1 + close_relative;
        for declaration in css[open + 1..close].split(';') {
            let declaration = declaration.trim();
            if declaration.is_empty() || !declaration.starts_with("--") {
                continue;
            }
            let Some((name, value)) = declaration.split_once(':') else {
                return Err(format!(
                    "invalid custom-property declaration `{declaration}`"
                ));
            };
            let name = name.trim().trim_start_matches("--").to_owned();
            let value = value.trim().to_owned();
            if value.is_empty() {
                return Err(format!("--{name} has no value"));
            }
            if declarations.insert(name.clone(), value).is_some() {
                return Err(format!("duplicate --{name} declaration in {selector}"));
            }
        }
        cursor = close + 1;
    }
    Ok(declarations)
}

pub(crate) fn cascade(root: &Declarations, overrides: &Declarations) -> Declarations {
    let mut result = root.clone();
    result.extend(overrides.clone());
    result
}

pub(crate) fn source<'a>(declarations: &'a Declarations, name: &str) -> Result<&'a str, String> {
    declarations
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| format!("missing required --{name} declaration"))
}

pub(crate) fn resolve(declarations: &Declarations, name: &str) -> Result<String, String> {
    resolve_inner(declarations, name, &mut BTreeSet::new())
}

fn resolve_inner(
    declarations: &Declarations,
    name: &str,
    visiting: &mut BTreeSet<String>,
) -> Result<String, String> {
    if !visiting.insert(name.to_owned()) {
        return Err(format!("cyclic custom-property reference at --{name}"));
    }
    let value = source(declarations, name)?;
    let mut resolved = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(start) = remaining.find("var(--") {
        resolved.push_str(&remaining[..start]);
        let reference_start = &remaining[start + "var(--".len()..];
        let end = reference_start
            .find(')')
            .ok_or_else(|| format!("unterminated custom-property reference in --{name}"))?;
        resolved.push_str(&resolve_inner(
            declarations,
            reference_start[..end].trim(),
            visiting,
        )?);
        remaining = &reference_start[end + 1..];
    }
    resolved.push_str(remaining);
    visiting.remove(name);
    Ok(resolved)
}

pub(crate) fn parse_hex(value: &str) -> Result<[u8; 4], String> {
    let hex = value
        .strip_prefix('#')
        .ok_or_else(|| format!("expected hexadecimal color, found `{value}`"))?;
    let expanded;
    let hex = match hex.len() {
        3 | 4 => {
            expanded = hex
                .chars()
                .flat_map(|character| [character, character])
                .collect::<String>();
            expanded.as_str()
        }
        6 | 8 => hex,
        _ => return Err(format!("unsupported hexadecimal color `{value}`")),
    };
    let byte = |offset| {
        u8::from_str_radix(&hex[offset..offset + 2], 16)
            .map_err(|_| format!("invalid hexadecimal color `{value}`"))
    };
    Ok([
        byte(0)?,
        byte(2)?,
        byte(4)?,
        if hex.len() == 8 { byte(6)? } else { 255 },
    ])
}

pub(crate) fn parse_color(value: &str) -> Result<[u8; 4], String> {
    if value.starts_with('#') {
        return parse_hex(value);
    }
    let inner = value
        .strip_prefix("color-mix(in srgb,")
        .and_then(|value| value.strip_suffix(')'))
        .ok_or_else(|| format!("expected a supported color, found `{value}`"))?
        .trim();
    let (color_and_weight, second_color) = inner
        .split_once(',')
        .ok_or_else(|| format!("invalid color-mix `{value}`"))?;
    let (color, weight) = color_and_weight
        .trim()
        .split_once(' ')
        .ok_or_else(|| format!("color-mix is missing a percentage in `{value}`"))?;
    let percent = weight
        .trim()
        .strip_suffix('%')
        .ok_or_else(|| format!("color-mix is missing a percentage in `{value}`"))?
        .parse::<u16>()
        .map_err(|_| format!("invalid color-mix percentage in `{value}`"))?;
    if percent > 100 {
        return Err(format!("color-mix percentage is out of range in `{value}`"));
    }
    let first = parse_hex(color)?;
    let second = match second_color.trim() {
        "transparent" => [0, 0, 0, 0],
        "white" => [255, 255, 255, 255],
        color => parse_hex(color)?,
    };
    let weight = u32::from(percent);
    let other_weight = 100 - weight;
    let alpha_sum = u32::from(first[3]) * weight + u32::from(second[3]) * other_weight;
    if alpha_sum == 0 {
        return Ok([0, 0, 0, 0]);
    }
    let alpha = u8::try_from((alpha_sum + 50) / 100)
        .map_err(|_| format!("color-mix alpha is out of range in `{value}`"))?;
    let channel = |index| {
        u8::try_from(
            (u32::from(first[index]) * u32::from(first[3]) * weight
                + u32::from(second[index]) * u32::from(second[3]) * other_weight
                + alpha_sum / 2)
                / alpha_sum,
        )
        .map_err(|_| format!("color-mix channel is out of range in `{value}`"))
    };
    Ok([channel(0)?, channel(1)?, channel(2)?, alpha])
}

pub(crate) fn parse_pixels(value: &str) -> Result<f32, String> {
    value
        .strip_suffix("px")
        .ok_or_else(|| format!("expected px dimension, found `{value}`"))?
        .parse::<f32>()
        .map_err(|_| format!("invalid px dimension `{value}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_after_dark_mode_cascade() {
        let (root, dark) =
            parse_theme_blocks(":root { --a: #fff; --b: var(--a); } .dark { --a: #00000080; }")
                .unwrap();
        assert_eq!(
            parse_hex(&resolve(&root, "b").unwrap()).unwrap(),
            [255, 255, 255, 255]
        );
        assert_eq!(
            parse_hex(&resolve(&cascade(&root, &dark), "b").unwrap()).unwrap(),
            [0, 0, 0, 128]
        );
    }

    #[test]
    fn malformed_missing_duplicate_and_cyclic_contracts_fail() {
        assert!(
            parse_theme_blocks(":root { --a: #fff; --a: #000; } .dark { --a: #000; }").is_err()
        );
        let (root, _) =
            parse_theme_blocks(":root { --a: var(--b); --b: var(--a); } .dark { --a: #000; }")
                .unwrap();
        assert!(resolve(&root, "a").is_err());
        assert!(resolve(&root, "missing").is_err());
        assert!(parse_pixels("1rem").is_err());
        assert!(parse_hex("red").is_err());
        assert!(parse_color("color-mix(in srgb, red 50%, transparent)").is_err());
    }

    #[test]
    fn color_mix_with_transparency_resolves_to_rgba() {
        assert_eq!(
            parse_color("color-mix(in srgb, #c2c2c2 50%, transparent)").unwrap(),
            [194, 194, 194, 128]
        );
    }

    #[test]
    fn color_mix_resolves_embedded_aliases_and_opaque_colors() {
        let (root, dark) = parse_theme_blocks(
            ":root { --primary: #006edd; --surface: #ffffff; --subtle: color-mix(in srgb, var(--primary) 12%, var(--surface)); } .dark { --surface: #1f1f1f; --subtle: color-mix(in srgb, var(--primary) 22%, var(--surface)); }",
        )
        .unwrap();
        assert_eq!(
            parse_color(&resolve(&root, "subtle").unwrap()).unwrap(),
            [224, 238, 251, 255]
        );
        assert_eq!(
            parse_color(&resolve(&cascade(&root, &dark), "subtle").unwrap()).unwrap(),
            [24, 48, 73, 255]
        );
        assert_eq!(
            parse_color("color-mix(in srgb, #006edd 65%, white)").unwrap(),
            [89, 161, 233, 255]
        );
    }
}
