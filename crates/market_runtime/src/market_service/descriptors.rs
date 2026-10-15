//! Provider and instrument metadata published by the market runtime.

use aeris_contracts::{
    InstallProviderInstrument, ProviderConnectionKind, ProviderPresentationDescriptor,
};
use aeris_instruments::{CalendarSourceKind, InstrumentClass, PriceGrid};
use aeris_market_data::{BarPeriod, BarSeriesKey, ChartInterval};

use crate::{CadenceError, resolve_series_key};

/// The connection UI required by a registered provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectMethod {
    Public,
    HostedBrokerOAuth,
    VaultCredentials { form: &'static str },
}

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
    pub(super) fn from_presentation(presentation: &'static ProviderPresentationDescriptor) -> Self {
        let connect = match presentation.connection_kind {
            ProviderConnectionKind::Public => ConnectMethod::Public,
            ProviderConnectionKind::HostedBroker => ConnectMethod::HostedBrokerOAuth,
            ProviderConnectionKind::Credentials => ConnectMethod::VaultCredentials {
                form: presentation.id,
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
        let terms = super::instrument_terms_from_install(instrument).ok();
        Ok(Self {
            instrument: instrument.clone(),
            display_symbol: instrument.display_symbol.clone(),
            venue: instrument.venue_id.clone(),
            asset_class: terms
                .as_ref()
                .map_or(InstrumentClass::Other, |terms| terms.instrument_class),
            currency: terms.as_ref().map(|terms| terms.currency.clone()),
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
