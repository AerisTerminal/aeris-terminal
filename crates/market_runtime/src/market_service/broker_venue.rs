//! Attachment of broker venue relays, routing the trading owner's broker orders
//! through registered broker adapters on dedicated bounded workers.

use super::{MarketService, MarketServiceError};
use aeris_provider_spi::{ConnectionId, DEFAULT_CONNECTION_ID};
pub use aeris_trading::venue::BrokerVenueLink;
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool},
    thread,
};

const MAXIMUM_ATTACHED_BROKER_VENUES: usize = 16;

/// Pluggable worker entry point for a broker venue relay.
///
/// The runner owns the venue transport loop and must exit promptly when the
/// provided shutdown flag is set to `true`.
pub type BrokerVenueRunner = Arc<dyn Fn(BrokerVenueLink, Arc<AtomicBool>) + Send + Sync + 'static>;

#[derive(Default)]
pub(super) struct BrokerVenueRunnerRegistry {
    runners: BTreeMap<String, BrokerVenueRunner>,
}

impl BrokerVenueRunnerRegistry {
    pub(super) fn register(
        &mut self,
        venue_id: impl Into<String>,
        runner: BrokerVenueRunner,
    ) -> Result<(), MarketServiceError> {
        let venue_id = venue_id.into();
        if self.runners.len() >= MAXIMUM_ATTACHED_BROKER_VENUES
            && !self.runners.contains_key(&venue_id)
        {
            return Err(MarketServiceError::AlreadyAttached(
                "maximum registered broker venue runners reached",
            ));
        }
        self.runners.insert(venue_id, runner);
        Ok(())
    }

    pub(super) fn get(&self, venue_id: &str) -> Option<BrokerVenueRunner> {
        self.runners.get(venue_id).cloned()
    }
}

impl MarketService {
    /// Starts the default broker venue relay (`("broker", "default")`) on its own worker.
    ///
    /// # Errors
    /// Returns an error when no runner is registered, the relay was already attached,
    /// or the worker cannot start.
    pub fn attach_broker_venue(&self, link: BrokerVenueLink) -> Result<(), String> {
        self.attach_broker_venue_for(
            aeris_trading::venue::BROKER_VENUE_ID,
            &ConnectionId::default_connection(),
            link,
        )
        .map_err(String::from)
    }

    /// Starts one named `(venue_id, connection_id)` broker venue relay on a dedicated worker,
    /// bounded by [`MAXIMUM_ATTACHED_BROKER_VENUES`].
    ///
    /// # Errors
    /// Returns [`MarketServiceError`] when shutdown is in progress, no runner is registered
    /// for `venue_id`, the key is already attached, capacity is exhausted, or the worker
    /// thread cannot spawn.
    pub fn attach_broker_venue_for(
        &self,
        venue_id: &str,
        connection_id: &ConnectionId,
        link: BrokerVenueLink,
    ) -> Result<(), MarketServiceError> {
        let runner = self
            .runtime
            .broker_venue_runners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(venue_id)
            .ok_or(MarketServiceError::UnsupportedProvider)?;
        let mut workers = self
            .runtime
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let workers = workers
            .as_mut()
            .ok_or(MarketServiceError::ShutdownInProgress)?;
        let key = (venue_id.to_string(), connection_id.as_str().to_string());
        {
            let mut attached = self
                .runtime
                .attached_broker_venues
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if attached.contains(&key) {
                return Err(MarketServiceError::AlreadyAttached(
                    "the broker venue is already attached",
                ));
            }
            if attached.len() >= MAXIMUM_ATTACHED_BROKER_VENUES {
                return Err(MarketServiceError::AlreadyAttached(
                    "maximum attached broker venues reached",
                ));
            }
            attached.insert(key.clone());
        }
        let shutdown = Arc::clone(&self.runtime.shutdown);
        let thread_name = if connection_id.as_str() == DEFAULT_CONNECTION_ID {
            "aeris-broker-venue".to_string()
        } else {
            format!("aeris-broker-venue-{}", connection_id.as_str())
        };
        match thread::Builder::new()
            .name(thread_name)
            .spawn(move || runner(link, shutdown))
        {
            Ok(worker) => {
                workers.push(worker);
                Ok(())
            }
            Err(error) => {
                self.runtime
                    .attached_broker_venues
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&key);
                Err(MarketServiceError::WorkerSpawnFailed(error.to_string()))
            }
        }
    }
}
