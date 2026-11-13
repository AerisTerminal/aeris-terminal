//! Opt-in local recording of live trades, quotes and aggregate depth.
//!
//! Recording is off until the user chooses instruments. The coordinator's tap
//! forwards accepted events for those instruments to the one
//! `aeris-market-recorder` thread through a bounded queue and never blocks:
//! a full queue drops the event and counts it, and the writer marks the loss
//! with a gap frame. Order-level (market-by-order) data is never recorded.
//! Files stay under the data root's `recordings` directory, rotate by size and
//! are evicted oldest first to keep the total under the user's cap.

mod format;
mod store;
mod tap;
mod writer;

pub(super) use tap::RecordingTap;

use super::clock::Clock;
use aeris_market_data::MAXIMUM_MARKET_DATA_FIELD_BYTES;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use writer::{RecorderControl, RecorderMessage};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
/// Instruments that may be recorded at once.
pub const MAXIMUM_RECORDED_INSTRUMENTS: usize = 16;
/// Total recording size used until the user chooses another cap.
pub const DEFAULT_RECORDING_CAPACITY_BYTES: u64 = 2 * GIB;
pub const MINIMUM_RECORDING_CAPACITY_BYTES: u64 = 256 * MIB;
pub const MAXIMUM_RECORDING_CAPACITY_BYTES: u64 = 64 * GIB;
/// Directory under the data root that holds every recording.
pub const RECORDINGS_DIRECTORY: &str = "recordings";
const EVENT_QUEUE_CAPACITY: usize = 4_096;
const CONTROL_QUEUE_CAPACITY: usize = 16;
/// Encoded bytes the queue may hold, so a burst of depth images stays bounded.
const MAXIMUM_IN_FLIGHT_BYTES: u64 = 8 * MIB;

/// One instrument chosen for recording, by its opaque runtime identities.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RecordedInstrument {
    pub provider_id: String,
    pub instrument_id: String,
}

/// The user's complete recording choice; an empty instrument list records nothing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketRecordingSettings {
    /// The instance's data root; recordings live in its `recordings` directory.
    pub data_root: PathBuf,
    pub instruments: Vec<RecordedInstrument>,
    pub capacity_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketRecordingError {
    TooManyInstruments,
    DuplicateInstrument,
    InvalidInstrument,
    CapacityOutOfRange,
    /// The recorder already serves another data root in this process.
    DataRootChanged,
    /// Recording has not been configured with a data root yet.
    NotConfigured,
    /// The recorder's bounded control queue is full; retry shortly.
    Busy,
    Unavailable,
}

impl std::fmt::Display for MarketRecordingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::TooManyInstruments => "too many instruments are selected for recording",
            Self::DuplicateInstrument => "an instrument is selected for recording twice",
            Self::InvalidInstrument => "a recorded instrument identity is invalid",
            Self::CapacityOutOfRange => "the recording size cap is out of range",
            Self::DataRootChanged => "the recorder already serves another data root",
            Self::NotConfigured => "market recording is not configured",
            Self::Busy => "the market recorder is busy",
            Self::Unavailable => "the market recorder is unavailable",
        })
    }
}

impl std::error::Error for MarketRecordingError {}

/// Why the recorder stopped writing until it is configured again.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketRecordingFailure {
    /// A recording file or directory could not be read, written or removed.
    StorageUnavailable,
    /// The recordings directory holds more entries than one scan may index.
    TooManyFiles,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketRecordingUsage {
    pub instrument: RecordedInstrument,
    pub display_symbol: String,
    pub bytes: u64,
    pub recording: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarketRecordingStatus {
    /// The recorder has indexed the recordings directory.
    pub ready: bool,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub instruments: Vec<MarketRecordingUsage>,
    /// Events dropped because the bounded queue was full, since start.
    pub dropped_events: u64,
    pub failure: Option<MarketRecordingFailure>,
}

/// State shared by the recording handle, the coordinator tap and the writer.
#[derive(Default)]
struct RecorderShared {
    status: Mutex<MarketRecordingStatus>,
    dropped_events: AtomicU64,
    in_flight_bytes: AtomicU64,
}

impl RecorderShared {
    fn count_drop(&self) {
        self.dropped_events.fetch_add(1, Ordering::Relaxed);
    }

    fn reserve_in_flight(&self, bytes: u64) -> bool {
        // Only the coordinator thread reserves; the writer only releases.
        let current = self.in_flight_bytes.load(Ordering::Acquire);
        if current.saturating_add(bytes) > MAXIMUM_IN_FLIGHT_BYTES {
            return false;
        }
        self.in_flight_bytes.fetch_add(bytes, Ordering::AcqRel);
        true
    }

    fn release_in_flight(&self, bytes: u64) {
        self.in_flight_bytes.fetch_sub(bytes, Ordering::AcqRel);
    }

    fn publish(&self, status: MarketRecordingStatus) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }
}

struct RecorderWorker {
    data_root: PathBuf,
    events: SyncSender<RecorderMessage>,
    controls: SyncSender<RecorderControl>,
    handle: JoinHandle<()>,
}

#[derive(Default)]
struct RecordingState {
    worker: Option<RecorderWorker>,
    instruments: Vec<RecordedInstrument>,
}

/// The runtime's one recorder: owns the writer thread and the current choice.
pub(super) struct MarketRecording {
    clock: Arc<dyn Clock>,
    revision: AtomicU64,
    shared: Arc<RecorderShared>,
    state: Mutex<RecordingState>,
}

impl MarketRecording {
    pub(super) fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            revision: AtomicU64::new(0),
            shared: Arc::default(),
            state: Mutex::default(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, RecordingState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    pub(super) fn configure(
        &self,
        settings: MarketRecordingSettings,
    ) -> Result<(), MarketRecordingError> {
        validate(&settings)?;
        let mut state = self.state();
        if let Some(worker) = &state.worker {
            if worker.data_root != settings.data_root {
                return Err(MarketRecordingError::DataRootChanged);
            }
        } else {
            state.worker = Some(self.spawn_worker(settings.data_root.clone())?);
        }
        let worker = state
            .worker
            .as_ref()
            .ok_or(MarketRecordingError::Unavailable)?;
        send_control(
            worker,
            RecorderControl::Configure {
                instruments: settings.instruments.clone(),
                capacity_bytes: settings.capacity_bytes,
            },
        )?;
        if state.instruments != settings.instruments {
            state.instruments = settings.instruments;
            self.revision.fetch_add(1, Ordering::AcqRel);
        }
        Ok(())
    }

    /// Deletes one instrument's recordings, or every recording for `None`.
    pub(super) fn delete(
        &self,
        instrument: Option<RecordedInstrument>,
    ) -> Result<(), MarketRecordingError> {
        let state = self.state();
        let worker = state
            .worker
            .as_ref()
            .ok_or(MarketRecordingError::NotConfigured)?;
        send_control(worker, RecorderControl::Delete(instrument))
    }

    pub(super) fn status(&self) -> MarketRecordingStatus {
        let mut status = self
            .shared
            .status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        status.dropped_events = self.shared.dropped_events.load(Ordering::Relaxed);
        status
    }

    fn tap_view(
        &self,
    ) -> (
        u64,
        Option<SyncSender<RecorderMessage>>,
        Vec<RecordedInstrument>,
    ) {
        let state = self.state();
        (
            self.revision(),
            state.worker.as_ref().map(|worker| worker.events.clone()),
            state.instruments.clone(),
        )
    }

    /// Stops the writer after it drains its queue and synchronizes open
    /// segments. Every event sender, the coordinator tap's included, must be
    /// gone for the writer to see the end of its queue.
    pub(super) fn close(&self) {
        let worker = self.state().worker.take();
        if let Some(RecorderWorker {
            events,
            controls,
            handle,
            ..
        }) = worker
        {
            drop(events);
            drop(controls);
            if handle.join().is_err() {
                aeris_observability::diagnostic!("Aeris market recorder panicked during shutdown");
            }
        }
    }

    fn spawn_worker(&self, data_root: PathBuf) -> Result<RecorderWorker, MarketRecordingError> {
        let (events, event_receiver) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
        let (controls, control_receiver) = mpsc::sync_channel(CONTROL_QUEUE_CAPACITY);
        let handle = writer::spawn_writer(
            data_root.join(RECORDINGS_DIRECTORY),
            Arc::clone(&self.clock),
            Arc::clone(&self.shared),
            event_receiver,
            control_receiver,
        )
        .map_err(|_| MarketRecordingError::Unavailable)?;
        Ok(RecorderWorker {
            data_root,
            events,
            controls,
            handle,
        })
    }
}

fn send_control(
    worker: &RecorderWorker,
    control: RecorderControl,
) -> Result<(), MarketRecordingError> {
    worker
        .controls
        .try_send(control)
        .map_err(|error| match error {
            TrySendError::Full(_) => MarketRecordingError::Busy,
            TrySendError::Disconnected(_) => MarketRecordingError::Unavailable,
        })?;
    // The writer applies controls between event batches; a full event queue
    // already keeps it awake.
    match worker.events.try_send(RecorderMessage::Wake) {
        Ok(()) | Err(TrySendError::Full(_)) => Ok(()),
        Err(TrySendError::Disconnected(_)) => Err(MarketRecordingError::Unavailable),
    }
}

fn validate(settings: &MarketRecordingSettings) -> Result<(), MarketRecordingError> {
    if settings.instruments.len() > MAXIMUM_RECORDED_INSTRUMENTS {
        return Err(MarketRecordingError::TooManyInstruments);
    }
    if !(MINIMUM_RECORDING_CAPACITY_BYTES..=MAXIMUM_RECORDING_CAPACITY_BYTES)
        .contains(&settings.capacity_bytes)
    {
        return Err(MarketRecordingError::CapacityOutOfRange);
    }
    let mut seen = BTreeSet::new();
    for instrument in &settings.instruments {
        let valid =
            |field: &str| !field.is_empty() && field.len() <= MAXIMUM_MARKET_DATA_FIELD_BYTES;
        if !valid(&instrument.provider_id) || !valid(&instrument.instrument_id) {
            return Err(MarketRecordingError::InvalidInstrument);
        }
        if !seen.insert(instrument) {
            return Err(MarketRecordingError::DuplicateInstrument);
        }
    }
    Ok(())
}

impl super::MarketService {
    /// Applies the user's recording choice. Recording is opt-in: until this is
    /// called with instruments nothing is written, and an empty list stops
    /// recording while keeping existing recordings. Instruments are recorded
    /// while they stream for a chart, order book or other demand.
    ///
    /// # Errors
    /// Returns an error for an invalid choice, a changed data root, or a busy
    /// or unavailable recorder.
    pub fn configure_recording(
        &self,
        settings: MarketRecordingSettings,
    ) -> Result<(), MarketRecordingError> {
        self.runtime.recording.configure(settings)
    }

    /// Deletes one instrument's recordings, or every recording for `None`.
    ///
    /// # Errors
    /// Returns an error before recording is configured or while the recorder
    /// is busy or unavailable.
    pub fn delete_recordings(
        &self,
        instrument: Option<RecordedInstrument>,
    ) -> Result<(), MarketRecordingError> {
        self.runtime.recording.delete(instrument)
    }

    /// The recorder's latest published usage; never touches the disk.
    #[must_use]
    pub fn recording_status(&self) -> MarketRecordingStatus {
        self.runtime.recording.status()
    }
}

#[cfg(test)]
mod fixtures {
    use super::RecordedInstrument;
    use aeris_contracts::InstallProviderInstrument;
    use aeris_market_data::{
        AggressorSide, DepthLevel, DepthSnapshot, EventMetadata, MarketTrade, QualifiedTimestamp,
        QuoteLevel, TopOfBookQuote,
    };
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(super) const PROVIDER: &str = "provider-a";
    pub(super) const INSTRUMENT: &str = "instrument-1";
    pub(super) const OTHER_INSTRUMENT: &str = "instrument-2";

    /// A unique directory under the system temporary directory, removed on drop.
    pub(super) struct TemporaryDirectory(pub(super) PathBuf);

    impl TemporaryDirectory {
        pub(super) fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "aeris-recording-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(super) fn recorded(instrument: &str) -> RecordedInstrument {
        RecordedInstrument {
            provider_id: PROVIDER.into(),
            instrument_id: instrument.into(),
        }
    }

    pub(super) fn install(instrument: &str) -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: PROVIDER.into(),
            session_generation: 3,
            selection_generation: 1,
            instrument_id: instrument.into(),
            provider_symbol: format!("{instrument}-symbol"),
            display_symbol: format!("{instrument} display"),
            venue_id: "VENUE".into(),
            price_scale: 2,
            quantity_scale: 0,
            entitlement_id: "entitlement".into(),
            price_increment: Some(25),
            contract_metadata: None,
        }
    }

    pub(super) fn metadata(instrument: &str, sequence: u64) -> EventMetadata {
        EventMetadata {
            provider_id: PROVIDER.into(),
            instrument_id: instrument.into(),
            entitlement_id: "entitlement".into(),
            source_sequence: sequence,
            session_generation: 3,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(
                    1_700_000_000_000_000_000 + i64::try_from(sequence).unwrap_or(0),
                ),
                provider_unix_nanos: Some(1_700_000_000_000_000_100),
                received_unix_nanos: 1_700_000_000_000_000_200,
            },
        }
    }

    pub(super) fn trade(instrument: &str, sequence: u64) -> MarketTrade {
        MarketTrade {
            metadata: metadata(instrument, sequence),
            trade_id: format!("t{sequence}"),
            price: 512_525,
            quantity: 2,
            aggressor: AggressorSide::Buy,
        }
    }

    pub(super) fn quote(instrument: &str, sequence: u64) -> TopOfBookQuote {
        TopOfBookQuote {
            metadata: metadata(instrument, sequence),
            bid: Some(QuoteLevel {
                price: 512_500,
                quantity: Some(5),
                order_count: None,
            }),
            ask: Some(QuoteLevel {
                price: 512_525,
                quantity: Some(7),
                order_count: Some(2),
            }),
        }
    }

    pub(super) fn depth(instrument: &str, sequence: u64) -> DepthSnapshot {
        DepthSnapshot {
            metadata: metadata(instrument, sequence),
            bids: vec![DepthLevel {
                price: 512_500,
                quantity: 5,
                order_count: Some(1),
            }],
            asks: vec![DepthLevel {
                price: 512_525,
                quantity: 7,
                order_count: None,
            }],
        }
    }

    /// Every segment file below `directory`, in path order.
    pub(super) fn segment_files(directory: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![directory.to_path_buf()];
        while let Some(next) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path
                    .extension()
                    .is_some_and(|extension| extension == "arec")
                {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{
        INSTRUMENT, OTHER_INSTRUMENT, PROVIDER, TemporaryDirectory, depth, install, quote,
        recorded, segment_files, trade,
    };
    use super::format::decode::{RecordedFrame, SegmentTail, decode_segment};
    use super::*;
    use crate::market_service::clock::ScriptedClock;
    use crate::market_service::provider_event::{ProviderEventKind, ProviderTradeBatch};
    use aeris_market_data::{OrderLevelEvent, OrderLevelInterruption, OrderLevelReset};
    use std::collections::BTreeMap;

    fn recording() -> Arc<MarketRecording> {
        Arc::new(MarketRecording::new(Arc::new(ScriptedClock::new(
            1_700_000_000_000_000_000,
        ))))
    }

    fn settings(
        root: &TemporaryDirectory,
        instruments: Vec<RecordedInstrument>,
    ) -> MarketRecordingSettings {
        MarketRecordingSettings {
            data_root: root.0.clone(),
            instruments,
            capacity_bytes: MINIMUM_RECORDING_CAPACITY_BYTES,
        }
    }

    fn catalog() -> BTreeMap<(String, String), aeris_contracts::InstallProviderInstrument> {
        [INSTRUMENT, OTHER_INSTRUMENT]
            .into_iter()
            .map(|instrument| {
                (
                    (PROVIDER.to_owned(), instrument.to_owned()),
                    install(instrument),
                )
            })
            .collect()
    }

    fn every_event_kind(tap: &mut RecordingTap, instrument: &str) {
        let catalog = catalog();
        for kind in [
            ProviderEventKind::Trades(ProviderTradeBatch::One(trade(instrument, 1))),
            ProviderEventKind::Quote(quote(instrument, 2)),
            ProviderEventKind::Depth(depth(instrument, 3)),
            ProviderEventKind::OrderLevels(OrderLevelEvent::Reset(OrderLevelReset {
                metadata: super::fixtures::metadata(instrument, 4),
                image: 1,
                reason: OrderLevelInterruption::SequenceGap,
                resnapshot: true,
            })),
            ProviderEventKind::Trades(ProviderTradeBatch::Many(vec![
                trade(instrument, 5),
                trade(instrument, 6),
            ])),
            ProviderEventKind::Connected,
        ] {
            tap.offer(PROVIDER, &kind, &catalog);
        }
    }

    #[test]
    fn recording_is_off_by_default_and_writes_nothing() {
        let root = TemporaryDirectory::new("off");
        let recording = recording();
        let mut tap = RecordingTap::attached(Arc::clone(&recording));
        every_event_kind(&mut tap, INSTRUMENT);
        assert_eq!(recording.status(), MarketRecordingStatus::default());

        recording
            .configure(settings(&root, Vec::new()))
            .expect("an empty choice is valid");
        every_event_kind(&mut tap, INSTRUMENT);
        drop(tap);
        recording.close();
        assert!(
            !root.0.exists(),
            "no recording directory without a chosen instrument"
        );
        assert_eq!(recording.status().dropped_events, 0);
    }

    #[test]
    fn recorded_instruments_keep_trades_quotes_and_depth_but_never_order_levels() {
        let root = TemporaryDirectory::new("streams");
        let recording = recording();
        recording
            .configure(settings(&root, vec![recorded(INSTRUMENT)]))
            .expect("recording configures");
        let mut tap = RecordingTap::attached(Arc::clone(&recording));
        every_event_kind(&mut tap, INSTRUMENT);
        every_event_kind(&mut tap, OTHER_INSTRUMENT);
        drop(tap);
        recording.close();

        let directory = root.0.join(RECORDINGS_DIRECTORY);
        let files = segment_files(&directory);
        assert_eq!(
            files.len(),
            1,
            "only the chosen instrument is recorded: {files:?}"
        );
        let decoded = decode_segment(&std::fs::read(&files[0]).expect("segment reads"));
        assert_eq!(decoded.tail, SegmentTail::Complete);
        assert_eq!(
            decoded.frames,
            vec![
                RecordedFrame::Instrument(format::InstrumentFrame::from_install(&install(
                    INSTRUMENT
                ))),
                RecordedFrame::Trade(trade(INSTRUMENT, 1)),
                RecordedFrame::Quote(quote(INSTRUMENT, 2)),
                RecordedFrame::Depth(depth(INSTRUMENT, 3)),
                RecordedFrame::Trade(trade(INSTRUMENT, 5)),
                RecordedFrame::Trade(trade(INSTRUMENT, 6)),
            ]
        );
        let index = directory
            .join(store::instrument_key(PROVIDER, INSTRUMENT))
            .join(store::INSTRUMENT_INDEX_FILE);
        let index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(index).expect("index reads")).expect("json");
        assert_eq!(index["instrument_id"], INSTRUMENT);
        let status = recording.status();
        assert!(status.ready);
        assert_eq!(status.dropped_events, 0);
        assert_eq!(status.instruments.len(), 1);
        assert_eq!(
            status.used_bytes,
            std::fs::metadata(&files[0])
                .expect("segment metadata")
                .len()
        );
        assert_eq!(status.instruments[0].display_symbol, "instrument-1 display");
    }

    #[test]
    fn a_recording_choice_is_validated_and_bound_to_one_data_root() {
        let root = TemporaryDirectory::new("validate");
        let other = TemporaryDirectory::new("validate-other");
        let recording = recording();
        let too_many = (0..=MAXIMUM_RECORDED_INSTRUMENTS)
            .map(|index| recorded(&format!("instrument-{index}")))
            .collect();
        assert_eq!(
            recording.configure(settings(&root, too_many)),
            Err(MarketRecordingError::TooManyInstruments)
        );
        assert_eq!(
            recording.configure(settings(
                &root,
                vec![recorded(INSTRUMENT), recorded(INSTRUMENT)]
            )),
            Err(MarketRecordingError::DuplicateInstrument)
        );
        assert_eq!(
            recording.configure(settings(&root, vec![recorded("")])),
            Err(MarketRecordingError::InvalidInstrument)
        );
        for capacity_bytes in [
            MINIMUM_RECORDING_CAPACITY_BYTES - 1,
            MAXIMUM_RECORDING_CAPACITY_BYTES + 1,
        ] {
            assert_eq!(
                recording.configure(MarketRecordingSettings {
                    capacity_bytes,
                    ..settings(&root, Vec::new())
                }),
                Err(MarketRecordingError::CapacityOutOfRange)
            );
        }
        assert_eq!(
            recording.delete(None),
            Err(MarketRecordingError::NotConfigured)
        );
        recording
            .configure(settings(&root, Vec::new()))
            .expect("valid choice");
        assert_eq!(
            recording.configure(settings(&other, Vec::new())),
            Err(MarketRecordingError::DataRootChanged)
        );
        recording.delete(None).expect("delete is accepted");
        recording.close();
    }
}
