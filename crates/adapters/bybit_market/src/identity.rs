//! Bybit V5 instrument identity and market classification.

use aeris_crypto_common::{CanonicalCryptoKey, CryptoError, CryptoMarketKind, CryptoResult};

/// Supported Bybit V5 market segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BybitMarketKind {
    /// Linear USDT/USDC perpetual futures.
    Linear,
    /// Spot market.
    Spot,
}

impl BybitMarketKind {
    /// Bybit V5 `category` query parameter and `provider_symbol` prefix.
    #[must_use]
    pub const fn category(self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Spot => "spot",
        }
    }

    /// Exchange label displayed in Aeris.
    #[must_use]
    pub const fn exchange_label(self) -> &'static str {
        match self {
            Self::Linear => "BYBIT-PERP",
            Self::Spot => "BYBIT-SPOT",
        }
    }

    /// Display instrument type label.
    #[must_use]
    pub const fn instrument_type_label(self) -> &'static str {
        match self {
            Self::Linear => "Perpetual",
            Self::Spot => "Spot",
        }
    }

    /// Canonical crypto market kind for cross-exchange aggregation.
    #[must_use]
    pub const fn crypto_kind(self) -> CryptoMarketKind {
        match self {
            Self::Linear => CryptoMarketKind::Perp,
            Self::Spot => CryptoMarketKind::Spot,
        }
    }
}

/// Parsed Bybit provider symbol (`"linear:BTCUSDT"` or `"spot:BTCUSDT"`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BybitSymbolRef {
    pub kind: BybitMarketKind,
    pub wire_symbol: String,
}

impl BybitSymbolRef {
    /// Formats the provider symbol stored in `InstallProviderInstrument`.
    #[must_use]
    pub fn provider_symbol(&self) -> String {
        format!("{}:{}", self.kind.category(), self.wire_symbol)
    }
}

/// Parses a Bybit `provider_symbol` (`"linear:BTCUSDT"` or `"spot:BTCUSDT"`).
///
/// # Errors
/// Returns an error if the prefix is unknown or the symbol is empty.
pub fn parse_provider_symbol(raw: &str) -> CryptoResult<BybitSymbolRef> {
    let (prefix, symbol) = raw.split_once(':').ok_or_else(|| {
        CryptoError::Unsupported(format!("bybit provider symbol is malformed: {raw}"))
    })?;
    let kind = match prefix {
        "linear" => BybitMarketKind::Linear,
        "spot" => BybitMarketKind::Spot,
        _ => {
            return Err(CryptoError::Unsupported(format!(
                "bybit market kind is unknown: {prefix}"
            )));
        }
    };
    let wire_symbol = symbol.trim().to_ascii_uppercase();
    if wire_symbol.is_empty() {
        return Err(CryptoError::Unsupported(
            "bybit wire symbol is empty".to_string(),
        ));
    }
    Ok(BybitSymbolRef { kind, wire_symbol })
}

/// Builds the canonical `instrument_id` for a Bybit instrument.
#[must_use]
pub fn instrument_id_for(kind: BybitMarketKind, wire_symbol: &str) -> String {
    format!(
        "bybit:{}:{}",
        kind.category(),
        wire_symbol.to_ascii_uppercase()
    )
}

/// Metadata for one Bybit trading instrument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BybitInstrument {
    pub kind: BybitMarketKind,
    pub wire_symbol: String,
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

impl BybitInstrument {
    /// Canonical cross-exchange key for aggregation.
    #[must_use]
    pub fn canonical_key(&self) -> CanonicalCryptoKey {
        CanonicalCryptoKey::new(&self.base_asset, &self.quote_asset, self.kind.crypto_kind())
    }
}
