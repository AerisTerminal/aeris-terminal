//! Deterministic binary fixture worker for the disconnected desktop Stage 2 path.
//!
//! This module does not implement or claim a socket, WebSocket, live provider,
//! entitlement service, or production transport. It exercises the same bounded
//! binary protocol and application model that a future connected adapter will own.

use axiusflow_application::{
    EmbeddedReplaySource, LoadEmbeddedReplay, MarketBarClientModel, MarketBarModelOutcome,
    MarketBarReplayPort, MarketGeneration, ProvenancedMarketBar, ReplayProvenance, ReplaySnapshot,
    ReplayStreamUpdate,
};
use axiusflow_chart_integration::ReplayRecoveryCommand;
use axiusflow_market_protocol_adapter::{
    BinaryMarketBarStreamDecoder, DecimalConvention, ProjectedMarketBarUpdate,
    encode_market_bar_stream_frame, try_encode_replay_delta_envelope,
    try_encode_replay_snapshot_chunk_envelopes,
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
    time::Duration,
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
}

pub(crate) enum MarketWorkerMessage {
    Update(MarketWorkerPublication),
    Recovery {
        request_id: u64,
        result: Result<MarketWorkerBootstrap, String>,
    },
    State {
        state: ChartState,
        message: String,
    },
}

struct MarketWorkerMailbox {
    queue: Mutex<VecDeque<MarketWorkerMessage>>,
    capacity: usize,
    sender_count: AtomicUsize,
    receiver_alive: AtomicBool,
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
            return Ok(());
        }
        if matches!(&message, MarketWorkerMessage::State { .. })
            && matches!(queue.back(), Some(MarketWorkerMessage::State { .. }))
        {
            queue.pop_back();
            queue.push_back(message);
            return Ok(());
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
            let successful_recovery_queued = queue.iter().any(|queued| {
                matches!(queued, MarketWorkerMessage::Recovery { result: Ok(_), .. })
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
            if let Some(invalidation) = invalidation {
                queue.retain(|queued| matches!(queued, MarketWorkerMessage::Recovery { .. }));
                queue.push_back(invalidation);
            }
            return Ok(());
        }
        replace_overflowed_queue(&mut queue, message);
        Ok(())
    }
}

fn replace_overflowed_queue(
    queue: &mut VecDeque<MarketWorkerMessage>,
    message: MarketWorkerMessage,
) {
    let covering_snapshot = take_covering_snapshot(queue);
    queue.clear();
    match message {
        MarketWorkerMessage::Update(publication)
            if matches!(&publication.update, ReplayStreamUpdate::Delta(_)) =>
        {
            if let Some(snapshot) = covering_snapshot {
                queue.push_back(snapshot);
            }
            queue.push_back(mailbox_overflow_state());
        }
        message => queue.push_back(message),
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
                MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Snapshot(_),
                    ..
                })
            )
        })
        .and_then(|index| queue.remove(index))
}

impl MarketWorkerReceiver {
    fn drain(&self) -> (Vec<MarketWorkerMessage>, bool) {
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
    Shutdown,
}

pub(crate) struct MarketDataWorker {
    commands: Option<SyncSender<MarketWorkerCommand>>,
    messages: Option<MarketWorkerReceiver>,
    shutdown_complete: Receiver<()>,
    connected: bool,
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
            },
        ))
    }

    pub fn start_coinbase(
        product_id: String,
        history_root: PathBuf,
        ui_thread: thread::ThreadId,
    ) -> Result<(MarketWorkerStartup, Self), String> {
        crate::live_market_worker::start(product_id, history_root, ui_thread)
    }

    pub(crate) const fn from_channels(
        commands: SyncSender<MarketWorkerCommand>,
        messages: MarketWorkerReceiver,
        shutdown_complete: Receiver<()>,
    ) -> Self {
        Self {
            commands: Some(commands),
            messages: Some(messages),
            shutdown_complete,
            connected: true,
        }
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
                TrySendError::Full(MarketWorkerCommand::Shutdown)
                | TrySendError::Disconnected(MarketWorkerCommand::Shutdown) => {
                    unreachable!("recovery send errors retain the recovery command")
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
            MarketWorkerCommand::Shutdown => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChartState, FixtureMarketWorker, MarketDataWorker, MarketWorkerCommand,
        MarketWorkerMessage, MarketWorkerPublication, MarketWorkerStartup, market_worker_channel,
    };
    use axiusflow_application::ReplayStreamUpdate;
    use std::num::NonZeroUsize;
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
                MarketWorkerMessage::Update(MarketWorkerPublication {
                    update: ReplayStreamUpdate::Snapshot(_),
                    ..
                }),
                MarketWorkerMessage::State {
                    state: ChartState::Recovering,
                    message,
                }
            ] if message.contains("covering snapshot")
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
}
