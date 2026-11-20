//! Provider-neutral resolved instrument and contract metadata shared between
//! concrete provider adapters and `market_runtime`.
//!
//! Adapters produce `ResolvedProviderInstrument` values without depending on the
//! persisted workspace protobuf schema (`aeris_contracts`); `market_runtime` maps
//! between `ResolvedProviderInstrument`, persisted `InstallProviderInstrument`,
//! `InstrumentDescriptor`, `HistoryInstrument`, and `InstrumentTerms` at one
//! authoritative boundary.

use aeris_instruments::InstrumentClass;

use crate::history::{HistoryContractMetadata, HistoryInstrument};

/// Provider-neutral instrument classification.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum ResolvedInstrumentClass {
    #[default]
    Other,
    Equity,
    Option,
    Future,
    ForeignExchange,
    CryptoSpot,
    CryptoPerpetual,
}

impl ResolvedInstrumentClass {
    /// Projects this provider classification into the canonical domain `InstrumentClass`.
    #[must_use]
    pub const fn to_instrument_class(self) -> InstrumentClass {
        match self {
            Self::Other => InstrumentClass::Other,
            Self::Equity => InstrumentClass::Equity,
            Self::Option => InstrumentClass::Option,
            Self::Future => InstrumentClass::Future,
            Self::ForeignExchange => InstrumentClass::ForeignExchange,
            Self::CryptoSpot => InstrumentClass::CryptoSpot,
            Self::CryptoPerpetual => InstrumentClass::CryptoPerpetual,
        }
    }

    /// Stable wire discriminant matching `ProviderInstrumentClass`.
    #[must_use]
    pub const fn to_wire_i32(self) -> i32 {
        match self {
            Self::Other => 0,
            Self::Equity => 1,
            Self::Option => 2,
            Self::Future => 3,
            Self::ForeignExchange => 4,
            Self::CryptoSpot => 5,
            Self::CryptoPerpetual => 6,
        }
    }

    /// Decodes a wire discriminant matching `ProviderInstrumentClass`.
    #[must_use]
    pub const fn from_wire_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Other),
            1 => Some(Self::Equity),
            2 => Some(Self::Option),
            3 => Some(Self::Future),
            4 => Some(Self::ForeignExchange),
            5 => Some(Self::CryptoSpot),
            6 => Some(Self::CryptoPerpetual),
            _ => None,
        }
    }
}

/// One provider-reported weekly exchange session segment.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedSessionHours {
    /// ISO weekday where Monday is 1 and Sunday is 7.
    pub weekday: u32,
    pub open_seconds: u32,
    pub close_seconds: u32,
    /// IANA timezone identity supplied by the provider/reference mapping.
    pub timezone: String,
}

/// Provider-reported contract metadata without workspace protobuf coupling.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedContractMetadata {
    pub point_value: Option<i64>,
    pub point_value_scale: Option<u32>,
    pub currency: Option<String>,
    pub contract_expiry: Option<String>,
    pub first_notice_date: Option<String>,
    pub last_trade_date: Option<String>,
    pub session_hours: Vec<ResolvedSessionHours>,
    pub order_quantity_increment: Option<i64>,
    pub price_significant_figures: Option<u32>,
    pub price_max_decimals: Option<u32>,
    pub instrument_class: Option<ResolvedInstrumentClass>,
}

/// Canonical provider-resolved instrument identity and contract terms.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedProviderInstrument {
    pub provider: String,
    pub session_generation: u64,
    pub selection_generation: u64,
    pub instrument_id: String,
    pub provider_symbol: String,
    pub display_symbol: String,
    pub venue_id: String,
    pub price_scale: u32,
    pub quantity_scale: u32,
    pub entitlement_id: String,
    pub price_increment: Option<i64>,
    pub contract_metadata: Option<Box<ResolvedContractMetadata>>,
}

impl ResolvedProviderInstrument {
    /// Projects this resolved instrument into the domain `InstrumentClass`.
    #[must_use]
    pub fn asset_class(&self) -> InstrumentClass {
        self.contract_metadata
            .as_deref()
            .and_then(|metadata| metadata.instrument_class)
            .map_or(
                InstrumentClass::Other,
                ResolvedInstrumentClass::to_instrument_class,
            )
    }

    /// Returns the non-blank settlement currency if reported by the provider.
    #[must_use]
    pub fn currency(&self) -> Option<&str> {
        self.contract_metadata
            .as_deref()
            .and_then(|metadata| metadata.currency.as_deref())
            .filter(|currency| !currency.trim().is_empty())
    }

    /// Projects this resolved instrument into the `HistoryInstrument` consumed by
    /// provider history fetchers.
    #[must_use]
    pub fn to_history_instrument(&self) -> HistoryInstrument {
        let contract = self.contract_metadata.as_deref().map(|metadata| {
            let point_value = match (metadata.point_value, metadata.point_value_scale) {
                (Some(value), Some(scale)) => u8::try_from(scale).ok().map(|scale| (value, scale)),
                _ => None,
            };
            HistoryContractMetadata {
                point_value,
                currency: metadata.currency.clone(),
                expiration_date: metadata.contract_expiry.clone(),
                first_notice_date: metadata.first_notice_date.clone(),
                last_trade_date: metadata.last_trade_date.clone(),
            }
        });
        HistoryInstrument {
            provider: self.provider.clone(),
            session_generation: self.session_generation,
            instrument_id: self.instrument_id.clone(),
            provider_symbol: self.provider_symbol.clone(),
            display_symbol: self.display_symbol.clone(),
            venue_id: self.venue_id.clone(),
            entitlement_id: self.entitlement_id.clone(),
            price_scale: self.price_scale,
            quantity_scale: self.quantity_scale,
            price_increment: self.price_increment,
            contract,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_instrument_projects_asset_class_currency_and_history_fields() {
        let instrument = ResolvedProviderInstrument {
            provider: "demo".into(),
            session_generation: 3,
            selection_generation: 7,
            instrument_id: "demo:fut:ES".into(),
            provider_symbol: "ESH6".into(),
            display_symbol: "ESH6".into(),
            venue_id: "CME".into(),
            price_scale: 2,
            quantity_scale: 0,
            entitlement_id: "demo-cme".into(),
            price_increment: Some(25),
            contract_metadata: Some(Box::new(ResolvedContractMetadata {
                point_value: Some(5000),
                point_value_scale: Some(2),
                currency: Some("USD".into()),
                contract_expiry: Some("2026-03-20".into()),
                first_notice_date: None,
                last_trade_date: Some("2026-03-20".into()),
                session_hours: Vec::new(),
                order_quantity_increment: Some(1),
                price_significant_figures: None,
                price_max_decimals: None,
                instrument_class: Some(ResolvedInstrumentClass::Future),
            })),
        };

        assert_eq!(instrument.asset_class(), InstrumentClass::Future);
        assert_eq!(instrument.currency(), Some("USD"));

        let history = instrument.to_history_instrument();
        assert_eq!(history.provider, "demo");
        assert_eq!(history.session_generation, 3);
        assert_eq!(history.provider_symbol, "ESH6");
        assert_eq!(history.price_increment, Some(25));
        let contract = history.contract.expect("contract metadata");
        assert_eq!(contract.point_value, Some((5000, 2)));
        assert_eq!(contract.currency.as_deref(), Some("USD"));
        assert_eq!(contract.expiration_date.as_deref(), Some("2026-03-20"));
    }

    #[test]
    fn instrument_class_wire_round_trip_covers_every_variant() {
        for class in [
            ResolvedInstrumentClass::Other,
            ResolvedInstrumentClass::Equity,
            ResolvedInstrumentClass::Option,
            ResolvedInstrumentClass::Future,
            ResolvedInstrumentClass::ForeignExchange,
            ResolvedInstrumentClass::CryptoSpot,
            ResolvedInstrumentClass::CryptoPerpetual,
        ] {
            assert_eq!(
                ResolvedInstrumentClass::from_wire_i32(class.to_wire_i32()),
                Some(class)
            );
        }
        assert_eq!(ResolvedInstrumentClass::from_wire_i32(99), None);
    }
}
