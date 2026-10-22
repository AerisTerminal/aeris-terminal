//! The Aeris instrument id of a Rithmic contract. Market data mints it and order routing
//! reads it back, so both name a contract by the same exchange and symbol.

const PREFIX: &str = "instrument:rithmic:";

/// The instrument id of `symbol` on `exchange`, the exchange as the ticker plant names it.
#[must_use]
pub fn rithmic_instrument_id(exchange: &str, symbol: &str) -> String {
    format!("{PREFIX}{exchange}:{symbol}")
}

/// The exchange and symbol of a Rithmic instrument id, or `None` for any other id.
#[must_use]
pub fn parse_rithmic_instrument_id(instrument_id: &str) -> Option<(&str, &str)> {
    let (exchange, symbol) = instrument_id.strip_prefix(PREFIX)?.split_once(':')?;
    (!exchange.is_empty() && !symbol.is_empty()).then_some((exchange, symbol))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instrument_ids_round_trip_and_other_ids_are_refused() {
        let id = rithmic_instrument_id("CME", "ESZ5");
        assert_eq!(id, "instrument:rithmic:CME:ESZ5");
        assert_eq!(parse_rithmic_instrument_id(&id), Some(("CME", "ESZ5")));
        for other in [
            "instrument:hyperliquid:BTC",
            "instrument:rithmic:CME",
            "instrument:rithmic::ESZ5",
            "instrument:rithmic:CME:",
        ] {
            assert_eq!(parse_rithmic_instrument_id(other), None, "{other}");
        }
    }
}
