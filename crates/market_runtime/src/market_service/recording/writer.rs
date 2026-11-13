//! The `aeris-market-recorder` thread: appends frames, rotates segments and
//! evicts the oldest segments to stay within the cap.

use super::format::{self, FrameRef, InstrumentFrame};
use super::store::{self, RecordingStore, SegmentOrder, StoredInstrument};
use super::{
    MarketRecordingFailure, MarketRecordingStatus, MarketRecordingUsage, RecordedInstrument,
    RecorderShared,
};
use crate::market_service::clock::Clock;
use aeris_market_data::{DepthSnapshot, EventMetadata, MarketTrade, TopOfBookQuote};
use aeris_observability::diagnostic;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const SEGMENT_BYTES: u64 = 32 * super::MIB;
const WRITE_BUFFER_BYTES: usize = 64 * 1024;
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const MAXIMUM_BATCH: usize = 1_024;
const MAXIMUM_SEGMENT_NAME_ATTEMPTS: u32 = 1_000;

pub(super) enum RecordedEvent {
    Trade(MarketTrade),
    Quote(TopOfBookQuote),
    Depth(DepthSnapshot),
}

impl RecordedEvent {
    fn metadata(&self) -> &EventMetadata {
        match self {
            Self::Trade(trade) => &trade.metadata,
            Self::Quote(quote) => &quote.metadata,
            Self::Depth(depth) => &depth.metadata,
        }
    }

    fn frame(&self) -> FrameRef<'_> {
        match self {
            Self::Trade(trade) => FrameRef::Trade(trade),
            Self::Quote(quote) => FrameRef::Quote(quote),
            Self::Depth(depth) => FrameRef::Depth(depth),
        }
    }

    /// An upper estimate of the queued event's memory, used for the in-flight bound.
    pub(super) fn estimated_bytes(&self) -> u64 {
        let identity = self.metadata().entitlement_id.len() as u64;
        identity
            + match self {
                Self::Trade(trade) => 96 + trade.trade_id.len() as u64,
                Self::Quote(_) => 112,
                Self::Depth(depth) => 96 + 24 * (depth.bids.len() as u64 + depth.asks.len() as u64),
            }
    }
}

pub(super) enum RecorderMessage {
    /// Interrupts the idle wait so a control applies promptly.
    Wake,
    Instrument(InstrumentFrame),
    Event {
        event: RecordedEvent,
        /// Events of this instrument the tap dropped since its previous send.
        dropped_before: u64,
        reserved_bytes: u64,
    },
}

pub(super) enum RecorderControl {
    Configure {
        instruments: Vec<RecordedInstrument>,
        capacity_bytes: u64,
    },
    Delete(Option<RecordedInstrument>),
}

type InstrumentId = (String, String);

struct OpenSegment {
    order: SegmentOrder,
    file: BufWriter<File>,
    bytes: u64,
}

struct ActiveInstrument {
    key: String,
    session_millis: Option<u64>,
    next_segment: u32,
    open: Option<OpenSegment>,
}

struct Writer {
    directory: PathBuf,
    store: Option<RecordingStore>,
    clock: Arc<dyn Clock>,
    shared: Arc<RecorderShared>,
    capacity_bytes: u64,
    active: BTreeMap<InstrumentId, ActiveInstrument>,
    headers: BTreeMap<InstrumentId, InstrumentFrame>,
    failure: Option<MarketRecordingFailure>,
    segment_bytes: u64,
    scratch: Vec<u8>,
    status_dirty: bool,
}

pub(super) fn spawn_writer(
    directory: PathBuf,
    clock: Arc<dyn Clock>,
    shared: Arc<RecorderShared>,
    events: Receiver<RecorderMessage>,
    controls: Receiver<RecorderControl>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("aeris-market-recorder".to_string())
        .spawn(move || {
            let mut writer = Writer::new(directory, clock, shared);
            writer.run(&events, &controls);
        })
}

impl Writer {
    fn new(directory: PathBuf, clock: Arc<dyn Clock>, shared: Arc<RecorderShared>) -> Self {
        let mut writer = Self {
            directory,
            store: None,
            clock,
            shared,
            capacity_bytes: super::DEFAULT_RECORDING_CAPACITY_BYTES,
            active: BTreeMap::new(),
            headers: BTreeMap::new(),
            failure: None,
            segment_bytes: SEGMENT_BYTES,
            scratch: Vec::new(),
            status_dirty: true,
        };
        writer.open_store();
        writer
    }

    fn open_store(&mut self) {
        match RecordingStore::open(self.directory.clone()) {
            Ok(store) => self.store = Some(store),
            Err(failure) => {
                diagnostic!("Aeris market recordings could not be indexed: {failure:?}");
                self.failure = Some(failure);
            }
        }
        self.status_dirty = true;
    }

    fn run(&mut self, events: &Receiver<RecorderMessage>, controls: &Receiver<RecorderControl>) {
        let mut last_flush = self.clock.monotonic();
        loop {
            let wait = FLUSH_INTERVAL
                .saturating_sub(self.clock.monotonic().saturating_duration_since(last_flush));
            let first = match events.recv_timeout(wait) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            for message in first
                .into_iter()
                .chain(events.try_iter().take(MAXIMUM_BATCH))
            {
                // A control is queued before the events that depend on it, so
                // applying pending controls first keeps a new choice ahead of them.
                for control in controls.try_iter() {
                    self.control(control);
                }
                self.handle(message);
            }
            for control in controls.try_iter() {
                self.control(control);
            }
            if self.clock.monotonic().saturating_duration_since(last_flush) >= FLUSH_INTERVAL {
                last_flush = self.clock.monotonic();
                self.flush_open_segments();
                self.status_dirty = true;
            }
            self.publish_if_dirty();
        }
        self.finish();
    }

    /// Synchronizes and closes every open segment.
    fn finish(&mut self) {
        let keys = self.active.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            self.close_segment(&key);
        }
        self.status_dirty = true;
        self.publish_if_dirty();
    }

    fn handle(&mut self, message: RecorderMessage) {
        match message {
            RecorderMessage::Wake => {}
            RecorderMessage::Instrument(frame) => self.instrument(frame),
            RecorderMessage::Event {
                event,
                dropped_before,
                reserved_bytes,
            } => {
                self.shared.release_in_flight(reserved_bytes);
                self.event(&event, dropped_before);
            }
        }
    }

    fn instrument(&mut self, frame: InstrumentFrame) {
        let id = (frame.provider_id.clone(), frame.instrument_id.clone());
        if self.headers.get(&id) == Some(&frame) {
            return;
        }
        let write = self
            .active
            .get(&id)
            .is_some_and(|active| active.open.is_some());
        self.headers.insert(id.clone(), frame);
        if write {
            let result = self.write_header(&id);
            self.check(result);
        }
    }

    fn event(&mut self, event: &RecordedEvent, dropped_before: u64) {
        let metadata = event.metadata();
        let id = (metadata.provider_id.clone(), metadata.instrument_id.clone());
        // Events of an instrument the user just stopped recording, or while
        // storage has failed, are not recorded and are not queue losses.
        if self.failure.is_some()
            || !self.active.contains_key(&id)
            || !self.headers.contains_key(&id)
        {
            return;
        }
        let result = self.append_event(&id, event, dropped_before);
        self.check(result);
    }

    fn append_event(
        &mut self,
        id: &InstrumentId,
        event: &RecordedEvent,
        dropped_before: u64,
    ) -> io::Result<()> {
        self.ensure_open(id)?;
        if dropped_before > 0 {
            self.write_frame(id, &FrameRef::Gap(dropped_before))?;
        }
        self.write_frame(id, &event.frame())?;
        if self
            .active
            .get(id)
            .and_then(|active| active.open.as_ref())
            .is_some_and(|open| open.bytes >= self.segment_bytes)
        {
            self.close_segment(id);
        }
        self.enforce_capacity()
    }

    fn write_header(&mut self, id: &InstrumentId) -> io::Result<()> {
        match self.headers.get(id).cloned() {
            Some(header) => self.write_frame(id, &FrameRef::Instrument(&header)),
            None => Ok(()),
        }
    }

    /// Encodes one frame and appends it to the instrument's open segment.
    fn write_frame(&mut self, id: &InstrumentId, frame: &FrameRef<'_>) -> io::Result<()> {
        self.scratch.clear();
        if let Err(error) = format::encode_frame(&mut self.scratch, frame) {
            diagnostic!("Aeris market recorder skipped an unencodable frame: {error:?}");
            self.shared.count_drop();
            return Ok(());
        }
        let Some(open) = self
            .active
            .get_mut(id)
            .and_then(|active| active.open.as_mut())
        else {
            return Ok(());
        };
        open.file.write_all(&self.scratch)?;
        let written = self.scratch.len() as u64;
        open.bytes += written;
        if let Some(store) = &mut self.store {
            store.grow(&open.order, written);
        }
        Ok(())
    }

    fn ensure_open(&mut self, id: &InstrumentId) -> io::Result<()> {
        let now_millis = self
            .clock
            .wall_nanos()
            .and_then(|nanos| u64::try_from(nanos.div_euclid(1_000_000)).ok())
            .unwrap_or_default();
        let Some(store) = &mut self.store else {
            return Err(io::Error::other("recordings are not indexed"));
        };
        let Some(active) = self.active.get_mut(id) else {
            return Ok(());
        };
        if active.open.is_some() {
            return Ok(());
        }
        let Some(header) = self.headers.get(id) else {
            return Ok(());
        };
        let instrument_directory = store.instrument_directory(&active.key);
        if !store.has_instrument(&active.key) {
            std::fs::create_dir_all(&instrument_directory)?;
            aeris_platform_runtime::fs::write_file_durably(
                &instrument_directory.join(store::INSTRUMENT_INDEX_FILE),
                &store::instrument_index_bytes(&id.0, &id.1, &header.display_symbol),
            )?;
            store.add_instrument(
                active.key.clone(),
                StoredInstrument {
                    provider_id: id.0.clone(),
                    instrument_id: id.1.clone(),
                    display_symbol: header.display_symbol.clone(),
                    bytes: 0,
                },
            );
        }
        let session_millis = *active.session_millis.get_or_insert(now_millis);
        std::fs::create_dir_all(
            instrument_directory.join(store::session_directory_name(session_millis)),
        )?;
        let mut attempts = 0;
        let (order, file) = loop {
            attempts += 1;
            let order = SegmentOrder {
                session_millis,
                segment: active.next_segment,
                instrument_key: active.key.clone(),
            };
            active.next_segment = active.next_segment.saturating_add(1);
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(store.segment_path(&order))
            {
                Ok(file) => break (order, file),
                Err(error)
                    if error.kind() == io::ErrorKind::AlreadyExists
                        && attempts < MAXIMUM_SEGMENT_NAME_ATTEMPTS => {}
                Err(error) => return Err(error),
            }
        };
        let mut file = BufWriter::with_capacity(WRITE_BUFFER_BYTES, file);
        file.write_all(&format::segment_header())?;
        let header_bytes = format::SEGMENT_HEADER_BYTES as u64;
        store.insert(order.clone(), header_bytes);
        active.open = Some(OpenSegment {
            order,
            file,
            bytes: header_bytes,
        });
        self.status_dirty = true;
        self.write_header(id)
    }

    fn close_segment(&mut self, id: &InstrumentId) {
        let Some(open) = self
            .active
            .get_mut(id)
            .and_then(|active| active.open.take())
        else {
            return;
        };
        self.status_dirty = true;
        let mut file = open.file;
        let synced = file.flush().and_then(|()| file.get_ref().sync_data());
        if let Err(error) = synced {
            diagnostic!("Aeris market recorder could not synchronize a segment: {error}");
            self.fail(MarketRecordingFailure::StorageUnavailable);
        }
    }

    fn flush_open_segments(&mut self) {
        let mut failed = false;
        for active in self.active.values_mut() {
            if let Some(open) = &mut active.open
                && let Err(error) = open.file.flush()
            {
                diagnostic!("Aeris market recorder could not flush a segment: {error}");
                failed = true;
            }
        }
        if failed {
            self.fail(MarketRecordingFailure::StorageUnavailable);
        }
    }

    fn enforce_capacity(&mut self) -> io::Result<()> {
        loop {
            let Some(store) = &self.store else {
                return Ok(());
            };
            if store.used_bytes() <= self.capacity_bytes {
                return Ok(());
            }
            let Some(oldest) = store.oldest().cloned() else {
                return Ok(());
            };
            let open = self
                .active
                .iter()
                .find(|(_, active)| {
                    active
                        .open
                        .as_ref()
                        .is_some_and(|open| open.order == oldest)
                })
                .map(|(id, _)| id.clone());
            if let Some(id) = open {
                self.close_segment(&id);
            }
            if let Some(store) = &mut self.store {
                store.remove_segment(&oldest)?;
            }
            self.status_dirty = true;
        }
    }

    fn control(&mut self, control: RecorderControl) {
        match control {
            RecorderControl::Configure {
                instruments,
                capacity_bytes,
            } => {
                self.capacity_bytes = capacity_bytes;
                if self.failure.is_some() {
                    self.failure = None;
                    if self.store.is_none() {
                        self.open_store();
                    }
                }
                let chosen = instruments
                    .into_iter()
                    .map(|instrument| (instrument.provider_id, instrument.instrument_id))
                    .collect::<std::collections::BTreeSet<_>>();
                let retired = self
                    .active
                    .keys()
                    .filter(|id| !chosen.contains(*id))
                    .cloned()
                    .collect::<Vec<_>>();
                for id in retired {
                    self.close_segment(&id);
                    self.active.remove(&id);
                }
                if self.failure.is_none() {
                    for id in chosen {
                        self.active
                            .entry(id)
                            .or_insert_with_key(|id| ActiveInstrument {
                                key: store::instrument_key(&id.0, &id.1),
                                session_millis: None,
                                next_segment: 1,
                                open: None,
                            });
                    }
                }
                let result = self.enforce_capacity();
                self.check(result);
            }
            RecorderControl::Delete(instrument) => {
                let result = self.delete(instrument);
                self.check(result);
            }
        }
        self.status_dirty = true;
    }

    fn delete(&mut self, instrument: Option<RecordedInstrument>) -> io::Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let keys = match instrument {
            Some(instrument) => vec![store::instrument_key(
                &instrument.provider_id,
                &instrument.instrument_id,
            )],
            None => store.instrument_keys(),
        };
        for key in keys {
            let open = self
                .active
                .iter()
                .filter(|(_, active)| active.key == key)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            for id in open {
                self.close_segment(&id);
            }
            if let Some(store) = &mut self.store {
                store.remove_instrument(&key)?;
            }
        }
        Ok(())
    }

    fn check(&mut self, result: io::Result<()>) {
        if let Err(error) = result {
            diagnostic!("Aeris market recorder stopped after a storage error: {error}");
            self.fail(MarketRecordingFailure::StorageUnavailable);
        }
    }

    /// Stops recording until the next configuration; recorded files stay.
    fn fail(&mut self, failure: MarketRecordingFailure) {
        self.failure = Some(failure);
        for (_, active) in std::mem::take(&mut self.active) {
            if let Some(mut open) = active.open
                && let Err(error) = open.file.flush()
            {
                diagnostic!("Aeris market recorder lost a buffered segment tail: {error}");
            }
        }
        self.status_dirty = true;
    }

    fn publish_if_dirty(&mut self) {
        if !self.status_dirty {
            return;
        }
        self.status_dirty = false;
        let mut instruments = BTreeMap::<InstrumentId, MarketRecordingUsage>::new();
        if let Some(store) = &self.store {
            for stored in store.instruments() {
                instruments.insert(
                    (stored.provider_id.clone(), stored.instrument_id.clone()),
                    MarketRecordingUsage {
                        instrument: RecordedInstrument {
                            provider_id: stored.provider_id.clone(),
                            instrument_id: stored.instrument_id.clone(),
                        },
                        display_symbol: stored.display_symbol.clone(),
                        bytes: stored.bytes,
                        recording: false,
                    },
                );
            }
        }
        for id in self.active.keys() {
            let display_symbol = self
                .headers
                .get(id)
                .map_or_else(|| id.1.clone(), |header| header.display_symbol.clone());
            instruments
                .entry(id.clone())
                .or_insert_with(|| MarketRecordingUsage {
                    instrument: RecordedInstrument {
                        provider_id: id.0.clone(),
                        instrument_id: id.1.clone(),
                    },
                    display_symbol,
                    bytes: 0,
                    recording: false,
                })
                .recording = true;
        }
        self.shared.publish(MarketRecordingStatus {
            ready: self.store.is_some(),
            capacity_bytes: self.capacity_bytes,
            used_bytes: self.store.as_ref().map_or(0, RecordingStore::used_bytes),
            instruments: instruments.into_values().collect(),
            dropped_events: 0,
            failure: self.failure,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{
        INSTRUMENT, OTHER_INSTRUMENT, PROVIDER, TemporaryDirectory, install, recorded,
        segment_files, trade,
    };
    use super::super::format::decode::{RecordedFrame, SegmentTail, decode_segment};
    use super::*;
    use crate::market_service::clock::ScriptedClock;

    fn writer(directory: &TemporaryDirectory, segment_bytes: u64, capacity_bytes: u64) -> Writer {
        writer_with_clock(
            directory,
            segment_bytes,
            capacity_bytes,
            Arc::new(ScriptedClock::new(1_700_000_000_000_000_000)),
        )
    }

    fn writer_with_clock(
        directory: &TemporaryDirectory,
        segment_bytes: u64,
        capacity_bytes: u64,
        clock: Arc<ScriptedClock>,
    ) -> Writer {
        let mut writer = Writer::new(directory.0.clone(), clock, Arc::default());
        writer.segment_bytes = segment_bytes;
        writer.control(RecorderControl::Configure {
            instruments: vec![recorded(INSTRUMENT), recorded(OTHER_INSTRUMENT)],
            capacity_bytes,
        });
        for instrument in [INSTRUMENT, OTHER_INSTRUMENT] {
            writer.handle(RecorderMessage::Instrument(InstrumentFrame::from_install(
                &install(instrument),
            )));
        }
        writer
    }

    fn record(writer: &mut Writer, instrument: &str, sequences: std::ops::Range<u64>) {
        for sequence in sequences {
            writer.handle(RecorderMessage::Event {
                event: RecordedEvent::Trade(trade(instrument, sequence)),
                dropped_before: 0,
                reserved_bytes: 0,
            });
        }
    }

    fn total_size(files: &[PathBuf]) -> u64 {
        files
            .iter()
            .map(|file| std::fs::metadata(file).expect("segment metadata").len())
            .sum()
    }

    #[test]
    fn segments_rotate_by_size_and_the_oldest_are_evicted_within_the_cap() {
        let directory = TemporaryDirectory::new("evict");
        let mut writer = writer(&directory, 600, 2_000);
        record(&mut writer, INSTRUMENT, 1..41);
        writer.finish();

        let files = segment_files(&directory.0);
        let used = writer.store.as_ref().expect("store").used_bytes();
        assert!(used <= 2_000, "{used} bytes exceed the cap");
        assert_eq!(total_size(&files), used, "the index matches the disk");
        let names = files
            .iter()
            .filter_map(|file| file.file_name()?.to_str().map(str::to_owned))
            .collect::<Vec<_>>();
        assert!(!names.contains(&store::segment_file_name(1)), "{names:?}");
        let mut last_sequence = 0;
        for file in &files {
            let decoded = decode_segment(&std::fs::read(file).expect("segment reads"));
            assert_eq!(decoded.tail, SegmentTail::Complete);
            assert!(matches!(decoded.frames[0], RecordedFrame::Instrument(_)));
            for frame in &decoded.frames[1..] {
                let RecordedFrame::Trade(trade) = frame else {
                    panic!("unexpected frame {frame:?}");
                };
                assert!(trade.metadata.source_sequence > last_sequence);
                last_sequence = trade.metadata.source_sequence;
            }
            assert!(std::fs::metadata(file).expect("metadata").len() < 600 + 200);
        }
        assert_eq!(last_sequence, 40, "the newest events survive eviction");

        let reopened = RecordingStore::open(directory.0.clone()).expect("store reopens");
        assert_eq!(reopened.used_bytes(), used);
    }

    #[test]
    fn a_lower_cap_evicts_immediately_across_instruments_oldest_first() {
        let directory = TemporaryDirectory::new("cap");
        let clock = Arc::new(ScriptedClock::new(1_700_000_000_000_000_000));
        let mut writer = writer_with_clock(&directory, 400, 1_000_000, Arc::clone(&clock));
        record(&mut writer, INSTRUMENT, 1..21);
        clock.advance(Duration::from_secs(1));
        record(&mut writer, OTHER_INSTRUMENT, 1..21);
        writer.control(RecorderControl::Configure {
            instruments: vec![recorded(INSTRUMENT), recorded(OTHER_INSTRUMENT)],
            capacity_bytes: 1_500,
        });
        let store = writer.store.as_ref().expect("store");
        assert!(store.used_bytes() <= 1_500);
        assert!(
            store
                .segments_of(&store::instrument_key(PROVIDER, INSTRUMENT))
                .is_empty(),
            "the older session went first"
        );
        assert_ne!(
            store.segments_of(&store::instrument_key(PROVIDER, OTHER_INSTRUMENT)),
            Vec::new()
        );
        writer.finish();
    }

    #[test]
    fn a_dropped_event_is_marked_with_a_gap_frame() {
        let directory = TemporaryDirectory::new("gap");
        let mut writer = writer(&directory, 1_000_000, 1_000_000);
        record(&mut writer, INSTRUMENT, 1..2);
        writer.handle(RecorderMessage::Event {
            event: RecordedEvent::Trade(trade(INSTRUMENT, 9)),
            dropped_before: 7,
            reserved_bytes: 0,
        });
        writer.finish();
        let files = segment_files(&directory.0);
        let decoded = decode_segment(&std::fs::read(&files[0]).expect("segment reads"));
        assert_eq!(decoded.frames[2], RecordedFrame::Gap(7));
        assert_eq!(
            decoded.frames[3],
            RecordedFrame::Trade(trade(INSTRUMENT, 9))
        );
    }

    #[test]
    fn deleting_an_instrument_removes_only_its_indexed_files() {
        let directory = TemporaryDirectory::new("delete");
        let mut writer = writer(&directory, 1_000_000, 1_000_000);
        record(&mut writer, INSTRUMENT, 1..5);
        record(&mut writer, OTHER_INSTRUMENT, 1..5);
        let first = directory
            .0
            .join(store::instrument_key(PROVIDER, INSTRUMENT));
        std::fs::write(first.join("notes.txt"), b"user file").expect("user file");
        std::fs::create_dir_all(directory.0.join("unrelated")).expect("unrelated directory");

        writer.control(RecorderControl::Delete(Some(recorded(INSTRUMENT))));
        assert!(
            first.join("notes.txt").exists(),
            "unknown files are preserved"
        );
        assert!(!first.join(store::INSTRUMENT_INDEX_FILE).exists());
        assert_eq!(segment_files(&first), Vec::<PathBuf>::new());
        assert_eq!(segment_files(&directory.0).len(), 1);

        record(&mut writer, INSTRUMENT, 5..6);
        assert_eq!(
            segment_files(&first).len(),
            1,
            "recording continues after a delete"
        );

        writer.control(RecorderControl::Delete(None));
        writer.finish();
        assert_eq!(segment_files(&directory.0), Vec::<PathBuf>::new());
        assert!(directory.0.join("unrelated").exists());
        assert_eq!(writer.store.as_ref().expect("store").used_bytes(), 0);
        let status = writer.shared.status.lock().expect("status").clone();
        assert!(
            status
                .instruments
                .iter()
                .all(|usage| usage.bytes == 0 && usage.recording)
        );
    }

    #[test]
    fn stopping_an_instrument_closes_its_segment_and_ignores_later_events() {
        let directory = TemporaryDirectory::new("stop");
        let mut writer = writer(&directory, 1_000_000, 1_000_000);
        record(&mut writer, INSTRUMENT, 1..3);
        writer.control(RecorderControl::Configure {
            instruments: vec![recorded(OTHER_INSTRUMENT)],
            capacity_bytes: 1_000_000,
        });
        let files = segment_files(&directory.0);
        let closed = std::fs::read(&files[0]).expect("segment reads");
        record(&mut writer, INSTRUMENT, 3..6);
        assert_eq!(std::fs::read(&files[0]).expect("segment reads"), closed);
        assert_eq!(decode_segment(&closed).frames.len(), 3);
        assert_eq!(
            writer
                .shared
                .dropped_events
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        writer.finish();
    }
}
