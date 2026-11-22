//! OKX V5 instrument identity and market classification.

use aeris_crypto_common::{CanonicalCryptoKey, CryptoError, CryptoMarketKind, CryptoResult};

/// Supported OKX V5 public market segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OkxMarketKind {
    /// Linear perpetual swap (`instType=SWAP`, e.g. `BTC-USDT-SWAP`).
    Swap,
    /// Spot market (`instType=SPOT`, e.g. `BTC-USDT`).
    Spot,
}

impl OkxMarketKind {
    /// OKX `instType` query value.
    #[must_use]
    pub const fn inst_type(self) -> &'static str {
        match self {
            Self::Swap => "SWAP",
            Self::Spot => "SPOT",
        }
    }

    /// Wire prefix used in `provider_symbol` and `instrument_id`.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Swap => "swap",
            Self::Spot => "spot",
        }
    }

    /// Exchange label displayed in Aeris.
    #[must_use]
    pub const fn exchange_label(self) -> &'static str {
        match self {
            Self::Swap => "OKX-PERP",
            Self::Spot => "OKX-SPOT",
        }
    }

    /// Display instrument type label.
    #[must_use]
    pub const fn instrument_type_label(self) -> &'static str {
        match self {
            Self::Swap => "Perpetual",
            Self::Spot => "Spot",
        }
    }

    /// Canonical crypto market kind for cross-exchange aggregation.
    #[must_use]
    pub const fn crypto_kind(self) -> CryptoMarketKind {
        match self {
            Self::Swap => CryptoMarketKind::Perp,
            Self::Spot => CryptoMarketKind::Spot,
        }
    }
}

/// Parsed OKX provider symbol (`"swap:BTC-USDT-SWAP"` or `"spot:BTC-USDT"`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OkxSymbolRef {
    pub kind: OkxMarketKind,
    pub inst_id: String,
}

impl OkxSymbolRef {
    /// Formats the provider symbol stored in `InstallProviderInstrument`.
    #[must_use]
    pub fn provider_symbol(&self) -> String {
        format!("{}:{}", self.kind.prefix(), self.inst_id)
    }
}

/// Parses an OKX `provider_symbol` (`"swap:BTC-USDT-SWAP"` or `"spot:BTC-USDT"`).
///
/// # Errors
/// Returns an error if the prefix is unknown or the instrument ID is empty.
pub fn parse_provider_symbol(raw: &str) -> CryptoResult<OkxSymbolRef> {
    let (prefix, inst_id) = raw.split_once(':').ok_or_else(|| {
        CryptoError::Unsupported(format!("okx provider symbol is malformed: {raw}"))
    })?;
    let kind = match prefix {
        "swap" => OkxMarketKind::Swap,
        "spot" => OkxMarketKind::Spot,
        _ => {
            return Err(CryptoError::Unsupported(format!(
                "okx market kind is unknown: {prefix}"
            )));
        }
    };
    let inst_id = inst_id.trim().to_ascii_uppercase();
    if inst_id.is_empty() {
        return Err(CryptoError::Unsupported("okx instId is empty".to_string()));
    }
    Ok(OkxSymbolRef { kind, inst_id })
}

/// Builds the canonical `instrument_id` for an OKX instrument.
#[must_use]
pub fn instrument_id_for(kind: OkxMarketKind, inst_id: &str) -> String {
    format!("okx:{}:{}", kind.prefix(), inst_id.to_ascii_uppercase())
}

/// Metadata for one OKX trading instrument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OkxInstrument {
    pub kind: OkxMarketKind,
    /// Native OKX `instId` (e.g. `"BTC-USDT-SWAP"` or `"BTC-USDT"`).
    pub inst_id: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub display_symbol: String,
    pub provider_symbol: String,
    pub instrument_id: String,
    pub price_decimals: u8,
    pub quantity_decimals: u8,
    pub tick_size_fixed: i64,
    /// Contract value in base units at `NORMALIZED_QUANTITY_SCALE` (10^8).
    /// Always `100_000_000` (1.0) for Spot; equals `ctVal` for linear SWAPs.
    pub ct_val_fixed: i64,
    pub volume_24h_quote: u64,
}

impl OkxInstrument {
    /// Canonical cross-exchange key for aggregation.
    #[must_use]
    pub fn canonical_key(&self) -> CanonicalCryptoKey {
        CanonicalCryptoKey::new(&self.base_asset, &self.quote_asset, self.kind.crypto_kind())
    }
}
