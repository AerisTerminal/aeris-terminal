//! Desktop market presentation mailbox and deterministic disconnected fixture.
//!
//! This module does not implement or claim a socket, WebSocket, live provider,
//! entitlement service, or production transport. The fixture exercises the same
//! application replay model used by engine publications without inventing a
//! second desktop wire protocol.

use axiusflow_application::ReplayRecoveryCommand;
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
    MarketGeneration, ProvenancedMarketBar, ReplaySnapshot, ReplayStreamUpdate,
};
use axiusflow_engine_protocol::{
    InstallProviderInstrument, ProviderCatalogRejected, ProviderInstrumentSearchResult,
    SearchProviderInstruments, SelectProviderInstrument,
};
use axiusflow_market_data::ChartInterval;
use axiusflow_market_data::DomFrame;
use axiusflow_observability::FeedConnectionState;
use axiusflow_observability::FeedDiagnosticsSnapshot;
use std::{
    collections::VecDeque,
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

/// A bounded coordinator mailbox whose receiving endpoint has been dropped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MailboxDisconnected;

impl std::fmt::Display for MailboxDisconnected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("coordinator mailbox is disconnected")
    }
}

impl std::error::Error for MailboxDisconnected {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProviderCommandUnavailable;

impl std::fmt::Display for ProviderCommandUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("provider command mailbox is unavailable")
    }
}

impl std::error::Error for ProviderCommandUnavailable {}

const SUBSCRIPTION_ID: &str = "desktop_fixture_market_bars";
const MODEL_ITEM_CAPACITY: usize = 600;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(test)]
const CONFIRMED_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

pub type DesktopMarketGeneration = MarketGeneration<ProvenancedMarketBar>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChartState {
    Loading,
    Ready,
    Stale,
    Recovering,
    Error,
}

impl ChartState {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Loading => "Loading chart",
            Self::Ready => "Chart ready",
            Self::Stale => "Chart stale",
            Self::Recovering => "Reconnecting chart",
            Self::Error => "Chart unavailable",
        }
    }
}

pub struct MarketWorkerBootstrap {
    pub snapshot: ReplaySnapshot,
    pub subscription_id: String,
    pub generation: DesktopMarketGeneration,
    pub worker_label: String,
}

pub enum MarketWorkerStartup {
    Rithmic,
    Loading(Box<CoinbaseWorkerStartup>),
}

pub struct CoinbaseWorkerStartup {
    pub coinbase_product: InstallProviderInstrument,
    pub subscription_id: String,
    pub worker_label: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderCatalogCommand {
    Search,
    Selection,
}

pub enum ProviderCatalogEvent {
    SearchCompleted(ProviderInstrumentSearchResult),
    SelectionInstalled(InstallProviderInstrument),
    CommandRejected {
        rejection: ProviderCatalogRejected,
        command: ProviderCatalogCommand,
    },
}

pub struct MarketWorkerPublication {
    pub update: ReplayStreamUpdate,
    pub generation: MarketPublicationGeneration,
    pub subscription_id: String,
    pub worker_label: String,
    pub ui_diagnostics: Option<PendingUiDiagnostics>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarketPublicationGeneration {
    publication_generation: u64,
    retained_items: usize,
    first_sequence: u64,
    last_sequence: u64,
}

impl MarketPublicationGeneration {
    #[must_use]
    pub fn from_generation(generation: &DesktopMarketGeneration) -> Self {
        let (first_sequence, last_sequence) = generation.sequence_range();
        Self {
            publication_generation: generation.publication_generation(),
            retained_items: generation.items().len(),
            first_sequence,
            last_sequence,
        }
    }

    #[must_use]
    pub const fn from_tail(
        publication_generation: u64,
        retained_items: usize,
        first_sequence: u64,
        last_sequence: u64,
    ) -> Self {
        Self {
            publication_generation,
            retained_items,
            first_sequence,
            last_sequence,
        }
    }

    #[must_use]
    pub const fn publication_generation(self) -> u64 {
        self.publication_generation
    }

    #[must_use]
    pub const fn retained_items(self) -> usize {
        self.retained_items
    }

    #[must_use]
    pub const fn sequence_range(self) -> (u64, u64) {
        (self.first_sequence, self.last_sequence)
    }
}

pub enum MarketWorkerMessage {
    Update(MarketWorkerPublication),
    Diagnostics(Box<FeedDiagnosticsSnapshot>),
    Recovery {
        request_id: u64,
        result: Result<MarketWorkerBootstrap, String>,
    },
    State {
        state: ChartState,
        message: String,
    },
    Connection {
        state: FeedConnectionState,
        message: String,
    },
    ProviderCatalog(ProviderCatalogEvent),
    RithmicHistory {
        selection_generation: NonZeroUsize,
        series_generation: NonZeroUsize,
        result: Result<Box<MarketWorkerBootstrap>, String>,
    },
    RithmicLive {
        selection_generation: NonZeroUsize,
        series_generation: NonZeroUsize,
        update: ReplayStreamUpdate,
    },
    RithmicDom(DomFrame),
    CoinbaseSwitchMarker {
        sequence: u64,
    },
    CoinbaseCatalog(Result<Vec<InstallProviderInstrument>, String>),
    CoinbaseDom(DomFrame),
    ChartViewport {
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    },
}

struct MarketWorkerMailbox {
    queue: Mutex<VecDeque<MarketWorkerMessage>>,
    capacity: usize,
    sender_count: AtomicUsize,
    receiver_alive: AtomicBool,
    coalesced_updates: Mutex<GenerationCoalescingQueue>,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    wake_pending: AtomicBool,
}

fn fire_mailbox_wake(mailbox: &MarketWorkerMailbox) {
    if !mailbox.receiver_alive.load(Ordering::Acquire) {
        return;
    }
    if mailbox.wake_pending.swap(true, Ordering::AcqRel) {
        return;
    }
    let wake = mailbox
        .wake
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    match wake {
        Some(wake) => wake(),
        None => mailbox.wake_pending.store(false, Ordering::Release),
    }
}

pub struct MarketWorkerSender {
    mailbox: Arc<MarketWorkerMailbox>,
}

pub struct MarketWorkerReceiver {
    mailbox: Arc<MarketWorkerMailbox>,
}

impl Clone for MarketWorkerSender {
    fn clone(&self) -> Self {
        self.mailbox.sender_count.fetch_add(1, Ordering::Relaxed);
        Self {
            mailbox: Arc::clone(&self.mailbox),
        }
    }
}

impl Drop for MarketWorkerSender {
    fn drop(&mut self) {
        if self.mailbox.sender_count.fetch_sub(1, Ordering::AcqRel) == 1 {
            fire_mailbox_wake(&self.mailbox);
        }
    }
}

impl MarketWorkerSender {
    /// Enqueues a worker publication and wakes the consumer.
    ///
    /// # Errors
    ///
    /// Returns [`MailboxDisconnected`] after the receiving endpoint is dropped.
    pub fn send(&self, message: MarketWorkerMessage) -> Result<(), MailboxDisconnected> {
        self.enqueue(message)?;
        fire_mailbox_wake(&self.mailbox);
        Ok(())
    }

    fn enqueue(&self, message: MarketWorkerMessage) -> Result<(), MailboxDisconnected> {
        if !self.mailbox.receiver_alive.load(Ordering::Acquire) {
            return Err(MailboxDisconnected);
        }
        let mut queue = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.mailbox.receiver_alive.load(Ordering::Acquire) {
            return Err(MailboxDisconnected);
        }
        let Some(message) = self.send_conflated(&mut queue, message) else {
            return Ok(());
        };
        if matches!(
            queue.back(),
            Some(MarketWorkerMessage::State {
                state: ChartState::Error,
                ..
            })
        ) && !matches!(
            &message,
            MarketWorkerMessage::State {
                state: ChartState::Error,
                ..
            }
        ) {
            self.record_coalesced_message(&message);
            return Ok(());
        }
        if matches!(&message, MarketWorkerMessage::State { .. })
            && matches!(queue.back(), Some(MarketWorkerMessage::State { .. }))
        {
            queue.pop_back();
            queue.push_back(message);
            return Ok(());
        }
        if queue.len() >= self.mailbox.capacity
            && !queue.iter().any(|queued| {
                matches!(
                    queued,
                    MarketWorkerMessage::Recovery { .. }
                        | MarketWorkerMessage::State {
                            state: ChartState::Error,
                            ..
                        }
                )
            })
            && let Some(index) = queue
                .iter()
                .position(|queued| matches!(queued, MarketWorkerMessage::Diagnostics(_)))
            && let Some(diagnostics) = queue.remove(index)
        {
            self.record_coalesced_message(&diagnostics);
        }
        if queue.len() < self.mailbox.capacity {
            queue.push_back(message);
            return Ok(());
        }
        if queue.iter().any(|queued| {
            matches!(
                queued,
                MarketWorkerMessage::State {
                    state: ChartState::Error,
                    ..
                }
            )
        }) && !matches!(
            &message,
            MarketWorkerMessage::State {
                state: ChartState::Error,
                ..
            }
        ) {
            self.record_coalesced_message(&message);
            return Ok(());
        }
        let recovery_queued = queue
            .iter()
            .any(|queued| matches!(queued, MarketWorkerMessage::Recovery { .. }));
        if recovery_queued
            && !matches!(
                &message,
                MarketWorkerMessage::State {
                    state: ChartState::Error,
                    ..
                }
            )
        {
            self.send_while_recovery_queued(&mut queue, message);
            return Ok(());
        }
        self.replace_overflowed_queue(&mut queue, message);
        Ok(())
    }

    fn send_conflated(
        &self,
        queue: &mut VecDeque<MarketWorkerMessage>,
        message: MarketWorkerMessage,
    ) -> Option<MarketWorkerMessage> {
        match message {
            message @ MarketWorkerMessage::Update(MarketWorkerPublication {
                update: ReplayStreamUpdate::Tail(_),
                ..
            }) => {
                self.send_live_tail(queue, message);
                None
            }
            message @ MarketWorkerMessage::Diagnostics(_) => {
                self.send_diagnostics(queue, message);
                None
            }
            message @ MarketWorkerMessage::Connection { .. } => {
                self.send_connection(queue, message);
                None
            }
            message @ MarketWorkerMessage::ProviderCatalog(_) => {
                self.send_rithmic_catalog(queue, message);
                None
            }
            message @ MarketWorkerMessage::RithmicHistory { .. } => {
                self.send_rithmic_history(queue, message);
                None
            }
            message @ MarketWorkerMessage::RithmicLive { .. } => {
                self.send_rithmic_live(queue, message);
                None
            }
            message @ MarketWorkerMessage::RithmicDom(_) => {
                self.send_rithmic_dom(queue, message);
                None
            }
            message @ MarketWorkerMessage::CoinbaseDom(_) => {
                self.send_coinbase_dom(queue, message);
                None
            }
            message @ MarketWorkerMessage::CoinbaseCatalog(_) => {
                self.send_coinbase_catalog(queue, message);
                None
            }
            message => Some(message),
        }
    }

    fn send_live_tail(
        &self,
        queue: &mut VecDeque<MarketWorkerMessage>,
        message: MarketWorkerMessage,
    ) {
        let incoming_generation = market_publication_generation(&message);
        if let Some(index) = queue.iter().position(|queued| {
            matches!(
                queued,
                MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Tail(_),
                    ..
                })
            )
        }) {
            if market_publication_generation(&queue[index]) <= incoming_generation {
                self.record_coalesced_message(&queue[index]);
                queue[index] = message;
            }
            return;
        }
        if queue.len() >= self.mailbox.capacity
            && let Some(index) = queue.iter().position(|queued| {
                matches!(
                    queued,
                    MarketWorkerMessage::Diagnostics(_) | MarketWorkerMessage::Connection { .. }
                )
            })
        {
            queue.remove(index);
        }
        if queue.len() < self.mailbox.capacity {
            queue.push_back(message);
        } else {
            self.record_coalesced_message(&message);
        }
    }

    fn send_connection(
        &self,
        queue: &mut VecDeque<MarketWorkerMessage>,
        message: MarketWorkerMessage,
    ) {
        if let Some(index) = queue
            .iter()
            .position(|queued| matches!(queued, MarketWorkerMessage::Connection { .. }))
        {
            queue[index] = message;
            return;
        }
        if queue.len() >= self.mailbox.capacity {
            let Some(index) = queue.iter().position(|queued| {
                !matches!(
                    queued,
                    MarketWorkerMessage::Recovery { .. }
                        | MarketWorkerMessage::State {
                            state: ChartState::Error,
                            ..
                        }
                )
            }) else {
                return;
            };
            queue.remove(index);
        }
        queue.push_back(message);
    }

    fn send_rithmic_catalog(
        &self,
        queue: &mut VecDeque<MarketWorkerMessage>,
        message: MarketWorkerMessage,
    ) {
        if let Some(index) = queue
            .iter()
            .position(|queued| matches!(queued, MarketWorkerMessage::ProviderCatalog(_)))
        {
            queue[index] = message;
            return;
        }
        if queue.len() >= self.mailbox.capacity
            && let Some(index) = queue.iter().position(|queued| {
                matches!(
                    queued,
                    MarketWorkerMessage::Diagnostics(_) | MarketWorkerMessage::Connection { .. }
                )
            })
        {
            queue.remove(index);
        }
        if queue.len() < self.mailbox.capacity {
            queue.push_back(message);
        }
    }

    fn send_rithmic_history(
        &self,
        queue: &mut VecDeque<MarketWorkerMessage>,
        message: MarketWorkerMessage,
    ) {
        let incoming_generation = rithmic_message_generation(&message);
        if let Some(index) = queue
            .iter()
            .position(|queued| matches!(queued, MarketWorkerMessage::RithmicHistory { .. }))
        {
            if rithmic_message_generation(&queue[index]) > incoming_generation {
                return;
            }
            queue[index] = message;
            return;
        }
        if queue.len() >= self.mailbox.capacity
            && let Some(index) = queue.iter().position(|queued| {
                matches!(
                    queued,
                    MarketWorkerMessage::Diagnostics(_) | MarketWorkerMessage::Connection { .. }
                )
            })
        {
            queue.remove(index);
        }
        if queue.len() < self.mailbox.capacity {
            queue.push_back(message);
        }
    }

    fn send_rithmic_live(
        &self,
        queue: &mut VecDeque<MarketWorkerMessage>,
        message: MarketWorkerMessage,
    ) {
        let incoming_generation = rithmic_message_generation(&message);
        if let Some(index) = queue
            .iter()
            .position(|queued| matches!(queued, MarketWorkerMessage::RithmicLive { .. }))
        {
            if rithmic_message_generation(&queue[index]) > incoming_generation {
                return;
            }
            queue[index] = message;
            return;
        }
        if queue.len() >= self.mailbox.capacity
            && let Some(index) = queue.iter().position(|queued| {
                matches!(
                    queued,
                    MarketWorkerMessage::Diagnostics(_) | MarketWorkerMessage::Connection { .. }
                )
            })
        {
            queue.remove(index);
        }
        if queue.len() < self.mailbox.capacity {
            queue.push_back(message);
        }
    }

    fn send_rithmic_dom(
        &self,
        queue: &mut VecDeque<MarketWorkerMessage>,
        message: MarketWorkerMessage,
    ) {
        if let Some(index) = queue
            .iter()
            .position(|queued| matches!(queued, MarketWorkerMessage::RithmicDom(_)))
        {
            let replace = match (&queue[index], &message) {
                (
                    MarketWorkerMessage::RithmicDom(current),
                    MarketWorkerMessage::RithmicDom(next),
                ) => {
                    (next.selection_generation, next.revision)
                        >= (current.selection_generation, current.revision)
                }
                _ => false,
            };
            if replace {
                queue[index] = message;
            }
            return;
        }
        if queue.len() >= self.mailbox.capacity
            && let Some(index) = queue.iter().position(|queued| {
                matches!(
                    queued,
                    MarketWorkerMessage::Diagnostics(_) | MarketWorkerMessage::Connection { .. }
                )
            })
        {
            queue.remove(index);
        }
        if queue.len() < self.mailbox.capacity {
            queue.push_back(message);
        }
    }

    fn send_coinbase_dom(
        &self,
        queue: &mut VecDeque<MarketWorkerMessage>,
        message: MarketWorkerMessage,
    ) {
        if let Some(index) = queue
            .iter()
            .position(|queued| matches!(queued, MarketWorkerMessage::CoinbaseDom(_)))
        {
            let replace = match (&queue[index], &message) {
                (
                    MarketWorkerMessage::CoinbaseDom(current),
                    MarketWorkerMessage::CoinbaseDom(next),
                ) => {
                    (next.session_generation, next.revision)
                        >= (current.session_generation, current.revision)
                }
                _ => false,
            };
            if replace {
                queue[index] = message;
            }
            return;
        }
        if queue.len() >= self.mailbox.capacity
            && let Some(index) = queue.iter().position(|queued| {
                matches!(
                    queued,
                    MarketWorkerMessage::Diagnostics(_) | MarketWorkerMessage::Connection { .. }
                )
            })
        {
            queue.remove(index);
        }
        if queue.len() < self.mailbox.capacity {
            queue.push_back(message);
        }
    }

    fn send_coinbase_catalog(
        &self,
        queue: &mut VecDeque<MarketWorkerMessage>,
        message: MarketWorkerMessage,
    ) {
        if let Some(index) = queue
            .iter()
            .position(|queued| matches!(queued, MarketWorkerMessage::CoinbaseCatalog(_)))
        {
            queue[index] = message;
            return;
        }
        if queue.len() >= self.mailbox.capacity
            && let Some(index) = queue.iter().position(|queued| {
                matches!(
                    queued,
                    MarketWorkerMessage::Diagnostics(_) | MarketWorkerMessage::Connection { .. }
                )
            })
        {
            queue.remove(index);
        }
        if queue.len() < self.mailbox.capacity {
            queue.push_back(message);
        }
    }

    fn send_diagnostics(
        &self,
        queue: &mut VecDeque<MarketWorkerMessage>,
        message: MarketWorkerMessage,
    ) {
        if let Some(index) = queue
            .iter()
            .position(|queued| matches!(queued, MarketWorkerMessage::Diagnostics(_)))
        {
            self.record_coalesced_message(&queue[index]);
            queue[index] = message;
            return;
        }
        if queue.len() >= self.mailbox.capacity {
            self.record_coalesced_message(&message);
            return;
        }
        queue.push_back(message);
    }

    fn send_while_recovery_queued(
        &self,
        queue: &mut VecDeque<MarketWorkerMessage>,
        message: MarketWorkerMessage,
    ) {
        let coalesced_generation = message_diagnostics_generation(&message);
        let successful_recovery_queued = queue.iter().any(|queued| {
            matches!(
                queued,
                MarketWorkerMessage::Recovery { result, .. } if result.is_ok()
            )
        });
        let invalidation = match message {
            MarketWorkerMessage::Update(publication)
                if matches!(&publication.update, ReplayStreamUpdate::Delta(_))
                    && successful_recovery_queued =>
            {
                Some(mailbox_overflow_state())
            }
            message @ MarketWorkerMessage::State {
                state: ChartState::Stale | ChartState::Recovering,
                ..
            } if successful_recovery_queued => Some(message),
            _ => None,
        };
        if let Some(generation) = coalesced_generation {
            self.record_coalesced_generation(generation, 1);
        }
        if let Some(invalidation) = invalidation {
            for queued in queue.iter() {
                if !matches!(queued, MarketWorkerMessage::Recovery { .. }) {
                    self.record_coalesced_message(queued);
                }
            }
            queue.retain(|queued| matches!(queued, MarketWorkerMessage::Recovery { .. }));
            queue.push_back(invalidation);
        }
    }

    pub fn occupancy(&self) -> (usize, usize) {
        let current = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        (current, self.mailbox.capacity)
    }

    pub fn try_take_coalesced_update(&self) -> Option<CoalescedUiUpdates> {
        self.mailbox
            .coalesced_updates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }

    fn record_coalesced_message(&self, message: &MarketWorkerMessage) {
        if let Some(generation) = message_diagnostics_generation(message) {
            self.record_coalesced_generation(generation, 1);
        }
    }

    fn record_coalesced_generation(&self, generation: NonZeroU64, count: u64) {
        self.mailbox
            .coalesced_updates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record(generation, count);
    }

    fn replace_overflowed_queue(
        &self,
        queue: &mut VecDeque<MarketWorkerMessage>,
        message: MarketWorkerMessage,
    ) {
        match message {
            MarketWorkerMessage::Update(publication)
                if matches!(&publication.update, ReplayStreamUpdate::Delta(_)) =>
            {
                let covering_snapshot = take_covering_snapshot(queue);
                for queued in queue.iter() {
                    self.record_coalesced_message(queued);
                }
                queue.clear();
                if let Some(generation) = publication
                    .ui_diagnostics
                    .as_ref()
                    .map(PendingUiDiagnostics::generation)
                {
                    self.record_coalesced_generation(generation, 1);
                }
                if let Some(snapshot) = covering_snapshot {
                    queue.push_back(snapshot);
                }
                queue.push_back(mailbox_overflow_state());
            }
            message => {
                for queued in queue.iter() {
                    self.record_coalesced_message(queued);
                }
                queue.clear();
                queue.push_back(message);
            }
        }
    }
}

fn rithmic_message_generation(message: &MarketWorkerMessage) -> Option<(usize, usize)> {
    match message {
        MarketWorkerMessage::RithmicHistory {
            selection_generation,
            series_generation,
            ..
        }
        | MarketWorkerMessage::RithmicLive {
            selection_generation,
            series_generation,
            ..
        } => Some((selection_generation.get(), series_generation.get())),
        _ => None,
    }
}

fn market_publication_generation(message: &MarketWorkerMessage) -> Option<u64> {
    match message {
        MarketWorkerMessage::Update(publication) => {
            Some(publication.generation.publication_generation())
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoalescedUiUpdates {
    pub generation: NonZeroU64,
    pub count: u64,
}

struct GenerationCoalescingQueue {
    counts: VecDeque<CoalescedUiUpdates>,
    capacity: usize,
}

impl GenerationCoalescingQueue {
    fn new(capacity: usize) -> Self {
        Self {
            counts: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn record(&mut self, generation: NonZeroU64, count: u64) {
        if let Some(existing) = self
            .counts
            .iter_mut()
            .find(|existing| existing.generation == generation)
        {
            existing.count = existing.count.saturating_add(count);
        } else if self.counts.len() < self.capacity {
            self.counts
                .push_back(CoalescedUiUpdates { generation, count });
        }
    }

    fn pop_front(&mut self) -> Option<CoalescedUiUpdates> {
        self.counts.pop_front()
    }
}

fn message_diagnostics_generation(message: &MarketWorkerMessage) -> Option<NonZeroU64> {
    match message {
        MarketWorkerMessage::Update(publication) => publication
            .ui_diagnostics
            .as_ref()
            .map(PendingUiDiagnostics::generation),
        MarketWorkerMessage::Diagnostics(snapshot) => snapshot.session_generation,
        MarketWorkerMessage::Recovery { .. }
        | MarketWorkerMessage::State { .. }
        | MarketWorkerMessage::Connection { .. }
        | MarketWorkerMessage::ProviderCatalog(_)
        | MarketWorkerMessage::RithmicHistory { .. }
        | MarketWorkerMessage::RithmicLive { .. }
        | MarketWorkerMessage::RithmicDom(_)
        | MarketWorkerMessage::CoinbaseSwitchMarker { .. }
        | MarketWorkerMessage::CoinbaseCatalog(_)
        | MarketWorkerMessage::CoinbaseDom(_)
        | MarketWorkerMessage::ChartViewport { .. } => None,
    }
}

fn take_covering_snapshot(
    queue: &mut VecDeque<MarketWorkerMessage>,
) -> Option<MarketWorkerMessage> {
    queue
        .iter()
        .rposition(|queued| {
            matches!(
                queued,
                MarketWorkerMessage::Update(publication)
                    if matches!(publication.update, ReplayStreamUpdate::Snapshot(_))
            )
        })
        .and_then(|index| queue.remove(index))
}

impl MarketWorkerReceiver {
    pub fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self
            .mailbox
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(wake);
        let pending = !self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty();
        if pending {
            fire_mailbox_wake(&self.mailbox);
        }
    }

    pub fn drain(&self) -> (Vec<MarketWorkerMessage>, bool) {
        let messages = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect::<Vec<_>>();
        self.mailbox.wake_pending.store(false, Ordering::Release);
        let (queued, disconnected) = {
            let queue = self
                .mailbox
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                !queue.is_empty(),
                self.mailbox.sender_count.load(Ordering::Acquire) == 0,
            )
        };
        if queued {
            fire_mailbox_wake(&self.mailbox);
        }
        (messages, disconnected)
    }
}

impl Drop for MarketWorkerReceiver {
    fn drop(&mut self) {
        self.mailbox.receiver_alive.store(false, Ordering::Release);
        *self
            .mailbox
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

#[must_use]
pub fn market_worker_channel(capacity: NonZeroUsize) -> (MarketWorkerSender, MarketWorkerReceiver) {
    let mailbox = Arc::new(MarketWorkerMailbox {
        queue: Mutex::new(VecDeque::with_capacity(capacity.get().saturating_add(1))),
        capacity: capacity.get(),
        sender_count: AtomicUsize::new(1),
        receiver_alive: AtomicBool::new(true),
        coalesced_updates: Mutex::new(GenerationCoalescingQueue::new(capacity.get())),
        wake: Mutex::new(None),
        wake_pending: AtomicBool::new(false),
    });
    (
        MarketWorkerSender {
            mailbox: Arc::clone(&mailbox),
        },
        MarketWorkerReceiver { mailbox },
    )
}

fn mailbox_overflow_state() -> MarketWorkerMessage {
    MarketWorkerMessage::State {
        state: ChartState::Recovering,
        message: "bounded market UI mailbox overflowed; a covering snapshot is required"
            .to_string(),
    }
}

pub enum MarketWorkerCommand {
    Recovery(ReplayRecoveryCommand),
    ProviderSearch(SearchProviderInstruments),
    ProviderSelect(SelectProviderInstrument),
    EngineSeries(EngineSeriesRequest),
    CoinbaseSelect(Box<CoinbaseSelectionRequest>),
    ChartViewport(ChartViewportUpdate),
    Shutdown,
}

#[derive(Clone, Debug)]
pub struct CoinbaseSelectionRequest {
    pub sequence: u64,
    pub product: InstallProviderInstrument,
    pub interval: ChartInterval,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChartViewportUpdate {
    pub start_unix_nanos: i64,
    pub end_unix_nanos: i64,
    pub selection_generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineSeriesRequest {
    pub selection_generation: NonZeroUsize,
    pub series_generation: NonZeroUsize,
    pub interval: ChartInterval,
}

pub struct PendingUiDiagnostics {
    generation: NonZeroU64,
    origin: Instant,
    ui_enqueue_nanos: i64,
    frame_submit_nanos: i64,
}

impl PendingUiDiagnostics {
    #[must_use]
    pub fn new(generation: NonZeroU64) -> Self {
        Self {
            generation,
            origin: Instant::now(),
            ui_enqueue_nanos: 0,
            frame_submit_nanos: 0,
        }
    }

    pub fn mark_ui_enqueue(&mut self) {
        self.ui_enqueue_nanos = self.elapsed_nanos();
    }

    pub fn mark_frame_submit(&mut self) {
        self.frame_submit_nanos = self.elapsed_nanos();
    }

    #[must_use]
    pub fn into_presented(self) -> UiDiagnosticsFeedback {
        UiDiagnosticsFeedback::Presented {
            generation: self.generation,
            ui_enqueue_nanos: self.ui_enqueue_nanos,
            frame_submit_nanos: self.frame_submit_nanos,
            present_nanos: self.elapsed_nanos(),
        }
    }

    #[must_use]
    pub const fn generation(&self) -> NonZeroU64 {
        self.generation
    }

    fn elapsed_nanos(&self) -> i64 {
        i64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(i64::MAX)
    }
}

pub enum UiDiagnosticsFeedback {
    Presented {
        generation: NonZeroU64,
        ui_enqueue_nanos: i64,
        frame_submit_nanos: i64,
        present_nanos: i64,
    },
    Coalesced {
        generation: NonZeroU64,
    },
}

struct UiDiagnosticsMailbox {
    queue: Mutex<VecDeque<UiDiagnosticsFeedback>>,
    capacity: usize,
    receiver_alive: AtomicBool,
    coalesced_feedback: Mutex<GenerationCoalescingQueue>,
}

#[derive(Clone)]
pub struct UiDiagnosticsSender {
    mailbox: Arc<UiDiagnosticsMailbox>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

pub struct UiDiagnosticsReceiver {
    mailbox: Arc<UiDiagnosticsMailbox>,
}

impl UiDiagnosticsSender {
    /// Enqueues UI presentation feedback.
    ///
    /// # Errors
    ///
    /// Returns [`MailboxDisconnected`] after the receiving endpoint is dropped.
    pub fn send(&self, feedback: UiDiagnosticsFeedback) -> Result<(), MailboxDisconnected> {
        if !self.mailbox.receiver_alive.load(Ordering::Acquire) {
            return Err(MailboxDisconnected);
        }
        let mut queue = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.mailbox.receiver_alive.load(Ordering::Acquire) {
            return Err(MailboxDisconnected);
        }
        if queue.len() >= self.mailbox.capacity
            && let Some(discarded) = queue.pop_front()
        {
            self.mailbox
                .coalesced_feedback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record(feedback_generation(&discarded), 1);
        }
        queue.push_back(feedback);
        drop(queue);
        (self.wake)();
        Ok(())
    }
}

impl UiDiagnosticsReceiver {
    pub fn try_recv(&self) -> Option<UiDiagnosticsFeedback> {
        self.mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }

    pub fn occupancy(&self) -> (usize, usize) {
        let current = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        (current, self.mailbox.capacity)
    }

    pub fn try_take_coalesced_feedback(&self) -> Option<CoalescedUiUpdates> {
        self.mailbox
            .coalesced_feedback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }
}

impl Drop for UiDiagnosticsReceiver {
    fn drop(&mut self) {
        self.mailbox.receiver_alive.store(false, Ordering::Release);
        self.mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

pub fn ui_diagnostics_channel(
    capacity: NonZeroUsize,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> (UiDiagnosticsSender, UiDiagnosticsReceiver) {
    let mailbox = Arc::new(UiDiagnosticsMailbox {
        queue: Mutex::new(VecDeque::with_capacity(capacity.get())),
        capacity: capacity.get(),
        receiver_alive: AtomicBool::new(true),
        coalesced_feedback: Mutex::new(GenerationCoalescingQueue::new(capacity.get())),
    });
    (
        UiDiagnosticsSender {
            mailbox: Arc::clone(&mailbox),
            wake,
        },
        UiDiagnosticsReceiver { mailbox },
    )
}

fn feedback_generation(feedback: &UiDiagnosticsFeedback) -> NonZeroU64 {
    match feedback {
        UiDiagnosticsFeedback::Presented { generation, .. }
        | UiDiagnosticsFeedback::Coalesced { generation } => *generation,
    }
}

pub struct MarketDataWorker {
    commands: Option<SyncSender<MarketWorkerCommand>>,
    messages: Option<MarketWorkerReceiver>,
    shutdown_complete: Option<Receiver<()>>,
    connected: bool,
    ui_diagnostics: Option<UiDiagnosticsSender>,
    coinbase_sequence: Option<Arc<AtomicU64>>,
}

impl MarketDataWorker {
    #[must_use]
    pub const fn from_channels(
        commands: SyncSender<MarketWorkerCommand>,
        messages: MarketWorkerReceiver,
        shutdown_complete: Receiver<()>,
        ui_diagnostics: Option<UiDiagnosticsSender>,
        coinbase_sequence: Option<Arc<AtomicU64>>,
    ) -> Self {
        Self {
            commands: Some(commands),
            messages: Some(messages),
            shutdown_complete: Some(shutdown_complete),
            connected: true,
            ui_diagnostics,
            coinbase_sequence,
        }
    }

    /// Requests a Coinbase selection without blocking.
    ///
    /// # Errors
    /// Returns the request when the command mailbox is full or disconnected.
    pub fn try_select_coinbase(
        &self,
        product: InstallProviderInstrument,
        interval: ChartInterval,
    ) -> Result<u64, TrySendError<Box<CoinbaseSelectionRequest>>> {
        let (Some(commands), Some(sequence)) =
            (self.commands.as_ref(), self.coinbase_sequence.as_ref())
        else {
            return Err(TrySendError::Disconnected(Box::new(
                CoinbaseSelectionRequest {
                    sequence: 0,
                    product,
                    interval,
                },
            )));
        };
        let next = sequence.load(Ordering::Acquire).saturating_add(1);
        let request = Box::new(CoinbaseSelectionRequest {
            sequence: next,
            product,
            interval,
        });
        commands
            .try_send(MarketWorkerCommand::CoinbaseSelect(request))
            .map_err(|error| {
                let (full, command) = match error {
                    TrySendError::Full(command) => (true, command),
                    TrySendError::Disconnected(command) => (false, command),
                };
                let MarketWorkerCommand::CoinbaseSelect(request) = command else {
                    unreachable!("Coinbase selection send errors retain the selection command");
                };
                if full {
                    TrySendError::Full(request)
                } else {
                    TrySendError::Disconnected(request)
                }
            })?;
        sequence.store(next, Ordering::Release);
        Ok(next)
    }

    pub fn send_ui_diagnostics(&self, feedback: UiDiagnosticsFeedback) {
        if let Some(sender) = &self.ui_diagnostics {
            let _ = sender.send(feedback);
        }
    }

    /// Persists a changed chart viewport through the worker boundary without blocking.
    ///
    /// # Errors
    /// Returns the update when the bounded command mailbox is full or disconnected.
    pub fn try_set_chart_viewport(
        &self,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), TrySendError<ChartViewportUpdate>> {
        let selection_generation = self
            .coinbase_sequence
            .as_ref()
            .map_or(0, |sequence| sequence.load(Ordering::Acquire));
        let update = ChartViewportUpdate {
            start_unix_nanos,
            end_unix_nanos,
            selection_generation,
        };
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(update));
        };
        commands
            .try_send(MarketWorkerCommand::ChartViewport(update))
            .map_err(|error| match error {
                TrySendError::Full(MarketWorkerCommand::ChartViewport(update)) => {
                    TrySendError::Full(update)
                }
                TrySendError::Disconnected(MarketWorkerCommand::ChartViewport(update)) => {
                    TrySendError::Disconnected(update)
                }
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    unreachable!("viewport send errors retain the viewport command")
                }
            })
    }

    #[must_use]
    pub fn ui_diagnostics_sender(&self) -> Option<UiDiagnosticsSender> {
        self.ui_diagnostics.clone()
    }

    pub fn set_message_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        if let Some(messages) = &self.messages {
            messages.set_wake(wake);
        }
    }

    /// Enqueues replay recovery without blocking.
    ///
    /// # Errors
    /// Returns the command when the command mailbox is full or disconnected.
    pub fn try_send_recovery(
        &self,
        command: ReplayRecoveryCommand,
    ) -> Result<(), TrySendError<ReplayRecoveryCommand>> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(command));
        };
        commands
            .try_send(MarketWorkerCommand::Recovery(command))
            .map_err(|error| match error {
                TrySendError::Full(MarketWorkerCommand::Recovery(command)) => {
                    TrySendError::Full(command)
                }
                TrySendError::Disconnected(MarketWorkerCommand::Recovery(command)) => {
                    TrySendError::Disconnected(command)
                }
                TrySendError::Full(
                    MarketWorkerCommand::Shutdown
                    | MarketWorkerCommand::ProviderSearch(_)
                    | MarketWorkerCommand::ProviderSelect(_)
                    | MarketWorkerCommand::EngineSeries(_)
                    | MarketWorkerCommand::CoinbaseSelect(_)
                    | MarketWorkerCommand::ChartViewport(_),
                )
                | TrySendError::Disconnected(
                    MarketWorkerCommand::Shutdown
                    | MarketWorkerCommand::ProviderSearch(_)
                    | MarketWorkerCommand::ProviderSelect(_)
                    | MarketWorkerCommand::EngineSeries(_)
                    | MarketWorkerCommand::CoinbaseSelect(_)
                    | MarketWorkerCommand::ChartViewport(_),
                ) => {
                    unreachable!("recovery send errors retain the recovery command")
                }
            })
    }

    /// Enqueues a Rithmic symbol search without blocking.
    ///
    /// # Errors
    /// Returns an error when the command mailbox is full or disconnected.
    pub fn try_search_provider(
        &self,
        search: SearchProviderInstruments,
    ) -> Result<(), ProviderCommandUnavailable> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(ProviderCommandUnavailable);
        };
        commands
            .try_send(MarketWorkerCommand::ProviderSearch(search))
            .map_err(|_| ProviderCommandUnavailable)
    }

    /// Enqueues a Rithmic selection without blocking.
    ///
    /// # Errors
    /// Returns an error when the command mailbox is full or disconnected.
    pub fn try_select_provider(
        &self,
        selection: SelectProviderInstrument,
    ) -> Result<(), ProviderCommandUnavailable> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(ProviderCommandUnavailable);
        };
        commands
            .try_send(MarketWorkerCommand::ProviderSelect(selection))
            .map_err(|_| ProviderCommandUnavailable)
    }

    /// Enqueues a Rithmic history request without blocking.
    ///
    /// # Errors
    /// Returns the request when the command mailbox is full or disconnected.
    pub fn try_request_engine_series(
        &self,
        request: EngineSeriesRequest,
    ) -> Result<(), TrySendError<EngineSeriesRequest>> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(request));
        };
        commands
            .try_send(MarketWorkerCommand::EngineSeries(request))
            .map_err(|error| match error {
                TrySendError::Full(MarketWorkerCommand::EngineSeries(request)) => {
                    TrySendError::Full(request)
                }
                TrySendError::Disconnected(MarketWorkerCommand::EngineSeries(request)) => {
                    TrySendError::Disconnected(request)
                }
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    unreachable!("Rithmic history send errors retain the history request")
                }
            })
    }

    pub fn drain_messages(&mut self) -> (Vec<MarketWorkerMessage>, bool) {
        let Some(receiver) = self.messages.as_ref() else {
            let newly_disconnected = self.connected;
            self.connected = false;
            return (Vec::new(), newly_disconnected);
        };
        let (messages, disconnected) = receiver.drain();
        if disconnected {
            let newly_disconnected = self.connected;
            self.connected = false;
            return (messages, newly_disconnected);
        }
        (messages, false)
    }

    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn mark_disconnected(&mut self) {
        self.connected = false;
    }

    /// Begins worker retirement without waiting for the background client to detach.
    ///
    /// The returned acknowledgement owns the bounded wait and may therefore only be
    /// consumed away from the GPUI thread.
    #[must_use]
    pub fn begin_retirement(&mut self) -> Option<MarketWorkerRetirement> {
        if self.commands.is_none() && self.messages.is_none() {
            return None;
        }
        if let Some(commands) = self.commands.take() {
            let _ = commands.try_send(MarketWorkerCommand::Shutdown);
            drop(commands);
        }
        drop(self.messages.take());
        self.connected = false;
        self.shutdown_complete
            .take()
            .map(|shutdown_complete| MarketWorkerRetirement { shutdown_complete })
    }

    #[cfg(test)]
    pub fn shutdown_and_wait(&mut self) -> bool {
        self.begin_retirement()
            .is_none_or(|retirement| retirement.wait_for(CONFIRMED_SHUTDOWN_TIMEOUT))
    }
}

impl Drop for MarketDataWorker {
    fn drop(&mut self) {
        if let Some(retirement) = self.begin_retirement() {
            let _ = retirement.wait();
        }
    }
}

/// Completion acknowledgement for one retiring desktop market-client worker.
pub struct MarketWorkerRetirement {
    shutdown_complete: Receiver<()>,
}

impl MarketWorkerRetirement {
    /// Waits for the worker to detach within the desktop shutdown budget.
    #[must_use]
    pub fn wait(self) -> bool {
        self.wait_for(SHUTDOWN_TIMEOUT)
    }

    fn wait_for(self, timeout: Duration) -> bool {
        !matches!(
            self.shutdown_complete.recv_timeout(timeout),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        )
    }
}

pub struct FixtureMarketWorker {
    source: EmbeddedReplaySource,
    model: MarketBarClientModel,
}

impl FixtureMarketWorker {
    /// Creates a deterministic disconnected fixture worker.
    ///
    /// # Errors
    /// Retained for the common worker-start contract; construction is infallible.
    pub fn try_new() -> Result<Self, String> {
        Ok(Self {
            source: EmbeddedReplaySource,
            model: MarketBarClientModel::new(
                NonZeroUsize::new(MODEL_ITEM_CAPACITY).unwrap_or(NonZeroUsize::MIN),
            ),
        })
    }

    /// Loads and publishes a bounded fixture snapshot.
    ///
    /// # Errors
    /// Returns an error if loading, decoding, validation, or publication fails.
    pub fn publish_snapshot(&mut self, bar_count: usize) -> Result<MarketWorkerBootstrap, String> {
        let snapshot = self
            .source
            .load_snapshot(LoadEmbeddedReplay { bar_count })
            .map_err(|error| error.to_string())?;
        let generation = self.model.current_generation().map_or(
            Ok(snapshot.evidence().publication_generation),
            |current| {
                current
                    .publication_generation()
                    .checked_add(1)
                    .ok_or_else(|| "desktop fixture generation overflow".to_string())
            },
        )?;
        let snapshot = snapshot
            .try_with_publication_generation(generation)
            .map_err(|error| error.to_string())?;
        let publication =
            self.finish_publication(ReplayStreamUpdate::Snapshot(snapshot.clone()))?;
        let ReplayStreamUpdate::Snapshot(decoded_snapshot) = publication.update else {
            return Err("fixture snapshot published a delta".to_string());
        };
        let generation = self
            .model
            .current_generation()
            .cloned()
            .ok_or_else(|| "fixture snapshot did not retain a generation".to_string())?;
        Ok(MarketWorkerBootstrap {
            snapshot: decoded_snapshot,
            subscription_id: SUBSCRIPTION_ID.to_string(),
            generation,
            worker_label: "deterministic fixture · disconnected".to_string(),
        })
    }

    /// Loads and publishes the fixture delta following `previous_sequence`.
    ///
    /// # Errors
    /// Returns an error if loading, decoding, validation, or publication fails.
    pub fn publish_delta(
        &mut self,
        previous_sequence: u64,
    ) -> Result<Option<MarketWorkerPublication>, String> {
        let Some(delta) = self
            .source
            .load_delta(previous_sequence)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        self.finish_publication(ReplayStreamUpdate::Delta(delta))
            .map(Some)
    }

    fn finish_publication(
        &mut self,
        update: ReplayStreamUpdate,
    ) -> Result<MarketWorkerPublication, String> {
        let outcome = self
            .model
            .apply_update(update.clone())
            .map_err(|error| error.to_string())?;
        let MarketBarModelOutcome::Published(generation) = outcome else {
            return Err("fixture update did not publish a client generation".to_string());
        };
        Ok(MarketWorkerPublication {
            update,
            generation: MarketPublicationGeneration::from_generation(&generation),
            subscription_id: SUBSCRIPTION_ID.to_string(),
            worker_label: "deterministic fixture · disconnected".to_string(),
            ui_diagnostics: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChartState, EngineSeriesRequest, FixtureMarketWorker, MarketDataWorker,
        MarketPublicationGeneration, MarketWorkerCommand, MarketWorkerMessage,
        MarketWorkerPublication, PendingUiDiagnostics, ProviderCatalogCommand,
        ProviderCatalogEvent, UiDiagnosticsFeedback, market_worker_channel, ui_diagnostics_channel,
    };
    use axiusflow_application::{Provenanced, ReplayStreamUpdate, ReplayTailUpdate};
    use axiusflow_engine_protocol::{
        ProviderCatalogRejected, ProviderCatalogRejectionReason, SearchProviderInstruments,
        SelectProviderInstrument,
    };
    use axiusflow_market_data::DomFrame;
    use axiusflow_market_data::{ChartInterval, OrderBookRecoveryReason, OrderBookState};
    use axiusflow_observability::{FeedDiagnostics, FeedIdentity};
    use std::num::{NonZeroU64, NonZeroUsize};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };

    #[test]
    fn chart_states_have_explicit_user_facing_labels() {
        assert_eq!(ChartState::Loading.label(), "Loading chart");
        assert_eq!(ChartState::Ready.label(), "Chart ready");
        assert_eq!(ChartState::Stale.label(), "Chart stale");
        assert_eq!(ChartState::Recovering.label(), "Reconnecting chart");
        assert_eq!(ChartState::Error.label(), "Chart unavailable");
    }

    #[test]
    fn dropping_worker_waits_for_shutdown_acknowledgement() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let acknowledged = Arc::new(AtomicBool::new(false));
        let worker_acknowledged = Arc::clone(&acknowledged);
        let worker_thread = thread::spawn(move || {
            assert!(matches!(
                command_rx.recv(),
                Ok(MarketWorkerCommand::Shutdown)
            ));
            worker_acknowledged.store(true, Ordering::Release);
            shutdown_tx
                .send(())
                .expect("shutdown acknowledgement sends");
        });

        drop(MarketDataWorker::from_channels(
            command_tx,
            message_rx,
            shutdown_rx,
            None,
            None,
        ));

        assert!(acknowledged.load(Ordering::Acquire));
        worker_thread.join().expect("worker exits");
    }

    #[test]
    fn explicit_retirement_moves_the_wait_off_the_caller() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let (observed_tx, observed_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let worker_thread = thread::spawn(move || {
            assert!(matches!(
                command_rx.recv(),
                Ok(MarketWorkerCommand::Shutdown)
            ));
            observed_tx.send(()).expect("retirement is observed");
            release_rx.recv().expect("test releases acknowledgement");
            shutdown_tx
                .send(())
                .expect("shutdown acknowledgement sends");
        });
        let mut worker =
            MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None);

        let retirement = worker
            .begin_retirement()
            .expect("active worker produces a retirement acknowledgement");
        observed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("retirement begins without waiting for acknowledgement");
        release_tx.send(()).expect("release worker acknowledgement");
        assert!(retirement.wait());
        assert!(worker.begin_retirement().is_none());
        worker_thread.join().expect("worker exits");
    }

    #[test]
    fn explicit_shutdown_waits_and_leaves_a_reusable_disconnected_handle() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let worker_thread = thread::spawn(move || {
            assert!(matches!(
                command_rx.recv(),
                Ok(MarketWorkerCommand::Shutdown)
            ));
            shutdown_tx
                .send(())
                .expect("shutdown acknowledgement sends");
        });
        let mut worker =
            MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None);

        assert!(worker.shutdown_and_wait());

        assert!(!worker.is_connected());
        let (messages, newly_disconnected) = worker.drain_messages();
        assert!(messages.is_empty());
        assert!(!newly_disconnected);
        worker_thread.join().expect("worker exits");
    }

    #[test]
    fn state_publications_coalesce_in_the_bounded_mailbox() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        assert!(
            sender
                .send(MarketWorkerMessage::State {
                    state: ChartState::Loading,
                    message: "loading".to_string(),
                })
                .is_ok()
        );
        assert!(
            sender
                .send(MarketWorkerMessage::State {
                    state: ChartState::Ready,
                    message: "ready".to_string(),
                })
                .is_ok()
        );

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::State {
                state: ChartState::Ready,
                message,
            }] if message == "ready"
        ));
        assert_eq!(sender.try_take_coalesced_update(), None);
    }

    #[test]
    fn diagnostics_snapshots_coalesce_without_displacing_market_updates() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        let snapshot = |generation| {
            let mut diagnostics = FeedDiagnostics::new(
                FeedIdentity::try_new("coinbase", "advanced_trade_public", "production")
                    .expect("identity validates"),
                None,
            );
            diagnostics
                .begin_session(generation, 0)
                .expect("session starts");
            diagnostics
                .try_snapshot(1, 1)
                .expect("snapshot succeeds")
                .expect("first snapshot publishes")
        };
        assert!(
            sender
                .send(MarketWorkerMessage::Diagnostics(Box::new(snapshot(
                    NonZeroU64::MIN,
                ))))
                .is_ok()
        );
        assert!(
            sender
                .send(MarketWorkerMessage::Diagnostics(Box::new(snapshot(
                    NonZeroU64::new(2).expect("generation is nonzero"),
                ))))
                .is_ok()
        );
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Diagnostics(_)]
        ));
        assert_eq!(
            sender.try_take_coalesced_update(),
            Some(super::CoalescedUiUpdates {
                generation: NonZeroU64::MIN,
                count: 1,
            })
        );
    }

    #[test]
    fn ui_diagnostics_feedback_is_bounded_and_timestamped() {
        let wake_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wake_count);
        let (sender, receiver) = ui_diagnostics_channel(
            NonZeroUsize::MIN,
            Arc::new(move || {
                wake_counter.fetch_add(1, Ordering::AcqRel);
            }),
        );
        let retired_generation = NonZeroU64::MIN;
        let generation = NonZeroU64::new(2).expect("generation is nonzero");
        assert!(
            sender
                .send(UiDiagnosticsFeedback::Coalesced {
                    generation: retired_generation,
                })
                .is_ok()
        );
        let mut pending = PendingUiDiagnostics::new(generation);
        pending.mark_ui_enqueue();
        pending.mark_frame_submit();
        assert!(sender.send(pending.into_presented()).is_ok());

        let (observed_items, capacity) = receiver.occupancy();
        assert_eq!(capacity, 1);
        assert_eq!(observed_items, 1);
        assert_eq!(
            receiver.try_take_coalesced_feedback(),
            Some(super::CoalescedUiUpdates {
                generation: retired_generation,
                count: 1,
            })
        );
        assert_eq!(wake_count.load(Ordering::Acquire), 2);
        let UiDiagnosticsFeedback::Presented {
            ui_enqueue_nanos,
            frame_submit_nanos,
            present_nanos,
            ..
        } = receiver
            .try_recv()
            .expect("latest presented feedback survives bounded conflation")
        else {
            panic!("latest presented feedback survives bounded conflation");
        };
        assert!(ui_enqueue_nanos >= 0);
        assert!(frame_submit_nanos >= ui_enqueue_nanos);
        assert!(present_nanos >= frame_submit_nanos);
    }

    #[test]
    fn mailbox_wake_fires_once_per_edge_until_drained() {
        let wake_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wake_count);
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(8).expect("capacity is nonzero"));
        receiver.set_wake(Arc::new(move || {
            wake_counter.fetch_add(1, Ordering::AcqRel);
        }));
        assert_eq!(wake_count.load(Ordering::Acquire), 0);

        let message = || MarketWorkerMessage::State {
            state: ChartState::Ready,
            message: "ready".to_string(),
        };
        sender.send(message()).expect("first state sends");
        sender.send(message()).expect("second state sends");
        assert_eq!(wake_count.load(Ordering::Acquire), 1);

        let (drained, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert_eq!(drained.len(), 1);
        sender.send(message()).expect("state after drain sends");
        assert_eq!(wake_count.load(Ordering::Acquire), 2);
    }

    #[test]
    fn live_tail_updates_conflate_to_one_frame_wake() {
        let wake_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wake_count);
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(8).expect("capacity is nonzero"));
        receiver.set_wake(Arc::new(move || {
            wake_counter.fetch_add(1, Ordering::AcqRel);
        }));
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        let bootstrap = fixture.publish_snapshot(2).expect("snapshot publishes");
        let source = bootstrap
            .snapshot
            .bars()
            .last()
            .cloned()
            .expect("tail exists");
        for publication_generation in 2_u64..=64 {
            let mut bar = *source.value();
            bar.close = bar.close.saturating_add(
                i64::try_from(publication_generation).expect("test generation fits"),
            );
            bar.high = bar.high.max(bar.close);
            let tail = ReplayTailUpdate::try_new(
                Provenanced::new(bar, source.provenance().clone()),
                publication_generation,
                true,
            )
            .expect("tail validates");
            sender
                .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Tail(tail),
                    generation: MarketPublicationGeneration::from_tail(
                        publication_generation,
                        2,
                        1,
                        2,
                    ),
                    subscription_id: "tail".to_string(),
                    worker_label: "tail".to_string(),
                    ui_diagnostics: None,
                }))
                .expect("tail sends");
        }
        assert_eq!(wake_count.load(Ordering::Acquire), 1);
        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert_eq!(messages.len(), 1);
        let MarketWorkerMessage::Update(publication) = &messages[0] else {
            panic!("latest tail remains queued");
        };
        assert_eq!(publication.generation.publication_generation(), 64);
    }

    #[test]
    fn mailbox_wake_fires_for_queued_messages_on_registration() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        sender
            .send(MarketWorkerMessage::State {
                state: ChartState::Loading,
                message: "loading".to_string(),
            })
            .expect("state sends before wake registration");

        let wake_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wake_count);
        receiver.set_wake(Arc::new(move || {
            wake_counter.fetch_add(1, Ordering::AcqRel);
        }));
        assert_eq!(wake_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn mailbox_wake_fires_when_last_sender_drops() {
        let wake_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wake_count);
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        receiver.set_wake(Arc::new(move || {
            wake_counter.fetch_add(1, Ordering::AcqRel);
        }));
        let cloned = sender.clone();
        drop(sender);
        assert_eq!(wake_count.load(Ordering::Acquire), 0);
        drop(cloned);
        assert_eq!(wake_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn mailbox_wake_stops_after_receiver_drops() {
        let wake_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wake_count);
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        receiver.set_wake(Arc::new(move || {
            wake_counter.fetch_add(1, Ordering::AcqRel);
        }));
        drop(receiver);
        assert!(
            sender
                .send(MarketWorkerMessage::State {
                    state: ChartState::Ready,
                    message: "ready".to_string(),
                })
                .is_err()
        );
        drop(sender);
        assert_eq!(wake_count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn mailbox_overflow_counts_discarded_market_publications_by_generation() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let generation = NonZeroU64::MIN;
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let bootstrap = worker.publish_snapshot(2).expect("snapshot publishes");
        let previous_sequence = bootstrap.snapshot.stream().last_sequence();
        assert!(
            sender
                .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Snapshot(bootstrap.snapshot),
                    generation: MarketPublicationGeneration::from_generation(
                        &bootstrap.generation,
                    ),
                    subscription_id: bootstrap.subscription_id,
                    worker_label: bootstrap.worker_label,
                    ui_diagnostics: Some(PendingUiDiagnostics::new(generation)),
                }))
                .is_ok()
        );
        let mut delta = worker
            .publish_delta(previous_sequence)
            .expect("delta publishes")
            .expect("fixture contains a delta");
        delta.ui_diagnostics = Some(PendingUiDiagnostics::new(generation));
        assert!(sender.send(MarketWorkerMessage::Update(delta)).is_ok());

        assert_eq!(
            sender.try_take_coalesced_update(),
            Some(super::CoalescedUiUpdates {
                generation,
                count: 1,
            })
        );
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::Update(_),
                MarketWorkerMessage::State {
                    state: ChartState::Recovering,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn diagnostics_never_reduce_market_mailbox_capacity() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(2).expect("capacity is nonzero"));
        let generation = NonZeroU64::MIN;
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let bootstrap = worker.publish_snapshot(2).expect("snapshot publishes");
        let previous_sequence = bootstrap.snapshot.stream().last_sequence();
        assert!(
            sender
                .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Snapshot(bootstrap.snapshot),
                    generation: MarketPublicationGeneration::from_generation(
                        &bootstrap.generation,
                    ),
                    subscription_id: bootstrap.subscription_id,
                    worker_label: bootstrap.worker_label,
                    ui_diagnostics: Some(PendingUiDiagnostics::new(generation)),
                }))
                .is_ok()
        );
        let mut diagnostics = FeedDiagnostics::new(
            FeedIdentity::try_new("coinbase", "advanced_trade_public", "production")
                .expect("identity validates"),
            None,
        );
        diagnostics
            .begin_session(NonZeroU64::MIN, 0)
            .expect("session starts");
        let snapshot = diagnostics
            .try_snapshot(1, 1)
            .expect("snapshot succeeds")
            .expect("snapshot publishes");
        assert!(
            sender
                .send(MarketWorkerMessage::Diagnostics(Box::new(snapshot)))
                .is_ok()
        );
        let mut delta = worker
            .publish_delta(previous_sequence)
            .expect("delta publishes")
            .expect("fixture contains a delta");
        delta.ui_diagnostics = Some(PendingUiDiagnostics::new(generation));
        assert!(sender.send(MarketWorkerMessage::Update(delta)).is_ok());

        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::Update(_),
                MarketWorkerMessage::Update(_)
            ]
        ));
        assert_eq!(
            sender.try_take_coalesced_update(),
            Some(super::CoalescedUiUpdates {
                generation,
                count: 1,
            })
        );
    }

    #[test]
    fn non_delta_overflow_counts_a_discarded_covering_snapshot() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let generation = NonZeroU64::MIN;
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let bootstrap = worker.publish_snapshot(2).expect("snapshot publishes");
        assert!(
            sender
                .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Snapshot(bootstrap.snapshot),
                    generation: MarketPublicationGeneration::from_generation(
                        &bootstrap.generation,
                    ),
                    subscription_id: bootstrap.subscription_id,
                    worker_label: bootstrap.worker_label,
                    ui_diagnostics: Some(PendingUiDiagnostics::new(generation)),
                }))
                .is_ok()
        );
        assert!(
            sender
                .send(MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message: "terminal".to_string(),
                })
                .is_ok()
        );

        assert_eq!(
            sender.try_take_coalesced_update(),
            Some(super::CoalescedUiUpdates {
                generation,
                count: 1,
            })
        );
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::State {
                state: ChartState::Error,
                ..
            }]
        ));
    }

    #[test]
    fn ordered_delta_overflow_fences_for_one_covering_snapshot() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        assert!(
            sender
                .send(MarketWorkerMessage::State {
                    state: ChartState::Ready,
                    message: "ready".to_string(),
                })
                .is_ok()
        );
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let bootstrap = worker.publish_snapshot(2).expect("snapshot publishes");
        let publication = worker
            .publish_delta(bootstrap.snapshot.stream().last_sequence())
            .expect("delta publishes")
            .expect("fixture has a following delta");
        assert!(
            sender
                .send(MarketWorkerMessage::Update(publication))
                .is_ok()
        );

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::State {
                state: ChartState::Recovering,
                message,
            }] if message.contains("covering snapshot")
        ));
    }

    #[test]
    fn ordered_delta_overflow_preserves_a_queued_covering_snapshot() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let bootstrap = worker.publish_snapshot(2).expect("snapshot publishes");
        let previous_sequence = bootstrap.snapshot.stream().last_sequence();
        assert!(
            sender
                .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Snapshot(bootstrap.snapshot),
                    generation: MarketPublicationGeneration::from_generation(
                        &bootstrap.generation,
                    ),
                    subscription_id: bootstrap.subscription_id,
                    worker_label: bootstrap.worker_label,
                    ui_diagnostics: None,
                }))
                .is_ok()
        );
        let publication = worker
            .publish_delta(previous_sequence)
            .expect("delta publishes")
            .expect("fixture has a following delta");
        assert!(
            sender
                .send(MarketWorkerMessage::Update(publication))
                .is_ok()
        );

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::Update(publication),
                MarketWorkerMessage::State {
                    state: ChartState::Recovering,
                    message,
                }
            ] if matches!(publication.update, ReplayStreamUpdate::Snapshot(_))
                && message.contains("covering snapshot")
        ));
    }

    #[test]
    fn terminal_and_failed_recovery_messages_survive_lower_priority_states() {
        let (error_sender, error_receiver) = market_worker_channel(NonZeroUsize::MIN);
        assert!(
            error_sender
                .send(MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message: "terminal".to_string(),
                })
                .is_ok()
        );
        assert!(
            error_sender
                .send(MarketWorkerMessage::State {
                    state: ChartState::Ready,
                    message: "obsolete".to_string(),
                })
                .is_ok()
        );
        let (messages, _) = error_receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::State {
                state: ChartState::Error,
                message,
            }] if message == "terminal"
        ));

        let (recovery_sender, recovery_receiver) = market_worker_channel(NonZeroUsize::MIN);
        assert!(
            recovery_sender
                .send(MarketWorkerMessage::Recovery {
                    request_id: 7,
                    result: Err("recovery failed".to_string()),
                })
                .is_ok()
        );
        assert!(
            recovery_sender
                .send(MarketWorkerMessage::State {
                    state: ChartState::Recovering,
                    message: "obsolete".to_string(),
                })
                .is_ok()
        );
        let (messages, _) = recovery_receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Recovery { request_id: 7, .. }]
        ));
    }

    #[test]
    fn successful_recovery_is_followed_by_newer_stream_invalidations() {
        let (covered_sender, covered_receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let bootstrap = fixture.publish_snapshot(2).expect("snapshot publishes");
        let publication = fixture
            .publish_delta(bootstrap.snapshot.stream().last_sequence())
            .expect("delta publishes")
            .expect("fixture has a following delta");
        assert!(
            covered_sender
                .send(MarketWorkerMessage::Recovery {
                    request_id: 8,
                    result: Ok(bootstrap),
                })
                .is_ok()
        );
        assert!(
            covered_sender
                .send(MarketWorkerMessage::Update(publication))
                .is_ok()
        );
        let (messages, _) = covered_receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::Recovery { request_id: 8, .. },
                MarketWorkerMessage::State {
                    state: ChartState::Recovering,
                    message,
                }
            ] if message.contains("covering snapshot")
        ));

        let (stale_sender, stale_receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut stale_fixture = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let stale_bootstrap = stale_fixture
            .publish_snapshot(2)
            .expect("new snapshot publishes");
        assert!(
            stale_sender
                .send(MarketWorkerMessage::Recovery {
                    request_id: 9,
                    result: Ok(stale_bootstrap),
                })
                .is_ok()
        );
        assert!(
            stale_sender
                .send(MarketWorkerMessage::State {
                    state: ChartState::Stale,
                    message: "network changed".to_string(),
                })
                .is_ok()
        );
        let (messages, _) = stale_receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::Recovery { request_id: 9, .. },
                MarketWorkerMessage::State {
                    state: ChartState::Stale,
                    message,
                }
            ] if message == "network changed"
        ));
    }

    #[test]
    fn latest_rithmic_catalog_result_is_conflated_without_displacing_market_state() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let generation = NonZeroU64::MIN;
        assert!(
            sender
                .send(MarketWorkerMessage::ProviderCatalog(
                    ProviderCatalogEvent::CommandRejected {
                        rejection: ProviderCatalogRejected {
                            consumer_id: 1,
                            provider: "rithmic".to_string(),
                            provider_generation: Some(generation.get()),
                            command_generation: 1,
                            reason: ProviderCatalogRejectionReason::SupersededSearch as i32,
                        },
                        command: ProviderCatalogCommand::Search,
                    },
                ))
                .is_ok()
        );
        let latest_generation = NonZeroUsize::new(2).expect("generation is nonzero");
        assert!(
            sender
                .send(MarketWorkerMessage::ProviderCatalog(
                    ProviderCatalogEvent::CommandRejected {
                        rejection: ProviderCatalogRejected {
                            consumer_id: 1,
                            provider: "rithmic".to_string(),
                            provider_generation: Some(generation.get()),
                            command_generation: latest_generation.get() as u64,
                            reason: ProviderCatalogRejectionReason::InstrumentUnavailable as i32,
                        },
                        command: ProviderCatalogCommand::Selection,
                    },
                ))
                .is_ok()
        );
        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::ProviderCatalog(
                ProviderCatalogEvent::CommandRejected {
                    rejection: ProviderCatalogRejected {
                        command_generation,
                        reason,
                        ..
                    },
                    command: ProviderCatalogCommand::Selection,
                }
            )] if *command_generation == latest_generation.get() as u64
                && *reason == ProviderCatalogRejectionReason::InstrumentUnavailable as i32
        ));

        assert!(
            sender
                .send(MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message: "terminal".to_string(),
                })
                .is_ok()
        );
        assert!(
            sender
                .send(MarketWorkerMessage::ProviderCatalog(
                    ProviderCatalogEvent::CommandRejected {
                        rejection: ProviderCatalogRejected {
                            consumer_id: 1,
                            provider: "rithmic".to_string(),
                            provider_generation: Some(generation.get()),
                            command_generation: latest_generation.get() as u64,
                            reason: ProviderCatalogRejectionReason::SubscriptionRejected as i32,
                        },
                        command: ProviderCatalogCommand::Selection,
                    },
                ))
                .is_ok()
        );
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::State {
                state: ChartState::Error,
                ..
            }]
        ));
    }

    #[test]
    fn latest_rithmic_history_result_is_generation_conflated() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let first = NonZeroUsize::MIN;
        let latest = NonZeroUsize::new(2).expect("generation is nonzero");
        assert!(
            sender
                .send(MarketWorkerMessage::RithmicHistory {
                    selection_generation: first,
                    series_generation: first,
                    result: Err("first failed".to_string()),
                })
                .is_ok()
        );
        assert!(
            sender
                .send(MarketWorkerMessage::RithmicHistory {
                    selection_generation: first,
                    series_generation: latest,
                    result: Err("latest failed".to_string()),
                })
                .is_ok()
        );
        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RithmicHistory {
                series_generation,
                result: Err(error),
                ..
            }] if *series_generation == latest && error == "latest failed"
        ));
    }

    #[test]
    fn stale_live_snapshot_cannot_replace_a_newer_generation() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        let snapshot = fixture
            .publish_snapshot(2)
            .expect("snapshot publishes")
            .snapshot;
        let first = NonZeroUsize::MIN;
        let latest = NonZeroUsize::new(2).expect("generation is nonzero");
        assert!(
            sender
                .send(MarketWorkerMessage::RithmicLive {
                    selection_generation: first,
                    series_generation: latest,
                    update: ReplayStreamUpdate::Snapshot(snapshot.clone()),
                })
                .is_ok()
        );
        assert!(
            sender
                .send(MarketWorkerMessage::RithmicLive {
                    selection_generation: first,
                    series_generation: first,
                    update: ReplayStreamUpdate::Snapshot(snapshot),
                })
                .is_ok()
        );
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RithmicLive { series_generation, .. }]
                if *series_generation == latest
        ));
    }

    #[test]
    fn dom_mailbox_retains_latest_complete_frame() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let frame = |revision, state| DomFrame {
            provider_id: "rithmic".to_string(),
            instrument_id: "rithmic:CME:MNQ".to_string(),
            entitlement_id: "test".to_string(),
            session_generation: 1,
            selection_generation: 2,
            revision,
            source_watermark: revision,
            state,
            rows: Vec::new(),
        };
        assert!(
            sender
                .send(MarketWorkerMessage::RithmicDom(frame(
                    3,
                    OrderBookState::Ready,
                )))
                .is_ok()
        );
        assert!(
            sender
                .send(MarketWorkerMessage::RithmicDom(frame(
                    2,
                    OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap),
                )))
                .is_ok()
        );
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RithmicDom(frame)]
                if frame.revision == 3 && frame.state == OrderBookState::Ready
        ));
    }

    #[test]
    fn rithmic_catalog_and_history_commands_use_the_bounded_worker_channel() {
        let (command_tx, command_rx) = mpsc::sync_channel(3);
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let worker =
            MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None);
        let search = SearchProviderInstruments {
            consumer_id: 0,
            search_generation: 1,
            provider: "rithmic".to_string(),
            query: "ES".to_string(),
            maximum_results: 16,
        };
        let selection = SelectProviderInstrument {
            consumer_id: 0,
            selection_generation: 1,
            search_generation: 1,
            provider: "rithmic".to_string(),
            symbol: "ESU6".to_string(),
            exchange: "CME".to_string(),
            entitlement_id: "rithmic-test-cme".to_string(),
        };
        worker
            .try_search_provider(search)
            .expect("search enters the bounded channel");
        worker
            .try_select_provider(selection)
            .expect("selection enters the bounded channel");
        worker
            .try_request_engine_series(EngineSeriesRequest {
                selection_generation: NonZeroUsize::MIN,
                series_generation: NonZeroUsize::MIN,
                interval: ChartInterval::Minute1,
            })
            .expect("history enters the bounded channel");
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::ProviderSearch(_))
        ));
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::ProviderSelect(_))
        ));
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::EngineSeries(_))
        ));
        let shutdown = thread::spawn(move || {
            assert!(matches!(
                command_rx.recv(),
                Ok(MarketWorkerCommand::Shutdown)
            ));
            shutdown_tx
                .send(())
                .expect("shutdown acknowledgement sends");
        });
        drop(worker);
        shutdown.join().expect("shutdown observer exits");
    }
}
