//! Desktop market presentation mailbox.
//!
//! This module does not implement a socket, live provider or production transport.

use aeris_application::ReplayRecoveryCommand;
use aeris_application::{ReplaySnapshot, ReplayStreamUpdate};
use aeris_contracts::{
    InstallProviderInstrument, ScreenProviderMarkets, SearchProviderInstruments,
    SelectProviderInstrument,
};
use aeris_market_data::ChartInterval;
use aeris_market_runtime::study::{NativeStudyRegistration, StudyInstanceId};
use aeris_market_runtime::{
    MarketConsumerResourceClass as ConsumerResourceClass, MarketPriceAlert, MarketRuntimeEvent,
};
use aeris_observability::FeedConnectionState;
use std::{
    collections::VecDeque,
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

mod pane_projection_queue;
use pane_projection_queue::{
    PaneProjectionReceiver, PaneProjectionSender, pane_projection_channel,
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

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(test)]
const CONFIRMED_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);

pub struct MarketWorkerBootstrap {
    pub snapshot: ReplaySnapshot,
    pub subscription_id: String,
    pub generation: MarketPublicationGeneration,
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

/// Whether a provider-verified selection carries current terms for the same
/// instrument. A provisional descriptor yields to it; a different
/// instrument's selection never replaces the charted one.
#[must_use]
pub fn verified_terms_supersede(
    current: &InstallProviderInstrument,
    verified: &InstallProviderInstrument,
) -> bool {
    current.provider == verified.provider
        && current.instrument_id == verified.instrument_id
        && verified.selection_generation >= current.selection_generation
        && verified.session_generation >= current.session_generation
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
    selection_generation: u64,
    publication_generation: u64,
    retained_items: usize,
    first_sequence: u64,
    last_sequence: u64,
}

impl MarketPublicationGeneration {
    #[must_use]
    pub fn from_snapshot(snapshot: &ReplaySnapshot) -> Self {
        let (first_sequence, last_sequence) = snapshot.sequence_range();
        Self::from_tail(
            snapshot.evidence().publication_generation,
            snapshot.bars().len(),
            first_sequence,
            last_sequence,
        )
    }

    #[must_use]
    pub const fn from_tail(
        publication_generation: u64,
        retained_items: usize,
        first_sequence: u64,
        last_sequence: u64,
    ) -> Self {
        Self {
            selection_generation: 1,
            publication_generation,
            retained_items,
            first_sequence,
            last_sequence,
        }
    }

    #[must_use]
    pub const fn with_selection_generation(mut self, selection_generation: u64) -> Self {
        self.selection_generation = selection_generation;
        self
    }

    #[must_use]
    pub const fn selection_generation(self) -> u64 {
        self.selection_generation
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

/// Why a chart recovery request ended without a covering snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketRecoveryFailure {
    /// A newer recovery request or market selection replaced this one. Nothing failed: the
    /// replacement's covering snapshot owns the chart, so the request is simply retired.
    Superseded,
    Failed(String),
}

pub enum MarketWorkerMessage {
    /// Coordinator events retain their consumer fence through the bounded
    /// queue; the pane projects each event at its owning presentation boundary.
    Runtime(aeris_market_runtime::MarketConsumerEvent),
    /// Coordinator-fenced bars; the pane converts them only after selection acceptance.
    RuntimeSeriesPublication {
        event: aeris_market_runtime::MarketConsumerEvent,
        received_unix_nanos: i64,
    },
    /// Coordinator provider state, retained with its consumer epoch and the
    /// selection that was active when the endpoint accepted it.
    RuntimeProviderState {
        event: aeris_market_runtime::MarketConsumerEvent,
        generation: u64,
    },
    /// The covering snapshot retains the coordinator consumer epoch until the
    /// pane accepts the request and projects it for the chart.
    RuntimeRecovery {
        event: aeris_market_runtime::MarketConsumerEvent,
        request_id: u64,
        received_unix_nanos: i64,
    },
    Recovery {
        generation: u64,
        request_id: u64,
        failure: MarketRecoveryFailure,
    },
    /// A catalog or screen endpoint has no chart selection generation.
    CatalogFailure {
        message: String,
    },
    /// Local endpoint or command failure, not a coordinator series transition.
    EndpointFailure {
        generation: u64,
        message: String,
    },
    Connection {
        generation: u64,
        state: FeedConnectionState,
        message: String,
        transport_rtt_nanos: Option<u64>,
    },
    MarketSessionStatus {
        generation: u64,
        status: aeris_contracts::MarketSessionStatus,
    },
    PriceAlertSync {
        sequence: u64,
        succeeded: bool,
    },
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
    /// A local select-and-demand acknowledgement, fenced by the exact pending sequence.
    EngineSelectionOutcome {
        sequence: u64,
        succeeded: bool,
    },
    SelectedInstrument {
        generation: u64,
        instrument: InstallProviderInstrument,
    },
    /// Catalog selection reply, fenced by both pane request and browser generation.
    CatalogSwitchOutcome {
        sequence: u64,
        browser_generation: u64,
        result: Result<(InstallProviderInstrument, ChartInterval), String>,
    },
}

pub type MarketWorkerSender = PaneProjectionSender<MarketWorkerMessage>;
pub type MarketWorkerReceiver = PaneProjectionReceiver<MarketWorkerMessage>;

fn is_market_publication(message: &MarketWorkerMessage) -> bool {
    matches!(
        message,
        MarketWorkerMessage::RuntimeSeriesPublication { .. }
            | MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                value: MarketRuntimeEvent::OrderBookSnapshot(_)
                    | MarketRuntimeEvent::StudyOutputSnapshot(_)
                    | MarketRuntimeEvent::TradeTapeSnapshot(_),
                ..
            })
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

#[must_use]
pub fn market_worker_channel(capacity: NonZeroUsize) -> (MarketWorkerSender, MarketWorkerReceiver) {
    pane_projection_channel(capacity, is_market_publication)
}

pub enum MarketWorkerCommand {
    Recovery {
        selection_generation: Option<u64>,
        command: ReplayRecoveryCommand,
    },
    ProviderSearch(SearchProviderInstruments),
    ProviderSelect(SelectProviderInstrument),
    ProviderScreen(ScreenProviderMarkets),
    EngineSelect(Box<EngineSelectionRequest>),
    SelectCatalog(Box<CatalogSelectionRequest>),
    ChartViewport(ChartViewportUpdate),
    DepthVisible(bool),
    ResourceClass(ConsumerResourceClass),
    ReplacePriceAlerts {
        sequence: u64,
        alerts: Vec<MarketPriceAlert>,
    },
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
#[derive(Clone, Debug)]
pub struct CatalogSelectionRequest {
    pub sequence: u64,
    pub browser_generation: u64,
    pub search_generation: u64,
    pub provider: String,
    pub symbol: String,
    pub exchange: String,
    pub entitlement_id: String,
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

struct WorkerCommandSender {
    sender: SyncSender<MarketWorkerCommand>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl WorkerCommandSender {
    fn notify(&self) {
        if let Some(wake) = &self.wake {
            wake();
        }
    }
}

impl std::ops::Deref for WorkerCommandSender {
    type Target = SyncSender<MarketWorkerCommand>;

    fn deref(&self) -> &Self::Target {
        &self.sender
    }
}

pub struct MarketDataWorker {
    commands: Option<WorkerCommandSender>,
    messages: Option<MarketWorkerReceiver>,
    shutdown_complete: Option<Receiver<()>>,
    connected: bool,
    ui_diagnostics: Option<UiDiagnosticsSender>,
    engine_selection_sequence: Option<Arc<AtomicU64>>,
    study_request_sequence: AtomicU64,
    price_alert_sequence: AtomicU64,
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
            commands: Some(WorkerCommandSender {
                sender: commands,
                wake: None,
            }),
            messages: Some(messages),
            shutdown_complete: Some(shutdown_complete),
            connected: true,
            ui_diagnostics,
            engine_selection_sequence,
            study_request_sequence: AtomicU64::new(0),
            price_alert_sequence: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub fn with_command_wake(mut self, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        if let Some(commands) = &mut self.commands {
            commands.wake = Some(wake);
        }
        self
    }

    fn notify_command(&self) {
        if let Some(commands) = &self.commands {
            commands.notify();
        }
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
        let Some(sequence) = self
            .price_alert_sequence
            .load(Ordering::Acquire)
            .checked_add(1)
        else {
            return Err(TrySendError::Full(alerts));
        };
        commands
            .try_send(MarketWorkerCommand::ReplacePriceAlerts { sequence, alerts })
            .inspect(|()| commands.notify())
            .map_err(|error| match error {
                TrySendError::Full(MarketWorkerCommand::ReplacePriceAlerts { alerts, .. }) => {
                    TrySendError::Full(alerts)
                }
                TrySendError::Disconnected(MarketWorkerCommand::ReplacePriceAlerts {
                    alerts,
                    ..
                }) => TrySendError::Disconnected(alerts),
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    unreachable!("alert send errors retain the alert snapshot")
                }
            })?;
        self.price_alert_sequence.store(sequence, Ordering::Release);
        Ok(())
    }

    #[must_use]
    pub fn latest_price_alert_sequence(&self) -> u64 {
        self.price_alert_sequence.load(Ordering::Acquire)
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
            .inspect(|()| commands.notify())
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
            .inspect(|()| commands.notify())
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
            .inspect(|()| commands.notify())
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

    /// Requests one runtime-owned instrument selection and series demand without blocking.
    ///
    /// # Errors
    /// Returns the request when the command mailbox is full or disconnected.
    pub fn try_select_instrument(
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
        commands
            .try_send(MarketWorkerCommand::EngineSelect(request))
            .inspect(|()| commands.notify())
            .map_err(|error| {
                let (full, command) = match error {
                    TrySendError::Full(command) => (true, command),
                    TrySendError::Disconnected(command) => (false, command),
                };
                let MarketWorkerCommand::EngineSelect(request) = command else {
                    unreachable!("instrument selection send errors retain the selection command");
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
    /// Sends a listed symbol directly to runtime-owned catalog resolution and
    /// demand installation. No desktop-side provider selection is required.
    ///
    /// # Errors
    /// Reports a full or disconnected bounded command queue.
    pub fn try_select_catalog(
        &self,
        mut request: CatalogSelectionRequest,
    ) -> Result<u64, ProviderCommandUnavailable> {
        let (Some(commands), Some(sequence)) = (
            self.commands.as_ref(),
            self.engine_selection_sequence.as_ref(),
        ) else {
            return Err(ProviderCommandUnavailable);
        };
        let next = sequence.load(Ordering::Acquire).saturating_add(1);
        request.sequence = next;
        commands
            .try_send(MarketWorkerCommand::SelectCatalog(Box::new(request)))
            .map_err(|_| ProviderCommandUnavailable)?;
        sequence.store(next, Ordering::Release);
        commands.notify();
        Ok(next)
    }

    pub fn send_ui_diagnostics(&self, feedback: UiDiagnosticsFeedback) {
        if let Some(sender) = &self.ui_diagnostics
            && sender.send(feedback).is_err()
        {
            // The diagnostics collector was retired with this worker. Frame
            // feedback is optional and never blocks market presentation.
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
            .inspect(|()| commands.notify())
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
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(visible));
        };
        commands
            .try_send(MarketWorkerCommand::DepthVisible(visible))
            .inspect(|()| commands.notify())
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
    /// Returns the requested class when the ordered mailbox is full or disconnected.
    /// Only adjacent queued class changes coalesce on the bridge thread.
    pub fn try_set_market_resource_class(
        &self,
        resource_class: ConsumerResourceClass,
    ) -> Result<(), TrySendError<ConsumerResourceClass>> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(resource_class));
        };
        commands
            .try_send(MarketWorkerCommand::ResourceClass(resource_class))
            .inspect(|()| {
                if let Some(messages) = &self.messages {
                    messages.set_market_publications_enabled(
                        resource_class == ConsumerResourceClass::Foreground,
                    );
                }
                commands.notify();
            })
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
            .inspect(|()| commands.notify())
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
                    | MarketWorkerCommand::ProviderScreen(_)
                    | MarketWorkerCommand::EngineSelect(_)
                    | MarketWorkerCommand::SelectCatalog(_)
                    | MarketWorkerCommand::ChartViewport(_)
                    | MarketWorkerCommand::DepthVisible(_)
                    | MarketWorkerCommand::ResourceClass(_)
                    | MarketWorkerCommand::ReplacePriceAlerts { .. }
                    | MarketWorkerCommand::RegisterStudy(_)
                    | MarketWorkerCommand::ReinitializeStudy(_)
                    | MarketWorkerCommand::RemoveStudy(_),
                )
                | TrySendError::Disconnected(
                    MarketWorkerCommand::Shutdown
                    | MarketWorkerCommand::ProviderSearch(_)
                    | MarketWorkerCommand::ProviderSelect(_)
                    | MarketWorkerCommand::ProviderScreen(_)
                    | MarketWorkerCommand::EngineSelect(_)
                    | MarketWorkerCommand::SelectCatalog(_)
                    | MarketWorkerCommand::ChartViewport(_)
                    | MarketWorkerCommand::DepthVisible(_)
                    | MarketWorkerCommand::ResourceClass(_)
                    | MarketWorkerCommand::ReplacePriceAlerts { .. }
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
            .inspect(|()| commands.notify())
            .map_err(|_| ProviderCommandUnavailable)
    }

    /// Enqueues one listed-market statistics screen without blocking.
    ///
    /// # Errors
    /// Returns an error when the command mailbox is full or disconnected.
    pub fn try_screen_provider(
        &self,
        screen: ScreenProviderMarkets,
    ) -> Result<(), ProviderCommandUnavailable> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(ProviderCommandUnavailable);
        };
        commands
            .try_send(MarketWorkerCommand::ProviderScreen(screen))
            .inspect(|()| commands.notify())
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
            .inspect(|()| commands.notify())
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
        if !messages.is_empty() {
            self.notify_command();
        }
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
            match commands.try_send(MarketWorkerCommand::Shutdown) {
                Ok(()) | Err(TrySendError::Full(_)) => {
                    // A full queue drains before sender closure ends the worker;
                    // wake it even when Shutdown itself did not fit.
                    commands.notify();
                }
                Err(TrySendError::Disconnected(_)) => {
                    // The background consumer has already retired.
                }
            }
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
        drop(self.begin_retirement());
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

#[cfg(test)]
mod tests {
    use super::{
        ChartViewportUpdate, ConsumerResourceClass, MarketDataWorker, MarketPublicationGeneration,
        MarketRecoveryFailure, MarketWorkerCommand, MarketWorkerMessage, PendingUiDiagnostics,
        UiDiagnosticsFeedback, market_worker_channel, ui_diagnostics_channel,
    };
    use crate::test_support::{FixtureMarketEvents, FixtureMarketWorker, FixtureSeries};
    use aeris_contracts::{
        InstallProviderInstrument, ProviderCatalogRejected, ProviderCatalogRejectionReason,
        SearchProviderInstruments, SelectProviderInstrument,
    };
    use aeris_market_data::{
        BarPeriod, BarSeriesKey, ChartInterval, OrderBookRecoveryReason, OrderBookState,
    };
    use aeris_market_data::{OrderBookFrame, OrderBookPublication};
    use aeris_market_runtime::study::StudyInstanceId;
    use aeris_market_runtime::{
        MarketConsumerId, MarketGenerationId, MarketRuntimeEvent, MarketTradeTapeSnapshot,
        SeriesTailOperation,
    };

    use aeris_study_sdk::builtins;
    use std::collections::VecDeque;
    use std::num::{NonZeroU64, NonZeroUsize};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU64, AtomicUsize, Ordering},
            mpsc::{self, TrySendError},
        },
        thread,
        time::Duration,
    };

    fn catalog_rejection(
        command_generation: u64,
        reason: ProviderCatalogRejectionReason,
        selection: bool,
    ) -> MarketWorkerMessage {
        MarketWorkerMessage::Runtime(FixtureMarketEvents::default().catalog(
            MarketRuntimeEvent::ProviderCatalogRejected {
                rejection: ProviderCatalogRejected {
                    consumer_id: 1,
                    provider: "rithmic".to_string(),
                    provider_generation: Some(1),
                    command_generation,
                    reason,
                },
                selection,
            },
        ))
    }

    fn tail_operation(message: &MarketWorkerMessage) -> Option<SeriesTailOperation> {
        match message {
            MarketWorkerMessage::RuntimeSeriesPublication {
                event:
                    aeris_market_runtime::fence::Fenced {
                        value: MarketRuntimeEvent::SeriesUpdate(update),
                        ..
                    },
                ..
            } => Some(update.operation),
            _ => None,
        }
    }

    fn book_message(frame: OrderBookFrame) -> MarketWorkerMessage {
        let event = FixtureMarketEvents::default().order_book(
            frame.selection_generation,
            OrderBookPublication {
                provider_id: frame.provider_id,
                instrument_id: frame.instrument_id,
                entitlement_id: frame.entitlement_id,
                session_generation: frame.session_generation,
                revision: frame.revision,
                source_watermark: frame.source_watermark,
                state: frame.state,
                bids: vec![],
                asks: vec![],
                best_bid: None,
                best_ask: None,
                bbo_source_watermark: frame.bbo_source_watermark,
                traded_volumes: frame.traded_volumes,
                trade_source_watermark: frame.trade_source_watermark,
                top_of_book_only: frame.top_of_book_only,
            },
        );
        MarketWorkerMessage::Runtime(event)
    }

    fn book_snapshot(
        message: &MarketWorkerMessage,
    ) -> Option<&aeris_market_runtime::MarketOrderBookSnapshot> {
        let MarketWorkerMessage::Runtime(event) = message else {
            return None;
        };
        let MarketRuntimeEvent::OrderBookSnapshot(snapshot) = &event.value else {
            return None;
        };
        Some(snapshot)
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
            MarketWorkerMessage::RuntimeSeriesPublication {
                event:
                    aeris_market_runtime::fence::Fenced {
                        value: MarketRuntimeEvent::SeriesUpdate(update),
                        ..
                    },
                ..
            } => Some(update.bar.source_sequence),
            _ => None,
        }
    }

    fn fixture_series() -> FixtureSeries {
        FixtureSeries {
            series: BarSeriesKey {
                provider_id: "hyperliquid".into(),
                instrument_id: "hyperliquid:perp:BTC".into(),
                entitlement_id: "fixture".into(),
                period: BarPeriod::Time { seconds: 60 },
                definition_version: 1,
            },
            provider_generation: 1,
            price_scale: 2,
            quantity_scale: 3,
        }
    }

    fn snapshot_message(fixture: &mut FixtureMarketWorker, count: usize) -> MarketWorkerMessage {
        MarketWorkerMessage::RuntimeSeriesPublication {
            event: fixture
                .publish_runtime_snapshot(
                    count,
                    &fixture_series(),
                    1,
                    &FixtureMarketEvents::default(),
                )
                .expect("fixture snapshot publishes"),
            received_unix_nanos: 0,
        }
    }

    fn update_message(
        generation: u64,
        bar: aeris_market_data::MarketBar,
        operation: SeriesTailOperation,
    ) -> MarketWorkerMessage {
        MarketWorkerMessage::RuntimeSeriesPublication {
            event: FixtureMarketEvents::default().update(
                &fixture_series(),
                1,
                generation,
                operation,
                bar,
                true,
            ),
            received_unix_nanos: 0,
        }
    }

    fn status_message(
        state: aeris_contracts::SeriesLoadState,
        detail: &str,
    ) -> MarketWorkerMessage {
        MarketWorkerMessage::Runtime(FixtureMarketEvents::default().readiness(1, state, detail))
    }

    fn snapshot_generation(message: &MarketWorkerMessage) -> Option<u64> {
        match message {
            MarketWorkerMessage::RuntimeSeriesPublication {
                event:
                    aeris_market_runtime::fence::Fenced {
                        value: MarketRuntimeEvent::SeriesSnapshot(snapshot),
                        ..
                    },
                ..
            } => Some(snapshot.publication_generation),
            _ => None,
        }
    }

    #[test]
    fn recovery_projection_uses_snapshot_evidence_without_retaining_a_second_bar_set() {
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        let bootstrap = fixture.publish_snapshot(3).expect("fixture snapshot");
        let evidence = MarketPublicationGeneration::from_snapshot(&bootstrap.snapshot);
        assert_eq!(bootstrap.generation, evidence);
        assert_eq!(evidence.retained_items(), bootstrap.snapshot.bars().len());
        assert_eq!(
            evidence.sequence_range(),
            bootstrap.snapshot.sequence_range()
        );
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
    fn full_retirement_queue_wakes_and_closes_after_draining() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let wakes = Arc::new(AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wakes);
        let mut worker =
            MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None)
                .with_command_wake(Arc::new(move || {
                    wake_counter.fetch_add(1, Ordering::Relaxed);
                }));
        worker
            .try_set_market_resource_class(ConsumerResourceClass::Background)
            .expect("fill the bounded worker queue");
        let worker_thread = thread::spawn(move || {
            release_rx.recv().expect("allow retirement to drain");
            assert!(matches!(
                command_rx.recv(),
                Ok(MarketWorkerCommand::ResourceClass(
                    ConsumerResourceClass::Background
                ))
            ));
            assert!(command_rx.recv().is_err(), "sender closure ends the worker");
            shutdown_tx.send(()).expect("acknowledge retirement");
        });
        let retirement = worker.begin_retirement().expect("retirement receipt");
        assert_eq!(wakes.load(Ordering::Relaxed), 2, "full retirement wakes");
        release_tx.send(()).expect("release worker");
        assert!(retirement.wait());
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
        drop(worker.begin_retirement());
    }

    #[test]
    fn engine_resource_class_reports_ordered_mailbox_overflow() {
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
        let mut worker =
            MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None);
        assert!(matches!(
            worker.try_set_market_resource_class(ConsumerResourceClass::Background),
            Err(TrySendError::Full(ConsumerResourceClass::Background))
        ));
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::ChartViewport(_))
        ));
        worker
            .try_set_market_resource_class(ConsumerResourceClass::Foreground)
            .expect("resource class queues once there is capacity");
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::ResourceClass(
                ConsumerResourceClass::Foreground
            ))
        ));
        drop(command_rx);
        drop(worker.begin_retirement());
    }

    #[test]
    fn engine_background_transition_discards_stale_ui_market_publications() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (_shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let mut worker =
            MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None, None);
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        message_tx
            .send(snapshot_message(&mut fixture, 2))
            .expect("foreground publication queues");

        worker
            .try_set_market_resource_class(ConsumerResourceClass::Background)
            .expect("background transition is accepted");
        assert!(matches!(
            command_rx.try_recv(),
            Ok(MarketWorkerCommand::ResourceClass(
                ConsumerResourceClass::Background
            ))
        ));
        let (messages, _) = worker.drain_messages();
        assert!(messages.is_empty());

        message_tx
            .send(snapshot_message(&mut fixture, 2))
            .expect("hidden publication is harmlessly suppressed");
        let (messages, _) = worker.drain_messages();
        assert!(messages.is_empty());

        worker
            .try_set_market_resource_class(ConsumerResourceClass::Foreground)
            .expect("foreground transition is accepted");
        message_tx
            .send(snapshot_message(&mut fixture, 2))
            .expect("foreground publication queues");
        let (messages, _) = worker.drain_messages();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RuntimeSeriesPublication { .. }]
        ));

        drop(command_rx);
        drop(worker.begin_retirement());
    }

    #[test]
    fn idempotent_foreground_assignment_preserves_initial_covering_snapshot() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        sender
            .send(snapshot_message(&mut fixture, 2))
            .expect("initial snapshot queues");

        receiver.set_market_publications_enabled(true);

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RuntimeSeriesPublication {
                event: aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::SeriesSnapshot(_),
                    ..
                },
                ..
            }]
        ));
    }

    #[test]
    fn control_states_preserve_fifo_order_in_the_bounded_mailbox() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        assert!(
            sender
                .send(status_message(
                    aeris_contracts::SeriesLoadState::Resolving,
                    "loading"
                ))
                .is_ok()
        );
        assert!(
            sender
                .send(status_message(
                    aeris_contracts::SeriesLoadState::Ready,
                    "ready"
                ))
                .is_ok()
        );

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::SeriesState(first), ..
                }),
                MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::SeriesState(second), ..
                })
            ] if first.state == aeris_contracts::SeriesLoadState::Resolving
                && first.detail.as_deref() == Some("loading")
                && second.state == aeris_contracts::SeriesLoadState::Ready
                && second.detail.as_deref() == Some("ready")
        ));
    }

    #[test]
    fn bounded_bridge_drains_ten_thousand_outbox_events_in_emission_order() {
        const FRAME_BUDGET: usize = 64;
        const OUTBOX_EVENTS: u64 = 10_000;
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(256).expect("capacity is nonzero"));
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        let snapshot = snapshot_message(&mut fixture, 2);
        let mut outbox = (1..=OUTBOX_EVENTS).collect::<VecDeque<_>>();
        let mut next_applied = 1;
        let mut overflow_count = 0;
        while next_applied <= OUTBOX_EVENTS {
            while let Some(&generation) = outbox.front() {
                let MarketWorkerMessage::RuntimeSeriesPublication { event, .. } = &snapshot else {
                    panic!("fixture snapshot is canonical");
                };
                let MarketRuntimeEvent::SeriesSnapshot(publication) = &event.value else {
                    panic!("fixture snapshot has series data");
                };
                let result = sender.send(MarketWorkerMessage::RuntimeSeriesPublication {
                    event: FixtureMarketEvents::default().snapshot(
                        &fixture_series(),
                        1,
                        generation,
                        publication.snapshot.bars.clone(),
                    ),
                    received_unix_nanos: 0,
                });
                if result == Err(super::MarketWorkerSendError::Full) {
                    overflow_count += 1;
                    break;
                }
                result.expect("receiver is alive");
                outbox.pop_front();
            }
            let (messages, disconnected) = receiver.drain_up_to(FRAME_BUDGET);
            assert!(!disconnected);
            assert!(!messages.is_empty());
            assert!(messages.len() <= FRAME_BUDGET);
            for message in messages {
                let Some(generation) = snapshot_generation(&message) else {
                    panic!("unexpected outbox projection");
                };
                assert_eq!(generation, next_applied);
                next_applied += 1;
            }
        }
        assert_eq!(next_applied, OUTBOX_EVENTS + 1);
        assert!(overflow_count > 0, "bounded queue must report backpressure");
    }

    #[test]
    fn fenced_runtime_events_share_publication_capacity_without_suppressing_controls() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let tape = || {
            MarketWorkerMessage::Runtime(FixtureMarketEvents::default().trade_tape(
                MarketTradeTapeSnapshot {
                    consumer_id: MarketConsumerId(NonZeroU64::MIN),
                    generation: MarketGenerationId(NonZeroU64::MIN),
                    provider_id: "fixture".into(),
                    instrument_id: "fixture:BTC".into(),
                    entitlement_id: "fixture".into(),
                    provider_generation: 1,
                    revision: 1,
                    source_watermark: 1,
                    rewrite_generation: 0,
                    price_scale: 2,
                    quantity_scale: 3,
                    trades: Arc::from([]),
                },
            ))
        };
        assert!(sender.send(tape()).is_ok());
        assert_eq!(sender.send(tape()), Err(super::MarketWorkerSendError::Full));
        sender
            .send(status_message(
                aeris_contracts::SeriesLoadState::Ready,
                "ready",
            ))
            .expect("fenced readiness uses the control reserve");
        sender
            .send(catalog_rejection(
                1,
                ProviderCatalogRejectionReason::SubscriptionRejected,
                false,
            ))
            .expect("fenced catalog uses the control reserve");
        let (first, _) = receiver.drain_up_to(1);
        assert!(matches!(
            first.as_slice(),
            [MarketWorkerMessage::Runtime(
                aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::TradeTapeSnapshot(_),
                    ..
                }
            )]
        ));
        let (remaining, _) = receiver.drain();
        assert!(matches!(
            remaining.as_slice(),
            [
                MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::SeriesState(_),
                    ..
                }),
                MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::ProviderCatalogRejected { .. },
                    ..
                })
            ]
        ));
        sender
            .send(tape())
            .expect("next tape enters freed capacity");
        receiver.set_market_publications_enabled(false);
        sender
            .send(tape())
            .expect("background tape suppression is explicit");
        assert!(receiver.drain().0.is_empty());
        assert_eq!(sender.occupancy().0, 0);
    }

    #[test]
    fn app_scoped_failures_preserve_fifo_order_and_report_backpressure() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(2).unwrap_or(NonZeroUsize::MIN));
        let failure = |generation: u64| MarketWorkerMessage::CatalogFailure {
            message: generation.to_string(),
        };
        assert!(sender.send(failure(1)).is_ok());
        assert!(sender.send(failure(2)).is_ok());
        let (messages, _) = receiver.drain();
        assert_eq!(messages.len(), 2);
        for (index, message) in messages.iter().enumerate() {
            let MarketWorkerMessage::CatalogFailure { message } = message else {
                panic!("app-scoped failures must remain ordered");
            };
            assert_eq!(message, &(index + 1).to_string());
        }
        sender
            .send(failure(1))
            .expect("first failure enters bounded queue");
        sender
            .send(failure(2))
            .expect("second failure enters bounded queue");
        for generation in 3..=10 {
            sender
                .send(failure(generation))
                .expect("failure enters bounded control reserve");
        }
        assert_eq!(
            sender.send(failure(11)),
            Err(super::MarketWorkerSendError::Full)
        );
        assert_eq!(sender.occupancy(), (10, 2));
        let (messages, _) = receiver.drain_up_to(1);
        let (later, _) = receiver.drain();
        let observed = messages
            .into_iter()
            .chain(later)
            .map(|message| {
                let MarketWorkerMessage::CatalogFailure { message } = message else {
                    panic!("app-scoped failures must remain ordered");
                };
                message.parse::<u64>().expect("fixture sequence")
            })
            .collect::<Vec<_>>();
        assert_eq!(observed, (1..=10).collect::<Vec<_>>());
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

        let message = || status_message(aeris_contracts::SeriesLoadState::Ready, "ready");
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
                .send(MarketWorkerMessage::EngineSelectionOutcome {
                    sequence,
                    succeeded: true,
                })
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
            sender
                .send(update_message(
                    publication_generation,
                    bar,
                    SeriesTailOperation::Revise,
                ))
                .expect("tail sends");
        }
        assert_eq!(wake_count.load(Ordering::Acquire), 1);
        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert_eq!(messages.len(), 8);
        let generations = messages
            .iter()
            .filter_map(|message| match message {
                MarketWorkerMessage::RuntimeSeriesPublication {
                    event:
                        aeris_market_runtime::fence::Fenced {
                            value: MarketRuntimeEvent::SeriesUpdate(update),
                            ..
                        },
                    ..
                } => Some(update.publication_generation),
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
        for (generation, close, operation) in [
            (3, bar.close, SeriesTailOperation::Append),
            (4, bar.close.saturating_add(1), SeriesTailOperation::Revise),
        ] {
            bar.close = close;
            bar.high = bar.high.max(close);
            sender
                .send(update_message(generation, bar, operation))
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
            [SeriesTailOperation::Append, SeriesTailOperation::Revise]
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
            sender
                .send(update_message(offset + 2, bar, SeriesTailOperation::Append))
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
            .send(MarketWorkerMessage::RuntimeSeriesPublication {
                event: FixtureMarketEvents::default().snapshot(
                    &fixture_series(),
                    1,
                    bootstrap.generation.publication_generation(),
                    bootstrap
                        .snapshot
                        .bars()
                        .iter()
                        .map(|item| *item.value())
                        .collect::<Vec<_>>(),
                ),
                received_unix_nanos: 0,
            })
            .expect("covering snapshot sends");

        for offset in 1_u64..=4 {
            let mut bar = *source.value();
            bar.source_sequence = first_sequence.saturating_add(offset);
            bar.exchange_timestamp_seconds = bar
                .exchange_timestamp_seconds
                .saturating_add(60 * i64::try_from(offset).expect("offset fits"));
            bar.exchange_timestamp_unix_nanos =
                bar.exchange_timestamp_seconds.saturating_mul(1_000_000_000);
            let result = sender.send(update_message(offset + 2, bar, SeriesTailOperation::Append));
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
            Some(MarketWorkerMessage::RuntimeSeriesPublication {
                event: aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::SeriesSnapshot(_),
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn mailbox_wake_fires_for_queued_messages_on_registration() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        sender
            .send(status_message(
                aeris_contracts::SeriesLoadState::Resolving,
                "loading",
            ))
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
                .send(status_message(
                    aeris_contracts::SeriesLoadState::Ready,
                    "ready"
                ))
                .is_err()
        );
        drop(sender);
        assert_eq!(wake_count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn app_scoped_failures_keep_fifo_order_with_market_publications() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(2).expect("capacity is nonzero"));
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let snapshot = snapshot_message(&mut worker, 2);
        let MarketWorkerMessage::RuntimeSeriesPublication { event, .. } = &snapshot else {
            panic!("canonical fixture snapshot");
        };
        let MarketRuntimeEvent::SeriesSnapshot(publication) = &event.value else {
            panic!("canonical fixture snapshot");
        };
        let source = *publication.snapshot.bars.last().expect("last bar");
        assert!(sender.send(snapshot).is_ok());
        assert!(
            sender
                .send(MarketWorkerMessage::CatalogFailure {
                    message: "fixture".into(),
                })
                .is_ok()
        );
        let mut next = source;
        next.source_sequence += 1;
        next.exchange_timestamp_seconds += 60;
        next.exchange_timestamp_unix_nanos = next.exchange_timestamp_seconds * 1_000_000_000;
        let delta = || update_message(3, next, SeriesTailOperation::Append);
        assert_eq!(
            sender.send(delta()),
            Err(super::MarketWorkerSendError::Full)
        );
        let (first, _) = receiver.drain_up_to(1);
        assert!(matches!(
            first.as_slice(),
            [MarketWorkerMessage::RuntimeSeriesPublication { .. }]
        ));
        sender
            .send(delta())
            .expect("publication enters after bounded drain");

        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::CatalogFailure { .. },
                MarketWorkerMessage::RuntimeSeriesPublication { .. }
            ]
        ));
    }

    #[test]
    fn terminal_control_preserves_a_queued_covering_snapshot() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        assert!(sender.send(snapshot_message(&mut worker, 2)).is_ok());
        assert!(
            sender
                .send(status_message(
                    aeris_contracts::SeriesLoadState::Failed,
                    "terminal"
                ))
                .is_ok()
        );

        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::RuntimeSeriesPublication {
                    event: aeris_market_runtime::fence::Fenced {
                        value: MarketRuntimeEvent::SeriesSnapshot(_),
                        ..
                    },
                    ..
                },
                MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::SeriesState(
                        aeris_market_runtime::MarketSeriesState {
                            state: aeris_contracts::SeriesLoadState::Failed,
                            ..
                        }
                    ),
                    ..
                })
            ]
        ));
    }

    #[test]
    fn market_publication_waits_behind_a_full_control_queue() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        assert!(
            sender
                .send(status_message(
                    aeris_contracts::SeriesLoadState::Ready,
                    "ready"
                ))
                .is_ok()
        );
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let snapshot = snapshot_message(&mut worker, 2);
        assert_eq!(
            sender.send(snapshot),
            Err(super::MarketWorkerSendError::Full)
        );

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                value: MarketRuntimeEvent::SeriesState(state), ..
            })] if state.state == aeris_contracts::SeriesLoadState::Ready
                && state.detail.as_deref() == Some("ready")
        ));
    }

    #[test]
    fn queued_covering_snapshot_is_unchanged_when_next_update_is_backpressured() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let snapshot = snapshot_message(&mut worker, 2);
        let MarketWorkerMessage::RuntimeSeriesPublication { event, .. } = &snapshot else {
            panic!("fixture snapshot is canonical");
        };
        let MarketRuntimeEvent::SeriesSnapshot(publication) = &event.value else {
            panic!("fixture snapshot contains bars");
        };
        let mut next = *publication.snapshot.bars.last().expect("last bar");
        next.source_sequence += 1;
        next.exchange_timestamp_seconds += 60;
        next.exchange_timestamp_unix_nanos = next.exchange_timestamp_seconds * 1_000_000_000;
        let next_generation = publication.publication_generation + 1;
        assert!(sender.send(snapshot).is_ok());
        let publication = update_message(next_generation, next, SeriesTailOperation::Append);
        assert_eq!(
            sender.send(publication),
            Err(super::MarketWorkerSendError::Full)
        );

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [publication] if snapshot_generation(publication) == Some(next_generation - 1)
        ));
    }

    #[test]
    fn control_messages_remain_fifo_while_market_publication_is_backpressured() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        sender
            .send(MarketWorkerMessage::EngineSelectionOutcome {
                sequence: 7,
                succeeded: true,
            })
            .expect("switch marker sends");
        assert_eq!(
            sender.send(snapshot_message(&mut worker, 2)),
            Err(super::MarketWorkerSendError::Full)
        );
        sender
            .send(status_message(
                aeris_contracts::SeriesLoadState::Resolving,
                "switching",
            ))
            .expect("state sends after the marker");

        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::EngineSelectionOutcome { sequence: 7, succeeded: true },
                MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::SeriesState(state), ..
                })
            ] if state.detail.as_deref() == Some("switching")
        ));
    }

    #[test]
    fn terminal_and_failed_recovery_messages_survive_lower_priority_states() {
        let (error_sender, error_receiver) = market_worker_channel(NonZeroUsize::MIN);
        assert!(
            error_sender
                .send(status_message(
                    aeris_contracts::SeriesLoadState::Failed,
                    "terminal"
                ))
                .is_ok()
        );
        assert!(
            error_sender
                .send(status_message(
                    aeris_contracts::SeriesLoadState::Ready,
                    "obsolete"
                ))
                .is_ok()
        );
        let (messages, _) = error_receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::SeriesState(terminal), ..
                }),
                MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::SeriesState(obsolete), ..
                })
            ] if terminal.state == aeris_contracts::SeriesLoadState::Failed
                && terminal.detail.as_deref() == Some("terminal")
                && obsolete.state == aeris_contracts::SeriesLoadState::Ready
                && obsolete.detail.as_deref() == Some("obsolete")
        ));

        let (recovery_sender, recovery_receiver) = market_worker_channel(NonZeroUsize::MIN);
        assert!(
            recovery_sender
                .send(MarketWorkerMessage::Recovery {
                    generation: 1,
                    request_id: 7,
                    failure: MarketRecoveryFailure::Failed("recovery failed".to_string()),
                })
                .is_ok()
        );
        assert!(
            recovery_sender
                .send(status_message(
                    aeris_contracts::SeriesLoadState::Resolving,
                    "obsolete"
                ))
                .is_ok()
        );
        let (messages, _) = recovery_receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::Recovery { request_id: 7, .. },
                MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::SeriesState(state), ..
                })
            ] if state.detail.as_deref() == Some("obsolete")
        ));
    }

    #[test]
    fn successful_recovery_and_newer_stream_update_preserve_fifo_order() {
        let (covered_sender, covered_receiver) =
            market_worker_channel(NonZeroUsize::new(2).expect("capacity is nonzero"));
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let bootstrap = fixture.publish_snapshot(2).expect("snapshot publishes");
        let mut bar = *bootstrap.snapshot.bars().last().expect("last bar").value();
        let next_publication_generation = bootstrap.generation.publication_generation() + 2;
        bar.source_sequence += 1;
        bar.exchange_timestamp_seconds += 60;
        bar.exchange_timestamp_unix_nanos = bar.exchange_timestamp_seconds * 1_000_000_000;
        let MarketWorkerMessage::RuntimeSeriesPublication {
            event,
            received_unix_nanos,
        } = snapshot_message(&mut fixture, 2)
        else {
            unreachable!("fixture publishes a snapshot");
        };
        assert!(
            covered_sender
                .send(MarketWorkerMessage::RuntimeRecovery {
                    event,
                    request_id: 8,
                    received_unix_nanos,
                })
                .is_ok()
        );
        assert!(
            covered_sender
                .send(update_message(
                    next_publication_generation,
                    bar,
                    SeriesTailOperation::Append
                ))
                .is_ok()
        );
        let (messages, _) = covered_receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::RuntimeRecovery { request_id: 8, .. },
                MarketWorkerMessage::RuntimeSeriesPublication { .. }
            ]
        ));

        let (stale_sender, stale_receiver) = market_worker_channel(NonZeroUsize::MIN);
        let mut stale_fixture = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let MarketWorkerMessage::RuntimeSeriesPublication {
            event,
            received_unix_nanos,
        } = snapshot_message(&mut stale_fixture, 2)
        else {
            unreachable!("fixture publishes a snapshot");
        };
        assert!(
            stale_sender
                .send(MarketWorkerMessage::RuntimeRecovery {
                    event,
                    request_id: 9,
                    received_unix_nanos,
                })
                .is_ok()
        );
        assert!(
            stale_sender
                .send(status_message(
                    aeris_contracts::SeriesLoadState::Partial,
                    "network changed"
                ))
                .is_ok()
        );
        let (messages, _) = stale_receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::RuntimeRecovery { request_id: 9, .. },
                MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::SeriesState(state), ..
                })
            ] if state.detail.as_deref() == Some("network changed")
        ));
    }

    #[test]
    fn latest_rithmic_catalog_result_is_conflated_without_displacing_market_state() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        assert!(
            sender
                .send(catalog_rejection(
                    1,
                    ProviderCatalogRejectionReason::SupersededSearch,
                    false,
                ))
                .is_ok()
        );
        let latest_generation = NonZeroUsize::new(2).expect("generation is nonzero");
        assert!(
            sender
                .send(catalog_rejection(
                    latest_generation.get() as u64,
                    ProviderCatalogRejectionReason::InstrumentUnavailable,
                    true,
                ))
                .is_ok()
        );
        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [
                MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::ProviderCatalogRejected {
                        rejection: ProviderCatalogRejected { command_generation: first_generation, .. },
                        selection: false,
                    },
                    ..
                }),
                MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                    value: MarketRuntimeEvent::ProviderCatalogRejected {
                        rejection: ProviderCatalogRejected { command_generation, reason, .. },
                        selection: true,
                    },
                    ..
                })
            ] if *first_generation == 1
                && *command_generation == latest_generation.get() as u64
                && *reason == ProviderCatalogRejectionReason::InstrumentUnavailable
        ));
    }

    #[test]
    fn series_state_survives_catalog_rejection_in_the_bounded_mailbox() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let latest_generation = NonZeroUsize::new(2).expect("generation is nonzero");
        assert!(
            sender
                .send(status_message(
                    aeris_contracts::SeriesLoadState::Failed,
                    "terminal"
                ))
                .is_ok()
        );
        assert!(
            sender
                .send(catalog_rejection(
                    latest_generation.get() as u64,
                    ProviderCatalogRejectionReason::SubscriptionRejected,
                    true,
                ))
                .is_ok()
        );
        let (messages, _) = receiver.drain();
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            messages[0],
            MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                value: MarketRuntimeEvent::SeriesState(aeris_market_runtime::MarketSeriesState {
                    state: aeris_contracts::SeriesLoadState::Failed,
                    ..
                }),
                ..
            })
        ));
        assert!(matches!(
            messages[1],
            MarketWorkerMessage::Runtime(aeris_market_runtime::fence::Fenced {
                value: MarketRuntimeEvent::ProviderCatalogRejected {
                    rejection: ProviderCatalogRejected {
                        command_generation: 2,
                        ..
                    },
                    selection: true,
                },
                ..
            })
        ));
    }

    #[test]
    fn mailbox_does_not_filter_publication_generations() {
        let (sender, receiver) =
            market_worker_channel(NonZeroUsize::new(2).expect("capacity is nonzero"));
        let mut fixture = FixtureMarketWorker::try_new().expect("fixture validates");
        let snapshot = snapshot_message(&mut fixture, 2);
        let MarketWorkerMessage::RuntimeSeriesPublication { event, .. } = snapshot else {
            panic!("fixture snapshot is canonical");
        };
        let MarketRuntimeEvent::SeriesSnapshot(publication) = event.value else {
            panic!("fixture snapshot contains bars");
        };
        let publication = |generation| MarketWorkerMessage::RuntimeSeriesPublication {
            event: FixtureMarketEvents::default().snapshot(
                &fixture_series(),
                1,
                generation,
                publication.snapshot.bars.clone(),
            ),
            received_unix_nanos: 0,
        };
        sender
            .send(publication(2))
            .expect("newer publication queues");
        sender
            .send(publication(1))
            .expect("older publication queues behind it");
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                first,
                second,
            ] if snapshot_generation(first) == Some(2)
                && snapshot_generation(second) == Some(1)
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
                .send(book_message(frame(3, OrderBookState::Ready,)))
                .is_ok()
        );
        assert!(
            sender
                .send(book_message(frame(
                    2,
                    OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap),
                )))
                .is_ok()
        );
        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                first,
                second,
            ] if book_snapshot(first).is_some_and(|item| item.publication.revision == 3
                && item.publication.state == OrderBookState::Ready)
                && book_snapshot(second).is_some_and(|item| item.publication.revision == 2
                && item.publication.state == OrderBookState::Recovering(OrderBookRecoveryReason::SequenceGap))
        ));
    }

    #[test]
    fn order_book_mailbox_preserves_newer_frames_after_earlier_frames() {
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
            .send(book_message(frame(2)))
            .expect("first frame queues");
        sender
            .send(book_message(frame(3)))
            .expect("newer frame queues behind it");

        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [first, second]
                if book_snapshot(first).is_some_and(|item| item.publication.revision == 2)
                    && book_snapshot(second).is_some_and(|item| item.publication.revision == 3)
        ));
    }

    #[test]
    fn trade_tape_mailbox_preserves_emission_order_per_runtime_generation() {
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
        let events = FixtureMarketEvents::default();
        sender
            .send(MarketWorkerMessage::Runtime(events.trade_tape(snapshot(2))))
            .expect("first tape queues");
        sender
            .send(MarketWorkerMessage::Runtime(events.trade_tape(snapshot(3))))
            .expect("newer tape queues behind it");

        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::Runtime(first), MarketWorkerMessage::Runtime(second)]
                if matches!(&first.value, MarketRuntimeEvent::TradeTapeSnapshot(tape) if tape.revision == 2)
                    && matches!(&second.value, MarketRuntimeEvent::TradeTapeSnapshot(tape) if tape.revision == 3)
                    && first.epoch == second.epoch
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
            .send(book_message(frame(7, 99, 40)))
            .expect("old session frame queues");
        sender
            .send(book_message(frame(8, 1, 1)))
            .expect("new session frame queues");
        sender
            .send(book_message(frame(7, 100, 99)))
            .expect("late old session frame is safely ignored");

        let (messages, _) = receiver.drain();
        assert!(matches!(
            messages.as_slice(),
            [
                first,
                second,
                third,
            ] if book_snapshot(first).is_some_and(|item| item.publication.session_generation == 7
                && item.publication.revision == 40)
                && book_snapshot(second).is_some_and(|item| item.publication.session_generation == 8
                    && item.publication.revision == 1)
                && book_snapshot(third).is_some_and(|item| item.publication.session_generation == 7
                    && item.publication.revision == 99)
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
            startup_resolve: false,
            consumer_id: 0,
            search_generation: 1,
            provider: "rithmic".to_string(),
            query: "ES".to_string(),
            maximum_results: 16,
            categories: aeris_contracts::InstrumentSearchCategories::ALL,
        };
        let selection = SelectProviderInstrument {
            startup_resolve: false,
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
    fn foreground_symbol_switch_reports_bounded_overflow_without_advancing_generation() {
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (_shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let sequence = Arc::new(AtomicU64::new(7));
        let worker = MarketDataWorker::from_channels(
            command_tx,
            message_rx,
            shutdown_rx,
            None,
            Some(Arc::clone(&sequence)),
        );

        worker
            .try_set_chart_viewport(1, 2)
            .expect("viewport fills the ordinary command mailbox");
        let selection = SelectProviderInstrument {
            startup_resolve: false,
            consumer_id: 0,
            selection_generation: 3,
            search_generation: 2,
            provider: "hyperliquid".to_string(),
            symbol: "ETH".to_string(),
            exchange: "Hyperliquid".to_string(),
            entitlement_id: "hyperliquid-public".to_string(),
        };
        assert!(worker.try_select_provider(selection.clone()).is_err());
        let selected = InstallProviderInstrument {
            provider: "hyperliquid".to_string(),
            provider_symbol: "ETH".to_string(),
            display_symbol: "ETH".to_string(),
            ..InstallProviderInstrument::default()
        };
        assert!(matches!(
            worker.try_select_instrument(selected.clone(), ChartInterval::Minute1),
            Err(TrySendError::Full(_))
        ));
        assert_eq!(sequence.load(Ordering::Acquire), 7);
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::ChartViewport(_))
        ));
        worker
            .try_select_provider(selection.clone())
            .expect("selection enters the freed ordered queue");
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::ProviderSelect(queued)) if queued == selection
        ));
        let next = worker
            .try_select_instrument(selected, ChartInterval::Minute1)
            .expect("engine selection follows the click");

        assert_eq!(next, 8);
        assert_eq!(sequence.load(Ordering::Acquire), 8);
        let Ok(MarketWorkerCommand::EngineSelect(pending_engine)) = command_rx.recv() else {
            panic!("ordered engine selection");
        };
        assert_eq!(pending_engine.sequence, 8);
        assert_eq!(pending_engine.product.provider_symbol, "ETH");
    }
}
