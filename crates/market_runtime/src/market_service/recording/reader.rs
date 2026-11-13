//! Bounded reading of one recorded session for replay.
//!
//! A session is read sequentially through its segments, decoding at most
//! `READ_AHEAD_EVENTS` events ahead of the replay clock and one frame payload
//! at a time. A segment that cannot be opened (the writer evicted it) or that
//! ends in a damaged or torn tail before the session's last segment is a gap
//! in the recording; the replay counts it and continues with the next segment.

use super::RecordedInstrument;
use super::format::InstrumentFrame;
use super::format::decode::{RecordedFrame, SegmentFrames, SegmentTail};
use super::replay::RecordingReplayError;
use super::store::{self, RecordingStore};
use aeris_market_data::{DepthSnapshot, MarketBar, MarketTrade, TopOfBookQuote};
use aeris_observability::diagnostic;
use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Decoded events a cursor holds ahead of the replay clock.
pub(super) const READ_AHEAD_EVENTS: usize = 1_024;
const READ_BUFFER_BYTES: usize = 64 * 1024;
/// Frames read from a segment's start while looking for its first event.
const MAXIMUM_LEADING_FRAMES: usize = 64;
const NANOS_PER_SECOND: i64 = 1_000_000_000;

/// One recorded market event, in the provider's original identity.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ReplayEvent {
    Trade(MarketTrade),
    Quote(TopOfBookQuote),
    Depth(DepthSnapshot),
}

impl ReplayEvent {
    /// The local receipt time, which orders a recording and paces its replay.
    pub(super) const fn received_unix_nanos(&self) -> i64 {
        match self {
            Self::Trade(trade) => trade.metadata.timestamps.received_unix_nanos,
            Self::Quote(quote) => quote.metadata.timestamps.received_unix_nanos,
            Self::Depth(depth) => depth.metadata.timestamps.received_unix_nanos,
        }
    }

    fn from_frame(frame: RecordedFrame) -> Option<Self> {
        match frame {
            RecordedFrame::Trade(trade) => Some(Self::Trade(trade)),
            RecordedFrame::Quote(quote) => Some(Self::Quote(quote)),
            RecordedFrame::Depth(depth) => Some(Self::Depth(depth)),
            RecordedFrame::Instrument(_) | RecordedFrame::Gap(_) => None,
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum CursorItem {
    Event(ReplayEvent),
    /// Recorded events are missing here: dropped by the recorder, evicted or damaged.
    Gap,
}

#[derive(Clone, Debug)]
pub(super) struct SegmentEntry {
    path: PathBuf,
    /// Receipt time of the segment's first event; `None` when it has none or is unreadable.
    first_unix_nanos: Option<i64>,
}

/// The indexed segments of one recorded session.
#[derive(Clone, Debug)]
pub(super) struct RecordedSession {
    pub(super) instrument: InstrumentFrame,
    pub(super) segments: Arc<[SegmentEntry]>,
    pub(super) start_unix_nanos: i64,
    pub(super) end_unix_nanos: i64,
}

/// Indexes one session's segments under the recordings directory. Reads the
/// first frames of every segment and the whole last readable one, at most
/// one segment (32 MiB) more than a fixed number of frames per segment.
pub(super) fn open_session(
    directory: PathBuf,
    instrument: &RecordedInstrument,
    started_unix_millis: u64,
) -> Result<RecordedSession, RecordingReplayError> {
    let store = RecordingStore::open(directory).map_err(|_| RecordingReplayError::Unreadable)?;
    let key = store::instrument_key(&instrument.provider_id, &instrument.instrument_id);
    let orders = store
        .segments_of(&key)
        .into_iter()
        .filter(|order| order.session_millis == started_unix_millis)
        .collect::<Vec<_>>();
    if orders.is_empty() {
        return Err(RecordingReplayError::NotFound);
    }
    let mut header: Option<InstrumentFrame> = None;
    let mut segments = Vec::with_capacity(orders.len());
    for order in &orders {
        let path = store.segment_path(order);
        let (segment_header, first_unix_nanos) = leading_frames(&path);
        if header.is_none() {
            header = segment_header;
        }
        segments.push(SegmentEntry {
            path,
            first_unix_nanos,
        });
    }
    let header = header
        .filter(|header| {
            header.provider_id == instrument.provider_id
                && header.instrument_id == instrument.instrument_id
        })
        .ok_or(RecordingReplayError::Unreadable)?;
    let start_unix_nanos = segments
        .iter()
        .find_map(|segment| segment.first_unix_nanos)
        .ok_or(RecordingReplayError::Empty)?;
    let end_unix_nanos = segments
        .iter()
        .rev()
        .find(|segment| segment.first_unix_nanos.is_some())
        .and_then(|segment| last_event_unix_nanos(&segment.path))
        .unwrap_or(start_unix_nanos)
        .max(start_unix_nanos);
    Ok(RecordedSession {
        instrument: header,
        segments: segments.into(),
        start_unix_nanos,
        end_unix_nanos,
    })
}

fn segment_frames(path: &Path) -> Option<SegmentFrames<BufReader<File>>> {
    File::open(path)
        .ok()
        .map(|file| SegmentFrames::new(BufReader::with_capacity(READ_BUFFER_BYTES, file)))
}

fn leading_frames(path: &Path) -> (Option<InstrumentFrame>, Option<i64>) {
    let Some(mut frames) = segment_frames(path) else {
        return (None, None);
    };
    let mut first = None;
    for _ in 0..MAXIMUM_LEADING_FRAMES {
        let Some(frame) = frames.next_frame() else {
            break;
        };
        if let Some(event) = ReplayEvent::from_frame(frame) {
            first = Some(event.received_unix_nanos());
            break;
        }
    }
    (frames.instrument().cloned(), first)
}

fn last_event_unix_nanos(path: &Path) -> Option<i64> {
    let mut frames = segment_frames(path)?;
    let mut last = None;
    while let Some(frame) = frames.next_frame() {
        if let Some(event) = ReplayEvent::from_frame(frame) {
            last = Some(event.received_unix_nanos());
        }
    }
    last
}

/// Sequential reader of one session from a receipt time onwards.
pub(super) struct SessionCursor {
    segments: Arc<[SegmentEntry]>,
    next_segment: usize,
    frames: Option<SegmentFrames<BufReader<File>>>,
    ahead: VecDeque<CursorItem>,
    after_unix_nanos: i64,
    /// Whether an event after `after_unix_nanos` was read; later gaps are in range.
    reached: bool,
    /// A gap read after the last event at or before `after_unix_nanos`; it is
    /// in range when the next event read is after it.
    gap_before_reached: bool,
}

impl SessionCursor {
    /// Reads events received after `after_unix_nanos`, starting with the last
    /// segment that begins at or before it.
    pub(super) fn after(segments: Arc<[SegmentEntry]>, after_unix_nanos: i64) -> Self {
        let next_segment = segments
            .iter()
            .rposition(|segment| {
                segment
                    .first_unix_nanos
                    .is_some_and(|first| first <= after_unix_nanos)
            })
            .unwrap_or(0);
        Self {
            segments,
            next_segment,
            frames: None,
            ahead: VecDeque::with_capacity(READ_AHEAD_EVENTS),
            after_unix_nanos,
            reached: false,
            gap_before_reached: false,
        }
    }

    /// Receipt time of the next event, or `None` once the session is read.
    /// A gap ahead reports the time of the event after it.
    pub(super) fn peek_unix_nanos(&mut self) -> Option<i64> {
        if self
            .ahead
            .iter()
            .all(|item| matches!(item, CursorItem::Gap))
        {
            self.fill();
        }
        self.ahead.iter().find_map(|item| match item {
            CursorItem::Event(event) => Some(event.received_unix_nanos()),
            CursorItem::Gap => None,
        })
    }

    pub(super) fn pop(&mut self) -> Option<CursorItem> {
        if self.ahead.is_empty() {
            self.fill();
        }
        self.ahead.pop_front()
    }

    fn gap(&mut self) {
        if self.reached {
            self.ahead.push_back(CursorItem::Gap);
        } else {
            self.gap_before_reached = true;
        }
    }

    fn event(&mut self, event: ReplayEvent) {
        if event.received_unix_nanos() <= self.after_unix_nanos {
            self.gap_before_reached = false;
            return;
        }
        if !self.reached && self.gap_before_reached {
            self.ahead.push_back(CursorItem::Gap);
        }
        self.reached = true;
        self.ahead.push_back(CursorItem::Event(event));
    }

    fn fill(&mut self) {
        while self.ahead.len() < READ_AHEAD_EVENTS {
            let Some(frames) = self.frames.as_mut() else {
                let Some(segment) = self.segments.get(self.next_segment) else {
                    return;
                };
                self.next_segment += 1;
                self.frames = segment_frames(&segment.path);
                if self.frames.is_none() {
                    self.gap();
                }
                continue;
            };
            match frames.next_frame() {
                Some(RecordedFrame::Gap(_)) => self.gap(),
                Some(frame) => {
                    if let Some(event) = ReplayEvent::from_frame(frame) {
                        self.event(event);
                    }
                }
                None => {
                    let last = self.next_segment >= self.segments.len();
                    let tail = frames.tail();
                    self.frames = None;
                    match tail {
                        Some(SegmentTail::Corrupt { offset }) => {
                            diagnostic!(
                                "Aeris recording replay stopped a damaged segment at byte {offset}"
                            );
                            self.gap();
                        }
                        // Only the newest segment of a session can end mid-frame
                        // when its writer stops; an older one was damaged.
                        Some(SegmentTail::Truncated { .. }) if !last => self.gap(),
                        Some(SegmentTail::Complete | SegmentTail::Truncated { .. }) | None => {}
                    }
                }
            }
        }
    }
}
/// One trade-built history request against a recording.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct HistoryWindow {
    pub(super) period_seconds: i64,
    pub(super) maximum_bars: usize,
    /// `[start, end)` of bar opens for a page before the chart's history.
    pub(super) range: Option<(i64, i64)>,
    /// Trades received at or before this time are history; later ones replay live.
    pub(super) through_unix_nanos: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct TradeHistory {
    pub(super) bars: Vec<MarketBar>,
    /// The period still open at the replay clock and its trade count.
    pub(super) forming: Option<(MarketBar, u32)>,
    /// Newest history-clock time of a trade inside the history.
    pub(super) handoff_boundary_unix_nanos: Option<i64>,
    pub(super) backwards_exhausted: bool,
}

/// The open (seconds and nanoseconds) and bar sequence of the fixed period
/// holding `at`, counted in whole periods from the Unix epoch.
fn period_of(at: i64, period_seconds: i64) -> Option<(i64, i64, u64)> {
    let bucket = at.div_euclid(NANOS_PER_SECOND).div_euclid(period_seconds);
    let open_seconds = bucket.saturating_mul(period_seconds);
    let sequence = u64::try_from(bucket).ok()?.checked_add(1)?;
    Some((
        open_seconds,
        open_seconds.saturating_mul(NANOS_PER_SECOND),
        sequence,
    ))
}

/// Builds fixed-period bars from the recorded trades in `window`, bucketed
/// like the coordinator's trade-built bars (whole periods from the Unix epoch
/// on the history clock). Work is bounded by the trades in the window.
pub(super) fn trade_history(session: &RecordedSession, window: &HistoryWindow) -> TradeHistory {
    let Some(period_nanos) = window.period_seconds.checked_mul(NANOS_PER_SECOND) else {
        return TradeHistory::default();
    };
    if window.period_seconds <= 0 || window.maximum_bars == 0 {
        return TradeHistory::default();
    }
    let span = i64::try_from(window.maximum_bars)
        .unwrap_or(i64::MAX)
        .saturating_add(1)
        .saturating_mul(period_nanos);
    let (from, until) = match window.range {
        Some((start, end)) => (start, end.min(window.through_unix_nanos)),
        None => (
            window.through_unix_nanos.saturating_sub(span),
            window.through_unix_nanos,
        ),
    };
    // The oldest bar starts at its period's open, so it holds every trade of it.
    let first_open = from.div_euclid(period_nanos).saturating_mul(period_nanos);
    let mut history = TradeHistory {
        backwards_exhausted: first_open <= session.start_unix_nanos,
        ..TradeHistory::default()
    };
    // A current request also holds the period still forming at the clock.
    let retained = window
        .maximum_bars
        .saturating_add(usize::from(window.range.is_none()));
    let mut bars = VecDeque::<(MarketBar, u32)>::with_capacity(retained.min(4_096));
    let mut cursor =
        SessionCursor::after(Arc::clone(&session.segments), first_open.saturating_sub(1));
    while let Some(item) = cursor.pop() {
        let CursorItem::Event(event) = item else {
            continue;
        };
        if event.received_unix_nanos() > window.through_unix_nanos {
            break;
        }
        let ReplayEvent::Trade(trade) = event else {
            continue;
        };
        let Some(at) = trade.metadata.timestamps.history_clock_unix_nanos() else {
            continue;
        };
        let Some((open_seconds, open, sequence)) = period_of(at, window.period_seconds) else {
            continue;
        };
        if open < first_open || at > until || window.range.is_some_and(|(_, end)| open >= end) {
            continue;
        }
        match bars.back_mut() {
            Some((bar, trades)) if bar.exchange_timestamp_unix_nanos == open => {
                bar.high = bar.high.max(trade.price);
                bar.low = bar.low.min(trade.price);
                bar.close = trade.price;
                bar.volume = bar.volume.saturating_add(trade.quantity);
                *trades = trades.saturating_add(1);
            }
            Some((bar, _)) if bar.exchange_timestamp_unix_nanos > open => continue,
            _ => {
                bars.push_back((
                    MarketBar {
                        source_sequence: sequence,
                        exchange_timestamp_seconds: open_seconds,
                        exchange_timestamp_unix_nanos: open,
                        open: trade.price,
                        high: trade.price,
                        low: trade.price,
                        close: trade.price,
                        volume: trade.quantity,
                    },
                    1,
                ));
                if bars.len() > retained {
                    bars.pop_front();
                    history.backwards_exhausted = false;
                }
            }
        }
        if window.range.is_none() {
            history.handoff_boundary_unix_nanos = Some(
                history
                    .handoff_boundary_unix_nanos
                    .map_or(at, |boundary| boundary.max(at)),
            );
        }
    }
    if window.range.is_none()
        && let Some((last, _)) = bars.back()
        && last
            .exchange_timestamp_unix_nanos
            .saturating_add(period_nanos)
            > window.through_unix_nanos
    {
        history.forming = bars.pop_back();
    }
    if bars.len() > window.maximum_bars {
        bars.pop_front();
        history.backwards_exhausted = false;
    }
    history.bars = bars.into_iter().map(|(bar, _)| bar).collect();
    history
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{INSTRUMENT, TemporaryDirectory, depth, quote, recorded, trade};
    use super::super::writer::RecordedEvent;
    use super::super::writer::test_sessions::{SESSION_MILLIS, SESSION_UNIX_NANOS, record_session};
    use super::*;

    const SECOND: i64 = 1_000_000_000;
    /// A minute boundary on the Unix epoch, 40 seconds into the test session.
    const MINUTE: i64 = SESSION_UNIX_NANOS + 40 * SECOND;

    fn at(seconds: i64) -> i64 {
        SESSION_UNIX_NANOS + seconds * SECOND
    }

    fn timed(mut event: RecordedEvent, seconds: i64) -> RecordedEvent {
        let metadata = match &mut event {
            RecordedEvent::Trade(trade) => &mut trade.metadata,
            RecordedEvent::Quote(quote) => &mut quote.metadata,
            RecordedEvent::Depth(depth) => &mut depth.metadata,
        };
        metadata.timestamps.received_unix_nanos = at(seconds);
        metadata.timestamps.exchange_unix_nanos = Some(at(seconds));
        metadata.timestamps.provider_unix_nanos = None;
        event
    }

    fn trade_at(seconds: i64, price: i64) -> (RecordedEvent, u64) {
        let mut recorded = trade(INSTRUMENT, u64::try_from(seconds).expect("positive"));
        recorded.price = price;
        (timed(RecordedEvent::Trade(recorded), seconds), 0)
    }

    fn trades(seconds: std::ops::Range<i64>) -> Vec<(RecordedEvent, u64)> {
        seconds
            .map(|second| trade_at(second, 100 + second))
            .collect()
    }

    fn open(directory: &TemporaryDirectory) -> RecordedSession {
        open_session(directory.0.clone(), &recorded(INSTRUMENT), SESSION_MILLIS)
            .expect("session opens")
    }

    /// Every item after `after_unix_nanos`: an event as its receipt second,
    /// a gap as `None`.
    fn read(session: &RecordedSession, after_unix_nanos: i64) -> Vec<Option<i64>> {
        let mut cursor = SessionCursor::after(Arc::clone(&session.segments), after_unix_nanos);
        std::iter::from_fn(|| cursor.pop())
            .map(|item| match item {
                CursorItem::Event(event) => {
                    Some((event.received_unix_nanos() - SESSION_UNIX_NANOS) / SECOND)
                }
                CursorItem::Gap => None,
            })
            .collect()
    }

    fn shorten(path: &Path, bytes: u64) {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("segment opens");
        let length = file.metadata().expect("segment metadata").len();
        file.set_len(length - bytes).expect("segment shortens");
    }

    #[test]
    fn a_complete_session_reads_every_event_in_order_from_any_time() {
        let directory = TemporaryDirectory::new("reader-complete");
        record_session(
            &directory.0,
            1_000_000,
            vec![
                trade_at(0, 100),
                (timed(RecordedEvent::Quote(quote(INSTRUMENT, 1)), 1), 0),
                (timed(RecordedEvent::Depth(depth(INSTRUMENT, 2)), 2), 0),
                trade_at(3, 103),
            ],
        );
        let session = open(&directory);
        assert_eq!(session.instrument.instrument_id, INSTRUMENT);
        assert_eq!(session.start_unix_nanos, at(0));
        assert_eq!(session.end_unix_nanos, at(3));
        assert_eq!(
            read(&session, at(0) - 1),
            [Some(0), Some(1), Some(2), Some(3)]
        );
        assert_eq!(read(&session, at(1)), [Some(2), Some(3)]);
        assert_eq!(read(&session, at(3)), []);

        let mut cursor = SessionCursor::after(Arc::clone(&session.segments), at(0) - 1);
        assert!(matches!(
            cursor.pop(),
            Some(CursorItem::Event(ReplayEvent::Trade(_)))
        ));
        assert!(matches!(
            cursor.pop(),
            Some(CursorItem::Event(ReplayEvent::Quote(_)))
        ));
        assert!(matches!(
            cursor.pop(),
            Some(CursorItem::Event(ReplayEvent::Depth(_)))
        ));

        assert_eq!(
            open_session(
                directory.0.clone(),
                &recorded(INSTRUMENT),
                SESSION_MILLIS + 1
            )
            .err(),
            Some(RecordingReplayError::NotFound)
        );
    }

    #[test]
    fn a_seek_starts_in_the_covering_segment_and_reads_ahead_boundedly() {
        let directory = TemporaryDirectory::new("reader-seek");
        record_session(&directory.0, 400, trades(0..30));
        let session = open(&directory);
        assert!(session.segments.len() >= 3, "{}", session.segments.len());
        assert_eq!(session.end_unix_nanos, at(29));
        assert_eq!(
            read(&session, at(20)),
            (21..30).map(Some).collect::<Vec<_>>()
        );

        let many = TemporaryDirectory::new("reader-ahead");
        record_session(&many.0, 1_000_000, trades(0..2_000));
        let session = open(&many);
        let mut cursor = SessionCursor::after(Arc::clone(&session.segments), at(0) - 1);
        assert_eq!(cursor.peek_unix_nanos(), Some(at(0)));
        assert_eq!(cursor.ahead.len(), READ_AHEAD_EVENTS);
        assert_eq!(read(&session, at(0) - 1).len(), 2_000);
    }

    #[test]
    fn a_torn_newest_segment_ends_the_session_without_a_gap() {
        let directory = TemporaryDirectory::new("reader-torn");
        record_session(&directory.0, 1_000_000, trades(0..5));
        shorten(&open(&directory).segments[0].path, 3);
        let session = open(&directory);
        assert_eq!(session.end_unix_nanos, at(3));
        assert_eq!(
            read(&session, at(0) - 1),
            [Some(0), Some(1), Some(2), Some(3)]
        );
    }

    #[test]
    fn a_damaged_older_segment_is_a_gap_and_reading_continues() {
        for (label, damage) in [("truncated", false), ("corrupt", true)] {
            let directory = TemporaryDirectory::new(&format!("reader-{label}"));
            record_session(&directory.0, 400, trades(0..30));
            let session = open(&directory);
            let first = &session.segments[0].path;
            if damage {
                let mut bytes = std::fs::read(first).expect("segment reads");
                let last = bytes.len() - 2;
                bytes[last] ^= 0xff;
                std::fs::write(first, bytes).expect("segment damaged");
            } else {
                shorten(first, 3);
            }
            let next = (session.segments[1]
                .first_unix_nanos
                .expect("second segment")
                - at(0))
                / SECOND;
            let items = read(&session, at(0) - 1);
            let gap = items.iter().position(Option::is_none).expect("a gap");
            assert_eq!(items[gap + 1], Some(next), "{label}: {items:?}");
            assert_eq!(items[gap - 1], Some(next - 2), "{label}: {items:?}");
            assert_eq!(items.iter().filter(|item| item.is_none()).count(), 1);
            assert_eq!(items.last(), Some(&Some(29)));
        }
    }

    #[test]
    fn dropped_events_and_evicted_segments_are_gaps() {
        let directory = TemporaryDirectory::new("reader-gaps");
        let mut events = trades(0..60);
        events[5].1 = 4;
        record_session(&directory.0, 400, events);
        let session = open(&directory);
        let evicted = &session.segments[session.segments.len() - 2];
        let evicted_first = (evicted.first_unix_nanos.expect("an older segment") - at(0)) / SECOND;
        assert!(evicted_first > 6, "{evicted_first}");
        std::fs::remove_file(&evicted.path).expect("segment evicted");
        let items = read(&session, at(0) - 1);
        assert_eq!(
            items[..7],
            [Some(0), Some(1), Some(2), Some(3), Some(4), None, Some(5)]
        );
        let missing = items
            .iter()
            .rposition(Option::is_none)
            .expect("the evicted segment is a gap");
        assert!(items[missing + 1].is_some_and(|second| second > evicted_first));
        assert!(!items.contains(&Some(evicted_first)));

        assert_eq!(
            read(&session, at(4))[..2],
            [None, Some(5)],
            "a gap after the seek point"
        );
        assert_eq!(
            read(&session, at(5))[0],
            Some(6),
            "a gap before the seek point"
        );
    }

    #[test]
    fn trade_history_holds_only_the_trades_received_up_to_the_clock() {
        let directory = TemporaryDirectory::new("reader-history");
        record_session(
            &directory.0,
            1_000_000,
            vec![
                trade_at(40, 10),
                trade_at(41, 14),
                trade_at(45, 9),
                trade_at(100, 20),
                trade_at(101, 22),
                trade_at(165, 30),
            ],
        );
        let session = open(&directory);
        let current = |maximum_bars, through_unix_nanos| {
            trade_history(
                &session,
                &HistoryWindow {
                    period_seconds: 60,
                    maximum_bars,
                    range: None,
                    through_unix_nanos,
                },
            )
        };

        let history = current(100, at(101));
        assert_eq!(history.bars.len(), 1);
        let bar = &history.bars[0];
        assert_eq!(bar.exchange_timestamp_unix_nanos, MINUTE);
        assert_eq!((bar.open, bar.high, bar.low, bar.close), (10, 14, 9, 9));
        assert_eq!(bar.volume, 6);
        let (forming, count) = history.forming.expect("the minute at the clock forms");
        assert_eq!(forming.exchange_timestamp_unix_nanos, MINUTE + 60 * SECOND);
        assert_eq!((forming.open, forming.close, count), (20, 22, 2));
        assert_eq!(history.handoff_boundary_unix_nanos, Some(at(101)));
        assert!(history.backwards_exhausted);

        let before = current(100, at(100));
        assert_eq!(
            before.forming.map(|(bar, count)| (bar.close, count)),
            Some((20, 1))
        );
        assert_eq!(before.handoff_boundary_unix_nanos, Some(at(100)));

        let bounded = current(1, at(165));
        assert_eq!(bounded.bars.len(), 1);
        assert_eq!(bounded.bars[0].close, 22);
        assert_eq!(bounded.forming.map(|(bar, _)| bar.close), Some(30));
        assert!(!bounded.backwards_exhausted);

        let page = trade_history(
            &session,
            &HistoryWindow {
                period_seconds: 60,
                maximum_bars: 100,
                range: Some((MINUTE, MINUTE + 60 * SECOND)),
                through_unix_nanos: at(165),
            },
        );
        assert_eq!(page.bars.len(), 1);
        assert_eq!(page.bars[0].close, 9);
        assert_eq!(page.forming, None);
        assert_eq!(page.handoff_boundary_unix_nanos, None);
    }
}
