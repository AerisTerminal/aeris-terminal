//! Canonical cross-exchange cryptocurrency instrument identity for aggregation.

use std::fmt;

/// Market kind shared across crypto exchanges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CryptoMarketKind {
    Perp,
    Spot,
}

impl CryptoMarketKind {
    /// Wire suffix used in canonical IDs (`PERP` / `SPOT`).
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Perp => "PERP",
            Self::Spot => "SPOT",
        }
    }
}

/// Cross-exchange canonical key uniting identical contracts across venues
/// (e.g. Binance `BTCUSDT` perp, Bybit `BTCUSDT` linear, OKX `BTC-USDT-SWAP`,
/// and Hyperliquid `BTC` perp all map to `BTC-USDT-PERP`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CanonicalCryptoKey {
    pub base: String,
    pub quote: String,
    pub kind: CryptoMarketKind,
}

impl CanonicalCryptoKey {
    /// Builds a normalized uppercase canonical key.
    #[must_use]
    pub fn new(base: &str, quote: &str, kind: CryptoMarketKind) -> Self {
        Self {
            base: base.trim().to_ascii_uppercase(),
            quote: quote.trim().to_ascii_uppercase(),
            kind,
        }
    }

    /// Canonical display symbol shown in Aeris (`BTCUSDT.P` for perps, `BTCUSDT` for spot).
    #[must_use]
    pub fn display_symbol(&self) -> String {
        match self.kind {
            CryptoMarketKind::Perp => format!("{}{}.P", self.base, self.quote),
            CryptoMarketKind::Spot => format!("{}{}", self.base, self.quote),
        }
    }

    /// Canonical provider-neutral instrument ID for the aggregated feed.
    #[must_use]
    pub fn aggregated_instrument_id(&self) -> String {
        format!(
            "crypto_agg:{}:{}-{}",
            self.kind.tag().to_ascii_lowercase(),
            self.base,
            self.quote
        )
    }

    /// Parses a canonical key from `"BTC-USDT-PERP"` or `"BTC-USDT-SPOT"`.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let mut parts = raw.split('-');
        let base = parts.next()?.trim();
        let quote = parts.next()?.trim();
        let kind = match parts.next()?.trim().to_ascii_uppercase().as_str() {
            "PERP" => CryptoMarketKind::Perp,
            "SPOT" => CryptoMarketKind::Spot,
            _ => return None,
        };
        if parts.next().is_some() || base.is_empty() || quote.is_empty() {
            return None;
        }
        Some(Self::new(base, quote, kind))
    }
}

impl fmt::Display for CanonicalCryptoKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}-{}", self.base, self.quote, self.kind.tag())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_key_round_trips() {
        let key = CanonicalCryptoKey::new("btc", "usdt", CryptoMarketKind::Perp);
        assert_eq!(key.to_string(), "BTC-USDT-PERP");
        assert_eq!(key.display_symbol(), "BTCUSDT.P");
        assert_eq!(key.aggregated_instrument_id(), "crypto_agg:perp:BTC-USDT");
        assert_eq!(CanonicalCryptoKey::parse("BTC-USDT-PERP"), Some(key));
    }
}
