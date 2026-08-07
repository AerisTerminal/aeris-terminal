//! Deterministic binary fixture worker for the disconnected desktop Stage 2 path.
//!
//! This module does not implement or claim a socket, WebSocket, live provider,
//! entitlement service, or production transport. It exercises the same bounded
//! binary protocol and application model that a future connected adapter will own.

use crate::rithmic_history::RithmicSeriesRequest;
use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
    MarketBarReplayPort, MarketGeneration, ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot,
    ReplayStreamUpdate,
};
use axiusflow_chart_integration::ReplayRecoveryCommand;
use axiusflow_desktop_provider_runtime::SessionGeneration;
use axiusflow_market_protocol_adapter::{
    BinaryMarketBarStreamDecoder, DecimalConvention, ProjectedMarketBarUpdate,
    encode_market_bar_stream_frame, try_encode_replay_delta_envelope,
    try_encode_replay_snapshot_chunk_envelopes,
};
use axiusflow_observability::FeedConnectionState;
use axiusflow_observability::FeedDiagnosticsSnapshot;
use axiusflow_rithmic_protocol_adapter::{
    RithmicCatalogEvent, RithmicInstrumentSelection, RithmicSymbolSearch,
};
use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

const SUBSCRIPTION_ID: &str = "desktop_fixture_market_bars";
const INITIAL_BAR_COUNT: usize = 576;
const STARTUP_DELTA_COUNT: usize = 24;
const RECOVERY_BAR_COUNT: usize = INITIAL_BAR_COUNT + STARTUP_DELTA_COUNT;
const MODEL_ITEM_CAPACITY: usize = RECOVERY_BAR_COUNT;
const MESSAGE_CAPACITY: usize = 32;
const COMMAND_CAPACITY: usize = 1;
const SNAPSHOT_CHUNK_ITEMS: usize = 64;
const MAXIMUM_FRAME_BYTES: usize = 65_536;
const MAXIMUM_BUFFERED_BYTES: usize = 131_072;
const FRAGMENT_BYTES: usize = 7;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) type DesktopMarketGeneration = MarketGeneration<ProvenancedMarketBar>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChartState {
    Loading,
    Ready,
    Stale,
    Recovering,
    Error,
}

impl ChartState {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Loading => "Loading",
            Self::Ready => "Ready",
            Self::Stale => "Stale",
            Self::Recovering => "Recovering",
            Self::Error => "Error",
        }
    }
}

pub(crate) struct MarketWorkerBootstrap {
    pub snapshot: ReplaySnapshot,
    pub subscription_id: String,
    pub generation: DesktopMarketGeneration,
    pub worker_label: String,
}

pub(crate) enum MarketWorkerStartup {
    Shell(crate::rithmic_shell::RithmicShellState),
    Loading {
        instrument: axiusflow_instruments::InstrumentRevision,
        subscription_id: String,
        worker_label: String,
    },
    Ready(Box<MarketWorkerBootstrap>),
}

pub(crate) struct MarketWorkerPublication {
    pub update: ReplayStreamUpdate,
    pub generation: DesktopMarketGeneration,
    pub subscription_id: String,
    pub worker_label: String,
    pub ui_diagnostics: Option<PendingUiDiagnostics>,
}

pub(crate) enum MarketWorkerMessage {
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
    RithmicCatalog(RithmicCatalogEvent),
    RithmicHistory {
        selection_generation: NonZeroUsize,
        series_generation: NonZeroUsize,
        result: Result<Box<MarketWorkerBootstrap>, String>,
    },
}

struct MarketWorkerMailbox {
    queue: Mutex<VecDeque<MarketWorkerMessage>>,
    capacity: usize,
    sender_count: AtomicUsize,
    receiver_alive: AtomicBool,
    coalesced_updates: Mutex<GenerationCoalescingQueue>,
}

pub(crate) struct MarketWorkerSender {
    mailbox: Arc<MarketWorkerMailbox>,
}

pub(crate) struct MarketWorkerReceiver {
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
        self.mailbox.sender_count.fetch_sub(1, Ordering::Release);
    }
}

impl MarketWorkerSender {
    pub(crate) fn send(&self, message: MarketWorkerMessage) -> Result<(), ()> {
        if !self.mailbox.receiver_alive.load(Ordering::Acquire) {
            return Err(());
        }
        let mut queue = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.mailbox.receiver_alive.load(Ordering::Acquire) {
            return Err(());
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
            message @ MarketWorkerMessage::Diagnostics(_) => {
                self.send_diagnostics(queue, message);
                None
            }
            message @ MarketWorkerMessage::Connection { .. } => {
                self.send_connection(queue, message);
                None
            }
            message @ MarketWorkerMessage::RithmicCatalog(_) => {
                self.send_rithmic_catalog(queue, message);
                None
            }
            message @ MarketWorkerMessage::RithmicHistory { .. } => {
                self.send_rithmic_history(queue, message);
                None
            }
            message => Some(message),
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
            .position(|queued| matches!(queued, MarketWorkerMessage::RithmicCatalog(_)))
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
        if let Some(index) = queue
            .iter()
            .position(|queued| matches!(queued, MarketWorkerMessage::RithmicHistory { .. }))
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

    pub(crate) fn occupancy(&self) -> (usize, usize) {
        let current = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        (current, self.mailbox.capacity)
    }

    pub(crate) fn try_take_coalesced_update(&self) -> Option<CoalescedUiUpdates> {
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

    fn record_coalesced_generation(&self, generation: SessionGeneration, count: u64) {
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CoalescedUiUpdates {
    pub(crate) generation: SessionGeneration,
    pub(crate) count: u64,
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

    fn record(&mut self, generation: SessionGeneration, count: u64) {
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

fn message_diagnostics_generation(message: &MarketWorkerMessage) -> Option<SessionGeneration> {
    match message {
        MarketWorkerMessage::Update(publication) => publication
            .ui_diagnostics
            .as_ref()
            .map(PendingUiDiagnostics::generation),
        MarketWorkerMessage::Diagnostics(snapshot) => {
            snapshot.session_generation.map(SessionGeneration::new)
        }
        MarketWorkerMessage::Recovery { .. }
        | MarketWorkerMessage::State { .. }
        | MarketWorkerMessage::Connection { .. }
        | MarketWorkerMessage::RithmicCatalog(_)
        | MarketWorkerMessage::RithmicHistory { .. } => None,
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
    pub(crate) fn drain(&self) -> (Vec<MarketWorkerMessage>, bool) {
        let mut queue = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let messages = queue.drain(..).collect::<Vec<_>>();
        let disconnected = self.mailbox.sender_count.load(Ordering::Acquire) == 0;
        (messages, disconnected)
    }
}

impl Drop for MarketWorkerReceiver {
    fn drop(&mut self) {
        self.mailbox.receiver_alive.store(false, Ordering::Release);
        self.mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

pub(crate) fn market_worker_channel(
    capacity: NonZeroUsize,
) -> (MarketWorkerSender, MarketWorkerReceiver) {
    let mailbox = Arc::new(MarketWorkerMailbox {
        queue: Mutex::new(VecDeque::with_capacity(capacity.get().saturating_add(1))),
        capacity: capacity.get(),
        sender_count: AtomicUsize::new(1),
        receiver_alive: AtomicBool::new(true),
        coalesced_updates: Mutex::new(GenerationCoalescingQueue::new(capacity.get())),
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

pub(crate) enum MarketWorkerCommand {
    Recovery(ReplayRecoveryCommand),
    RithmicSearch(RithmicSymbolSearch),
    RithmicSelect(RithmicInstrumentSelection),
    RithmicHistory(RithmicSeriesRequest),
    Shutdown,
}

pub(crate) struct PendingUiDiagnostics {
    generation: SessionGeneration,
    origin: Instant,
    ui_enqueue_nanos: i64,
    frame_submit_nanos: i64,
}

impl PendingUiDiagnostics {
    pub(crate) fn new(generation: SessionGeneration) -> Self {
        Self {
            generation,
            origin: Instant::now(),
            ui_enqueue_nanos: 0,
            frame_submit_nanos: 0,
        }
    }

    pub(crate) fn mark_ui_enqueue(&mut self) {
        self.ui_enqueue_nanos = self.elapsed_nanos();
    }

    pub(crate) fn mark_frame_submit(&mut self) {
        self.frame_submit_nanos = self.elapsed_nanos();
    }

    pub(crate) fn into_presented(self) -> UiDiagnosticsFeedback {
        UiDiagnosticsFeedback::Presented {
            generation: self.generation,
            ui_enqueue_nanos: self.ui_enqueue_nanos,
            frame_submit_nanos: self.frame_submit_nanos,
            present_nanos: self.elapsed_nanos(),
        }
    }

    pub(crate) const fn generation(&self) -> SessionGeneration {
        self.generation
    }

    fn elapsed_nanos(&self) -> i64 {
        i64::try_from(self.origin.elapsed().as_nanos()).unwrap_or(i64::MAX)
    }
}

pub(crate) enum UiDiagnosticsFeedback {
    Presented {
        generation: SessionGeneration,
        ui_enqueue_nanos: i64,
        frame_submit_nanos: i64,
        present_nanos: i64,
    },
    Coalesced {
        generation: SessionGeneration,
    },
}

struct UiDiagnosticsMailbox {
    queue: Mutex<VecDeque<UiDiagnosticsFeedback>>,
    capacity: usize,
    receiver_alive: AtomicBool,
    coalesced_feedback: Mutex<GenerationCoalescingQueue>,
}

#[derive(Clone)]
pub(crate) struct UiDiagnosticsSender {
    mailbox: Arc<UiDiagnosticsMailbox>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

pub(crate) struct UiDiagnosticsReceiver {
    mailbox: Arc<UiDiagnosticsMailbox>,
}

impl UiDiagnosticsSender {
    pub(crate) fn send(&self, feedback: UiDiagnosticsFeedback) -> Result<(), ()> {
        if !self.mailbox.receiver_alive.load(Ordering::Acquire) {
            return Err(());
        }
        let mut queue = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !self.mailbox.receiver_alive.load(Ordering::Acquire) {
            return Err(());
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
    pub(crate) fn try_recv(&self) -> Option<UiDiagnosticsFeedback> {
        self.mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
    }

    pub(crate) fn occupancy(&self) -> (usize, usize) {
        let current = self
            .mailbox
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        (current, self.mailbox.capacity)
    }

    pub(crate) fn try_take_coalesced_feedback(&self) -> Option<CoalescedUiUpdates> {
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

pub(crate) fn ui_diagnostics_channel(
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

fn feedback_generation(feedback: &UiDiagnosticsFeedback) -> SessionGeneration {
    match feedback {
        UiDiagnosticsFeedback::Presented { generation, .. }
        | UiDiagnosticsFeedback::Coalesced { generation } => *generation,
    }
}

pub(crate) struct MarketDataWorker {
    commands: Option<SyncSender<MarketWorkerCommand>>,
    messages: Option<MarketWorkerReceiver>,
    shutdown_complete: Receiver<()>,
    connected: bool,
    ui_diagnostics: Option<UiDiagnosticsSender>,
}

impl MarketDataWorker {
    pub fn start() -> Result<(MarketWorkerStartup, Self), String> {
        let (bootstrap_tx, bootstrap_rx) = mpsc::sync_channel(1);
        let (message_tx, message_rx) =
            market_worker_channel(NonZeroUsize::new(MESSAGE_CAPACITY).unwrap_or(NonZeroUsize::MIN));
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("axiusflow-market-fixture-worker".to_string())
            .spawn(move || {
                run_worker(&bootstrap_tx, &message_tx, &command_rx);
                let _ = shutdown_tx.send(());
            })
            .map_err(|error| error.to_string())?;
        let bootstrap = bootstrap_rx
            .recv()
            .map_err(|_| "market fixture worker stopped before bootstrap".to_string())??;
        Ok((
            MarketWorkerStartup::Ready(Box::new(bootstrap)),
            Self {
                commands: Some(command_tx),
                messages: Some(message_rx),
                shutdown_complete: shutdown_rx,
                connected: true,
                ui_diagnostics: None,
            },
        ))
    }

    pub fn start_coinbase(
        product_id: String,
        history_root: PathBuf,
        ui_thread: thread::ThreadId,
        detailed_diagnostics: bool,
    ) -> Result<(MarketWorkerStartup, Self), String> {
        crate::live_market_worker::start(product_id, history_root, ui_thread, detailed_diagnostics)
    }

    pub fn start_rithmic(
        history_root: PathBuf,
        ui_thread: thread::ThreadId,
        detailed_diagnostics: bool,
    ) -> Result<(MarketWorkerStartup, Self), String> {
        crate::rithmic_market_worker::start(history_root, ui_thread, detailed_diagnostics)
    }

    pub(crate) const fn from_channels(
        commands: SyncSender<MarketWorkerCommand>,
        messages: MarketWorkerReceiver,
        shutdown_complete: Receiver<()>,
        ui_diagnostics: Option<UiDiagnosticsSender>,
    ) -> Self {
        Self {
            commands: Some(commands),
            messages: Some(messages),
            shutdown_complete,
            connected: true,
            ui_diagnostics,
        }
    }

    pub(crate) fn send_ui_diagnostics(&self, feedback: UiDiagnosticsFeedback) {
        if let Some(sender) = &self.ui_diagnostics {
            let _ = sender.send(feedback);
        }
    }

    pub(crate) fn ui_diagnostics_sender(&self) -> Option<UiDiagnosticsSender> {
        self.ui_diagnostics.clone()
    }

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
                    | MarketWorkerCommand::RithmicSearch(_)
                    | MarketWorkerCommand::RithmicSelect(_)
                    | MarketWorkerCommand::RithmicHistory(_),
                )
                | TrySendError::Disconnected(
                    MarketWorkerCommand::Shutdown
                    | MarketWorkerCommand::RithmicSearch(_)
                    | MarketWorkerCommand::RithmicSelect(_)
                    | MarketWorkerCommand::RithmicHistory(_),
                ) => {
                    unreachable!("recovery send errors retain the recovery command")
                }
            })
    }

    pub fn try_search_rithmic(
        &self,
        search: RithmicSymbolSearch,
    ) -> Result<(), TrySendError<RithmicSymbolSearch>> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(search));
        };
        commands
            .try_send(MarketWorkerCommand::RithmicSearch(search))
            .map_err(|error| match error {
                TrySendError::Full(MarketWorkerCommand::RithmicSearch(search)) => {
                    TrySendError::Full(search)
                }
                TrySendError::Disconnected(MarketWorkerCommand::RithmicSearch(search)) => {
                    TrySendError::Disconnected(search)
                }
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    unreachable!("Rithmic search send errors retain the search command")
                }
            })
    }

    pub fn try_select_rithmic(
        &self,
        selection: RithmicInstrumentSelection,
    ) -> Result<(), TrySendError<RithmicInstrumentSelection>> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(selection));
        };
        commands
            .try_send(MarketWorkerCommand::RithmicSelect(selection))
            .map_err(|error| match error {
                TrySendError::Full(MarketWorkerCommand::RithmicSelect(selection)) => {
                    TrySendError::Full(selection)
                }
                TrySendError::Disconnected(MarketWorkerCommand::RithmicSelect(selection)) => {
                    TrySendError::Disconnected(selection)
                }
                TrySendError::Full(_) | TrySendError::Disconnected(_) => {
                    unreachable!("Rithmic selection send errors retain the selection command")
                }
            })
    }

    pub fn try_request_rithmic_history(
        &self,
        request: RithmicSeriesRequest,
    ) -> Result<(), TrySendError<RithmicSeriesRequest>> {
        let Some(commands) = self.commands.as_ref() else {
            return Err(TrySendError::Disconnected(request));
        };
        commands
            .try_send(MarketWorkerCommand::RithmicHistory(request))
            .map_err(|error| match error {
                TrySendError::Full(MarketWorkerCommand::RithmicHistory(request)) => {
                    TrySendError::Full(request)
                }
                TrySendError::Disconnected(MarketWorkerCommand::RithmicHistory(request)) => {
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

    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn mark_disconnected(&mut self) {
        self.connected = false;
    }
}

impl Drop for MarketDataWorker {
    fn drop(&mut self) {
        if let Some(commands) = self.commands.take() {
            let _ = commands.try_send(MarketWorkerCommand::Shutdown);
            drop(commands);
        }
        drop(self.messages.take());
        let _ = self.shutdown_complete.recv_timeout(SHUTDOWN_TIMEOUT);
    }
}

pub(crate) struct FixtureMarketWorker {
    expected_subscription_id: String,
    source: EmbeddedReplaySource,
    convention: DecimalConvention,
    decoder: BinaryMarketBarStreamDecoder,
    model: MarketBarClientModel,
    publisher_context: Option<ReplaySnapshot>,
    maximum_frame_bytes: NonZeroUsize,
}

impl FixtureMarketWorker {
    pub(crate) fn try_new() -> Result<Self, String> {
        Self::try_new_with_convention("usd_minor", "shares")
    }

    pub(crate) fn try_new_with_convention(
        price_unit: &str,
        quantity_unit: &str,
    ) -> Result<Self, String> {
        let convention = DecimalConvention::try_new(price_unit, quantity_unit)
            .map_err(|error| error.to_string())?;
        let maximum_frame_bytes =
            NonZeroUsize::new(MAXIMUM_FRAME_BYTES).unwrap_or(NonZeroUsize::MIN);
        let maximum_buffered_bytes =
            NonZeroUsize::new(MAXIMUM_BUFFERED_BYTES).unwrap_or(NonZeroUsize::MIN);
        let decoder = BinaryMarketBarStreamDecoder::try_new(
            convention.clone(),
            ReplayProvenance::EmbeddedFixture,
            maximum_frame_bytes,
            maximum_buffered_bytes,
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            expected_subscription_id: SUBSCRIPTION_ID.to_string(),
            source: EmbeddedReplaySource,
            convention,
            decoder,
            model: MarketBarClientModel::new(
                NonZeroUsize::new(MODEL_ITEM_CAPACITY).unwrap_or(NonZeroUsize::MIN),
            ),
            publisher_context: None,
            maximum_frame_bytes,
        })
    }

    pub(crate) fn publish_snapshot(
        &mut self,
        bar_count: usize,
    ) -> Result<MarketWorkerBootstrap, String> {
        let snapshot = self
            .source
            .load_snapshot(LoadEmbeddedReplay { bar_count })
            .map_err(|error| error.to_string())?;
        let generation = self.model.current_generation().map_or(
            Ok(snapshot.evidence().generation),
            |current| {
                current
                    .generation()
                    .checked_add(1)
                    .ok_or_else(|| "desktop fixture generation overflow".to_string())
            },
        )?;
        let snapshot = snapshot
            .try_with_generation(generation)
            .map_err(|error| error.to_string())?;
        let snapshot_id = format!(
            "desktop_fixture_generation_{}_sequence_{}",
            snapshot.evidence().generation,
            snapshot.evidence().last_sequence
        );
        let envelopes = try_encode_replay_snapshot_chunk_envelopes(
            SUBSCRIPTION_ID,
            snapshot_id,
            &snapshot,
            &self.convention,
            NonZeroUsize::new(SNAPSHOT_CHUNK_ITEMS).unwrap_or(NonZeroUsize::MIN),
        )
        .map_err(|error| error.to_string())?;
        let mut projected = Vec::new();
        let envelope_count = envelopes.len();
        for (index, envelope) in envelopes.into_iter().enumerate() {
            let frame = encode_market_bar_stream_frame(&envelope, self.maximum_frame_bytes)
                .map_err(|error| error.to_string())?;
            let updates = self.decode_frame(&frame)?;
            if index + 1 < envelope_count && !updates.is_empty() {
                return Err("chunked fixture snapshot published before completion".to_string());
            }
            projected.extend(updates);
        }
        let publication = self.finish_publication(projected)?;
        let ReplayStreamUpdate::Snapshot(decoded_snapshot) = publication.update else {
            return Err("fixture snapshot chunks projected a delta".to_string());
        };
        self.publisher_context = Some(snapshot);
        Ok(MarketWorkerBootstrap {
            snapshot: decoded_snapshot,
            subscription_id: SUBSCRIPTION_ID.to_string(),
            generation: publication.generation,
            worker_label: "binary fixture worker · disconnected".to_string(),
        })
    }

    pub(crate) fn publish_delta(
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
        let context = self
            .publisher_context
            .as_ref()
            .ok_or_else(|| "fixture publisher has no snapshot context".to_string())?;
        let envelope = try_encode_replay_delta_envelope(
            SUBSCRIPTION_ID,
            context.instrument(),
            context.bar_definition(),
            &delta,
            &self.convention,
        )
        .map_err(|error| error.to_string())?;
        let frame = encode_market_bar_stream_frame(&envelope, self.maximum_frame_bytes)
            .map_err(|error| error.to_string())?;
        let projected = self.decode_frame(&frame)?;
        self.finish_publication(projected).map(Some)
    }

    fn decode_frame(&mut self, frame: &[u8]) -> Result<Vec<ProjectedMarketBarUpdate>, String> {
        let mut projected = Vec::new();
        for chunk in frame.chunks(FRAGMENT_BYTES) {
            projected.extend(
                self.decoder
                    .push(chunk)
                    .map_err(|error| error.to_string())?,
            );
        }
        Ok(projected)
    }

    fn finish_publication(
        &mut self,
        mut projected: Vec<ProjectedMarketBarUpdate>,
    ) -> Result<MarketWorkerPublication, String> {
        if projected.len() != 1 {
            return Err(format!(
                "binary fixture frame projected {} updates instead of one",
                projected.len()
            ));
        }
        let projected = projected
            .pop()
            .ok_or_else(|| "binary fixture frame projected no update".to_string())?;
        if projected.subscription_id != self.expected_subscription_id {
            return Err("binary fixture subscription identity changed".to_string());
        }
        let outcome = self
            .model
            .apply_update(projected.update.clone())
            .map_err(|error| error.to_string())?;
        let MarketBarModelOutcome::Published(generation) = outcome else {
            return Err("binary fixture update did not publish a client generation".to_string());
        };
        Ok(MarketWorkerPublication {
            update: projected.update,
            generation,
            subscription_id: SUBSCRIPTION_ID.to_string(),
            worker_label: "binary fixture worker · disconnected".to_string(),
            ui_diagnostics: None,
        })
    }

    fn recover(&mut self) -> Result<MarketWorkerBootstrap, String> {
        self.decoder.reset();
        self.publish_snapshot(RECOVERY_BAR_COUNT)
    }
}

fn run_worker(
    bootstrap_tx: &SyncSender<Result<MarketWorkerBootstrap, String>>,
    message_tx: &MarketWorkerSender,
    command_rx: &Receiver<MarketWorkerCommand>,
) {
    let mut worker = match FixtureMarketWorker::try_new() {
        Ok(worker) => worker,
        Err(error) => {
            let _ = bootstrap_tx.send(Err(error));
            return;
        }
    };
    let bootstrap = match worker.publish_snapshot(INITIAL_BAR_COUNT) {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            let _ = bootstrap_tx.send(Err(error));
            return;
        }
    };
    let mut previous_sequence = bootstrap.snapshot.stream().last_sequence();
    if bootstrap_tx.send(Ok(bootstrap)).is_err() {
        return;
    }
    for _ in 0..STARTUP_DELTA_COUNT {
        let publication = match worker.publish_delta(previous_sequence) {
            Ok(Some(publication)) => publication,
            Ok(None) => break,
            Err(error) => {
                let _ = message_tx.send(MarketWorkerMessage::State {
                    state: ChartState::Error,
                    message: error,
                });
                return;
            }
        };
        previous_sequence = publication.generation.sequence_range().1;
        if message_tx
            .send(MarketWorkerMessage::Update(publication))
            .is_err()
        {
            return;
        }
    }
    while let Ok(command) = command_rx.recv() {
        match command {
            MarketWorkerCommand::Recovery(command) => {
                let result = worker.recover();
                if message_tx
                    .send(MarketWorkerMessage::Recovery {
                        request_id: command.request_id,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
            }
            MarketWorkerCommand::RithmicSearch(_)
            | MarketWorkerCommand::RithmicSelect(_)
            | MarketWorkerCommand::RithmicHistory(_) => {}
            MarketWorkerCommand::Shutdown => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChartState, FixtureMarketWorker, MarketDataWorker, MarketWorkerCommand,
        MarketWorkerMessage, MarketWorkerPublication, MarketWorkerStartup, PendingUiDiagnostics,
        UiDiagnosticsFeedback, market_worker_channel, ui_diagnostics_channel,
    };
    use axiusflow_application::ReplayStreamUpdate;
    use axiusflow_desktop_provider_runtime::SessionGeneration;
    use axiusflow_observability::{FeedDiagnostics, FeedIdentity};
    use axiusflow_rithmic_protocol_adapter::{
        RithmicCatalogEvent, RithmicCatalogRejection, RithmicInstrumentSelection,
        RithmicReadOnlySubscription, RithmicSymbolSearch, SearchPattern,
    };
    use std::num::{NonZeroU64, NonZeroUsize};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        thread,
    };

    #[test]
    fn shipping_worker_bootstraps_only_the_disconnected_fixture() {
        let (startup, worker) = MarketDataWorker::start().expect("fixture worker starts");
        let MarketWorkerStartup::Ready(bootstrap) = startup else {
            panic!("fixture worker must bootstrap a ready chart");
        };
        assert_eq!(bootstrap.subscription_id, "desktop_fixture_market_bars");
        assert_eq!(
            bootstrap.worker_label,
            "binary fixture worker · disconnected"
        );
        assert!(worker.is_connected());
    }

    #[test]
    fn chart_states_have_explicit_user_facing_labels() {
        assert_eq!(ChartState::Loading.label(), "Loading");
        assert_eq!(ChartState::Ready.label(), "Ready");
        assert_eq!(ChartState::Stale.label(), "Stale");
        assert_eq!(ChartState::Recovering.label(), "Recovering");
        assert_eq!(ChartState::Error.label(), "Error");
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
        ));

        assert!(acknowledged.load(Ordering::Acquire));
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
                generation: SessionGeneration::new(NonZeroU64::MIN),
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
        let retired_generation = SessionGeneration::new(NonZeroU64::MIN);
        let generation = SessionGeneration::new(NonZeroU64::new(2).expect("generation is nonzero"));
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
    fn mailbox_overflow_counts_discarded_market_publications_by_generation() {
        let (sender, receiver) = market_worker_channel(NonZeroUsize::MIN);
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let bootstrap = worker.publish_snapshot(2).expect("snapshot publishes");
        let previous_sequence = bootstrap.snapshot.stream().last_sequence();
        assert!(
            sender
                .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Snapshot(bootstrap.snapshot),
                    generation: bootstrap.generation,
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
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let bootstrap = worker.publish_snapshot(2).expect("snapshot publishes");
        let previous_sequence = bootstrap.snapshot.stream().last_sequence();
        assert!(
            sender
                .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Snapshot(bootstrap.snapshot),
                    generation: bootstrap.generation,
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
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        let mut worker = FixtureMarketWorker::try_new().expect("fixture worker validates");
        let bootstrap = worker.publish_snapshot(2).expect("snapshot publishes");
        assert!(
            sender
                .send(MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Snapshot(bootstrap.snapshot),
                    generation: bootstrap.generation,
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
                    generation: bootstrap.generation,
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
        let generation = SessionGeneration::new(NonZeroU64::MIN);
        assert!(
            sender
                .send(MarketWorkerMessage::RithmicCatalog(
                    RithmicCatalogEvent::CommandRejected {
                        session_generation: generation,
                        command_generation: NonZeroUsize::MIN,
                        reason: RithmicCatalogRejection::SupersededSearch,
                    },
                ))
                .is_ok()
        );
        let latest_generation = NonZeroUsize::new(2).expect("generation is nonzero");
        assert!(
            sender
                .send(MarketWorkerMessage::RithmicCatalog(
                    RithmicCatalogEvent::CommandRejected {
                        session_generation: generation,
                        command_generation: latest_generation,
                        reason: RithmicCatalogRejection::InstrumentUnavailable,
                    },
                ))
                .is_ok()
        );
        let (messages, disconnected) = receiver.drain();
        assert!(!disconnected);
        assert!(matches!(
            messages.as_slice(),
            [MarketWorkerMessage::RithmicCatalog(
                RithmicCatalogEvent::CommandRejected {
                    command_generation,
                    reason: RithmicCatalogRejection::InstrumentUnavailable,
                    ..
                }
            )] if *command_generation == latest_generation
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
                .send(MarketWorkerMessage::RithmicCatalog(
                    RithmicCatalogEvent::CommandRejected {
                        session_generation: generation,
                        command_generation: latest_generation,
                        reason: RithmicCatalogRejection::SubscriptionRejected,
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
    fn rithmic_catalog_and_history_commands_use_the_bounded_worker_channel() {
        let (command_tx, command_rx) = mpsc::sync_channel(3);
        let (_message_tx, message_rx) = market_worker_channel(NonZeroUsize::MIN);
        let (shutdown_tx, shutdown_rx) = mpsc::sync_channel(1);
        let worker = MarketDataWorker::from_channels(command_tx, message_rx, shutdown_rx, None);
        let search = RithmicSymbolSearch::try_new(
            NonZeroUsize::MIN,
            "ES",
            None,
            None,
            None,
            SearchPattern::Contains,
            NonZeroUsize::new(16).expect("result bound is nonzero"),
        )
        .expect("search validates");
        let selection = RithmicInstrumentSelection::try_new(
            NonZeroUsize::MIN,
            NonZeroUsize::MIN,
            "ESU6",
            "CME",
            "rithmic-test-cme",
            RithmicReadOnlySubscription::try_new(true, true, false)
                .expect("read-only subscription validates"),
        )
        .expect("selection validates");
        worker
            .try_search_rithmic(search)
            .expect("search enters the bounded channel");
        worker
            .try_select_rithmic(selection)
            .expect("selection enters the bounded channel");
        worker
            .try_request_rithmic_history(crate::rithmic_history::RithmicSeriesRequest {
                selection_generation: NonZeroUsize::MIN,
                series_generation: NonZeroUsize::MIN,
                series: crate::rithmic_history::RithmicSeries::Minute1,
            })
            .expect("history enters the bounded channel");
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::RithmicSearch(_))
        ));
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::RithmicSelect(_))
        ));
        assert!(matches!(
            command_rx.recv(),
            Ok(MarketWorkerCommand::RithmicHistory(_))
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
