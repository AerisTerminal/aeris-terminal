//! Exact, checked decimal normalization shared by provider adapters.

/// Parses a decimal string directly into `value * 10^scale`.
///
/// Accepts an optional leading `-`, one optional `.`, and ASCII digits only.
///
/// # Errors
///
/// Returns an error for empty or malformed input, an unsupported scale,
/// more fractional digits than `scale` allows, or any overflowing
/// intermediate.
pub fn parse_decimal_to_fixed(raw: &str, scale: u32) -> Result<i64, String> {
    if scale > 18 {
        return Err("market decimal scale is unsupported".to_string());
    }
    let text = raw.trim();
    if text.is_empty() {
        return Err("market decimal value is empty".to_string());
    }
    // Shortest-repr floats can surface exponent notation (e.g. `1e-7`);
    // normalize the decimal point first so the strict parser below still
    // sees plain decimal text.
    let text = if text.bytes().any(|byte| byte == b'e' || byte == b'E') {
        expand_decimal_exponent(text)?
    } else {
        text.to_string()
    };
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(&text)),
    };
    if digits.is_empty() {
        return Err("market decimal value is malformed".to_string());
    }
    let mut parts = digits.split('.');
    let whole = parts.next().unwrap_or("");
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some() {
        return Err("market decimal value is malformed".to_string());
    }
    if whole.is_empty() && fraction.is_empty() {
        return Err("market decimal value is malformed".to_string());
    }
    for part in [whole, fraction] {
        if !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("market decimal value is malformed".to_string());
        }
    }
    if u32::try_from(fraction.len()).unwrap_or(u32::MAX) > scale {
        return Err("market decimal precision exceeds scale".to_string());
    }
    let mut value: i64 = 0;
    for byte in whole.bytes() {
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i64::from(byte - b'0')))
            .ok_or_else(|| "market decimal value overflowed".to_string())?;
    }
    let missing = scale - u32::try_from(fraction.len()).unwrap_or(scale);
    for byte in fraction.bytes() {
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i64::from(byte - b'0')))
            .ok_or_else(|| "market decimal value overflowed".to_string())?;
    }
    value = ten_pow(missing)
        .and_then(|factor| value.checked_mul(factor))
        .ok_or_else(|| "market decimal value overflowed".to_string())?;
    if negative {
        value = value
            .checked_neg()
            .ok_or_else(|| "market decimal value overflowed".to_string())?;
    }
    Ok(value)
}

/// Expands one exponent-notation decimal (`1.25e-4`) into plain text.
///
/// # Errors
/// Rejects malformed or unbounded exponents.
pub fn expand_decimal_exponent(raw: &str) -> Result<String, String> {
    let malformed = || "market decimal value is malformed".to_string();
    let (mantissa, exp_text) = raw.split_once(['e', 'E']).ok_or_else(malformed)?;
    let exp: i32 = exp_text.parse().map_err(|_| malformed())?;
    if exp.abs() > 36 {
        return Err(malformed());
    }
    let (negative, digits) = match mantissa.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, mantissa.strip_prefix('+').unwrap_or(mantissa)),
    };
    let mut parts = digits.split('.');
    let whole = parts.next().unwrap_or("");
    let fraction = parts.next().unwrap_or("");
    if parts.next().is_some()
        || (whole.is_empty() && fraction.is_empty())
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(malformed());
    }
    let mut digits = format!("{whole}{fraction}");
    if digits.trim_matches('0').is_empty() {
        return Ok("0".to_string());
    }
    let mut point = i64::try_from(whole.len().max(1)).map_err(|_| malformed())? + i64::from(exp);
    // Strip leading zeroes; they shift the point with the digits.
    let leading = digits.bytes().take_while(|byte| *byte == b'0').count();
    digits.drain(..leading);
    point -= i64::try_from(leading).map_err(|_| malformed())?;
    let expanded = if point <= 0 {
        let zeros = usize::try_from(-point).map_err(|_| malformed())?;
        format!("0.{}{digits}", "0".repeat(zeros))
    } else {
        let at = usize::try_from(point).map_err(|_| malformed())?;
        if at >= digits.len() {
            format!("{digits}{}", "0".repeat(at - digits.len()))
        } else {
            let mut expanded = digits;
            expanded.insert(at, '.');
            expanded
        }
    };
    Ok(if negative {
        format!("-{expanded}")
    } else {
        expanded
    })
}

fn ten_pow(exp: u32) -> Option<i64> {
    let mut value: i64 = 1;
    for _ in 0..exp {
        value = value.checked_mul(10)?;
    }
    Some(value)
}
