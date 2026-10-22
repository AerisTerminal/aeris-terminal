//! Attachment of the broker venue relay, which routes the trading owner's broker orders
//! through the saved Rithmic login on a dedicated worker.

use super::MarketService;
use aeris_trading::{
    venue::{VenueCapabilities, VenueEvent},
    venue_outbox::VenueRequestReceiver,
};
use std::{
    sync::{Arc, atomic::Ordering},
    thread,
};

/// One venue session generation and the receiver of its requests.
pub type VenueSession = Result<(u64, VenueRequestReceiver), String>;

/// The trading owner's two ends of a broker venue. `attach` starts one venue session
/// generation with the venue's capabilities and returns its request receiver; the relay
/// calls it again after every reconnect so the owner reconciles each account under the
/// new session. `events` delivers the venue's reports to the owner's bounded inbox.
pub struct BrokerVenueLink {
    pub attach: Box<dyn FnMut(VenueCapabilities) -> VenueSession + Send + 'static>,
    pub events: Box<dyn Fn(VenueEvent) -> Result<(), String> + Send + 'static>,
}

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
            .ok_or_else(|| "market engine shutdown is in progress".to_string())?;
        let login = &self.runtime.rithmic_login;
        if login.venue_attached.swap(true, Ordering::AcqRel) {
            return Err("the broker venue is already attached".into());
        }
        let shutdown = Arc::clone(&self.runtime.shutdown);
        let revision = login.revision.clone();
        match thread::Builder::new()
            .name("aeris-broker-venue".into())
            .spawn(move || crate::rithmic_venue::run(link, &shutdown, &revision))
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
