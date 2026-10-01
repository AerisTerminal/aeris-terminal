use crate::MarketBar;

/// Merges a live candle replacement into closed history by open timestamp.
///
/// Returns the updated closed tail plus whether the update replaced the
/// forming candle in place (`false`) or completed it and opened the next
/// period (`true`). Duplicate updates with identical OHLCV are idempotent.
///
/// # Errors
///
/// Returns an error for invalid bars, stale updates, non-canonical
/// rollovers, or sequence overflow.
pub fn merge_live_candle(
    closed: &mut Vec<MarketBar>,
    forming: &mut Option<MarketBar>,
    update: MarketBar,
) -> Result<bool, String> {
    update.validate().map_err(|error| error.to_string())?;
    if let Some(open) = forming {
        if open.exchange_timestamp_unix_nanos == update.exchange_timestamp_unix_nanos {
            if *open == update {
                return Ok(false);
            }
            *open = update;
            return Ok(false);
        }
        if update.exchange_timestamp_unix_nanos < open.exchange_timestamp_unix_nanos {
            return Err("provider live candle is stale".to_string());
        }
        // The open period completed: it joins closed history, the update
        // becomes the new forming candle.
        let mut completed = *open;
        completed.source_sequence = closed
            .last()
            .map_or(0, |bar| bar.source_sequence)
            .checked_add(1)
            .ok_or_else(|| "provider candle sequence overflowed".to_string())?;
        if closed.last().is_some_and(|last| {
            last.exchange_timestamp_unix_nanos >= completed.exchange_timestamp_unix_nanos
        }) {
            return Err("provider candle rollover is not canonical".to_string());
        }
        closed.push(completed);
        *open = update;
        return Ok(true);
    }
    // No forming candle yet: the update must extend history, never rewrite it.
    if closed.last().is_some_and(|last| {
        update.exchange_timestamp_unix_nanos <= last.exchange_timestamp_unix_nanos
    }) {
        // Exact duplicate of the tail is idempotent; anything older is stale.
        if closed.last().is_some_and(|last| *last == update) {
            return Ok(false);
        }
        return Err("provider live candle is stale".to_string());
    }
    *forming = Some(update);
    Ok(false)
}
