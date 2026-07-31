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
    num::NonZeroUsize,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread,
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

pub(crate) type DesktopMarketGeneration = MarketGeneration<ProvenancedMarketBar>;

pub(crate) struct MarketWorkerBootstrap {
    pub snapshot: ReplaySnapshot,
    pub subscription_id: String,
    pub generation: DesktopMarketGeneration,
}

pub(crate) struct MarketWorkerPublication {
    pub update: ReplayStreamUpdate,
    pub generation: DesktopMarketGeneration,
}

pub(crate) enum MarketWorkerMessage {
    Update(MarketWorkerPublication),
    Recovery {
        request_id: u64,
        result: Result<MarketWorkerBootstrap, String>,
    },
    Failed(String),
}

pub(crate) struct MarketDataWorker {
    commands: SyncSender<ReplayRecoveryCommand>,
    messages: Receiver<MarketWorkerMessage>,
    connected: bool,
}

impl MarketDataWorker {
    pub fn start() -> Result<(MarketWorkerBootstrap, Self), String> {
        let (bootstrap_tx, bootstrap_rx) = mpsc::sync_channel(1);
        let (message_tx, message_rx) = mpsc::sync_channel(MESSAGE_CAPACITY);
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        thread::Builder::new()
            .name("axiusflow-market-fixture-worker".to_string())
            .spawn(move || run_worker(&bootstrap_tx, &message_tx, &command_rx))
            .map_err(|error| error.to_string())?;
        let bootstrap = bootstrap_rx
            .recv()
            .map_err(|_| "market fixture worker stopped before bootstrap".to_string())??;
        Ok((
            bootstrap,
            Self {
                commands: command_tx,
                messages: message_rx,
                connected: true,
            },
        ))
    }

    pub fn try_send_recovery(
        &self,
        command: ReplayRecoveryCommand,
    ) -> Result<(), TrySendError<ReplayRecoveryCommand>> {
        self.commands.try_send(command)
    }

    pub fn drain_messages(&mut self) -> (Vec<MarketWorkerMessage>, bool) {
        let mut messages = Vec::new();
        loop {
            match self.messages.try_recv() {
                Ok(message) => messages.push(message),
                Err(TryRecvError::Empty) => return (messages, false),
                Err(TryRecvError::Disconnected) => {
                    let newly_disconnected = self.connected;
                    self.connected = false;
                    return (messages, newly_disconnected);
                }
            }
        }
    }

    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    pub fn mark_disconnected(&mut self) {
        self.connected = false;
    }
}

struct FixtureMarketWorker {
    source: EmbeddedReplaySource,
    convention: DecimalConvention,
    decoder: BinaryMarketBarStreamDecoder,
    model: MarketBarClientModel,
    publisher_context: Option<ReplaySnapshot>,
    maximum_frame_bytes: NonZeroUsize,
}

impl FixtureMarketWorker {
    fn try_new() -> Result<Self, String> {
        let convention =
            DecimalConvention::try_new("usd_minor", "shares").map_err(|error| error.to_string())?;
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

    fn publish_snapshot(&mut self, bar_count: usize) -> Result<MarketWorkerBootstrap, String> {
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
        })
    }

    fn publish_delta(
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
        if projected.subscription_id != SUBSCRIPTION_ID {
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
        })
    }

    fn recover(&mut self) -> Result<MarketWorkerBootstrap, String> {
        self.decoder.reset();
        self.publish_snapshot(RECOVERY_BAR_COUNT)
    }
}

fn run_worker(
    bootstrap_tx: &SyncSender<Result<MarketWorkerBootstrap, String>>,
    message_tx: &SyncSender<MarketWorkerMessage>,
    command_rx: &Receiver<ReplayRecoveryCommand>,
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
                let _ = message_tx.send(MarketWorkerMessage::Failed(error));
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
}
