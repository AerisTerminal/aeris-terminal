//! Provider and instrument metadata published by the market runtime.

use crate::{ProviderConnectionKind, ProviderPresentationDescriptor};
use aeris_contracts::{
    InstallProviderInstrument, ProviderContractMetadata, ProviderInstrumentClass,
    ProviderSessionHours,
};
use aeris_instruments::{CalendarSourceKind, InstrumentClass, PriceGrid};
use aeris_market_data::{BarPeriod, BarSeriesKey, ChartInterval};
use aeris_market_engine::{GenerationId, StreamRequirements, Viewport};
use aeris_provider_spi::{
    ResolvedContractMetadata, ResolvedInstrumentClass, ResolvedProviderInstrument,
    ResolvedSessionHours,
};

use crate::{CadenceError, resolve_series_key};

/// The connection UI required by a registered provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectMethod {
    Public,
    VaultCredentials {
        schema: &'static LoginSchema,
    },
    /// A local source with no connection to manage.
    Local,
}

pub use aeris_provider_spi::auth::{
    LoginField, LoginFieldKind, LoginFieldValue, LoginSchema, PendingAgreement,
    ProviderLoginServer, SavedProviderLogin,
};

/// Published provider identity and catalog affordances.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderDescriptor {
    pub id: &'static str,
    pub display_name: &'static str,
    pub logo_key: &'static str,
    pub connect: ConnectMethod,
    pub symbol_browser: bool,
    pub autoload_catalog: bool,
    pub offered_intervals: &'static [&'static str],
}

impl ProviderDescriptor {
    pub(super) fn from_registration(registration: &super::ProviderDescriptor) -> Self {
        let presentation = registration.presentation;
        let connect = match presentation.connection_kind {
            ProviderConnectionKind::Public => ConnectMethod::Public,
            ProviderConnectionKind::Local => ConnectMethod::Local,
            ProviderConnectionKind::Credentials => ConnectMethod::VaultCredentials {
                schema: registration
                    .login_schema
                    .expect("credential provider publishes login schema"),
            },
        };
        Self {
            id: presentation.id,
            display_name: presentation.display_name,
            logo_key: presentation.logo_key,
            connect,
            symbol_browser: !presentation.search_hint.is_empty(),
            autoload_catalog: presentation.catalog_refresh_on_startup,
            offered_intervals: presentation.chart_interval_labels,
        }
    }
}

/// Provider-resolved catalog metadata, with unknown terms represented explicitly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstrumentDescriptor {
    pub instrument: InstallProviderInstrument,
    pub display_symbol: String,
    pub venue: String,
    pub asset_class: InstrumentClass,
    pub currency: Option<String>,
    pub price_scale: u8,
    pub price_grid: PriceGrid,
    pub offered_intervals: &'static [&'static str],
    pub calendar_source: CalendarSourceKind,
    pub quote_capability: bool,
    pub entitlement_id: String,
    pub logo_key: &'static str,
}

/// A provider catalog entry cannot be represented as a published descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptorError {
    InvalidInstrument,
}

/// Stable catalog identity requested by a chart consumer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstrumentRef {
    pub provider_id: String,
    pub instrument_id: String,
    pub entitlement_id: String,
    /// Optional provider catalog query for an instrument restored before its
    /// current catalog entry has been installed. Identity is still checked.
    pub query: Option<String>,
}

/// A fresh workspace requests the runtime's public default, not a desktop fixture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstrumentRequest {
    Ref(InstrumentRef),
    /// A search result has provider routing fields, but only its catalog
    /// resolver can supply the stable instrument identity.
    CatalogSymbol {
        provider_id: String,
        symbol: String,
        exchange: String,
        entitlement_id: String,
        search_generation: u64,
    },
    Default,
}
impl InstrumentRequest {
    /// A fresh workspace's provisional identity is resolved by the runtime,
    /// not treated as authoritative restored catalog metadata.
    #[must_use]
    pub fn startup(instrument: &InstallProviderInstrument) -> Self {
        let default = default_instrument();
        if instrument.provider == default.provider
            && instrument.instrument_id == default.instrument_id
            && instrument.session_generation <= default.session_generation
        {
            Self::Default
        } else {
            Self::restored(instrument)
        }
    }

    /// A persisted pane's identity is authoritative even when it happens to
    /// have the same fields as the runtime's provisional public default.
    #[must_use]
    pub fn restored(instrument: &InstallProviderInstrument) -> Self {
        Self::Ref(InstrumentRef {
            provider_id: instrument.provider.clone(),
            instrument_id: instrument.instrument_id.clone(),
            entitlement_id: instrument.entitlement_id.clone(),
            query: Some(
                match super::built_in_provider_presentations()
                    .iter()
                    .find(|provider| provider.id == instrument.provider)
                    .map(|provider| provider.catalog_symbol)
                {
                    Some(crate::ProviderCatalogSymbol::DisplaySymbol) => {
                        instrument.display_symbol.clone()
                    }
                    _ => instrument.provider_symbol.clone(),
                },
            ),
        })
    }
}

/// Public, ungated default for a fresh workspace. Catalog refresh replaces
/// these provisional terms before the provider starts its selected stream.
pub(super) fn default_instrument() -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: "hyperliquid".into(),
        session_generation: 1,
        selection_generation: 1,
        instrument_id: "hyperliquid:perp:BTC".into(),
        provider_symbol: "BTC".into(),
        display_symbol: "BTC-USDC".into(),
        venue_id: "Hyperliquid".into(),
        price_scale: 8,
        quantity_scale: 8,
        entitlement_id: "hyperliquid-public".into(),
        contract_metadata: Some(Box::new(ProviderContractMetadata {
            instrument_class: Some(ProviderInstrumentClass::CryptoPerpetual as i32),
            currency: Some("USDC".into()),
            ..Default::default()
        })),
        ..Default::default()
    }
}

/// One bounded pane selection command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionRequest {
    pub consumer_id: u64,
    pub generation: u64,
    pub instrument: InstrumentRequest,
    pub interval: ChartInterval,
    pub streams: StreamRequirements,
    pub viewport: Option<Viewport>,
}

/// The exact outcome of one attempted binding generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionOutcome {
    Selected {
        generation: GenerationId,
        descriptor: Box<InstrumentDescriptor>,
        series: ResolvedSeries,
    },
    Failed {
        generation: GenerationId,
        error: SelectionError,
    },
}

/// Selection failures do not retire the consumer's prior series.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionError {
    Superseded,
    InstrumentUnavailable,
    UnsupportedInterval,
    InvalidViewport,
    DemandRejected(String),
}

/// Projects a persisted `InstallProviderInstrument` into the provider-neutral SPI domain type.
#[must_use]
pub fn resolved_from_install(instrument: &InstallProviderInstrument) -> ResolvedProviderInstrument {
    ResolvedProviderInstrument {
        provider: instrument.provider.clone(),
        session_generation: instrument.session_generation,
        selection_generation: instrument.selection_generation,
        instrument_id: instrument.instrument_id.clone(),
        provider_symbol: instrument.provider_symbol.clone(),
        display_symbol: instrument.display_symbol.clone(),
        venue_id: instrument.venue_id.clone(),
        price_scale: instrument.price_scale,
        quantity_scale: instrument.quantity_scale,
        entitlement_id: instrument.entitlement_id.clone(),
        price_increment: instrument.price_increment,
        contract_metadata: instrument.contract_metadata.as_deref().map(|metadata| {
            Box::new(ResolvedContractMetadata {
                point_value: metadata.point_value,
                point_value_scale: metadata.point_value_scale,
                currency: metadata.currency.clone(),
                contract_expiry: metadata.contract_expiry.clone(),
                first_notice_date: metadata.first_notice_date.clone(),
                last_trade_date: metadata.last_trade_date.clone(),
                session_hours: metadata
                    .session_hours
                    .iter()
                    .map(|hours| ResolvedSessionHours {
                        weekday: hours.weekday,
                        open_seconds: hours.open_seconds,
                        close_seconds: hours.close_seconds,
                        timezone: hours.timezone.clone(),
                    })
                    .collect(),
                order_quantity_increment: metadata.order_quantity_increment,
                price_significant_figures: metadata.price_significant_figures,
                price_max_decimals: metadata.price_max_decimals,
                instrument_class: metadata
                    .instrument_class
                    .and_then(ResolvedInstrumentClass::from_wire_i32),
            })
        }),
    }
}

/// Projects a provider-neutral `ResolvedProviderInstrument` into the persisted workspace contract.
#[must_use]
pub fn install_from_resolved(instrument: ResolvedProviderInstrument) -> InstallProviderInstrument {
    InstallProviderInstrument {
        provider: instrument.provider,
        session_generation: instrument.session_generation,
        selection_generation: instrument.selection_generation,
        instrument_id: instrument.instrument_id,
        provider_symbol: instrument.provider_symbol,
        display_symbol: instrument.display_symbol,
        venue_id: instrument.venue_id,
        price_scale: instrument.price_scale,
        quantity_scale: instrument.quantity_scale,
        entitlement_id: instrument.entitlement_id,
        price_increment: instrument.price_increment,
        contract_metadata: instrument.contract_metadata.map(|metadata| {
            Box::new(ProviderContractMetadata {
                point_value: metadata.point_value,
                point_value_scale: metadata.point_value_scale,
                currency: metadata.currency,
                contract_expiry: metadata.contract_expiry,
                first_notice_date: metadata.first_notice_date,
                last_trade_date: metadata.last_trade_date,
                session_hours: metadata
                    .session_hours
                    .into_iter()
                    .map(|hours| ProviderSessionHours {
                        weekday: hours.weekday,
                        open_seconds: hours.open_seconds,
                        close_seconds: hours.close_seconds,
                        timezone: hours.timezone,
                    })
                    .collect(),
                order_quantity_increment: metadata.order_quantity_increment,
                price_significant_figures: metadata.price_significant_figures,
                price_max_decimals: metadata.price_max_decimals,
                instrument_class: metadata
                    .instrument_class
                    .map(ResolvedInstrumentClass::to_wire_i32),
            })
        }),
    }
}

impl InstrumentDescriptor {
    /// Projects validated catalog metadata without guessing absent contract terms.
    ///
    /// # Errors
    /// Rejects malformed identities or precision instead of truncating a wire scale.
    pub fn from_install(
        instrument: &InstallProviderInstrument,
        presentation: &'static ProviderPresentationDescriptor,
        quote_capability: bool,
    ) -> Result<Self, DescriptorError> {
        super::instrument_selection::validate_provider_instrument(instrument)
            .map_err(|_| DescriptorError::InvalidInstrument)?;
        let resolved = resolved_from_install(instrument);
        let terms = super::instrument_terms_from_install(instrument).ok();
        Ok(Self {
            instrument: instrument.clone(),
            display_symbol: instrument.display_symbol.clone(),
            venue: instrument.venue_id.clone(),
            asset_class: resolved.asset_class(),
            currency: resolved.currency().map(str::to_string),
            price_scale: u8::try_from(instrument.price_scale)
                .map_err(|_| DescriptorError::InvalidInstrument)?,
            price_grid: terms.map_or(PriceGrid::Unknown, |terms| terms.price_grid),
            offered_intervals: presentation.chart_interval_labels,
            calendar_source: super::calendar::source_kind_for(instrument),
            quote_capability,
            entitlement_id: instrument.entitlement_id.clone(),
            logo_key: presentation.logo_key,
        })
    }
}

/// One selected interval and its canonical, calendar-resolved series key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSeries {
    pub interval: ChartInterval,
    pub key: BarSeriesKey,
    pub cadence: BarPeriod,
}

impl ResolvedSeries {
    /// Resolves provider-supported intervals without desktop cadence construction.
    ///
    /// # Errors
    /// Returns a cadence error for unsupported provider/calendar combinations.
    pub fn resolve(
        instrument: &InstallProviderInstrument,
        interval: ChartInterval,
        presentation: &ProviderPresentationDescriptor,
    ) -> Result<Self, CadenceError> {
        let key = resolve_series_key(instrument, interval, presentation)?;
        Ok(Self {
            interval,
            cadence: key.period,
            key,
        })
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    #[test]
    fn fresh_default_descriptor_has_chart_terms_before_trading_terms_resolve() {
        let default = default_instrument();
        assert!(super::super::instrument_terms_from_install(&default).is_err());
        let descriptor = InstrumentDescriptor::from_install(
            &default,
            &super::super::providers::hyperliquid::HYPERLIQUID_PRESENTATION,
            true,
        )
        .expect("provisional default descriptor");
        assert_eq!(descriptor.asset_class, InstrumentClass::CryptoPerpetual);
        assert_eq!(descriptor.currency.as_deref(), Some("USDC"));
        assert_eq!(descriptor.price_grid, PriceGrid::Unknown);
    }

    #[test]
    fn fresh_default_selection_is_ungated_and_restored_symbols_use_catalog_refs() {
        let default = default_instrument();
        assert_eq!(
            InstrumentRequest::startup(&default),
            InstrumentRequest::Default
        );
        let restored = InstallProviderInstrument {
            instrument_id: "hyperliquid:perp:ETH".into(),
            ..default
        };
        assert!(matches!(InstrumentRequest::startup(&restored),
            InstrumentRequest::Ref(InstrumentRef { instrument_id, .. })
                if instrument_id == "hyperliquid:perp:ETH"));
    }

    #[test]
    fn resolved_instrument_round_trips_install_contract_without_loss() {
        let mut original = default_instrument();
        original.price_increment = Some(25);
        let metadata = original
            .contract_metadata
            .as_deref_mut()
            .expect("default metadata");
        metadata.point_value = Some(5000);
        metadata.point_value_scale = Some(2);
        metadata.contract_expiry = Some("2026-06-19".into());
        metadata.session_hours.push(ProviderSessionHours {
            weekday: 1,
            open_seconds: 0,
            close_seconds: 86_400,
            timezone: "America/Chicago".into(),
        });
        metadata.order_quantity_increment = Some(1);
        metadata.price_significant_figures = Some(5);
        metadata.price_max_decimals = Some(2);

        let resolved = resolved_from_install(&original);
        assert_eq!(resolved.asset_class(), InstrumentClass::CryptoPerpetual);
        assert_eq!(resolved.currency(), Some("USDC"));
        assert_eq!(install_from_resolved(resolved), original);
    }
}
