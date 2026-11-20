//! Attachment of broker venue relays, routing the trading owner's broker orders
//! through registered broker adapters on dedicated bounded workers.

use super::{MarketService, MarketServiceError};
use aeris_provider_spi::{ConnectionId, DEFAULT_CONNECTION_ID};
pub use aeris_trading::venue::BrokerVenueLink;
use std::{sync::Arc, thread};

const MAXIMUM_ATTACHED_BROKER_VENUES: usize = 16;

impl MarketService {
    /// Starts the default broker venue relay (`("broker", "default")`) on its own worker.
    /// It signs in with the saved Rithmic login, follows login changes and stops at market shutdown.
    ///
    /// # Errors
    /// Returns an error when the relay was already attached or the worker cannot start.
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
    /// Returns [`MarketServiceError`] when shutdown is in progress, the key is already attached,
    /// capacity is exhausted, or the worker thread cannot spawn.
    pub fn attach_broker_venue_for(
        &self,
        venue_id: &str,
        connection_id: &ConnectionId,
        link: BrokerVenueLink,
    ) -> Result<(), MarketServiceError> {
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
        let revision = self.runtime.rithmic_login.revision.clone();
        let thread_name = if connection_id.as_str() == DEFAULT_CONNECTION_ID {
            "aeris-broker-venue".to_string()
        } else {
            format!("aeris-broker-venue-{}", connection_id.as_str())
        };
        match thread::Builder::new()
            .name(thread_name)
            .spawn(move || aeris_rithmic_protocol_adapter::venue::run(link, &shutdown, &revision))
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
