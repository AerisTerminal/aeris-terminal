//! Desktop market presentation mailbox and deterministic disconnected fixture.
//!
//! This module does not implement or claim a socket, WebSocket, live provider,
//! entitlement service, or production transport. The fixture exercises the same
//! application replay model used by engine publications without inventing a
//! second desktop wire protocol.

use aeris_application::ReplayRecoveryCommand;
use aeris_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
    MarketGeneration, ProvenancedMarketBar, ReplaySnapshot, ReplayStreamUpdate,
};
use aeris_contracts::{
    InstallProviderInstrument, ProviderCatalogRejected, ProviderCatalogRejectionReason,
    ProviderInstrumentSearchResult, SearchProviderInstruments, SelectProviderInstrument,
};
use aeris_market_data::ChartInterval;
use aeris_market_data::OrderBookFrame;
use aeris_market_runtime::study::{NativeStudyRegistration, StudyInstanceId};
use aeris_market_runtime::{
    MarketConsumerResourceClass as ConsumerResourceClass, MarketDeltaDivergenceTrigger,
    MarketPriceAlert, MarketPriceAlertTrigger, MarketRuntimeEvent, MarketStudyOutputSnapshot,
    MarketStudyOutputsInvalidated, MarketStudyRemoved, MarketTradeTapeSnapshot,
};
use aeris_observability::FeedConnectionState;
use aeris_observability::FeedDiagnosticsSnapshot;
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

/// A presentation message could not enter the bounded GPUI mailbox.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketWorkerSendError {
    Full,
    Disconnected,
}

impl std::fmt::Display for MarketWorkerSendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => formatter.write_str("market presentation mailbox is full"),
            Self::Disconnected => {
                formatter.write_str("market presentation mailbox is disconnected")
            }
        }
    }
}

impl std::error::Error for MarketWorkerSendError {}

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
const CONTROL_RESERVE: usize = 8;
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
    Loading(Box<EngineWorkerStartup>),
}

pub struct EngineWorkerStartup {
    pub product: InstallProviderInstrument,
    pub interval: ChartInterval,
    pub restored_viewport: Option<(i64, i64)>,
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
    SearchPreview(ProviderInstrumentSearchResult),
    SelectionInstalled {
        command_generation: u64,
        instrument: InstallProviderInstrument,
    },
    /// The runtime re-resolved the restored startup instrument against the
    /// live provider catalog. Identity is unchanged; metadata such as the
    /// price increment is authoritative and replaces the restored copy.
    StartupInstrumentResolved(InstallProviderInstrument),
    CommandRejected {
        rejection: ProviderCatalogRejected,
        command: ProviderCatalogCommand,
    },
}

#[must_use]
pub fn classify_provider_catalog_event(
    event: MarketRuntimeEvent,
) -> (Option<ProviderCatalogEvent>, Option<MarketRuntimeEvent>) {
    match event {
        // Consumer ownership, provider identity and callback generation are
        // already fenced before catalog events enter the runtime-owned consumer
        // outbox. Desktop only translates the accepted callback into its
        // presentation command shape.
        MarketRuntimeEvent::ProviderInstrumentSearchResult(result) => {
            (Some(ProviderCatalogEvent::SearchCompleted(result)), None)
        }
        MarketRuntimeEvent::ProviderInstrumentSearchPreview(result) => {
            (Some(ProviderCatalogEvent::SearchPreview(result)), None)
        }
        MarketRuntimeEvent::ProviderInstrumentSelection(selection) => {
            let instrument = selection.instrument;
            (
                Some(ProviderCatalogEvent::SelectionInstalled {
                    command_generation: selection.command_generation,
                    instrument,
                }),
                None,
            )
        }
        MarketRuntimeEvent::ProviderCatalogRejected(rejection) => (
            Some(ProviderCatalogEvent::CommandRejected {
                command: provider_catalog_command(rejection.reason),
                rejection,
            }),
            None,
        ),
        event => (None, Some(event)),
    }
}

#[must_use]
pub const fn provider_catalog_command(
    reason: ProviderCatalogRejectionReason,
) -> ProviderCatalogCommand {
    match reason {
        ProviderCatalogRejectionReason::SearchRejected
        | ProviderCatalogRejectionReason::SupersededSearch
        | ProviderCatalogRejectionReason::SearchTimedOut => ProviderCatalogCommand::Search,
        _ => ProviderCatalogCommand::Selection,
    }
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
    StudyOutput(MarketStudyOutputSnapshot),
    StudyOutputsInvalidated(MarketStudyOutputsInvalidated),
    StudyRemoved(MarketStudyRemoved),
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
        transport_rtt_nanos: Option<u64>,
    },
    ProviderCatalog(ProviderCatalogEvent),
    MarketSessionStatus(aeris_contracts::MarketSessionStatus),
    OrderBook(OrderBookFrame),
    TradeTape(MarketTradeTapeSnapshot),
    DeltaDivergenceTriggered(MarketDeltaDivergenceTrigger),
    PriceAlertTriggered(MarketPriceAlertTrigger),
    PriceAlertSyncFailed(String),
    StudyRegistered {
        request_sequence: u64,
        study_id: StudyInstanceId,
    },
    StudyReinitialized {
        study_id: StudyInstanceId,
    },
    StudyRegistrationFailed {
        request_sequence: u64,
        message: String,
    },
    StudyReinitializationFailed {
        study_id: StudyInstanceId,
        message: String,
    },
    StudyRemovalFailed {
        study_id: StudyInstanceId,
        message: String,
    },
    EngineSwitchMarker {
        sequence: u64,
    },
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
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    wake_pending: AtomicBool,
    market_publications_enabled: AtomicBool,
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
    /// Enqueues one presentation message and wakes the consumer.
    ///
    /// # Errors
    ///
    /// Returns explicit backpressure when the bounded GPUI mailbox is full, or
    /// a disconnect after the receiving endpoint is dropped. Market runtime
    /// callers reserve capacity before polling canonical events, so `Full` is an
    /// invariant violation rather than a signal to discard or reconstruct bars.
    pub fn send(&self, message: MarketWorkerMessage) -> Result<(), MarketWorkerSendError> {
        self.enqueue(message)?;
        fire_mailbox_wake(&self.mailbox);
        Ok(())
    }

    fn enqueue(&self, message: MarketWorkerMessage) -> Result<(), MarketWorkerSendError> {
        if !self.mailbox.receiver_alive.load(Ordering::Acquire) {
            return Err(MarketWorkerSendError::Disconnected);
        }
        if !self.accepts_message(&message) {
            return Ok(());
        }
        let mut queue = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.mailbox.receiver_alive.load(Ordering::Acquire) {
            return Err(MarketWorkerSendError::Disconnected);
        }
        if let MarketWorkerMessage::StudyOutput(next) = &message
            && let Some(index) = queue.iter().position(|queued| {
                matches!(
                    queued,
                    MarketWorkerMessage::StudyOutput(current)
                        if current.output_id == next.output_id
                )
            })
        {
            queue[index] = message;
            return Ok(());
        }
        if let MarketWorkerMessage::StudyRemoved(removed) = &message {
            queue.retain(|queued| {
                !matches!(
                    queued,
                    MarketWorkerMessage::StudyOutput(output)
                        if removed.study_ids.contains(&output.study_id)
                )
            });
        }
        if let MarketWorkerMessage::StudyOutputsInvalidated(invalidated) = &message {
            queue.retain(|queued| {
                !matches!(
                    queued,
                    MarketWorkerMessage::StudyOutput(output)
                        if invalidated.study_ids.contains(&output.study_id)
                )
            });
        }
        if let MarketWorkerMessage::OrderBook(next) = &message
            && let Some(index) = queue.iter().position(|queued| {
                matches!(
                    queued,
                    MarketWorkerMessage::OrderBook(current)
                        if order_book_frame_can_be_superseded(current, next)
                )
            })
        {
            queue[index] = message;
            return Ok(());
        }
        if let Some(index) = coalesced_trade_tape_index(&queue, &message) {
            queue[index] = message;
            return Ok(());
        }
        if matches!(message, MarketWorkerMessage::Diagnostics(_)) {
            if let Some(index) = queue
                .iter()
                .position(|queued| matches!(queued, MarketWorkerMessage::Diagnostics(_)))
            {
                queue[index] = message;
                return Ok(());
            }
            if queue.len() >= self.mailbox.capacity {
                return Ok(());
            }
            queue.push_back(message);
            return Ok(());
        }
        let limit = if is_market_publication(&message) {
            self.mailbox.capacity
        } else {
            self.mailbox.capacity.saturating_add(CONTROL_RESERVE)
        };
        if is_market_publication(&message)
            && queue.len() >= limit
            && let Some(index) = queue
                .iter()
                .position(|queued| matches!(queued, MarketWorkerMessage::Diagnostics(_)))
        {
            queue.remove(index);
        }
        if queue.len() >= limit {
            return Err(MarketWorkerSendError::Full);
        }
        queue.push_back(message);
        Ok(())
    }

    fn accepts_message(&self, message: &MarketWorkerMessage) -> bool {
        self.mailbox
            .market_publications_enabled
            .load(Ordering::Acquire)
            || !is_market_publication(message)
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
}

fn order_book_frame_can_be_superseded(current: &OrderBookFrame, next: &OrderBookFrame) -> bool {
    current.provider_id == next.provider_id
        && current.instrument_id == next.instrument_id
        && current.entitlement_id == next.entitlement_id
        && current.session_generation == next.session_generation
        && current.selection_generation == next.selection_generation
        && (next.revision > current.revision
            || (next.revision == current.revision
                && next.trade_source_watermark >= current.trade_source_watermark))
}

fn coalesced_trade_tape_index(
    queue: &VecDeque<MarketWorkerMessage>,
    message: &MarketWorkerMessage,
) -> Option<usize> {
    let MarketWorkerMessage::TradeTape(next) = message else {
        return None;
    };
    queue.iter().position(|queued| {
        matches!(
            queued,
            MarketWorkerMessage::TradeTape(current)
                if current.consumer_id == next.consumer_id
                    && current.generation == next.generation
                    && current.provider_id == next.provider_id
                    && current.instrument_id == next.instrument_id
                    && current.entitlement_id == next.entitlement_id
                    && current.provider_generation == next.provider_generation
                    && next.revision >= current.revision
        )
    })
}

fn is_market_publication(message: &MarketWorkerMessage) -> bool {
    matches!(
        message,
        MarketWorkerMessage::Update(_)
            | MarketWorkerMessage::StudyOutput(_)
            | MarketWorkerMessage::OrderBook(_)
            | MarketWorkerMessage::TradeTape(_)
    )
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

impl MarketWorkerReceiver {
    fn set_market_publications_enabled(&self, enabled: bool) {
        let mut queue = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self
            .mailbox
            .market_publications_enabled
            .load(Ordering::Acquire)
            == enabled
        {
            return;
        }
        queue.retain(|message| !is_market_publication(message));
        self.mailbox
            .market_publications_enabled
            .store(enabled, Ordering::Release);
    }

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

    #[must_use]
    pub fn drain(&self) -> (Vec<MarketWorkerMessage>, bool) {
        self.drain_up_to(usize::MAX)
    }

    /// Drains at most `limit` messages while preserving the mailbox wake edge.
    ///
    /// The desktop consumes this bounded form from its frame callback so a burst
    /// of provider updates cannot monopolize GPUI's event loop. Any remaining
    /// messages stay queued and trigger another wake/frame.
    pub fn drain_up_to(&self, limit: usize) -> (Vec<MarketWorkerMessage>, bool) {
        let mut queue = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let messages = if limit == usize::MAX {
            queue.drain(..).collect::<Vec<_>>()
        } else {
            let end = limit.min(queue.len());
            queue.drain(..end).collect::<Vec<_>>()
        };
        drop(queue);
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
        wake: Mutex::new(None),
        wake_pending: AtomicBool::new(false),
        market_publications_enabled: AtomicBool::new(true),
    });
    (
        MarketWorkerSender {
            mailbox: Arc::clone(&mailbox),
        },
        MarketWorkerReceiver { mailbox },
    )
}

pub enum MarketWorkerCommand {
    Recovery {
        selection_generation: Option<u64>,
        command: ReplayRecoveryCommand,
    },
    ProviderSearch(SearchProviderInstruments),
    ProviderSelect(SelectProviderInstrument),
    EngineSelect(Box<EngineSelectionRequest>),
    ChartViewport(ChartViewportUpdate),
    DepthVisible(bool),
    ResourceClass(ConsumerResourceClass),
    ReplacePriceAlerts(Vec<MarketPriceAlert>),
    RegisterStudy(Box<StudyRegistrationRequest>),
    ReinitializeStudy(Box<StudyReinitializationRequest>),
    RemoveStudy(StudyInstanceId),
    Shutdown,
}

#[derive(Clone)]
pub struct StudyRegistrationRequest {
    pub sequence: u64,
    pub registration: NativeStudyRegistration,
}

#[derive(Clone)]
pub struct StudyReinitializationRequest {
    pub study_id: StudyInstanceId,
    pub registration: NativeStudyRegistration,
}

#[derive(Clone, Debug)]
pub struct EngineSelectionRequest {
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
    resource_class: Option<Arc<Mutex<Option<ConsumerResourceClass>>>>,
    depth_visible: Option<Arc<Mutex<Option<bool>>>>,
    provider_selection: Option<Arc<Mutex<Option<SelectProviderInstrument>>>>,
    engine_selection: Option<Arc<Mutex<Option<Box<EngineSelectionRequest>>>>>,
    price_alerts: Option<Arc<Mutex<Option<Vec<MarketPriceAlert>>>>>,
    messages: Option<MarketWorkerReceiver>,
    shutdown_complete: Option<Receiver<()>>,
    connected: bool,
    ui_diagnostics: Option<UiDiagnosticsSender>,
    engine_selection_sequence: Option<Arc<AtomicU64>>,
    study_request_sequence: AtomicU64,
}

impl MarketDataWorker {
    #[must_use]
    pub fn from_channels(
        commands: SyncSender<MarketWorkerCommand>,
        messages: MarketWorkerReceiver,
        shutdown_complete: Receiver<()>,
        ui_diagnostics: Option<UiDiagnosticsSender>,
        engine_selection_sequence: Option<Arc<AtomicU64>>,
    ) -> Self {
        Self {
            commands: Some(commands),
            resource_class: None,
            depth_visible: None,
            provider_selection: None,
            engine_selection: None,
            price_alerts: None,
            messages: Some(messages),
            shutdown_complete: Some(shutdown_complete),
            connected: true,
            ui_diagnostics,
            engine_selection_sequence,
            study_request_sequence: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub fn with_resource_class_slot(
        mut self,
        resource_class: Arc<Mutex<Option<ConsumerResourceClass>>>,
    ) -> Self {
        self.resource_class = Some(resource_class);
        self
    }

    #[must_use]
    pub fn with_depth_visibility_slot(mut self, depth_visible: Arc<Mutex<Option<bool>>>) -> Self {
        self.depth_visible = Some(depth_visible);
        self
    }

    /// Installs bounded single-item slots for foreground instrument switching.
    ///
    /// Symbol selection must not lose to a burst of lower-priority viewport or
    /// recovery commands. Each slot retains only the newest pending request, so
    /// foreground intent survives command-mailbox pressure without introducing
    /// an unbounded queue.
    #[must_use]
    pub fn with_foreground_selection_slots(
        mut self,
        provider_selection: Arc<Mutex<Option<SelectProviderInstrument>>>,
        engine_selection: Arc<Mutex<Option<Box<EngineSelectionRequest>>>>,
    ) -> Self {
        self.provider_selection = Some(provider_selection);
        self.engine_selection = Some(engine_selection);
        self
    }

    /// Installs a coalescing slot for complete price-alert snapshots.
    #[must_use]
    pub fn with_price_alert_slot(
        mut self,
        price_alerts: Arc<Mutex<Option<Vec<MarketPriceAlert>>>>,
    ) -> Self {
        self.price_alerts = Some(price_alerts);
        self
    }

    /// Replaces the runtime-owned alert definitions without blocking GPUI.
    ///
    /// The latest complete snapshot wins, so rapid edits stay bounded and can
    /// never leave the runtime with a partial set.
    ///
    /// # Errors
    /// Returns the snapshot when the market worker is disconnected.
    pub fn try_replace_price_alerts(
        &self,
        alerts: Vec<MarketPriceAlert>,
    ) -> Result<(), TrySendError<Vec<MarketPriceAlert>>> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(alerts));
        };
        if let Some(slot) = &self.price_alerts {
            *slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(alerts);
            return Ok(());
        }
        commands
            .try_send(MarketWorkerCommand::ReplacePriceAlerts(alerts))
            .map_err(|error| match error {
                TrySendError::Full(MarketWorkerCommand::ReplacePriceAlerts(alerts)) => {
                    TrySendError::Full(alerts)
                }
                TrySendError::Disconnected(MarketWorkerCommand::ReplacePriceAlerts(alerts)) => {
                    TrySendError::Disconnected(alerts)
                }
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    unreachable!("alert send errors retain the alert snapshot")
                }
            })
    }

    /// Queues one trusted native study registration without blocking GPUI.
    ///
    /// The returned sequence is acknowledged by either `StudyRegistered` or
    /// `StudyRegistrationFailed`, so callers never infer runtime ownership from
    /// whether an output happened to be ready immediately.
    ///
    /// # Errors
    /// Returns the complete request when the bounded command mailbox is full or
    /// disconnected.
    pub fn try_register_study(
        &self,
        registration: NativeStudyRegistration,
    ) -> Result<u64, TrySendError<Box<StudyRegistrationRequest>>> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(Box::new(
                StudyRegistrationRequest {
                    sequence: 0,
                    registration,
                },
            )));
        };
        let next = self
            .study_request_sequence
            .load(Ordering::Acquire)
            .saturating_add(1);
        let request = Box::new(StudyRegistrationRequest {
            sequence: next,
            registration,
        });
        commands
            .try_send(MarketWorkerCommand::RegisterStudy(request))
            .map_err(|error| {
                let (full, command) = match error {
                    TrySendError::Full(command) => (true, command),
                    TrySendError::Disconnected(command) => (false, command),
                };
                let MarketWorkerCommand::RegisterStudy(request) = command else {
                    unreachable!("study registration errors retain the study request");
                };
                if full {
                    TrySendError::Full(request)
                } else {
                    TrySendError::Disconnected(request)
                }
            })?;
        self.study_request_sequence.store(next, Ordering::Release);
        Ok(next)
    }

    /// Queues removal of one runtime-owned study subtree.
    ///
    /// Successful removal is acknowledged by the ordinary `StudyRemoved`
    /// publication containing the complete dependency subtree.
    ///
    /// # Errors
    /// Returns the study identity when the bounded worker command lane is full or
    /// disconnected.
    pub fn try_remove_study(
        &self,
        study_id: StudyInstanceId,
    ) -> Result<(), TrySendError<StudyInstanceId>> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(study_id));
        };
        commands
            .try_send(MarketWorkerCommand::RemoveStudy(study_id))
            .map_err(|error| match error {
                TrySendError::Full(MarketWorkerCommand::RemoveStudy(study_id)) => {
                    TrySendError::Full(study_id)
                }
                TrySendError::Disconnected(MarketWorkerCommand::RemoveStudy(study_id)) => {
                    TrySendError::Disconnected(study_id)
                }
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    unreachable!("study removal errors retain the study identity")
                }
            })
    }

    /// Replaces one study's static inputs/settings/program while preserving its
    /// runtime identity and chart-local output identity.
    ///
    /// # Errors
    /// Returns the complete request when the bounded worker command lane is full
    /// or disconnected.
    pub fn try_reinitialize_study(
        &self,
        study_id: StudyInstanceId,
        registration: NativeStudyRegistration,
    ) -> Result<(), TrySendError<Box<StudyReinitializationRequest>>> {
        let request = Box::new(StudyReinitializationRequest {
            study_id,
            registration,
        });
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(request));
        };
        commands
            .try_send(MarketWorkerCommand::ReinitializeStudy(request))
            .map_err(|error| {
                let (full, command) = match error {
                    TrySendError::Full(command) => (true, command),
                    TrySendError::Disconnected(command) => (false, command),
                };
                let MarketWorkerCommand::ReinitializeStudy(request) = command else {
                    unreachable!("study reinitialization errors retain the study request");
                };
                if full {
                    TrySendError::Full(request)
                } else {
                    TrySendError::Disconnected(request)
                }
            })
    }

    /// Requests a Rithmic selection without blocking.
    ///
    /// # Errors
    /// Returns the request when the command mailbox is full or disconnected.
    pub fn try_select_engine(
        &self,
        product: InstallProviderInstrument,
        interval: ChartInterval,
    ) -> Result<u64, TrySendError<Box<EngineSelectionRequest>>> {
        let (Some(commands), Some(sequence)) = (
            self.commands.as_ref(),
            self.engine_selection_sequence.as_ref(),
        ) else {
            return Err(TrySendError::Disconnected(Box::new(
                EngineSelectionRequest {
                    sequence: 0,
                    product,
                    interval,
                },
            )));
        };
        let next = sequence.load(Ordering::Acquire).saturating_add(1);
        let request = Box::new(EngineSelectionRequest {
            sequence: next,
            product,
            interval,
        });
        if let Some(pending) = &self.engine_selection {
            *pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(request);
            sequence.store(next, Ordering::Release);
            return Ok(next);
        }
        commands
            .try_send(MarketWorkerCommand::EngineSelect(request))
            .map_err(|error| {
                let (full, command) = match error {
                    TrySendError::Full(command) => (true, command),
                    TrySendError::Disconnected(command) => (false, command),
                };
                let MarketWorkerCommand::EngineSelect(request) = command else {
                    unreachable!("Rithmic selection send errors retain the selection command");
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
            .engine_selection_sequence
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

    /// Enables or disables depth demand for the currently selected market
    /// without changing symbol/timeframe generation.
    ///
    /// # Errors
    /// Returns the requested visibility when the bounded worker command lane is
    /// full or disconnected.
    pub fn try_set_order_book_visible(&self, visible: bool) -> Result<(), TrySendError<bool>> {
        if let Some(slot) = &self.depth_visible {
            if self.commands.is_none() {
                return Err(TrySendError::Disconnected(visible));
            }
            *slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(visible);
            return Ok(());
        }
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(visible));
        };
        commands
            .try_send(MarketWorkerCommand::DepthVisible(visible))
            .map_err(|error| match error {
                TrySendError::Full(MarketWorkerCommand::DepthVisible(visible)) => {
                    TrySendError::Full(visible)
                }
                TrySendError::Disconnected(MarketWorkerCommand::DepthVisible(visible)) => {
                    TrySendError::Disconnected(visible)
                }
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    unreachable!("depth visibility send errors retain the depth command")
                }
            })
    }

    /// Updates engine resource priority for a visible or hidden chart consumer.
    ///
    /// # Errors
    /// Returns the requested state when the bounded worker mailbox is full or disconnected.
    pub fn try_set_market_visibility(&self, visible: bool) -> Result<(), TrySendError<bool>> {
        self.try_set_market_resource_class(if visible {
            ConsumerResourceClass::Foreground
        } else {
            ConsumerResourceClass::Background
        })
        .map_err(|error| match error {
            TrySendError::Full(_) => TrySendError::Full(visible),
            TrySendError::Disconnected(_) => TrySendError::Disconnected(visible),
        })
    }

    /// Updates the exact engine resource class for this consumer.
    ///
    /// # Errors
    /// Returns the requested class when the worker boundary is disconnected. Engine-backed
    /// workers conflate rapid transitions into one bounded latest-value slot.
    pub fn try_set_market_resource_class(
        &self,
        resource_class: ConsumerResourceClass,
    ) -> Result<(), TrySendError<ConsumerResourceClass>> {
        if let Some(slot) = &self.resource_class {
            if self.commands.is_none() {
                return Err(TrySendError::Disconnected(resource_class));
            }
            if let Some(messages) = &self.messages {
                messages.set_market_publications_enabled(
                    resource_class == ConsumerResourceClass::Foreground,
                );
            }
            *slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(resource_class);
            return Ok(());
        }
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(resource_class));
        };
        commands
            .try_send(MarketWorkerCommand::ResourceClass(resource_class))
            .map_err(|error| match error {
                TrySendError::Full(MarketWorkerCommand::ResourceClass(resource_class)) => {
                    TrySendError::Full(resource_class)
                }
                TrySendError::Disconnected(MarketWorkerCommand::ResourceClass(resource_class)) => {
                    TrySendError::Disconnected(resource_class)
                }
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    unreachable!("visibility send errors retain the visibility command")
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
        let selection_generation = self
            .engine_selection_sequence
            .as_ref()
            .map(|generation| generation.load(Ordering::Acquire));
        commands
            .try_send(MarketWorkerCommand::Recovery {
                selection_generation,
                command,
            })
            .map_err(|error| match error {
                TrySendError::Full(MarketWorkerCommand::Recovery { command, .. }) => {
                    TrySendError::Full(command)
                }
                TrySendError::Disconnected(MarketWorkerCommand::Recovery { command, .. }) => {
                    TrySendError::Disconnected(command)
                }
                TrySendError::Full(
                    MarketWorkerCommand::Shutdown
                    | MarketWorkerCommand::ProviderSearch(_)
                    | MarketWorkerCommand::ProviderSelect(_)
                    | MarketWorkerCommand::EngineSelect(_)
                    | MarketWorkerCommand::ChartViewport(_)
                    | MarketWorkerCommand::DepthVisible(_)
                    | MarketWorkerCommand::ResourceClass(_)
                    | MarketWorkerCommand::ReplacePriceAlerts(_)
                    | MarketWorkerCommand::RegisterStudy(_)
                    | MarketWorkerCommand::ReinitializeStudy(_)
                    | MarketWorkerCommand::RemoveStudy(_),
                )
                | TrySendError::Disconnected(
                    MarketWorkerCommand::Shutdown
                    | MarketWorkerCommand::ProviderSearch(_)
                    | MarketWorkerCommand::ProviderSelect(_)
                    | MarketWorkerCommand::EngineSelect(_)
                    | MarketWorkerCommand::ChartViewport(_)
                    | MarketWorkerCommand::DepthVisible(_)
                    | MarketWorkerCommand::ResourceClass(_)
                    | MarketWorkerCommand::ReplacePriceAlerts(_)
                    | MarketWorkerCommand::RegisterStudy(_)
                    | MarketWorkerCommand::ReinitializeStudy(_)
                    | MarketWorkerCommand::RemoveStudy(_),
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
        if let Some(pending) = &self.provider_selection {
            *pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(selection);
            return Ok(());
        }
        commands
            .try_send(MarketWorkerCommand::ProviderSelect(selection))
            .map_err(|_| ProviderCommandUnavailable)
    }

    pub fn drain_messages(&mut self) -> (Vec<MarketWorkerMessage>, bool) {
        self.drain_messages_up_to(usize::MAX)
    }

    pub fn drain_messages_up_to(&mut self, limit: usize) -> (Vec<MarketWorkerMessage>, bool) {
        let Some(receiver) = self.messages.as_ref() else {
            let newly_disconnected = self.connected;
            self.connected = false;
            return (Vec::new(), newly_disconnected);
        };
        let (messages, disconnected) = receiver.drain_up_to(limit);
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
        // GPUI owns these handles. An implicit drop can therefore happen on the
        // UI thread, where waiting for a worker acknowledgement would freeze the
        // application for the shutdown timeout. Normal pane/window retirement
        // hands the acknowledgement to DesktopLifecycle, which waits for it on
        // the background executor. An unexpected drop still requests shutdown
        // and releases the receiver; it simply does not block its caller.
        let _ = self.begin_retirement();
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
        ChartState, ChartViewportUpdate, ConsumerResourceClass, FixtureMarketWorker,
        MarketDataWorker, MarketPublicationGeneration, MarketWorkerCommand, MarketWorkerMessage,
        MarketWorkerPublication, PendingUiDiagnostics, ProviderCatalogCommand,
        ProviderCatalogEvent, UiDiagnosticsFeedback, market_worker_channel, ui_diagnostics_channel,
    };
    use aeris_application::{
        Provenanced, ReplayStreamUpdate, ReplayTailOperation, ReplayTailUpdate,
    };
    use aeris_contracts::{
        InstallProviderInstrument, ProviderCatalogRejected, ProviderCatalogRejectionReason,
        SearchProviderInstruments, SelectProviderInstrument,
    };
    use aeris_market_data::OrderBookFrame;
    use aeris_market_data::{
        BarPeriod, BarSeriesKey, ChartInterval, OrderBookRecoveryReason, OrderBookState,
    };
    use aeris_market_runtime::study::StudyInstanceId;
    use aeris_market_runtime::{MarketConsumerId, MarketGenerationId, MarketTradeTapeSnapshot};
    use aeris_observability::{FeedDiagnostics, FeedIdentity};
    use aeris_study_sdk::builtins;
    use std::num::{NonZeroU64, NonZeroUsize};
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };

    fn tail_operation(message: &MarketWorkerMessage) -> Option<ReplayTailOperation> {
        match message {
            MarketWorkerMessage::Update(MarketWorkerPublication {
                update: ReplayStreamUpdate::Tail(tail),
                ..
            }) => Some(tail.operation()),
            _ => None,
        }
    }

    fn study_series() -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:ESU6".to_string(),
            entitlement_id: "rithmic-test:CME:ESU6".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        }
    }

    fn live_tail_sequence(message: &MarketWorkerMessage) -> Option<u64> {
        match message {
            MarketWorkerMessage::Update(MarketWorkerPublication {
                update: ReplayStreamUpdate::Tail(tail),
                ..
            }) => Some(tail.item().value().source_sequence),
            _ => None,
        }
    }

    #[test]
    fn chart_states_have_explicit_user_facing_labels() {
        assert_eq!(ChartState::Loading.label(), "Loading chart");
        assert_eq!(ChartState::Ready.label(), "Chart ready");
        assert_eq!(ChartState::Stale.label(), "Chart stale");
        assert_eq!(ChartState::Recovering.label(), "Reconnecting chart");
        assert_eq!(ChartState::Error.label(), "Chart unavailable");
    }

    #[test]
    fn dropping_worker_requests_shutdown_without_waiting_for_acknowledgement() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (_shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let (observed_tx, observed_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let worker_thread = thread::spawn(move || {
            assert!(matches!(
                command_rx.recv(),
                Ok(MarketWorkerCommand::Shutdown)
            ));
            observed_tx.send(()).expect("shutdown request is observed");
            release_rx.recv().expect("test releases acknowledgement");
        });
        let worker =
            MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None);
        let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
        let drop_thread = thread::spawn(move || {
            drop(worker);
            dropped_tx.send(()).expect("drop completion reports");
        });

        observed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("drop requests shutdown promptly");
        dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("drop does not wait for the worker acknowledgement");
        release_tx.send(()).expect("release worker acknowledgement");
        drop_thread.join().expect("drop caller exits");
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
    fn workspace_visibility_crosses_the_bounded_worker_boundary() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (_shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let mut worker =
            MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None);

        worker
            .try_set_market_visibility(false)
            .expect("visibility command is accepted");
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::ResourceClass(
                ConsumerResourceClass::Background
            ))
        ));

        drop(command_rx);
        let _ = worker.begin_retirement();
    }

    #[test]
    fn engine_resource_class_transitions_conflate_when_command_mailbox_is_full() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        command_tx
            .try_send(MarketWorkerCommand::ChartViewport(ChartViewportUpdate {
                start_unix_nanos: 1,
                end_unix_nanos: 2,
                selection_generation: 3,
            }))
            .expect("command mailbox is filled");
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (_shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let resource_class = Arc::new(Mutex::new(None));
        let mut worker =
            MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None)
                .with_resource_class_slot(Arc::clone(&resource_class));

        worker
            .try_set_market_resource_class(ConsumerResourceClass::Background)
            .expect("background transition is conflated");
        worker
            .try_set_market_resource_class(ConsumerResourceClass::Foreground)
            .expect("foreground transition replaces background");

        assert_eq!(
            resource_class
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
            Some(ConsumerResourceClass::Foreground)
        );
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::ChartViewport(_))
        ));
        drop(command_rx);
        let _ = worker.begin_retirement();
    }

    #[test]
    fn engine_background_transition_discards_stale_ui_market_publications() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (_shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let resource_class = Arc::new(Mutex::new(None));
        let mut worker =
            MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None)
                .with_resource_class_slot(Arc::clone(&resource_class));
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        let make_publication = |bootstrap: super::MarketWorkerBootstrap| MarketWorkerPublication {
            generation: MarketPublicationGeneration::from_generation(&bootstrap.generation),
            update: ReplayStreamUpdate::Snapshot(bootstrap.snapshot),
            subscription_id: bootstrap.subscription_id,
            worker_label: bootstrap.worker_label,
            ui_diagnostics: None,
        };
        let publication =
            make_publication(fixture.publish_snapshot(2).expect("snapshot publishes"));
        message_tx
            .send(MarketWorkerMessage::Update(publication))
            .expect("foreground publication queues");

        worker
            .try_set_market_resource_class(ConsumerResourceClass::Background)
            .expect("background transition is accepted");
        let (messages, _) = worker.drain_messages();
        assert!(messages.is_empty());

        let publication =
            make_publication(fixture.publish_snapshot(2).expect("snapshot republishes"));
        message_tx
            .send(MarketWorkerMessage::Update(publication))
            .expect("hidden publication is harmlessly suppressed");
        let (messages, _) = worker.drain_messages();
        assert!(messages.is_empty());

        worker
            .try_set_market_resource_class(ConsumerResourceClass::Foreground)
            .expect("foreground transition is accepted");
        let publication =
            make_publication(fixture.publish_snapshot(2).expect("snapshot republishes"));
        message_tx
            .send(MarketWorkerMessage::Update(publication))
            .expect("foreground publication queues");
        let (messages, _) = worker.drain_messages();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Update(_)]
        ));

        drop(command_rx);
        let _ = worker.begin_retirement();
    }

    #[test]
    fn idempotent_foreground_assignment_preserves_initial_covering_snapshot() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        let bootstrap = fixture.publish_snapshot(2).expect("snapshot publishes");
        sender
            .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                generation: MarketPublicationGeneration::from_generation(&bootstrap.generation),
                update: ReplayStreamUpdate::Snapshot(bootstrap.snapshot),
                subscription_id: bootstrap.subscription_id,
                worker_label: bootstrap.worker_label,
                ui_diagnostics: None,
            }))
            .expect("initial snapshot queues");

        receiver.set_market_publications_enabled(true);

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Update(MarketWorkerPublication {
                update: ReplayStreamUpdate::Snapshot(_),
                ..
            })]
        ));
    }

    #[test]
    fn control_states_preserve_fifo_order_in_the_bounded_mailbox() {
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
            [
                MarketWorkerMessage::State {
                    state: ChartState::Loading,
                    message: first,
                },
                MarketWorkerMessage::State {
                state: ChartState::Ready,
                    message: second,
                }
            ] if first == "loading" && second == "ready"
        ));
    }

    #[test]
    fn diagnostics_snapshots_coalesce_without_displacing_market_updates() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        let snapshot = |generation| {
            let mut diagnostics = FeedDiagnostics::new(
                FeedIdentity::try_new("rithmic", "advanced_trade_public", "production")
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
        assert_eq!(drained.len(), 2);
        sender.send(message()).expect("state after drain sends");
        assert_eq!(wake_count.load(Ordering::Acquire), 2);
    }

    #[test]
    fn bounded_drain_rearms_wake_while_messages_remain() {
        let wake_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wake_count);
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(8).expect("capacity is nonzero"));
        receiver.set_wake(Arc::new(move || {
            wake_counter.fetch_add(1, Ordering::AcqRel);
        }));
        for sequence in 1..=3 {
            sender
                .send(MarketWorkerMessage::EngineSwitchMarker { sequence })
                .expect("switch marker sends");
        }
        assert_eq!(wake_count.load(Ordering::Acquire), 1);

        let (first, disconnected) = receiver.drain_up_to(1);

        assert!(!disconnected);
        assert_eq!(first.len(), 1);
        assert_eq!(wake_count.load(Ordering::Acquire), 2);
        let (remaining, disconnected) = receiver.drain_up_to(8);
        assert!(!disconnected);
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn live_tail_updates_share_one_frame_wake_until_the_mailbox_is_drained() {
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
        for publication_generation in 2_u64..=9 {
            let mut bar = *source.value();
            bar.close = bar.close.saturating_add(
                i64::try_from(publication_generation).expect("test generation fits"),
            );
            bar.high = bar.high.max(bar.close);
            let tail = ReplayTailUpdate::try_new(
                Provenanced::new(bar, source.provenance().clone()),
                publication_generation,
                true,
                ReplayTailOperation::Revise,
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
        assert_eq!(messages.len(), 8);
        let generations = messages
            .iter()
            .filter_map(|message| match message {
                MarketWorkerMessage::Update(publication) => {
                    Some(publication.generation.publication_generation())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(generations, (2_u64..=9).collect::<Vec<_>>());
    }

    #[test]
    fn queued_append_is_delivered_before_its_same_candle_revision() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(8).expect("capacity is nonzero"));
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        let bootstrap = fixture.publish_snapshot(2).expect("snapshot publishes");
        let source = bootstrap
            .snapshot
            .bars()
            .last()
            .cloned()
            .expect("tail exists");
        let mut bar = *source.value();
        bar.source_sequence = bar.source_sequence.saturating_add(1);
        bar.exchange_timestamp_seconds = bar.exchange_timestamp_seconds.saturating_add(60);
        bar.exchange_timestamp_unix_nanos =
            bar.exchange_timestamp_seconds.saturating_mul(1_000_000_000);
        let mut provenance = source.provenance().clone();
        provenance.source_sequence = bar.source_sequence;
        provenance.exchange_timestamp_unix_nanos = bar.exchange_timestamp_unix_nanos;
        for (generation, close, operation) in [
            (3, bar.close, ReplayTailOperation::Append),
            (4, bar.close.saturating_add(1), ReplayTailOperation::Revise),
        ] {
            bar.close = close;
            bar.high = bar.high.max(close);
            let tail = ReplayTailUpdate::try_new(
                Provenanced::new(bar, provenance.clone()),
                generation,
                true,
                operation,
            )
            .expect("tail validates");
            sender
                .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Tail(tail),
                    generation: MarketPublicationGeneration::from_tail(
                        generation,
                        3,
                        1,
                        bar.source_sequence,
                    ),
                    subscription_id: "tail".to_string(),
                    worker_label: "tail".to_string(),
                    ui_diagnostics: None,
                }))
                .expect("tail sends");
        }

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        let operations = messages
            .iter()
            .filter_map(tail_operation)
            .collect::<Vec<_>>();
        assert_eq!(
            operations,
            [ReplayTailOperation::Append, ReplayTailOperation::Revise]
        );
    }

    /// Tails carrying different bars must all reach the UI.
    ///
    /// Conflation used to replace whichever tail happened to be queued, so a
    /// completed bar was dropped whenever a second one arrived in the same
    /// frame. The chart's replay bridge reads that gap as corruption and stalls
    /// on a covering snapshot it has to ask the engine for.
    #[test]
    fn live_tails_for_distinct_bars_are_all_delivered() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(8).expect("capacity is nonzero"));
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        let bootstrap = fixture.publish_snapshot(2).expect("snapshot publishes");
        let source = bootstrap
            .snapshot
            .bars()
            .last()
            .cloned()
            .expect("tail exists");
        let first_sequence = source.value().source_sequence;
        for offset in 1_u64..=3 {
            let mut bar = *source.value();
            bar.source_sequence = first_sequence.saturating_add(offset);
            bar.exchange_timestamp_seconds = bar
                .exchange_timestamp_seconds
                .saturating_add(60 * i64::try_from(offset).expect("offset fits"));
            bar.exchange_timestamp_unix_nanos =
                bar.exchange_timestamp_seconds.saturating_mul(1_000_000_000);
            let mut provenance = source.provenance().clone();
            provenance.source_sequence = bar.source_sequence;
            provenance.exchange_timestamp_unix_nanos = bar.exchange_timestamp_unix_nanos;
            let tail = ReplayTailUpdate::try_new(
                Provenanced::new(bar, provenance),
                offset.saturating_add(2),
                true,
                ReplayTailOperation::Append,
            )
            .expect("tail validates");
            sender
                .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Tail(tail),
                    generation: MarketPublicationGeneration::from_tail(
                        offset.saturating_add(2),
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

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        let sequences = messages
            .iter()
            .filter_map(live_tail_sequence)
            .collect::<Vec<_>>();
        assert_eq!(
            sequences,
            vec![first_sequence + 1, first_sequence + 2, first_sequence + 3]
        );
    }

    #[test]
    fn full_market_mailbox_returns_backpressure_without_mutating_queued_bars() {
        let capacity = NonZeroUsize::new(4).expect("capacity is nonzero");
        let (sender, receiver) = market_worker_channel(capacity);
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        let bootstrap = fixture.publish_snapshot(2).expect("snapshot publishes");
        let source = bootstrap
            .snapshot
            .bars()
            .last()
            .cloned()
            .expect("tail exists");
        let first_sequence = source.value().source_sequence;
        sender
            .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                update: ReplayStreamUpdate::Snapshot(bootstrap.snapshot.clone()),
                generation: MarketPublicationGeneration::from_generation(&bootstrap.generation),
                subscription_id: "burst".to_string(),
                worker_label: "burst".to_string(),
                ui_diagnostics: None,
            }))
            .expect("covering snapshot sends");

        for offset in 1_u64..=4 {
            let mut bar = *source.value();
            bar.source_sequence = first_sequence.saturating_add(offset);
            bar.exchange_timestamp_seconds = bar
                .exchange_timestamp_seconds
                .saturating_add(60 * i64::try_from(offset).expect("offset fits"));
            bar.exchange_timestamp_unix_nanos =
                bar.exchange_timestamp_seconds.saturating_mul(1_000_000_000);
            let mut provenance = source.provenance().clone();
            provenance.source_sequence = bar.source_sequence;
            provenance.exchange_timestamp_unix_nanos = bar.exchange_timestamp_unix_nanos;
            let tail = ReplayTailUpdate::try_new(
                Provenanced::new(bar, provenance),
                offset.saturating_add(2),
                true,
                ReplayTailOperation::Append,
            )
            .expect("tail validates");
            let result = sender.send(MarketWorkerMessage::Update(MarketWorkerPublication {
                update: ReplayStreamUpdate::Tail(tail),
                generation: MarketPublicationGeneration::from_tail(
                    offset.saturating_add(2),
                    2,
                    1,
                    2,
                ),
                subscription_id: "burst".to_string(),
                worker_label: "burst".to_string(),
                ui_diagnostics: None,
            }));
            if offset < 4 {
                assert!(result.is_ok());
            } else {
                assert_eq!(result, Err(super::MarketWorkerSendError::Full));
            }
        }

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        let sequences = messages
            .iter()
            .filter_map(live_tail_sequence)
            .collect::<Vec<_>>();
        assert_eq!(
            sequences,
            vec![first_sequence + 1, first_sequence + 2, first_sequence + 3]
        );
        assert!(matches!(
            messages.first(),
            Some(MarketWorkerMessage::Update(MarketWorkerPublication {
                update: ReplayStreamUpdate::Snapshot(_),
                ..
            }))
        ));
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
            FeedIdentity::try_new("rithmic", "advanced_trade_public", "production")
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
    }

    #[test]
    fn terminal_control_preserves_a_queued_covering_snapshot() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
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
                    ui_diagnostics: None,
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

        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Snapshot(_),
                    ..
                }),
                MarketWorkerMessage::State {
                    state: ChartState::Error,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn market_publication_waits_behind_a_full_control_queue() {
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
        assert_eq!(
            sender.send(MarketWorkerMessage::Update(publication)),
            Err(super::MarketWorkerSendError::Full)
        );

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::State {
                state: ChartState::Ready,
                message: ready,
            }] if ready == "ready"
        ));
    }

    #[test]
    fn queued_covering_snapshot_is_unchanged_when_next_update_is_backpressured() {
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
        assert_eq!(
            sender.send(MarketWorkerMessage::Update(publication)),
            Err(super::MarketWorkerSendError::Full)
        );

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Update(publication)]
                if matches!(publication.update, ReplayStreamUpdate::Snapshot(_))
        ));
    }

    #[test]
    fn control_messages_remain_fifo_while_market_publication_is_backpressured() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let bootstrap = worker.publish_snapshot(2).expect("snapshot publishes");
        sender
            .send(MarketWorkerMessage::EngineSwitchMarker { sequence: 7 })
            .expect("switch marker sends");
        assert_eq!(
            sender.send(MarketWorkerMessage::Update(MarketWorkerPublication {
                update: ReplayStreamUpdate::Snapshot(bootstrap.snapshot),
                generation: MarketPublicationGeneration::from_generation(&bootstrap.generation),
                subscription_id: bootstrap.subscription_id,
                worker_label: bootstrap.worker_label,
                ui_diagnostics: None,
            })),
            Err(super::MarketWorkerSendError::Full)
        );
        sender
            .send(MarketWorkerMessage::State {
                state: ChartState::Loading,
                message: "switching".to_string(),
            })
            .expect("state sends after the marker");

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::EngineSwitchMarker { sequence: 7 },
                MarketWorkerMessage::State {
                    state: ChartState::Loading,
                    message,
                }
            ] if message == "switching"
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
            [
                MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message: terminal,
                },
                MarketWorkerMessage::State {
                    state: ChartState::Ready,
                    message: obsolete,
                }
            ] if terminal == "terminal" && obsolete == "obsolete"
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
            [
                MarketWorkerMessage::Recovery { request_id: 7, .. },
                MarketWorkerMessage::State {
                    state: ChartState::Recovering,
                    message,
                }
            ] if message == "obsolete"
        ));
    }

    #[test]
    fn successful_recovery_and_newer_stream_update_preserve_fifo_order() {
        let (covered_sender, covered_receiver) =
            market_worker_channel(NonZeroUsize::new(2).expect("capacity is nonzero"));
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
                MarketWorkerMessage::Update(_)
            ]
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
                            reason: ProviderCatalogRejectionReason::SupersededSearch,
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
                            reason: ProviderCatalogRejectionReason::InstrumentUnavailable,
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
            [
                MarketWorkerMessage::ProviderCatalog(
                    ProviderCatalogEvent::CommandRejected {
                        rejection: ProviderCatalogRejected {
                            command_generation: first_generation,
                            ..
                        },
                        command: ProviderCatalogCommand::Search,
                    }
                ),
                MarketWorkerMessage::ProviderCatalog(
                    ProviderCatalogEvent::CommandRejected {
                        rejection: ProviderCatalogRejected {
                            command_generation,
                            reason,
                            ..
                        },
                        command: ProviderCatalogCommand::Selection,
                    }
                )
            ] if *first_generation == 1
                && *command_generation == latest_generation.get() as u64
                && *reason == ProviderCatalogRejectionReason::InstrumentUnavailable
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
                            reason: ProviderCatalogRejectionReason::SubscriptionRejected,
                        },
                        command: ProviderCatalogCommand::Selection,
                    },
                ))
                .is_ok()
        );
        let (messages, _) = receiver.drain();
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            messages[0],
            MarketWorkerMessage::State {
                state: ChartState::Error,
                ..
            }
        ));
        assert!(matches!(
            messages[1],
            MarketWorkerMessage::ProviderCatalog(_)
        ));
    }

    #[test]
    fn mailbox_does_not_filter_publication_generations() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(2).expect("capacity is nonzero"));
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        let snapshot = fixture
            .publish_snapshot(2)
            .expect("snapshot publishes")
            .snapshot;
        let publication = |generation, snapshot| {
            MarketWorkerMessage::Update(MarketWorkerPublication {
                update: ReplayStreamUpdate::Snapshot(snapshot),
                generation: MarketPublicationGeneration::from_tail(generation, 1, 1, 1),
                subscription_id: "fixture".to_string(),
                worker_label: "fixture".to_string(),
                ui_diagnostics: None,
            })
        };
        sender
            .send(publication(2, snapshot.clone()))
            .expect("newer publication queues");
        sender
            .send(publication(1, snapshot))
            .expect("older publication queues behind it");
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::Update(first),
                MarketWorkerMessage::Update(second),
            ] if first.generation.publication_generation() == 2
                && second.generation.publication_generation() == 1
        ));
    }

    #[test]
    fn order_book_mailbox_preserves_regressing_runtime_order() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(2).expect("capacity is nonzero"));
        let frame = |revision, state| OrderBookFrame {
            provider_id: "rithmic".to_string(),
            instrument_id: "rithmic:CME:MNQ".to_string(),
            entitlement_id: "test".to_string(),
            session_generation: 1,
            selection_generation: 2,
            revision,
            source_watermark: revision,
            bbo_source_watermark: revision,
            state,
            price_scale: 2,
            quantity_scale: 0,
            price_increment: Some(25),
            best_bid: None,
            best_ask: None,
            traded_volumes: std::collections::BTreeMap::default(),
            trade_source_watermark: revision,
            top_of_book_only: false,
            rows: Vec::new(),
        };
        assert!(
            sender
                .send(MarketWorkerMessage::OrderBook(frame(
                    3,
                    OrderBookState::Ready,
                )))
                .is_ok()
        );
        assert!(
            sender
                .send(MarketWorkerMessage::OrderBook(frame(
                    2,
                    OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap),
                )))
                .is_ok()
        );
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::OrderBook(first),
                MarketWorkerMessage::OrderBook(second),
            ] if first.revision == 3
                && first.state == OrderBookState::Ready
                && second.revision == 2
                && second.state == OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap)
        ));
    }

    #[test]
    fn order_book_mailbox_coalesces_newer_frames_for_the_same_selection() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(2).expect("capacity is nonzero"));
        let frame = |revision| OrderBookFrame {
            provider_id: "rithmic".to_string(),
            instrument_id: "rithmic:CME:MNQ".to_string(),
            entitlement_id: "test".to_string(),
            session_generation: 1,
            selection_generation: 2,
            revision,
            source_watermark: revision,
            bbo_source_watermark: revision,
            state: OrderBookState::Ready,
            price_scale: 2,
            quantity_scale: 0,
            price_increment: Some(25),
            best_bid: None,
            best_ask: None,
            traded_volumes: std::collections::BTreeMap::default(),
            trade_source_watermark: revision,
            top_of_book_only: false,
            rows: Vec::new(),
        };
        sender
            .send(MarketWorkerMessage::OrderBook(frame(2)))
            .expect("first frame queues");
        sender
            .send(MarketWorkerMessage::OrderBook(frame(3)))
            .expect("newer frame replaces it");

        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::OrderBook(frame)] if frame.revision == 3
        ));
    }

    #[test]
    fn trade_tape_mailbox_keeps_one_latest_image_per_runtime_generation() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(2).expect("capacity is nonzero"));
        let snapshot = |revision| MarketTradeTapeSnapshot {
            consumer_id: MarketConsumerId(NonZeroU64::MIN),
            generation: MarketGenerationId(NonZeroU64::MIN),
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQ".to_string(),
            entitlement_id: "test".to_string(),
            provider_generation: 4,
            revision,
            source_watermark: revision,
            rewrite_generation: 0,
            price_scale: 2,
            quantity_scale: 0,
            trades: Arc::from([]),
        };
        sender
            .send(MarketWorkerMessage::TradeTape(snapshot(2)))
            .expect("first tape queues");
        sender
            .send(MarketWorkerMessage::TradeTape(snapshot(3)))
            .expect("newer tape replaces it");

        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::TradeTape(snapshot)] if snapshot.revision == 3
        ));
    }

    #[test]
    fn order_book_mailbox_does_not_reinterpret_session_generations() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(3).expect("capacity is nonzero"));
        let frame = |session_generation, selection_generation, revision| OrderBookFrame {
            provider_id: "rithmic".to_string(),
            instrument_id: "rithmic:CME:MNQ".to_string(),
            entitlement_id: "test".to_string(),
            session_generation,
            selection_generation,
            revision,
            source_watermark: revision,
            bbo_source_watermark: revision,
            state: OrderBookState::Ready,
            price_scale: 2,
            quantity_scale: 0,
            price_increment: Some(25),
            best_bid: None,
            best_ask: None,
            traded_volumes: std::collections::BTreeMap::default(),
            trade_source_watermark: revision,
            top_of_book_only: false,
            rows: Vec::new(),
        };
        sender
            .send(MarketWorkerMessage::OrderBook(frame(7, 99, 40)))
            .expect("old session frame queues");
        sender
            .send(MarketWorkerMessage::OrderBook(frame(8, 1, 1)))
            .expect("new session frame queues");
        sender
            .send(MarketWorkerMessage::OrderBook(frame(7, 100, 99)))
            .expect("late old session frame is safely ignored");

        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::OrderBook(first),
                MarketWorkerMessage::OrderBook(second),
                MarketWorkerMessage::OrderBook(third),
            ] if first.session_generation == 7
                && first.revision == 40
                && second.session_generation == 8
                && second.revision == 1
                && third.session_generation == 7
                && third.revision == 99
        ));
    }

    #[test]
    fn rithmic_catalog_commands_use_the_bounded_worker_channel() {
        let (command_tx, command_rx) = mpsc::sync_channel(2);
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let mut worker =
            MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None);
        let search = SearchProviderInstruments {
            consumer_id: 0,
            search_generation: 1,
            provider: "rithmic".to_string(),
            query: "ES".to_string(),
            maximum_results: 16,
            categories: aeris_contracts::InstrumentSearchCategories::ALL,
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
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::ProviderSearch(_))
        ));
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::ProviderSelect(_))
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
        let retirement = worker
            .begin_retirement()
            .expect("active worker begins explicit retirement");
        assert!(retirement.wait());
        shutdown.join().expect("shutdown observer exits");
    }

    #[test]
    fn study_commands_preserve_registration_sequence_and_runtime_identity() {
        let (command_tx, command_rx) = mpsc::sync_channel(4);
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let mut worker =
            MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None);
        let registration = builtins::sma(
            study_series(),
            NonZeroUsize::new(20).unwrap_or(NonZeroUsize::MIN),
        )
        .expect("SMA registration is valid");

        assert_eq!(
            worker
                .try_register_study(registration.clone())
                .expect("study registration enters the bounded command lane"),
            1
        );
        let study_id = StudyInstanceId::try_from_u64(7).expect("study identity");
        worker
            .try_reinitialize_study(study_id, registration)
            .expect("study reinitialization enters the bounded command lane");
        worker
            .try_remove_study(study_id)
            .expect("study removal enters the bounded command lane");

        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::RegisterStudy(request)) if request.sequence == 1
        ));
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::ReinitializeStudy(request)) if request.study_id == study_id
        ));
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::RemoveStudy(received)) if received == study_id
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
        assert!(worker.shutdown_and_wait());
        shutdown.join().expect("shutdown observer exits");
    }

    #[test]
    fn foreground_symbol_switch_survives_a_full_command_mailbox() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (_shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let provider_selection = Arc::new(Mutex::new(None));
        let engine_selection = Arc::new(Mutex::new(None));
        let sequence = Arc::new(AtomicU64::new(7));
        let worker = MarketDataWorker::from_channels(
            command_tx,
            message_rx,
            shutdown_rx,
            None,
            Some(Arc::clone(&sequence)),
        )
        .with_foreground_selection_slots(
            Arc::clone(&provider_selection),
            Arc::clone(&engine_selection),
        );

        worker
            .try_set_chart_viewport(1, 2)
            .expect("viewport fills the ordinary command mailbox");
        let selection = SelectProviderInstrument {
            consumer_id: 0,
            selection_generation: 3,
            search_generation: 2,
            provider: "hyperliquid".to_string(),
            symbol: "ETH".to_string(),
            exchange: "Hyperliquid".to_string(),
            entitlement_id: "hyperliquid-public".to_string(),
        };
        worker
            .try_select_provider(selection.clone())
            .expect("provider selection uses the foreground slot");
        let replacement = SelectProviderInstrument {
            selection_generation: 4,
            symbol: "SOL".to_string(),
            ..selection.clone()
        };
        worker
            .try_select_provider(replacement.clone())
            .expect("newer provider selection replaces the pending foreground selection");
        let next = worker
            .try_select_engine(
                InstallProviderInstrument {
                    provider: "hyperliquid".to_string(),
                    provider_symbol: "ETH".to_string(),
                    display_symbol: "ETH".to_string(),
                    ..InstallProviderInstrument::default()
                },
                ChartInterval::Minute1,
            )
            .expect("engine selection uses the foreground slot");

        assert_eq!(next, 8);
        assert_eq!(sequence.load(Ordering::Acquire), 8);
        assert_eq!(
            provider_selection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref(),
            Some(&replacement)
        );
        let pending_engine = engine_selection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("engine selection retained");
        assert_eq!(pending_engine.sequence, 8);
        assert_eq!(pending_engine.product.provider_symbol, "ETH");
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::ChartViewport(_))
        ));
    }
}
