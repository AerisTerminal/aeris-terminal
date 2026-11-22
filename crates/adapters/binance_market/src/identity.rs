//! Binance instrument identity and market classification.

use aeris_crypto_common::{CanonicalCryptoKey, CryptoError, CryptoMarketKind, CryptoResult};

/// Supported Binance public market segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BinanceMarketKind {
    /// USDT-margined perpetual futures (`fapi.binance.com`).
    UsdmPerp,
    /// Spot market (`api.binance.com`).
    Spot,
}

impl BinanceMarketKind {
    /// Wire prefix used in `provider_symbol` and `instrument_id`.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::UsdmPerp => "usdm",
            Self::Spot => "spot",
        }
    }

    /// Exchange label displayed in Aeris.
    #[must_use]
    pub const fn exchange_label(self) -> &'static str {
        match self {
            Self::UsdmPerp => "BINANCE-PERP",
            Self::Spot => "BINANCE-SPOT",
        }
    }

    /// Display instrument type label.
    #[must_use]
    pub const fn instrument_type_label(self) -> &'static str {
        match self {
            Self::UsdmPerp => "Perpetual",
            Self::Spot => "Spot",
        }
    }

    /// Canonical crypto market kind for cross-exchange aggregation.
    #[must_use]
    pub const fn crypto_kind(self) -> CryptoMarketKind {
        match self {
            Self::UsdmPerp => CryptoMarketKind::Perp,
            Self::Spot => CryptoMarketKind::Spot,
        }
    }
}

/// Parsed Binance provider symbol (`"usdm:BTCUSDT"` or `"spot:BTCUSDT"`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BinanceSymbolRef {
    pub kind: BinanceMarketKind,
    pub wire_symbol: String,
}

impl BinanceSymbolRef {
    /// Formats the provider symbol stored in `InstallProviderInstrument`.
    #[must_use]
    pub fn provider_symbol(&self) -> String {
        format!("{}:{}", self.kind.prefix(), self.wire_symbol)
    }
}

/// Parses a Binance `provider_symbol` (`"usdm:BTCUSDT"` or `"spot:BTCUSDT"`).
///
/// # Errors
/// Returns an error if the prefix is unknown or the symbol is empty.
pub fn parse_provider_symbol(raw: &str) -> CryptoResult<BinanceSymbolRef> {
    let (prefix, symbol) = raw.split_once(':').ok_or_else(|| {
        CryptoError::Unsupported(format!("binance provider symbol is malformed: {raw}"))
    })?;
    let kind = match prefix {
        "usdm" => BinanceMarketKind::UsdmPerp,
        "spot" => BinanceMarketKind::Spot,
        _ => {
            return Err(CryptoError::Unsupported(format!(
                "binance market kind is unknown: {prefix}"
            )));
        }
    };
    let wire_symbol = symbol.trim().to_ascii_uppercase();
    if wire_symbol.is_empty() {
        return Err(CryptoError::Unsupported(
            "binance wire symbol is empty".to_string(),
        ));
    }
    Ok(BinanceSymbolRef { kind, wire_symbol })
}

/// Builds the canonical `instrument_id` for a Binance instrument.
#[must_use]
pub fn instrument_id_for(kind: BinanceMarketKind, wire_symbol: &str) -> String {
    format!(
        "binance:{}:{}",
        kind.prefix(),
        wire_symbol.to_ascii_uppercase()
    )
}

/// Metadata for one Binance trading instrument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BinanceInstrument {
    pub kind: BinanceMarketKind,
    /// Native exchange symbol (e.g. `"BTCUSDT"`).
    pub wire_symbol: String,
    /// Base asset (e.g. `"BTC"`).
    pub base_asset: String,
    /// Quote asset (e.g. `"USDT"`).
    pub quote_asset: String,
    /// Display symbol in Aeris (`"BTCUSDT.P"` for perps, `"BTCUSDT"` for spot).
    pub display_symbol: String,
    /// Provider symbol (`"usdm:BTCUSDT"` / `"spot:BTCUSDT"`).
    pub provider_symbol: String,
    /// Canonical instrument ID (`"binance:usdm:BTCUSDT"`).
    pub instrument_id: String,
    /// Price decimal places inferred from `PRICE_FILTER.tickSize`.
    pub price_decimals: u8,
    /// Quantity decimal places inferred from `LOT_SIZE.stepSize`.
    pub quantity_decimals: u8,
    /// Minimum price increment at `NORMALIZED_PRICE_SCALE` (10^8).
    pub tick_size_fixed: i64,
    /// 24h quote volume in whole USD/quote units for ranking search results.
    pub volume_24h_quote: u64,
}

impl BinanceInstrument {
    /// Canonical cross-exchange key for aggregation.
    #[must_use]
    pub fn canonical_key(&self) -> CanonicalCryptoKey {
        CanonicalCryptoKey::new(&self.base_asset, &self.quote_asset, self.kind.crypto_kind())
    }
}
