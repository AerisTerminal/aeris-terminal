//! The coordinator's recording tap: forwards accepted provider events for the
//! chosen instruments without ever blocking the coordinator.

use super::format::InstrumentFrame;
use super::writer::{RecordedEvent, RecorderMessage};
use super::{MarketRecording, RecordedInstrument};
use crate::market_service::provider_event::{ProviderEventKind, ProviderTradeBatch};
use aeris_contracts::InstallProviderInstrument;
use aeris_market_data::EventMetadata;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::SyncSender;

struct TapInstrument {
    instrument: RecordedInstrument,
    /// Session generation whose instrument frame the writer last received.
    described_generation: Option<u64>,
    /// Events dropped since the previous successful send, written as a gap.
    dropped: u64,
}

pub(in crate::market_service) struct RecordingTap {
    recording: Option<Arc<MarketRecording>>,
    revision: u64,
    events: Option<SyncSender<RecorderMessage>>,
    instruments: Vec<TapInstrument>,
}

impl RecordingTap {
    pub(in crate::market_service) const fn detached() -> Self {
        Self {
            recording: None,
            revision: 0,
            events: None,
            instruments: Vec::new(),
        }
    }

    pub(in crate::market_service) fn attached(recording: Arc<MarketRecording>) -> Self {
        Self {
            recording: Some(recording),
            ..Self::detached()
        }
    }

    /// Offers one accepted provider event. Trades, quotes and aggregate depth
    /// of a chosen instrument are recorded; order levels never are.
    pub(in crate::market_service) fn offer(
        &mut self,
        provider: &str,
        kind: &ProviderEventKind,
        catalog: &BTreeMap<(String, String), InstallProviderInstrument>,
    ) {
        // A replay re-emits recorded events; recording them again would copy
        // the recording into itself.
        if provider == super::replay::PROVIDER_ID {
            return;
        }
        let Some(revision) = self
            .recording
            .as_ref()
            .map(|recording| recording.revision())
        else {
            return;
        };
        if revision != self.revision {
            self.refresh();
        }
        if self.instruments.is_empty() {
            return;
        }
        match kind {
            ProviderEventKind::Trades(ProviderTradeBatch::One(trade)) => {
                self.offer_event(provider, &trade.metadata, catalog, || {
                    RecordedEvent::Trade(trade.clone())
                });
            }
            ProviderEventKind::Trades(ProviderTradeBatch::Many(trades)) => {
                for trade in trades {
                    self.offer_event(provider, &trade.metadata, catalog, || {
                        RecordedEvent::Trade(trade.clone())
                    });
                }
            }
            ProviderEventKind::Quote(quote) => {
                self.offer_event(provider, &quote.metadata, catalog, || {
                    RecordedEvent::Quote(quote.clone())
                });
            }
            ProviderEventKind::Depth(depth) => {
                self.offer_event(provider, &depth.metadata, catalog, || {
                    RecordedEvent::Depth(depth.clone())
                });
            }
            ProviderEventKind::OrderLevels(_)
            | ProviderEventKind::TransportReplaced
            | ProviderEventKind::Connecting
            | ProviderEventKind::Connected
            | ProviderEventKind::Heartbeat(_)
            | ProviderEventKind::Recovering { .. }
            | ProviderEventKind::Failed(_)
            | ProviderEventKind::Disconnected(_)
            | ProviderEventKind::Candle { .. } => {}
        }
    }

    fn refresh(&mut self) {
        let Some(recording) = &self.recording else {
            return;
        };
        let (revision, events, instruments) = recording.tap_view();
        let mut previous = std::mem::take(&mut self.instruments);
        self.instruments = instruments
            .into_iter()
            .map(|instrument| {
                previous
                    .iter()
                    .position(|entry| entry.instrument == instrument)
                    .map_or(
                        TapInstrument {
                            instrument,
                            described_generation: None,
                            dropped: 0,
                        },
                        |index| previous.swap_remove(index),
                    )
            })
            .collect();
        self.revision = revision;
        self.events = events;
    }

    fn offer_event(
        &mut self,
        provider: &str,
        metadata: &EventMetadata,
        catalog: &BTreeMap<(String, String), InstallProviderInstrument>,
        event: impl FnOnce() -> RecordedEvent,
    ) {
        let Some(entry) = self.instruments.iter_mut().find(|entry| {
            entry.instrument.provider_id == provider
                && entry.instrument.instrument_id == metadata.instrument_id
        }) else {
            return;
        };
        let (Some(recording), Some(events)) = (&self.recording, &self.events) else {
            return;
        };
        let dropped = |entry: &mut TapInstrument| {
            entry.dropped = entry.dropped.saturating_add(1);
            recording.shared.count_drop();
        };
        if entry.described_generation != Some(metadata.session_generation) {
            let Some(install) = catalog.get(&(provider.to_owned(), metadata.instrument_id.clone()))
            else {
                dropped(entry);
                return;
            };
            if events
                .try_send(RecorderMessage::Instrument(InstrumentFrame::from_install(
                    install,
                )))
                .is_err()
            {
                dropped(entry);
                return;
            }
            entry.described_generation = Some(metadata.session_generation);
        }
        let event = event();
        let reserved_bytes = event.estimated_bytes();
        if !recording.shared.reserve_in_flight(reserved_bytes) {
            dropped(entry);
            return;
        }
        let sent = events.try_send(RecorderMessage::Event {
            event,
            dropped_before: entry.dropped,
            reserved_bytes,
        });
        if sent.is_ok() {
            entry.dropped = 0;
        } else {
            recording.shared.release_in_flight(reserved_bytes);
            dropped(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{INSTRUMENT, PROVIDER, install, recorded, trade};
    use super::*;
    use crate::market_service::clock::ScriptedClock;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;

    #[test]
    fn a_full_queue_drops_and_counts_without_blocking_then_reports_the_gap() {
        let recording = Arc::new(MarketRecording::new(Arc::new(ScriptedClock::new(1))));
        let (events, receiver) = mpsc::sync_channel(2);
        let mut tap = RecordingTap {
            recording: Some(Arc::clone(&recording)),
            revision: recording.revision(),
            events: Some(events),
            instruments: vec![TapInstrument {
                instrument: recorded(INSTRUMENT),
                described_generation: None,
                dropped: 0,
            }],
        };
        let catalog = [(
            (PROVIDER.to_owned(), INSTRUMENT.to_owned()),
            install(INSTRUMENT),
        )]
        .into_iter()
        .collect();
        for sequence in 1..=5 {
            tap.offer(
                PROVIDER,
                &ProviderEventKind::Trades(ProviderTradeBatch::One(trade(INSTRUMENT, sequence))),
                &catalog,
            );
        }
        assert_eq!(recording.shared.dropped_events.load(Ordering::Relaxed), 4);
        let queued = receiver.try_iter().collect::<Vec<_>>();
        assert!(matches!(queued[0], RecorderMessage::Instrument(_)));
        let RecorderMessage::Event {
            dropped_before,
            reserved_bytes,
            ..
        } = &queued[1]
        else {
            panic!("the first trade is queued");
        };
        assert_eq!(*dropped_before, 0);
        assert_eq!(
            recording.shared.in_flight_bytes.load(Ordering::Relaxed),
            *reserved_bytes,
            "dropped events release their reservation"
        );
        tap.offer(
            PROVIDER,
            &ProviderEventKind::Trades(ProviderTradeBatch::One(trade(INSTRUMENT, 6))),
            &catalog,
        );
        let Some(RecorderMessage::Event { dropped_before, .. }) = receiver.try_iter().next() else {
            panic!("the next trade is queued");
        };
        assert_eq!(
            dropped_before, 4,
            "the writer learns how many events it missed"
        );
        assert_eq!(recording.status().dropped_events, 4);
    }

    #[test]
    fn the_in_flight_bound_drops_instead_of_queueing_unbounded_memory() {
        let recording = Arc::new(MarketRecording::new(Arc::new(ScriptedClock::new(1))));
        assert!(
            recording
                .shared
                .reserve_in_flight(super::super::MAXIMUM_IN_FLIGHT_BYTES)
        );
        assert!(!recording.shared.reserve_in_flight(1));
        recording
            .shared
            .release_in_flight(super::super::MAXIMUM_IN_FLIGHT_BYTES);
        assert!(recording.shared.reserve_in_flight(1));
    }
}
