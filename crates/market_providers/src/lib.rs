//! Composition crate wiring concrete market-data and broker-venue adapters
//! (`aeris_hyperliquid_market_adapter`, `aeris_rithmic_protocol_adapter`) into
//! the provider-neutral [`aeris_market_runtime::MarketServiceBuilder`].
//!
//! `aeris_market_runtime` holds zero dependencies on concrete adapter crates;
//! composition roots (`apps/desktop`, `tools/aeris_probes`) depend on this
//! crate to obtain the default composed [`MarketService`] and published
//! provider metadata without importing adapter crates directly.

pub(crate) use aeris_hyperliquid_market_adapter::display_depth as hyperliquid_display_depth;

mod hyperliquid;
mod hyperliquid_market_scan;
mod hyperliquid_realtime;
mod rithmic;
mod rithmic_realtime;

use std::sync::{Arc, OnceLock};

use aeris_contracts::InstallProviderInstrument;
use aeris_market_runtime::{
    MarketService, MarketServiceBuilder, ProviderPresentationDescriptor,
    plugin::{
        ProviderAuthAdapter, ProviderDescriptor, PublishedProviderDescriptor,
        published_descriptor_from_registration, recording_replay_descriptor,
    },
};

pub use rithmic::{RithmicLoginSummary, RithmicServer};

const BUILT_IN_PROVIDER_DESCRIPTORS: &[ProviderDescriptor] = &[
    rithmic::RITHMIC_DESCRIPTOR,
    hyperliquid::HYPERLIQUID_DESCRIPTOR,
    recording_replay_descriptor(),
];

/// Returns a [`MarketServiceBuilder`] pre-registered with Aeris Terminal's
/// built-in providers (Rithmic, Hyperliquid, and local recording replay),
/// Rithmic authentication, and the Rithmic broker venue runner.
#[must_use]
pub fn market_service_builder() -> MarketServiceBuilder {
    let rithmic_revision = rithmic_realtime::RithmicLoginRevision::default();
    let rithmic_login = Arc::new(rithmic::RithmicLogin::new(rithmic_revision.clone()));
    let broker_revision = rithmic_login.revision.clone();
    MarketService::builder()
        .register_provider(rithmic::provider_spec(
            Box::new(rithmic::LiveRithmicHistory),
            true,
            rithmic_revision,
        ))
        .register_provider(hyperliquid::provider_spec(
            Box::<hyperliquid::LiveHyperliquidHistory>::default(),
            true,
        ))
        .with_recording_replay()
        .register_auth(
            rithmic::RITHMIC_DESCRIPTOR.id,
            rithmic_login as Arc<dyn ProviderAuthAdapter>,
        )
        .register_broker_venue(
            aeris_trading::venue::BROKER_VENUE_ID,
            rithmic::default_broker_venue_runner(broker_revision),
        )
}

/// Starts the default in-process [`MarketService`] with all built-in providers.
///
/// # Errors
/// Returns an error when the market engine or any bounded provider runtime
/// worker cannot be initialized.
pub fn start_market_service() -> Result<MarketService, String> {
    market_service_builder().start().map_err(String::from)
}

/// Built-in provider presentation metadata for provider-neutral desktop UI.
#[must_use]
pub fn built_in_provider_presentations() -> &'static [ProviderPresentationDescriptor] {
    static PRESENTATIONS: OnceLock<Vec<ProviderPresentationDescriptor>> = OnceLock::new();
    PRESENTATIONS
        .get_or_init(|| {
            BUILT_IN_PROVIDER_DESCRIPTORS
                .iter()
                .map(|descriptor| *descriptor.presentation)
                .collect()
        })
        .as_slice()
}

/// Published provider metadata derived from the same registry that starts sessions.
#[must_use]
pub fn published_provider_descriptors() -> &'static [PublishedProviderDescriptor] {
    static DESCRIPTORS: OnceLock<Vec<PublishedProviderDescriptor>> = OnceLock::new();
    DESCRIPTORS
        .get_or_init(|| {
            BUILT_IN_PROVIDER_DESCRIPTORS
                .iter()
                .map(published_descriptor_from_registration)
                .collect()
        })
        .as_slice()
}

/// Returns a safe provider-owned replacement for a legacy durable display
/// symbol when the old identity carries enough information to migrate it.
#[must_use]
pub fn migrate_retained_provider_display_symbol(
    instrument: &InstallProviderInstrument,
) -> Option<String> {
    match instrument.provider.as_str() {
        "hyperliquid" => aeris_hyperliquid_market_adapter::legacy_display_label(
            &instrument.instrument_id,
            &instrument.provider_symbol,
            &instrument.display_symbol,
        ),
        "rithmic" => aeris_rithmic_protocol_adapter::legacy_display_label(
            &instrument.instrument_id,
            &instrument.display_symbol,
        ),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) struct ScriptedClock {
    origin: std::time::Instant,
    elapsed: std::sync::Mutex<std::time::Duration>,
    wall_nanos: std::sync::atomic::AtomicI64,
}

#[cfg(test)]
impl ScriptedClock {
    pub(crate) fn new(wall_nanos: i64) -> Self {
        Self {
            origin: std::time::Instant::now(),
            elapsed: std::sync::Mutex::new(std::time::Duration::ZERO),
            wall_nanos: std::sync::atomic::AtomicI64::new(wall_nanos),
        }
    }

    pub(crate) fn advance(&self, by: std::time::Duration) {
        *self.elapsed.lock().expect("scripted clock lock") += by;
        self.wall_nanos.fetch_add(
            i64::try_from(by.as_nanos()).expect("scripted interval fits i64"),
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

#[cfg(test)]
impl aeris_market_runtime::plugin::Clock for ScriptedClock {
    fn monotonic(&self) -> std::time::Instant {
        self.origin + *self.elapsed.lock().expect("scripted clock lock")
    }

    fn wall_nanos(&self) -> Option<i64> {
        Some(self.wall_nanos.load(std::sync::atomic::Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_provider_has_a_complete_published_descriptor() {
        let descriptors = published_provider_descriptors();
        let presentations = built_in_provider_presentations();
        assert_eq!(descriptors.len(), BUILT_IN_PROVIDER_DESCRIPTORS.len());
        assert_eq!(presentations.len(), BUILT_IN_PROVIDER_DESCRIPTORS.len());
        for descriptor in descriptors {
            assert_ne!(descriptor.id, "");
            assert_ne!(descriptor.display_name, "");
        }
        for presentation in presentations {
            assert_ne!(presentation.chart_interval_labels.len(), 0);
        }
    }
}
