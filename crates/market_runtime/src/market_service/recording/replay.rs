//! Replay of local market recordings as one runtime-owned market source.
//!
//! A replay is registered like any other provider: the coordinator owns its
//! session, demand, epochs and canonical series, and charts select a replay
//! through the catalog like any instrument. The source never connects to a
//! network or a venue, never persists anything and never reaches trading.
//!
//! Each open replay (at most `MAXIMUM_RECORDING_REPLAYS`) reads one recorded
//! session through a bounded cursor and keeps its own clock: paused, playing
//! at one of `RecordingReplaySpeed::ALL`, or ended at the recording's last
//! event. Events are re-identified as the replay instrument of the current
//! source session, so a canonical series never mixes recorded and live data.
//! A seek starts a new source session: the worker reports a replaced
//! transport, the coordinator mints a new epoch and drops every event still
//! queued under the old one by equality, and history reloads at each replay's
//! clock. History is built from the recorded trades received up to the clock;
//! it is never requested from the recorded provider and never stored.

use super::RecordedInstrument;
use super::reader::{self, CursorItem, HistoryWindow, RecordedSession, ReplayEvent, SessionCursor};
use crate::market_service::clock::Clock;
use crate::market_service::provider_session::{
    ProviderEventSink, ProviderSession, ProviderSessionError,
};
use crate::market_service::{
    ActiveWorkerGuard, Command, FormingBar, HistoryRequest, HistorySnapshot, HistorySource,
    MarketEngine, MarketStream, ProviderCatalogChannel, ProviderCatalogChannels,
    ProviderCatalogCommand, ProviderCatalogEvent, ProviderCoordinatorWake, ProviderDemand,
    ProviderEvent, ProviderEventKind, ProviderRuntimeRecord, ProviderRuntimeRegistry,
    ProviderRuntimeSpec, ProviderStartContext, ProviderTradeBatch, REALTIME_CAPACITY,
    RecordedSourceChannels,
};
use aeris_contracts::InstallProviderInstrument;
use aeris_market_data::BarPeriod;
use aeris_market_engine::fence::{Epoch, ProviderTag};
use aeris_observability::diagnostic;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// Replays that may be open at once.
pub const MAXIMUM_RECORDING_REPLAYS: usize = 4;
/// The opaque runtime identity of the replay source.
pub(in crate::market_service) const PROVIDER_ID: &str = "recording-replay";
/// The entitlement every replay instrument carries; recordings need no account.
pub(in crate::market_service) const ENTITLEMENT_ID: &str = "local-recording";
pub(in crate::market_service) const WORKER_NAME: &str = "aeris-recording-replay";
pub(in crate::market_service) const HISTORY_WORKER_NAME: &str = "aeris-recording-replay-history";
const INSTRUMENT_PREFIX: &str = "recording-replay-";
const WORKER_QUEUE_CAPACITY: usize = 32;
const CATALOG_QUEUE_CAPACITY: usize = 16;
/// Events one replay emits per worker turn before yielding to the others.
const EVENTS_PER_TURN: usize = 512;
const IDLE_WAIT: Duration = Duration::from_millis(100);
const PLAYING_WAIT: Duration = Duration::from_millis(20);
const BACKPRESSURE_WAIT: Duration = Duration::from_millis(5);
/// How long the worker waits for the coordinator to adopt a replaced session.
const REPLACEMENT_RETRY: Duration = Duration::from_secs(1);

/// Identifies one open replay for its transport controls.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RecordingReplayId(u64);

/// Playback rate relative to the recorded time.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum RecordingReplaySpeed {
    #[default]
    X1,
    X2,
    X5,
    X10,
    X50,
    X100,
}

impl RecordingReplaySpeed {
    pub const ALL: [Self; 6] = [
        Self::X1,
        Self::X2,
        Self::X5,
        Self::X10,
        Self::X50,
        Self::X100,
    ];

    #[must_use]
    pub const fn multiplier(self) -> u32 {
        match self {
            Self::X1 => 1,
            Self::X2 => 2,
            Self::X5 => 5,
            Self::X10 => 10,
            Self::X50 => 50,
            Self::X100 => 100,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingReplayPlayback {
    Playing,
    Paused,
    /// The clock reached the recording's last event; only a seek resumes it.
    Ended,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingReplayControl {
    Play,
    Pause,
    Speed(RecordingReplaySpeed),
    /// Moves the clock to this receipt time, clamped to the recording.
    Seek {
        unix_nanos: i64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordingReplayStatus {
    pub id: RecordingReplayId,
    pub source: RecordedInstrument,
    pub display_symbol: String,
    /// The instrument a chart selects to show the replay. It carries no
    /// contract terms, so trading never admits it.
    pub instrument: InstallProviderInstrument,
    pub start_unix_nanos: i64,
    pub end_unix_nanos: i64,
    pub clock_unix_nanos: i64,
    pub speed: RecordingReplaySpeed,
    pub playback: RecordingReplayPlayback,
    /// Places where recorded events are missing (dropped, evicted or damaged).
    pub gaps: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingReplayError {
    /// Recording has not been configured with a data root yet.
    NotConfigured,
    /// No recorded session of the instrument starts at that time.
    NotFound,
    /// The session holds no replayable event.
    Empty,
    /// The session's files could not be read.
    Unreadable,
    TooManyReplays,
    /// The replay was closed.
    UnknownReplay,
}

impl std::fmt::Display for RecordingReplayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotConfigured => "market recording is not configured",
            Self::NotFound => "the recorded session is no longer available",
            Self::Empty => "the recorded session holds no market events",
            Self::Unreadable => "the recorded session could not be read",
            Self::TooManyReplays => "too many recording replays are open",
            Self::UnknownReplay => "the recording replay is closed",
        })
    }
}

impl std::error::Error for RecordingReplayError {}

/// The recorded time a replay shows, advanced from the monotonic clock.
#[derive(Clone, Copy, Debug)]
struct ReplayClock {
    anchor_unix_nanos: i64,
    anchor: Instant,
    speed: RecordingReplaySpeed,
    playing: bool,
    start_unix_nanos: i64,
    end_unix_nanos: i64,
}

impl ReplayClock {
    fn paused_at_start(start_unix_nanos: i64, end_unix_nanos: i64, now: Instant) -> Self {
        Self {
            anchor_unix_nanos: start_unix_nanos,
            anchor: now,
            speed: RecordingReplaySpeed::X1,
            playing: false,
            start_unix_nanos,
            end_unix_nanos,
        }
    }

    fn position(&self, now: Instant) -> i64 {
        if !self.playing {
            return self.anchor_unix_nanos;
        }
        let elapsed = i64::try_from(now.saturating_duration_since(self.anchor).as_nanos())
            .unwrap_or(i64::MAX);
        self.anchor_unix_nanos
            .saturating_add(elapsed.saturating_mul(i64::from(self.speed.multiplier())))
            .min(self.end_unix_nanos)
    }

    fn reanchor(&mut self, now: Instant) {
        self.anchor_unix_nanos = self.position(now);
        self.anchor = now;
    }

    fn play(&mut self, now: Instant) {
        if !self.playing {
            self.anchor = now;
            self.playing = true;
        }
    }

    fn pause(&mut self, now: Instant) {
        self.reanchor(now);
        self.playing = false;
    }

    fn set_speed(&mut self, speed: RecordingReplaySpeed, now: Instant) {
        self.reanchor(now);
        self.speed = speed;
    }

    fn seek(&mut self, unix_nanos: i64, now: Instant) {
        self.anchor_unix_nanos = unix_nanos.clamp(self.start_unix_nanos, self.end_unix_nanos);
        self.anchor = now;
    }

    fn finish(&mut self, now: Instant) {
        self.anchor_unix_nanos = self.end_unix_nanos;
        self.anchor = now;
        self.playing = false;
    }

    /// Monotonic time until the clock reaches `unix_nanos`.
    fn until(&self, unix_nanos: i64, now: Instant) -> Duration {
        let remaining = u64::try_from(unix_nanos.saturating_sub(self.position(now))).unwrap_or(0);
        Duration::from_nanos(remaining / u64::from(self.speed.multiplier()))
    }
}

struct ReplaySlot {
    id: RecordingReplayId,
    symbol: String,
    source: RecordedInstrument,
    session: RecordedSession,
    clock: ReplayClock,
    /// Changes whenever the clock jumps, so the worker reopens its cursor.
    revision: u64,
    gaps: u64,
    ended: bool,
}

impl ReplaySlot {
    fn status(&self, now: Instant) -> RecordingReplayStatus {
        RecordingReplayStatus {
            id: self.id,
            source: self.source.clone(),
            display_symbol: self.session.instrument.display_symbol.clone(),
            instrument: self.instrument(),
            start_unix_nanos: self.session.start_unix_nanos,
            end_unix_nanos: self.session.end_unix_nanos,
            clock_unix_nanos: self.clock.position(now),
            speed: self.clock.speed,
            playback: if self.ended {
                RecordingReplayPlayback::Ended
            } else if self.clock.playing {
                RecordingReplayPlayback::Playing
            } else {
                RecordingReplayPlayback::Paused
            },
            gaps: self.gaps,
        }
    }

    /// The instrument a chart installs for this replay. Its session joins the
    /// coordinator's current source session when a selection resolves.
    fn instrument(&self) -> InstallProviderInstrument {
        let frame = &self.session.instrument;
        InstallProviderInstrument {
            provider: PROVIDER_ID.to_owned(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: self.symbol.clone(),
            provider_symbol: self.symbol.clone(),
            display_symbol: frame.display_symbol.clone(),
            venue_id: frame.venue_id.clone(),
            price_scale: frame.price_scale,
            quantity_scale: frame.quantity_scale,
            entitlement_id: ENTITLEMENT_ID.to_owned(),
            price_increment: frame.price_increment,
            contract_metadata: None,
        }
    }
}

#[derive(Default)]
struct ReplayTable {
    next_id: u64,
    slots: Vec<ReplaySlot>,
    /// Counts seeks; each one replaces the source session.
    seeks: u64,
}

/// The runtime's one owner of open replays, shared by the market service
/// handle, the replay worker, its history worker and its catalog.
pub(in crate::market_service) struct RecordingReplays {
    clock: Arc<dyn Clock>,
    recording: Arc<super::MarketRecording>,
    table: Mutex<ReplayTable>,
    worker: Mutex<Option<SyncSender<WorkerMessage>>>,
    /// Set while a wake is pending, so controls queue at most one wake.
    woken: AtomicBool,
}

impl RecordingReplays {
    pub(in crate::market_service) fn new(
        recording: Arc<super::MarketRecording>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            clock,
            recording,
            table: Mutex::default(),
            worker: Mutex::default(),
            woken: AtomicBool::new(false),
        }
    }

    fn table(&self) -> MutexGuard<'_, ReplayTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn attach_worker(&self, worker: SyncSender<WorkerMessage>) {
        *self.worker.lock().unwrap_or_else(PoisonError::into_inner) = Some(worker);
    }

    fn wake_worker(&self) {
        if self.woken.swap(true, Ordering::AcqRel) {
            return;
        }
        let worker = self.worker.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(worker) = worker.as_ref()
            && let Err(TrySendError::Disconnected(_)) = worker.try_send(WorkerMessage::Wake)
        {
            diagnostic!("Aeris recording replay worker is unavailable");
        }
    }

    /// Indexes the session's segments on the calling thread, so callers keep
    /// it off the UI thread.
    fn open(
        &self,
        instrument: &RecordedInstrument,
        session_started_unix_millis: u64,
    ) -> Result<RecordingReplayStatus, RecordingReplayError> {
        let directory = self
            .recording
            .recordings_directory()
            .ok_or(RecordingReplayError::NotConfigured)?;
        if self.table().slots.len() >= MAXIMUM_RECORDING_REPLAYS {
            return Err(RecordingReplayError::TooManyReplays);
        }
        let session = reader::open_session(directory, instrument, session_started_unix_millis)?;
        let now = self.clock.monotonic();
        let mut table = self.table();
        if table.slots.len() >= MAXIMUM_RECORDING_REPLAYS {
            return Err(RecordingReplayError::TooManyReplays);
        }
        table.next_id += 1;
        let id = RecordingReplayId(table.next_id);
        let slot = ReplaySlot {
            id,
            symbol: format!("{INSTRUMENT_PREFIX}{}", id.0),
            source: instrument.clone(),
            clock: ReplayClock::paused_at_start(
                session.start_unix_nanos,
                session.end_unix_nanos,
                now,
            ),
            session,
            revision: 0,
            gaps: 0,
            ended: false,
        };
        let status = slot.status(now);
        table.slots.push(slot);
        drop(table);
        self.wake_worker();
        Ok(status)
    }

    fn control(
        &self,
        id: RecordingReplayId,
        control: RecordingReplayControl,
    ) -> Result<RecordingReplayStatus, RecordingReplayError> {
        let now = self.clock.monotonic();
        let mut table = self.table();
        let ReplayTable { slots, seeks, .. } = &mut *table;
        let slot = slots
            .iter_mut()
            .find(|slot| slot.id == id)
            .ok_or(RecordingReplayError::UnknownReplay)?;
        match control {
            RecordingReplayControl::Play if !slot.ended => slot.clock.play(now),
            RecordingReplayControl::Play => {}
            RecordingReplayControl::Pause => slot.clock.pause(now),
            RecordingReplayControl::Speed(speed) => slot.clock.set_speed(speed, now),
            RecordingReplayControl::Seek { unix_nanos } => {
                slot.clock.seek(unix_nanos, now);
                slot.ended = false;
                slot.revision += 1;
                *seeks += 1;
            }
        }
        let status = slot.status(now);
        drop(table);
        self.wake_worker();
        Ok(status)
    }

    fn close(&self, id: RecordingReplayId) -> Result<(), RecordingReplayError> {
        let mut table = self.table();
        let before = table.slots.len();
        table.slots.retain(|slot| slot.id != id);
        if table.slots.len() == before {
            return Err(RecordingReplayError::UnknownReplay);
        }
        drop(table);
        self.wake_worker();
        Ok(())
    }

    fn statuses(&self) -> Vec<RecordingReplayStatus> {
        let now = self.clock.monotonic();
        self.table()
            .slots
            .iter()
            .map(|slot| slot.status(now))
            .collect()
    }

    fn instrument(&self, symbol: &str) -> Option<InstallProviderInstrument> {
        self.table()
            .slots
            .iter()
            .find(|slot| slot.symbol == symbol)
            .map(ReplaySlot::instrument)
    }

    /// The session and clock a history request reads.
    fn history_view(&self, symbol: &str) -> Option<(RecordedSession, i64)> {
        let now = self.clock.monotonic();
        self.table()
            .slots
            .iter()
            .find(|slot| slot.symbol == symbol)
            .map(|slot| (slot.session.clone(), slot.clock.position(now)))
    }
}

impl crate::market_service::MarketService {
    /// Opens one recorded session for replay, paused at its first event. The
    /// recording's segment index is read on the calling thread.
    ///
    /// # Errors
    /// Returns an error before recording is configured, when the session is
    /// missing, empty or unreadable, or while `MAXIMUM_RECORDING_REPLAYS` are open.
    pub fn open_recording_replay(
        &self,
        instrument: &RecordedInstrument,
        session_started_unix_millis: u64,
    ) -> Result<RecordingReplayStatus, RecordingReplayError> {
        self.runtime
            .recording_replays
            .open(instrument, session_started_unix_millis)
    }

    /// Plays, pauses, changes the speed of or seeks one replay. A seek starts
    /// a new source session, so every replay chart reloads at its own clock.
    ///
    /// # Errors
    /// Returns an error when the replay is closed.
    pub fn control_recording_replay(
        &self,
        id: RecordingReplayId,
        control: RecordingReplayControl,
    ) -> Result<RecordingReplayStatus, RecordingReplayError> {
        self.runtime.recording_replays.control(id, control)
    }

    /// Closes one replay; its instrument stops receiving events.
    ///
    /// # Errors
    /// Returns an error when the replay is already closed.
    pub fn close_recording_replay(
        &self,
        id: RecordingReplayId,
    ) -> Result<(), RecordingReplayError> {
        self.runtime.recording_replays.close(id)
    }

    /// Every open replay at its current clock; never touches the disk.
    #[must_use]
    pub fn recording_replays(&self) -> Vec<RecordingReplayStatus> {
        self.runtime.recording_replays.statuses()
    }
}

/// Recorded streams one replay instrument delivers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(in crate::market_service) struct ReplayStreams {
    trades: bool,
    quotes: bool,
    depth: bool,
}

fn replay_demand(demand: &ProviderDemand) -> BTreeMap<String, ReplayStreams> {
    demand
        .instruments
        .iter()
        .filter_map(|requested| {
            let streams = ReplayStreams {
                trades: requested.streams.contains(MarketStream::Bars)
                    || requested.streams.contains(MarketStream::Trades)
                    || requested.streams.contains(MarketStream::Depth),
                quotes: requested.streams.contains(MarketStream::Quotes),
                depth: requested.streams.contains(MarketStream::Depth),
            };
            (streams != ReplayStreams::default())
                .then(|| (requested.instrument.instrument_id.clone(), streams))
        })
        .collect()
}

pub(in crate::market_service) enum WorkerMessage {
    Connect(ProviderEventSink),
    Rebind(ProviderEventSink),
    Subscribe(BTreeMap<String, ReplayStreams>),
    Stop,
    Wake,
}

fn try_message<T>(sender: &SyncSender<T>, message: T) -> Result<bool, ProviderSessionError> {
    match sender.try_send(message) {
        Ok(()) => Ok(true),
        Err(TrySendError::Full(_)) => Ok(false),
        Err(TrySendError::Disconnected(_)) => Err(ProviderSessionError::Unavailable),
    }
}

/// The coordinator's session for the replay source.
pub(in crate::market_service) struct ReplaySession {
    pub(in crate::market_service) controls: SyncSender<WorkerMessage>,
    pub(in crate::market_service) history: SyncSender<HistoryRequest>,
}

impl ProviderSession for ReplaySession {
    fn adapter_labels_sessions(&self) -> bool {
        true
    }

    fn connect(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        debug_assert_eq!(epoch, sink.epoch);
        try_message(&self.controls, WorkerMessage::Connect(sink))
    }

    fn rebind(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        debug_assert_eq!(epoch, sink.epoch);
        try_message(&self.controls, WorkerMessage::Rebind(sink))
    }

    fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError> {
        try_message(
            &self.controls,
            WorkerMessage::Subscribe(replay_demand(demand)),
        )
    }

    fn request_history(&self, request: HistoryRequest) -> Result<bool, ProviderSessionError> {
        try_message(&self.history, request)
    }

    fn release_idle(&self) -> Result<bool, ProviderSessionError> {
        try_message(&self.controls, WorkerMessage::Stop)
    }

    fn shutdown(&self) {
        if self.release_idle().is_err() {
            diagnostic!("Aeris recording replay worker stopped before shutdown");
        }
    }
}

/// Trade-built history read from the recording up to each replay's clock.
struct ReplayHistory {
    replays: Arc<RecordingReplays>,
}

impl HistorySource for ReplayHistory {
    fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
        let instrument = request
            .instrument
            .as_ref()
            .ok_or_else(|| "Recording replay instrument is not installed".to_string())?;
        let BarPeriod::Time { seconds } = request.series.period else {
            return Err("Recording replay serves time-based bars only".to_string());
        };
        let (session, through_unix_nanos) = self
            .replays
            .history_view(&instrument.instrument_id)
            .ok_or_else(|| RecordingReplayError::UnknownReplay.to_string())?;
        let history = reader::trade_history(
            &session,
            &HistoryWindow {
                period_seconds: i64::from(seconds),
                maximum_bars: request.maximum_bars,
                range: request
                    .range
                    .map(|range| (range.start_unix_nanos, range.end_unix_nanos)),
                through_unix_nanos,
            },
        );
        let scale = |value: u32| {
            u8::try_from(value).map_err(|_| "Recording replay scale is invalid".to_string())
        };
        Ok(HistorySnapshot {
            price_scale: scale(instrument.price_scale)?,
            quantity_scale: scale(instrument.quantity_scale)?,
            bars: history.bars,
            forming: history.forming.map(|(bar, trades)| FormingBar {
                bar,
                trades: Some(trades),
            }),
            handoff_boundary_unix_nanos: history.handoff_boundary_unix_nanos,
            backwards_exhausted: history.backwards_exhausted,
        })
    }
}

/// Resolves a chart's selection of an open replay; replays are not searchable.
struct ReplayCatalog {
    replays: Arc<RecordingReplays>,
    resolved: Receiver<ProviderCatalogEvent>,
    publish: SyncSender<ProviderCatalogEvent>,
    wake: ProviderCoordinatorWake,
}

impl ProviderCatalogChannel for ReplayCatalog {
    fn try_recv(&self) -> Option<ProviderCatalogEvent> {
        self.resolved.try_recv().ok()
    }

    fn send(&self, _: &str, command: ProviderCatalogCommand) -> Result<(), String> {
        let event = match command {
            // A chart restoring a replay instrument resolves it by its exact symbol;
            // replays are not listed in any symbol search.
            ProviderCatalogCommand::Search(search) => {
                ProviderCatalogEvent::SearchCompleted(crate::ProviderInstrumentSearchResult {
                    consumer_id: search.consumer_id,
                    provider: PROVIDER_ID.to_owned(),
                    provider_generation: 0,
                    search_generation: search.search_generation,
                    instruments: self
                        .replays
                        .instrument(&search.query)
                        .map(|instrument| crate::ProviderInstrumentSummary {
                            symbol: instrument.provider_symbol,
                            display_symbol: instrument.display_symbol,
                            exchange: instrument.venue_id,
                            ..crate::ProviderInstrumentSummary::default()
                        })
                        .into_iter()
                        .collect(),
                })
            }
            ProviderCatalogCommand::Select(selection) => {
                let instrument = self
                    .replays
                    .instrument(&selection.symbol)
                    .filter(|_| selection.entitlement_id == ENTITLEMENT_ID)
                    .ok_or_else(|| RecordingReplayError::UnknownReplay.to_string())?;
                ProviderCatalogEvent::SelectionResolved {
                    consumer_id: selection.consumer_id,
                    command_generation: selection.selection_generation,
                    instrument,
                }
            }
            ProviderCatalogCommand::Screen(_) | ProviderCatalogCommand::Scan(_) => {
                return Err("Recording replays open from the market recording list".to_string());
            }
        };
        self.publish
            .try_send(event)
            .map_err(|_| "Recording replay catalog queue is full".to_string())?;
        self.wake.notify();
        Ok(())
    }
}
/// Starts the replay source: its history worker and its playback worker.
pub(in crate::market_service) fn start_runtime(
    mut spec: ProviderRuntimeSpec,
    completions: &SyncSender<Command>,
    wake: ProviderCoordinatorWake,
    _engine: &MarketEngine,
    active_workers: &Arc<Mutex<BTreeSet<String>>>,
    context: &ProviderStartContext,
) -> Result<ProviderRuntimeRecord, String> {
    let replays = Arc::clone(&context.recording_replays);
    spec.history = Some(Box::new(ReplayHistory {
        replays: Arc::clone(&replays),
    }));
    let descriptor = spec.descriptor;
    let mut started =
        ProviderRuntimeRegistry::start_history_runtime(spec, completions, active_workers)?;
    let (controls, messages) = mpsc::sync_channel(WORKER_QUEUE_CAPACITY);
    let (events_tx, events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let (publish, resolved) = mpsc::sync_channel(CATALOG_QUEUE_CAPACITY);
    replays.attach_worker(controls.clone());
    let worker = ReplayWorker {
        replays: Arc::clone(&replays),
        messages,
        events: events_tx,
        wake: wake.clone(),
        stop: Arc::clone(&started.cancellation),
        sink: None,
        wire_generation: 0,
        replacing: None,
        demand: BTreeMap::new(),
        cursors: BTreeMap::new(),
        outgoing: VecDeque::new(),
        seeks_seen: 0,
    };
    let activity = Arc::clone(active_workers);
    let lifecycle = Arc::clone(&started.lifecycle);
    let spawned = thread::Builder::new()
        .name(WORKER_NAME.to_owned())
        .spawn(move || {
            let _activity = ActiveWorkerGuard::register(WORKER_NAME, activity);
            let stop = Arc::clone(&worker.stop);
            worker.run();
            if !stop.load(Ordering::Acquire) {
                lifecycle.mark_terminal_failure("Recording replay stopped unexpectedly");
            }
        });
    match spawned {
        Ok(handle) => started.workers.push(handle),
        Err(error) => {
            started.cancel_and_join();
            return Err(error.to_string());
        }
    }
    Ok(ProviderRuntimeRecord {
        descriptor,
        history: started.history,
        cancellation: started.cancellation,
        lifecycle: started.lifecycle,
        hyperliquid: None,
        rithmic: None,
        recorded: Some(RecordedSourceChannels { controls, events }),
        catalog: ProviderCatalogChannels {
            enabled: true,
            channels: Box::new(ReplayCatalog {
                replays,
                resolved,
                publish,
                wake,
            }),
        },
        workers: started.workers,
    })
}

/// The worker's reading position in one replay.
struct PlayCursor {
    revision: u64,
    cursor: SessionCursor,
    /// Source sequence of the replay's last delivered event; never reset, so
    /// sequences stay increasing across seeks and sessions.
    sequence: u64,
}

/// One replay's state copied out of the table, so reads never hold its lock.
struct SlotView {
    id: RecordingReplayId,
    symbol: String,
    revision: u64,
    session: RecordedSession,
    clock: ReplayClock,
}

struct ReplayWorker {
    replays: Arc<RecordingReplays>,
    messages: Receiver<WorkerMessage>,
    events: SyncSender<ProviderEvent>,
    wake: ProviderCoordinatorWake,
    stop: Arc<AtomicBool>,
    sink: Option<ProviderEventSink>,
    wire_generation: u64,
    /// The next source session reported as replacing the bound one, and when.
    replacing: Option<(u64, Instant)>,
    demand: BTreeMap<String, ReplayStreams>,
    cursors: BTreeMap<RecordingReplayId, PlayCursor>,
    outgoing: VecDeque<ProviderEvent>,
    seeks_seen: u64,
}

impl ReplayWorker {
    fn run(mut self) {
        let mut wait = IDLE_WAIT;
        loop {
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            match self.messages.recv_timeout(wait) {
                Ok(message) => self.apply(message),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            while let Ok(message) = self.messages.try_recv() {
                self.apply(message);
            }
            if !self.flush() {
                return;
            }
            wait = self.turn();
            if !self.flush() {
                return;
            }
        }
    }

    fn apply(&mut self, message: WorkerMessage) {
        match message {
            WorkerMessage::Connect(sink) => {
                self.wire_generation += 1;
                self.replacing = None;
                self.seeks_seen = self.replays.table().seeks;
                self.begin_session(sink);
            }
            WorkerMessage::Rebind(sink) => {
                if let Some((wire_generation, _)) = self.replacing.take() {
                    self.wire_generation = wire_generation;
                    self.begin_session(sink);
                } else {
                    // A repeated replacement report was adopted after the
                    // first; the source session continues under the newer epoch.
                    self.sink = Some(sink);
                    self.outgoing.clear();
                    self.outgoing.push_back(ProviderEvent {
                        provider: PROVIDER_ID,
                        generation: sink.epoch,
                        wire_generation: self.wire_generation,
                        kind: ProviderEventKind::Connected,
                    });
                }
            }
            WorkerMessage::Subscribe(demand) => self.demand = demand,
            WorkerMessage::Stop => {
                self.sink = None;
                self.replacing = None;
                self.demand.clear();
            }
            WorkerMessage::Wake => {}
        }
    }

    fn begin_session(&mut self, sink: ProviderEventSink) {
        self.sink = Some(sink);
        // Events queued for the previous session would be dropped as stale.
        self.outgoing.clear();
        for kind in [ProviderEventKind::Connecting, ProviderEventKind::Connected] {
            self.outgoing.push_back(ProviderEvent {
                provider: PROVIDER_ID,
                generation: sink.epoch,
                wire_generation: self.wire_generation,
                kind,
            });
        }
    }

    /// Sends queued events; returns `false` once the coordinator is gone.
    fn flush(&mut self) -> bool {
        let mut sent = false;
        while let Some(event) = self.outgoing.pop_front() {
            match self.events.try_send(event) {
                Ok(()) => sent = true,
                Err(TrySendError::Full(event)) => {
                    self.outgoing.push_front(event);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        if sent {
            self.wake.notify();
        }
        true
    }

    /// Advances every replay and returns how long to wait for the next turn.
    fn turn(&mut self) -> Duration {
        // Every turn reads the whole table, so a control made after this point
        // queues a new wake.
        self.replays.woken.store(false, Ordering::Release);
        let now = self.replays.clock.monotonic();
        let (views, seeks) = {
            let table = self.replays.table();
            let views = table
                .slots
                .iter()
                .map(|slot| SlotView {
                    id: slot.id,
                    symbol: slot.symbol.clone(),
                    revision: slot.revision,
                    session: slot.session.clone(),
                    clock: slot.clock,
                })
                .collect::<Vec<_>>();
            (views, table.seeks)
        };
        self.cursors
            .retain(|id, _| views.iter().any(|view| view.id == *id));
        if let Some(wait) = self.replace_session(seeks, now) {
            return wait;
        }
        if !self.outgoing.is_empty() {
            return BACKPRESSURE_WAIT;
        }
        let mut wait = IDLE_WAIT;
        let mut ended = Vec::new();
        let mut gaps = Vec::new();
        for view in &views {
            let (slot_wait, slot_ended, slot_gaps) = self.advance(view, now);
            if let Some(slot_wait) = slot_wait {
                wait = wait.min(slot_wait);
            }
            if slot_ended || slot_gaps > 0 {
                ended.extend(slot_ended.then_some((view.id, view.revision)));
                gaps.push((view.id, slot_gaps));
            }
        }
        if !ended.is_empty() || !gaps.is_empty() {
            let mut table = self.replays.table();
            for slot in &mut table.slots {
                if let Some((_, count)) = gaps.iter().find(|(id, _)| *id == slot.id) {
                    slot.gaps = slot.gaps.saturating_add(*count);
                }
                if ended.contains(&(slot.id, slot.revision)) {
                    slot.clock.finish(now);
                    slot.ended = true;
                }
            }
        }
        wait
    }

    /// Reports a replaced source session after a seek, and holds playback
    /// until the coordinator adopts it.
    fn replace_session(&mut self, seeks: u64, now: Instant) -> Option<Duration> {
        let sink = self.sink?;
        if let Some((wire_generation, reported)) = self.replacing {
            if now.saturating_duration_since(reported) < REPLACEMENT_RETRY {
                return Some(BACKPRESSURE_WAIT);
            }
            self.report_replacement(sink, wire_generation, now);
            return Some(BACKPRESSURE_WAIT);
        }
        if seeks == self.seeks_seen {
            return None;
        }
        self.seeks_seen = seeks;
        self.outgoing.clear();
        self.report_replacement(sink, self.wire_generation + 1, now);
        Some(BACKPRESSURE_WAIT)
    }

    fn report_replacement(&mut self, sink: ProviderEventSink, wire_generation: u64, now: Instant) {
        self.replacing = Some((wire_generation, now));
        self.outgoing.push_back(ProviderEvent {
            provider: PROVIDER_ID,
            generation: sink.epoch,
            wire_generation,
            kind: ProviderEventKind::TransportReplaced,
        });
    }

    /// Delivers one replay's events up to its clock. Returns the wait until
    /// its next event, whether it reached the end, and the gaps it passed.
    fn advance(&mut self, view: &SlotView, now: Instant) -> (Option<Duration>, bool, u64) {
        let position = view.clock.position(now);
        let cursor = self.cursors.entry(view.id).or_insert_with(|| PlayCursor {
            revision: view.revision,
            cursor: SessionCursor::after(Arc::clone(&view.session.segments), position),
            sequence: 0,
        });
        if cursor.revision != view.revision {
            cursor.revision = view.revision;
            cursor.cursor = SessionCursor::after(Arc::clone(&view.session.segments), position);
        }
        let mut gaps = 0_u64;
        let mut delivered = Vec::new();
        let mut wait = None;
        let mut ended = false;
        loop {
            if delivered.len() >= EVENTS_PER_TURN {
                wait = Some(Duration::ZERO);
                break;
            }
            match cursor.cursor.peek_unix_nanos() {
                None => {
                    ended = view.clock.playing;
                    break;
                }
                Some(at) if at > position => {
                    if view.clock.playing {
                        wait = Some(view.clock.until(at, now).min(PLAYING_WAIT));
                    }
                    break;
                }
                Some(_) => match cursor.cursor.pop() {
                    Some(CursorItem::Gap) => gaps += 1,
                    Some(CursorItem::Event(event)) => delivered.push(event),
                    None => break,
                },
            }
        }
        for event in delivered {
            self.deliver(view, event);
        }
        (wait, ended, gaps)
    }

    fn deliver(&mut self, view: &SlotView, mut event: ReplayEvent) {
        let Some(sink) = self.sink else {
            return;
        };
        let Some(streams) = self.demand.get(&view.symbol).copied() else {
            return;
        };
        let wanted = match &event {
            ReplayEvent::Trade(_) => streams.trades,
            ReplayEvent::Quote(_) => streams.quotes,
            ReplayEvent::Depth(_) => streams.depth,
        };
        let Some(cursor) = self.cursors.get_mut(&view.id).filter(|_| wanted) else {
            return;
        };
        cursor.sequence += 1;
        let metadata = match &mut event {
            ReplayEvent::Trade(trade) => &mut trade.metadata,
            ReplayEvent::Quote(quote) => &mut quote.metadata,
            ReplayEvent::Depth(depth) => &mut depth.metadata,
        };
        PROVIDER_ID.clone_into(&mut metadata.provider_id);
        metadata.instrument_id.clone_from(&view.symbol);
        ENTITLEMENT_ID.clone_into(&mut metadata.entitlement_id);
        metadata.session_generation = self.wire_generation;
        metadata.source_sequence = cursor.sequence;
        let kind = match event {
            ReplayEvent::Trade(trade) => ProviderEventKind::Trades(ProviderTradeBatch::One(trade)),
            ReplayEvent::Quote(quote) => ProviderEventKind::Quote(quote),
            ReplayEvent::Depth(depth) => ProviderEventKind::Depth(depth),
        };
        self.outgoing.push_back(ProviderEvent {
            provider: PROVIDER_ID,
            generation: sink.epoch,
            wire_generation: self.wire_generation,
            kind,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{INSTRUMENT, TemporaryDirectory, recorded, trade};
    use super::super::writer::RecordedEvent;
    use super::super::writer::test_sessions::{SESSION_MILLIS, SESSION_UNIX_NANOS, record_session};
    use super::super::{
        DEFAULT_RECORDING_CAPACITY_BYTES, MarketRecording, MarketRecordingSettings,
        RECORDINGS_DIRECTORY,
    };
    use super::*;
    use crate::market_service::clock::ScriptedClock;
    use aeris_market_data::BarSeriesKey;
    use aeris_market_engine::ProviderGeneration;
    use aeris_market_engine::fence::EpochSequence;
    use std::num::NonZeroU64;

    const SECOND: i64 = 1_000_000_000;

    fn at(seconds: i64) -> i64 {
        SESSION_UNIX_NANOS + seconds * SECOND
    }

    fn trade_at(seconds: i64) -> (RecordedEvent, u64) {
        let mut recorded = trade(INSTRUMENT, u64::try_from(seconds).expect("positive"));
        recorded.price = 100 + seconds;
        let timestamps = &mut recorded.metadata.timestamps;
        timestamps.received_unix_nanos = at(seconds);
        timestamps.exchange_unix_nanos = Some(at(seconds));
        timestamps.provider_unix_nanos = None;
        (RecordedEvent::Trade(recorded), 0)
    }

    #[derive(Debug, Eq, PartialEq)]
    enum Seen {
        Connecting,
        Connected,
        Replaced {
            wire_generation: u64,
        },
        Trade {
            second: i64,
            session: u64,
            sequence: u64,
        },
        Other,
    }

    struct Harness {
        clock: Arc<ScriptedClock>,
        recording: Arc<MarketRecording>,
        replays: Arc<RecordingReplays>,
        worker: ReplayWorker,
        events: Receiver<ProviderEvent>,
        epochs: EpochSequence<ProviderTag>,
        _directory: TemporaryDirectory,
    }

    impl Harness {
        /// A replay worker over one recorded session of trades at `seconds`.
        fn new(label: &str, seconds: std::ops::Range<i64>) -> Self {
            let directory = TemporaryDirectory::new(label);
            record_session(
                &directory.0.join(RECORDINGS_DIRECTORY),
                1_000_000,
                seconds.map(trade_at).collect(),
            );
            let clock = Arc::new(ScriptedClock::new(at(1_000)));
            let recording = Arc::new(MarketRecording::new(clock.clone()));
            recording
                .configure(MarketRecordingSettings {
                    data_root: directory.0.clone(),
                    instruments: Vec::new(),
                    capacity_bytes: DEFAULT_RECORDING_CAPACITY_BYTES,
                })
                .expect("recording configures");
            let replays = Arc::new(RecordingReplays::new(Arc::clone(&recording), clock.clone()));
            let (controls, messages) = mpsc::sync_channel(WORKER_QUEUE_CAPACITY);
            replays.attach_worker(controls);
            let (events_tx, events) = mpsc::sync_channel(REALTIME_CAPACITY);
            let worker = ReplayWorker {
                replays: Arc::clone(&replays),
                messages,
                events: events_tx,
                wake: ProviderCoordinatorWake::for_tests(),
                stop: Arc::default(),
                sink: None,
                wire_generation: 0,
                replacing: None,
                demand: BTreeMap::new(),
                cursors: BTreeMap::new(),
                outgoing: VecDeque::new(),
                seeks_seen: 0,
            };
            Self {
                clock,
                recording,
                replays,
                worker,
                events,
                epochs: EpochSequence::default(),
                _directory: directory,
            }
        }

        fn open(&self) -> RecordingReplayStatus {
            self.replays
                .open(&recorded(INSTRUMENT), SESSION_MILLIS)
                .expect("replay opens")
        }

        fn sink(&mut self) -> ProviderEventSink {
            ProviderEventSink {
                epoch: self.epochs.next(),
            }
        }

        fn connect(&mut self, symbol: &str) -> ProviderEventSink {
            let sink = self.sink();
            self.worker.apply(WorkerMessage::Connect(sink));
            self.worker.apply(WorkerMessage::Subscribe(BTreeMap::from([(
                symbol.to_owned(),
                ReplayStreams {
                    trades: true,
                    quotes: true,
                    depth: true,
                },
            )])));
            sink
        }

        fn control(
            &self,
            id: RecordingReplayId,
            control: RecordingReplayControl,
        ) -> RecordingReplayStatus {
            self.replays.control(id, control).expect("replay is open")
        }

        fn advance(&self, seconds: u64) {
            self.clock.advance(Duration::from_secs(seconds));
        }

        /// Runs one worker turn and returns what reached the coordinator.
        fn step(&mut self) -> Vec<(Epoch<ProviderTag>, Seen)> {
            assert!(self.worker.flush());
            self.worker.turn();
            assert!(self.worker.flush());
            self.events
                .try_iter()
                .map(|event| {
                    let seen = match event.kind {
                        ProviderEventKind::Connecting => Seen::Connecting,
                        ProviderEventKind::Connected => Seen::Connected,
                        ProviderEventKind::TransportReplaced => Seen::Replaced {
                            wire_generation: event.wire_generation,
                        },
                        ProviderEventKind::Trades(ProviderTradeBatch::One(trade)) => {
                            assert_eq!(trade.metadata.provider_id, PROVIDER_ID);
                            assert_eq!(trade.metadata.entitlement_id, ENTITLEMENT_ID);
                            assert_eq!(trade.metadata.session_generation, event.wire_generation);
                            Seen::Trade {
                                second: (trade.metadata.timestamps.received_unix_nanos
                                    - SESSION_UNIX_NANOS)
                                    / SECOND,
                                session: event.wire_generation,
                                sequence: trade.metadata.source_sequence,
                            }
                        }
                        _ => Seen::Other,
                    };
                    (event.generation, seen)
                })
                .collect()
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.recording.close();
        }
    }

    fn trades(
        epoch: Epoch<ProviderTag>,
        session: u64,
        seconds: std::ops::RangeInclusive<i64>,
        first_sequence: u64,
    ) -> Vec<(Epoch<ProviderTag>, Seen)> {
        seconds
            .zip(first_sequence..)
            .map(|(second, sequence)| {
                (
                    epoch,
                    Seen::Trade {
                        second,
                        session,
                        sequence,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn the_clock_scales_elapsed_time_by_speed_and_pauses() {
        let start = Instant::now();
        let later = |seconds| start + Duration::from_secs(seconds);
        let mut clock = ReplayClock::paused_at_start(0, 1_000 * SECOND, start);
        assert_eq!(clock.position(later(5)), 0, "a new replay is paused");
        clock.play(later(5));
        assert_eq!(clock.position(later(7)), 2 * SECOND);
        clock.set_speed(RecordingReplaySpeed::X10, later(7));
        assert_eq!(clock.position(later(8)), 12 * SECOND);
        assert_eq!(clock.until(22 * SECOND, later(8)), Duration::from_secs(1));
        clock.pause(later(8));
        assert_eq!(clock.position(later(50)), 12 * SECOND, "paused time stands");
        clock.set_speed(RecordingReplaySpeed::X100, later(50));
        clock.play(later(50));
        assert_eq!(clock.position(later(51)), 112 * SECOND);
        assert_eq!(
            clock.position(later(70)),
            1_000 * SECOND,
            "the clock stops at the end"
        );
        clock.seek(-SECOND, later(70));
        assert_eq!(
            clock.position(later(70)),
            0,
            "a seek clamps to the recording"
        );
        clock.seek(500 * SECOND, later(70));
        assert_eq!(
            clock.position(later(71)),
            600 * SECOND,
            "a seek keeps playing"
        );
        let multipliers = RecordingReplaySpeed::ALL.map(RecordingReplaySpeed::multiplier);
        assert_eq!(multipliers, [1, 2, 5, 10, 50, 100]);
    }

    #[test]
    fn playback_follows_the_clock_pauses_and_ends_at_the_last_event() {
        let mut harness = Harness::new("replay-playback", 0..10);
        let status = harness.open();
        assert_eq!(status.playback, RecordingReplayPlayback::Paused);
        assert_eq!(
            (
                status.start_unix_nanos,
                status.end_unix_nanos,
                status.clock_unix_nanos
            ),
            (at(0), at(9), at(0))
        );
        let sink = harness.connect(&status.instrument.instrument_id);
        assert_eq!(
            harness.step(),
            [
                (sink.epoch, Seen::Connecting),
                (sink.epoch, Seen::Connected)
            ],
            "the first trade is history at the clock, never replayed live"
        );
        harness.advance(5);
        assert_eq!(harness.step(), [], "a paused replay delivers nothing");

        harness.control(status.id, RecordingReplayControl::Play);
        harness.advance(2);
        assert_eq!(harness.step(), trades(sink.epoch, 1, 1..=2, 1));
        harness.control(
            status.id,
            RecordingReplayControl::Speed(RecordingReplaySpeed::X5),
        );
        harness.advance(1);
        assert_eq!(harness.step(), trades(sink.epoch, 1, 3..=7, 3));

        let paused = harness.control(status.id, RecordingReplayControl::Pause);
        harness.advance(10);
        assert_eq!(harness.step(), []);
        assert_eq!(paused.clock_unix_nanos, at(7));
        assert_eq!(harness.replays.statuses()[0].clock_unix_nanos, at(7));

        harness.control(status.id, RecordingReplayControl::Play);
        harness.advance(3);
        assert_eq!(harness.step(), trades(sink.epoch, 1, 8..=9, 8));
        let ended = harness.replays.statuses()[0].clone();
        assert_eq!(ended.playback, RecordingReplayPlayback::Ended);
        assert_eq!(ended.clock_unix_nanos, at(9));
        let resumed = harness.control(status.id, RecordingReplayControl::Play);
        assert_eq!(
            resumed.playback,
            RecordingReplayPlayback::Ended,
            "only a seek resumes"
        );
        harness.advance(60);
        assert_eq!(harness.step(), []);

        let rewound = harness.control(
            status.id,
            RecordingReplayControl::Seek { unix_nanos: at(0) },
        );
        assert_eq!(rewound.playback, RecordingReplayPlayback::Paused);
        harness.replays.close(status.id).expect("replay closes");
        assert_eq!(
            harness.replays.close(status.id),
            Err(RecordingReplayError::UnknownReplay)
        );
        assert!(harness.worker.cursors.contains_key(&status.id));
        harness.step();
        assert!(
            harness.worker.cursors.is_empty(),
            "a closed replay is forgotten"
        );
    }

    #[test]
    fn a_seek_replaces_the_source_session_and_resumes_under_the_new_epoch() {
        let mut harness = Harness::new("replay-seek", 0..20);
        let status = harness.open();
        let first = harness.connect(&status.instrument.instrument_id);
        harness.step();
        harness.control(status.id, RecordingReplayControl::Play);
        harness.advance(3);
        assert_eq!(harness.step(), trades(first.epoch, 1, 1..=3, 1));

        harness.control(
            status.id,
            RecordingReplayControl::Seek { unix_nanos: at(1) },
        );
        assert_eq!(
            harness.step(),
            [(first.epoch, Seen::Replaced { wire_generation: 2 })],
            "playback holds until the coordinator adopts the new session"
        );
        harness.advance(2);
        assert_eq!(
            harness.step(),
            [(first.epoch, Seen::Replaced { wire_generation: 2 })],
            "an unanswered replacement is reported again"
        );

        let second = harness.sink();
        harness.worker.apply(WorkerMessage::Rebind(second));
        assert_eq!(
            harness.step(),
            [
                (second.epoch, Seen::Connecting),
                (second.epoch, Seen::Connected)
            ],
            "trades up to the clock reload as history at the new epoch"
        );
        harness.advance(1);
        assert_eq!(
            harness.step(),
            trades(second.epoch, 2, 4..=4, 4),
            "the replay resumes after its clock"
        );

        let third = harness.sink();
        harness.worker.apply(WorkerMessage::Rebind(third));
        harness.advance(1);
        let mut expected = vec![(third.epoch, Seen::Connected)];
        expected.extend(trades(third.epoch, 2, 5..=5, 5));
        assert_eq!(
            harness.step(),
            expected,
            "a repeated adoption keeps the session under the newer epoch"
        );
    }

    #[test]
    fn history_comes_from_recorded_trades_up_to_the_replay_clock() {
        let harness = Harness::new("replay-history", 0..10);
        let status = harness.open();
        harness.control(status.id, RecordingReplayControl::Play);
        harness.advance(3);
        let symbol = status.instrument.instrument_id.clone();
        let series = BarSeriesKey {
            provider_id: PROVIDER_ID.to_owned(),
            instrument_id: symbol.clone(),
            entitlement_id: ENTITLEMENT_ID.to_owned(),
            period: BarPeriod::time(60).expect("period"),
            definition_version: 1,
        };
        let request = HistoryRequest {
            ticket: crate::market_service::fixture_history_ticket(&series),
            series,
            provider_generation: ProviderGeneration(NonZeroU64::MIN),
            instrument: harness.replays.instrument(&symbol),
            maximum_bars: 10,
            range: None,
            stop: Arc::default(),
        };
        let instrument = request.instrument.clone().expect("the replay instrument");
        assert_eq!(
            instrument.contract_metadata, None,
            "replays never carry trading terms"
        );
        let mut history = ReplayHistory {
            replays: Arc::clone(&harness.replays),
        };
        let snapshot = history.fetch(&request).expect("history reads");
        assert_eq!(snapshot.bars.len(), 0);
        let forming = snapshot.forming.expect("the minute at the clock forms");
        assert_eq!((forming.bar.open, forming.bar.close), (100, 103));
        assert_eq!(forming.trades, Some(4));
        assert_eq!(snapshot.handoff_boundary_unix_nanos, Some(at(3)));

        harness.replays.close(status.id).expect("replay closes");
        assert!(
            history.fetch(&request).is_err(),
            "a closed replay has no history"
        );
    }

    #[test]
    fn at_most_four_replays_are_open_at_once() {
        let harness = Harness::new("replay-bound", 0..3);
        let open = (0..MAXIMUM_RECORDING_REPLAYS)
            .map(|_| harness.open())
            .collect::<Vec<_>>();
        assert_eq!(
            harness
                .replays
                .open(&recorded(INSTRUMENT), SESSION_MILLIS)
                .err(),
            Some(RecordingReplayError::TooManyReplays)
        );
        harness.replays.close(open[0].id).expect("replay closes");
        let reopened = harness.open();
        assert!(
            open.iter()
                .all(|status| status.instrument.instrument_id != reopened.instrument.instrument_id)
        );
        assert_eq!(
            harness
                .replays
                .control(open[0].id, RecordingReplayControl::Play)
                .err(),
            Some(RecordingReplayError::UnknownReplay)
        );
        harness.replays.close(reopened.id).expect("replay closes");
        assert_eq!(
            harness
                .replays
                .open(&recorded(INSTRUMENT), SESSION_MILLIS + 1)
                .err(),
            Some(RecordingReplayError::NotFound)
        );
    }
    #[test]
    fn a_chart_resolves_an_open_replay_by_its_exact_symbol_only() {
        let harness = Harness::new("replay-catalog", 0..3);
        let status = harness.open();
        let (publish, resolved) = mpsc::sync_channel(CATALOG_QUEUE_CAPACITY);
        let catalog = ReplayCatalog {
            replays: Arc::clone(&harness.replays),
            resolved,
            publish,
            wake: ProviderCoordinatorWake::for_tests(),
        };
        let search = |query: &str| {
            catalog
                .send(
                    PROVIDER_ID,
                    ProviderCatalogCommand::Search(crate::SearchProviderInstruments {
                        consumer_id: 1,
                        search_generation: 2,
                        startup_resolve: false,
                        provider: PROVIDER_ID.to_owned(),
                        query: query.to_owned(),
                        maximum_results: 32,
                        categories: crate::InstrumentSearchCategories::ALL,
                        exchange: None,
                    }),
                )
                .expect("search is answered");
            match catalog.try_recv() {
                Some(ProviderCatalogEvent::SearchCompleted(result)) => result
                    .instruments
                    .into_iter()
                    .map(|instrument| instrument.symbol)
                    .collect::<Vec<_>>(),
                _ => panic!("a search completes"),
            }
        };
        assert_eq!(
            search(&status.instrument.instrument_id),
            std::slice::from_ref(&status.instrument.instrument_id)
        );
        assert_eq!(search(""), Vec::<String>::new(), "replays are never listed");
        let select = |entitlement_id: &str| {
            catalog.send(
                PROVIDER_ID,
                ProviderCatalogCommand::Select(crate::SelectProviderInstrument {
                    consumer_id: 1,
                    selection_generation: 3,
                    search_generation: 2,
                    provider: PROVIDER_ID.to_owned(),
                    symbol: status.instrument.instrument_id.clone(),
                    entitlement_id: entitlement_id.to_owned(),
                    ..crate::SelectProviderInstrument::default()
                }),
            )
        };
        assert!(select("another-entitlement").is_err());
        select(ENTITLEMENT_ID).expect("the open replay resolves");
        assert!(matches!(
            catalog.try_recv(),
            Some(ProviderCatalogEvent::SelectionResolved { instrument, .. })
                if instrument == status.instrument
        ));
        harness.replays.close(status.id).expect("replay closes");
        assert!(
            select(ENTITLEMENT_ID).is_err(),
            "a closed replay no longer resolves"
        );
    }
}
