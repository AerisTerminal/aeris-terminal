//! Bounded multi-venue broker registry owned by [`crate::TradingCore`].
//!
//! Replaces the single-venue `Option<VenueRequestSender>` field so the trading
//! owner can host multiple concurrent broker relays (for example multiple
//! Rithmic prop-firm logins or additional broker adapters like Tradovate/CQG)
//! keyed by `(venue_id, connection_id)`, while preserving the legacy default
//! `("broker", "default")` route for all existing persisted accounts.

use aeris_trading::{
    venue::{BROKER_VENUE_ID, VenueCapabilities},
    venue_outbox::VenueRequestSender,
};
use core::fmt;
use std::collections::BTreeMap;

/// Hard upper bound on concurrently attached broker venue sessions.
pub const MAXIMUM_BROKER_VENUES: usize = 16;

/// Default logical connection ID used by migrated legacy single-venue attachments.
pub const DEFAULT_VENUE_CONNECTION_ID: &str = "default";

/// Maximum UTF-8 bytes allowed in one venue or connection identifier.
const MAXIMUM_KEY_BYTES: usize = 64;

/// Stable key identifying one attached broker venue relay.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BrokerVenueKey {
    pub venue_id: String,
    pub connection_id: String,
}

/// Typed validation and capacity errors for [`BrokerVenueRegistry`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VenueRegistryError {
    EmptyVenueId,
    EmptyConnectionId,
    KeyTooLong,
    InvalidCharacter,
    CapacityExceeded,
}

impl fmt::Display for VenueRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyVenueId => formatter.write_str("broker venue ID must not be empty"),
            Self::EmptyConnectionId => {
                formatter.write_str("broker connection ID must not be empty")
            }
            Self::KeyTooLong => formatter.write_str("broker venue key exceeds 64 bytes"),
            Self::InvalidCharacter => {
                formatter.write_str("broker venue key contains invalid characters")
            }
            Self::CapacityExceeded => formatter.write_str("maximum attached broker venues reached"),
        }
    }
}

impl std::error::Error for VenueRegistryError {}

impl BrokerVenueKey {
    /// Validates and builds one `(venue_id, connection_id)` key.
    ///
    /// # Errors
    /// Returns [`VenueRegistryError`] for empty, oversized, or malformed identifiers.
    pub fn try_new(
        venue_id: impl Into<String>,
        connection_id: impl Into<String>,
    ) -> Result<Self, VenueRegistryError> {
        let venue_id = venue_id.into();
        let connection_id = connection_id.into();
        validate_key_segment(&venue_id, VenueRegistryError::EmptyVenueId)?;
        validate_key_segment(&connection_id, VenueRegistryError::EmptyConnectionId)?;
        Ok(Self {
            venue_id,
            connection_id,
        })
    }

    /// Canonical key for the legacy single broker venue (`"broker"`, `"default"`).
    #[must_use]
    pub fn default_broker() -> Self {
        Self {
            venue_id: BROKER_VENUE_ID.to_string(),
            connection_id: DEFAULT_VENUE_CONNECTION_ID.to_string(),
        }
    }
}

fn validate_key_segment(
    value: &str,
    empty_error: VenueRegistryError,
) -> Result<(), VenueRegistryError> {
    if value.is_empty() {
        return Err(empty_error);
    }
    if value.len() > MAXIMUM_KEY_BYTES {
        return Err(VenueRegistryError::KeyTooLong);
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(VenueRegistryError::InvalidCharacter);
    }
    Ok(())
}

/// One live attached broker venue session inside [`BrokerVenueRegistry`].
pub(crate) struct AttachedBrokerVenue {
    pub(crate) generation: u64,
    pub(crate) outbound: VenueRequestSender,
    pub(crate) capabilities: VenueCapabilities,
}

/// Single-owner bounded map of attached broker venue relays.
#[derive(Default)]
pub(crate) struct BrokerVenueRegistry {
    venues: BTreeMap<BrokerVenueKey, AttachedBrokerVenue>,
}

impl BrokerVenueRegistry {
    /// Registers or replaces the session for `key`. Re-attaching an existing key
    /// always succeeds without consuming another capacity slot.
    pub(crate) fn attach(
        &mut self,
        key: BrokerVenueKey,
        generation: u64,
        outbound: VenueRequestSender,
        capabilities: VenueCapabilities,
    ) -> Result<(), VenueRegistryError> {
        if !self.venues.contains_key(&key) && self.venues.len() >= MAXIMUM_BROKER_VENUES {
            return Err(VenueRegistryError::CapacityExceeded);
        }
        self.venues.insert(
            key,
            AttachedBrokerVenue {
                generation,
                outbound,
                capabilities,
            },
        );
        Ok(())
    }

    /// Returns the attached venue for an exact `(venue_id, connection_id)` key.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn get(&self, key: &BrokerVenueKey) -> Option<&AttachedBrokerVenue> {
        self.venues.get(key)
    }

    /// Resolves the attached venue for `venue_id`, preferring [`DEFAULT_VENUE_CONNECTION_ID`]
    /// and falling back to the sole attached connection for that venue when unambiguous.
    pub(crate) fn resolve_for_venue(&self, venue_id: &str) -> Option<&AttachedBrokerVenue> {
        if let Some(default) = self.venues.get(&BrokerVenueKey {
            venue_id: venue_id.to_string(),
            connection_id: DEFAULT_VENUE_CONNECTION_ID.to_string(),
        }) {
            return Some(default);
        }
        let mut matches = self
            .venues
            .iter()
            .filter(|(key, _)| key.venue_id == venue_id)
            .map(|(_, entry)| entry);
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    /// Returns the default legacy broker venue (`BROKER_VENUE_ID`).
    pub(crate) fn default_broker(&self) -> Option<&AttachedBrokerVenue> {
        self.resolve_for_venue(BROKER_VENUE_ID)
    }

    pub(crate) fn is_default_broker_attached(&self) -> bool {
        self.default_broker().is_some()
    }

    pub(crate) fn outbound_for_venue(&self, venue_id: &str) -> Option<&VenueRequestSender> {
        self.resolve_for_venue(venue_id)
            .map(|entry| &entry.outbound)
    }

    pub(crate) fn capabilities_for_venue(&self, venue_id: &str) -> Option<VenueCapabilities> {
        self.resolve_for_venue(venue_id)
            .map(|entry| entry.capabilities)
    }

    /// Returns the attached `(key, generation)` pairs in deterministic key order.
    pub(crate) fn entries(&self) -> Vec<(BrokerVenueKey, u64)> {
        self.venues
            .iter()
            .map(|(key, entry)| (key.clone(), entry.generation))
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn keys(&self) -> Vec<BrokerVenueKey> {
        self.venues.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aeris_trading::venue_outbox::bounded_venue_requests;

    const CAPS_A: VenueCapabilities = VenueCapabilities {
        bracket_levels: 4,
        client_order_id_limit: 64,
    };
    const CAPS_B: VenueCapabilities = VenueCapabilities {
        bracket_levels: 0,
        client_order_id_limit: 32,
    };

    #[test]
    fn registry_hosts_multiple_venues_and_connections_within_bound() {
        let mut registry = BrokerVenueRegistry::default();
        let (tx_a, _rx_a) = bounded_venue_requests(4);
        let (tx_b, _rx_b) = bounded_venue_requests(4);
        let (tx_c, _rx_c) = bounded_venue_requests(4);

        let default_key = BrokerVenueKey::default_broker();
        let prop_key = BrokerVenueKey::try_new("broker", "prop-firm-2").expect("valid key");
        let tradovate_key = BrokerVenueKey::try_new("tradovate", "default").expect("valid key");

        registry
            .attach(default_key.clone(), 1, tx_a, CAPS_A)
            .expect("attach default");
        registry
            .attach(prop_key.clone(), 2, tx_b, CAPS_B)
            .expect("attach second rithmic connection");
        registry
            .attach(tradovate_key.clone(), 3, tx_c, CAPS_B)
            .expect("attach second broker venue");

        assert_eq!(registry.keys().len(), 3);
        assert_eq!(
            registry.capabilities_for_venue(BROKER_VENUE_ID),
            Some(CAPS_A)
        );
        assert_eq!(registry.capabilities_for_venue("tradovate"), Some(CAPS_B));
        assert_eq!(
            registry.get(&prop_key).map(|entry| entry.generation),
            Some(2)
        );

        // Re-attaching an existing key replaces its generation without growing the map.
        let (tx_a2, _rx_a2) = bounded_venue_requests(4);
        registry
            .attach(default_key, 4, tx_a2, CAPS_A)
            .expect("re-attach default");
        assert_eq!(registry.keys().len(), 3);
        assert_eq!(
            registry.default_broker().map(|entry| entry.generation),
            Some(4)
        );
    }

    #[test]
    fn registry_enforces_maximum_broker_venue_bound() {
        let mut registry = BrokerVenueRegistry::default();
        for index in 0..MAXIMUM_BROKER_VENUES {
            let (tx, _rx) = bounded_venue_requests(1);
            let key = BrokerVenueKey::try_new("broker", format!("conn-{index}")).expect("key");
            registry
                .attach(key, index as u64 + 1, tx, CAPS_A)
                .expect("fits");
        }
        let (overflow_tx, _rx) = bounded_venue_requests(1);
        let overflow_key = BrokerVenueKey::try_new("broker", "conn-overflow").expect("key");
        assert_eq!(
            registry.attach(overflow_key, 99, overflow_tx, CAPS_A),
            Err(VenueRegistryError::CapacityExceeded)
        );
    }

    #[test]
    fn key_validation_rejects_empty_oversized_and_bad_characters() {
        assert_eq!(
            BrokerVenueKey::try_new("", "default"),
            Err(VenueRegistryError::EmptyVenueId)
        );
        assert_eq!(
            BrokerVenueKey::try_new("broker", ""),
            Err(VenueRegistryError::EmptyConnectionId)
        );
        assert_eq!(
            BrokerVenueKey::try_new("broker", "a".repeat(MAXIMUM_KEY_BYTES + 1)),
            Err(VenueRegistryError::KeyTooLong)
        );
        assert_eq!(
            BrokerVenueKey::try_new("broker/bad", "default"),
            Err(VenueRegistryError::InvalidCharacter)
        );
    }

    #[test]
    fn trading_service_attaches_multiple_venues_and_preserves_legacy_broker_route() {
        let directory = crate::tests::TestDirectory::new("multi_venue_registry");
        let service = crate::TradingService::start(crate::tests::config(&directory))
            .expect("trading service starts");

        let (gen1, _rx1) = service
            .attach_broker_venue(CAPS_A)
            .expect("legacy default broker attaches");
        let second_key = BrokerVenueKey::try_new("tradovate", "prop-01").expect("valid key");
        let (gen2, _rx2) = service
            .attach_broker_venue_for(second_key.clone(), CAPS_B)
            .expect("second broker venue attaches");

        assert!(gen2 > gen1);
        let attached = service
            .attached_broker_venues()
            .expect("attached venues query");
        assert_eq!(
            attached,
            vec![(BrokerVenueKey::default_broker(), gen1), (second_key, gen2),]
        );

        service
            .shutdown(std::time::Duration::from_secs(2))
            .expect("shutdown");

        // Reopening the persisted store migrates transparently and continues monotonic generations.
        let reopened = crate::TradingService::start(crate::tests::config(&directory))
            .expect("trading service reopens");
        let (gen3, _rx3) = reopened
            .attach_broker_venue(CAPS_A)
            .expect("legacy broker re-attaches after restart");
        assert!(gen3 > gen2);
        reopened
            .shutdown(std::time::Duration::from_secs(2))
            .expect("reopened shutdown");
    }
}
