//! Attachment of broker venue relays, routing the trading owner's broker orders
//! through registered broker adapters on dedicated bounded workers.

use super::{MarketService, MarketServiceError};
pub use aeris_trading::venue::BrokerVenueLink;
use std::{
    sync::{Arc, atomic::Ordering},
    thread,
};

impl MarketService {
    /// Starts the broker venue relay on its own worker. It signs in with the saved
    /// Rithmic login, follows login changes and stops at market shutdown.
    ///
    /// # Errors
    /// Returns an error when the relay was already attached or the worker cannot start.
    pub fn attach_broker_venue(&self, link: BrokerVenueLink) -> Result<(), String> {
        let mut workers = self
            .runtime
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let workers = workers
            .as_mut()
            .ok_or(MarketServiceError::ShutdownInProgress)?;
        let login = &self.runtime.rithmic_login;
        if login.venue_attached.swap(true, Ordering::AcqRel) {
            return Err(MarketServiceError::AlreadyAttached(
                "the broker venue is already attached",
            )
            .into());
        }
        let shutdown = Arc::clone(&self.runtime.shutdown);
        let revision = login.revision.clone();
        match thread::Builder::new()
            .name("aeris-broker-venue".into())
            .spawn(move || aeris_rithmic_protocol_adapter::venue::run(link, &shutdown, &revision))
        {
            Ok(worker) => {
                workers.push(worker);
                Ok(())
            }
            Err(error) => {
                login.venue_attached.store(false, Ordering::Release);
                Err(error.to_string())
            }
        }
    }
}
