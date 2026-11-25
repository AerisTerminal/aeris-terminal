//! Coinbase public instrument identity and market classification.

use aeris_crypto_common::{CanonicalCryptoKey, CryptoError, CryptoMarketKind, CryptoResult};

/// Supported Coinbase public market segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CoinbaseMarketKind {
    /// Spot market (`product_type=SPOT`, e.g. `BTC-USD`).
    Spot,
}

impl CoinbaseMarketKind {
    /// Wire prefix used in `provider_symbol` and `instrument_id`.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Spot => "spot",
        }
    }

    /// Exchange label displayed in Aeris.
    #[must_use]
    pub const fn exchange_label(self) -> &'static str {
        match self {
            Self::Spot => "COINBASE",
        }
    }

    /// Display instrument type label.
    #[must_use]
    pub const fn instrument_type_label(self) -> &'static str {
        match self {
            Self::Spot => "Spot",
        }
    }

    /// Canonical crypto market kind for cross-exchange aggregation.
    #[must_use]
    pub const fn crypto_kind(self) -> CryptoMarketKind {
        match self {
            Self::Spot => CryptoMarketKind::Spot,
        }
    }
}

/// Parsed Coinbase provider symbol (`"spot:BTC-USD"`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CoinbaseSymbolRef {
    pub kind: CoinbaseMarketKind,
    pub product_id: String,
}

impl CoinbaseSymbolRef {
    /// Formats the provider symbol stored in `InstallProviderInstrument`.
    #[must_use]
    pub fn provider_symbol(&self) -> String {
        format!("{}:{}", self.kind.prefix(), self.product_id)
    }
}

/// Parses a Coinbase `provider_symbol` (`"spot:BTC-USD"`).
///
/// # Errors
/// Returns an error if the prefix is unknown or the product ID is empty.
pub fn parse_provider_symbol(raw: &str) -> CryptoResult<CoinbaseSymbolRef> {
    let (prefix, product_id) = raw.split_once(':').ok_or_else(|| {
        CryptoError::Unsupported(format!("coinbase provider symbol is malformed: {raw}"))
    })?;
    let kind = match prefix {
        "spot" => CoinbaseMarketKind::Spot,
        _ => {
            return Err(CryptoError::Unsupported(format!(
                "coinbase market kind is unknown: {prefix}"
            )));
        }
    };
    let product_id = product_id.trim().to_ascii_uppercase();
    if product_id.is_empty() {
        return Err(CryptoError::Unsupported(
            "coinbase product_id is empty".to_string(),
        ));
    }
    Ok(CoinbaseSymbolRef { kind, product_id })
}

/// Builds the canonical `instrument_id` for a Coinbase instrument.
#[must_use]
pub fn instrument_id_for(kind: CoinbaseMarketKind, product_id: &str) -> String {
    format!(
        "coinbase:{}:{}",
        kind.prefix(),
        product_id.to_ascii_uppercase()
    )
}

/// Metadata for one Coinbase trading instrument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinbaseInstrument {
    pub kind: CoinbaseMarketKind,
    /// Native Coinbase `product_id` (e.g. `"BTC-USD"`).
    pub product_id: String,
    /// Aliased product IDs that share this product's unified matching book (e.g. `["BTC-USDC"]`).
    pub alias_to: Vec<String>,
    pub base_asset: String,
    pub quote_asset: String,
    pub display_symbol: String,
    pub provider_symbol: String,
    pub instrument_id: String,
    pub price_decimals: u8,
    pub quantity_decimals: u8,
    pub tick_size_fixed: i64,
    pub volume_24h_quote: u64,
}

impl CoinbaseInstrument {
    /// Canonical cross-exchange key for aggregation.
    #[must_use]
    pub fn canonical_key(&self) -> CanonicalCryptoKey {
        CanonicalCryptoKey::new(&self.base_asset, &self.quote_asset, self.kind.crypto_kind())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_provider_symbol_and_builds_instrument_id() {
        let sym = parse_provider_symbol("spot:btc-usd").expect("valid symbol");
        assert_eq!(sym.kind, CoinbaseMarketKind::Spot);
        assert_eq!(sym.product_id, "BTC-USD");
        assert_eq!(sym.provider_symbol(), "spot:BTC-USD");
        assert_eq!(
            instrument_id_for(sym.kind, &sym.product_id),
            "coinbase:spot:BTC-USD"
        );
    }

    #[test]
    fn rejects_invalid_provider_symbols() {
        assert!(parse_provider_symbol("BTC-USD").is_err());
        assert!(parse_provider_symbol("perp:BTC-USD").is_err());
        assert!(parse_provider_symbol("spot:  ").is_err());
    }
}
