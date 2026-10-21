//! Offline composition of the production coordinator and one scripted provider.
//! This is deliberately absent from ordinary market-runtime builds.

use super::{
    Arc, AtomicBool, BTreeSet, Command, Duration, HISTORY_CAPACITY, HYPERLIQUID_DESCRIPTOR,
    HYPERLIQUID_PUBLIC_ACCOUNT_ID, HyperliquidChannels, HyperliquidDemand,
    InstallProviderInstrument, MarketEngine, MarketService, Mutex, Ordering,
    ProviderCatalogChannel, ProviderCatalogChannels, ProviderCatalogCommand,
    ProviderCoordinatorWake, ProviderRealtimeSpec, ProviderRuntimeLifecycle, ProviderRuntimeRecord,
    ProviderRuntimeSpec, ProviderStartContext, REALTIME_CAPACITY,
    RITHMIC_REALTIME_CONTROL_CAPACITY, Receiver, RecvTimeoutError, SyncSender, TopOfBookQuote,
    mpsc, thread, validate_provider_instrument,
};
use crate::hyperliquid_realtime::{HyperliquidRealtimeControl, HyperliquidRealtimeEvent};
use aeris_market_data::{EventMetadata, QualifiedTimestamp, QuoteLevel};
use aeris_market_engine::fence::{Fenced, ProviderTag};

struct ScriptedQuote {
    bid: i64,
    ask: i64,
    sequence: u64,
    received_unix_nanos: i64,
    reply: SyncSender<Result<(), String>>,
}

enum ScriptedRequest {
    Quote(ScriptedQuote),
    Subscriptions(SyncSender<(usize, usize)>),
}

pub(super) struct ScriptedChannels {
    quotes: Receiver<ScriptedRequest>,
    instrument: InstallProviderInstrument,
}

/// Test-only handle to the bounded, lease-aware provider worker.
pub struct ScriptedQuoteProvider {
    quotes: SyncSender<ScriptedRequest>,
}

impl ScriptedQuoteProvider {
    /// Submits one quote through the provider lane, rather than the trading inbox.
    /// The provider rejects quotes until its real instrument subscription is active.
    ///
    /// # Errors
    /// Returns an error for invalid prices, no lease, or exhausted/disconnected queues.
    pub fn quote(
        &self,
        bid: i64,
        ask: i64,
        sequence: u64,
        received_unix_nanos: i64,
    ) -> Result<(), String> {
        if bid <= 0 || ask <= bid || sequence == 0 || received_unix_nanos <= 0 {
            return Err("invalid scripted quote".into());
        }
        let (reply, result) = mpsc::sync_channel(1);
        self.quotes
            .try_send(ScriptedRequest::Quote(ScriptedQuote {
                bid,
                ask,
                sequence,
                received_unix_nanos,
                reply,
            }))
            .map_err(|_| "scripted provider queue is full or unavailable".to_string())?;
        result
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "scripted provider did not acknowledge quote".to_string())?
    }

    /// Reports the provider worker's current instrument-quote and candle
    /// subscriptions, read directly from its last coordinator demand.
    ///
    /// # Errors
    /// Returns an error if the bounded worker queue is unavailable.
    pub fn subscriptions(&self) -> Result<(usize, usize), String> {
        let (reply, result) = mpsc::sync_channel(1);
        self.quotes
            .try_send(ScriptedRequest::Subscriptions(reply))
            .map_err(|_| "scripted provider queue is full or unavailable".to_string())?;
        result
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "scripted provider did not report subscriptions".to_string())
    }
}

impl MarketService {
    /// Composes the real bounded coordinator, demand registry and feed bridge
    /// with one offline provider. No chart consumer or network session is started.
    ///
    /// # Errors
    /// Returns an error for invalid fixture terms or a runtime startup failure.
    pub fn start_scripted_quotes(
        instrument: InstallProviderInstrument,
    ) -> Result<(Self, ScriptedQuoteProvider), String> {
        if instrument.provider != HYPERLIQUID_DESCRIPTOR.id
            || instrument.entitlement_id != HYPERLIQUID_PUBLIC_ACCOUNT_ID
            || instrument.session_generation == 0
        {
            return Err("scripted instrument must use the fixture's provider route".into());
        }
        validate_provider_instrument(&instrument)?;
        let installed = instrument.clone();
        let (quotes, receiver) = mpsc::sync_channel(8);
        let spec = ProviderRuntimeSpec {
            descriptor: super::ProviderDescriptor {
                start: start_scripted_runtime,
                ..HYPERLIQUID_DESCRIPTOR
            },
            history: None,
            realtime: ProviderRealtimeSpec { enabled: true },
            scripted: Some(ScriptedChannels {
                quotes: receiver,
                instrument,
            }),
        };
        // Install the fixture's verified Terms before trading asks for its
        // route. Demand and subsequent quotes still use the production lanes.
        let service = Self::start_composed(vec![spec])?;
        service.install_provider_instrument(&installed)?;
        Ok((service, ScriptedQuoteProvider { quotes }))
    }
}

fn start_scripted_runtime(
    mut spec: ProviderRuntimeSpec,
    _completions: &SyncSender<Command>,
    wake: ProviderCoordinatorWake,
    _engine: &MarketEngine,
    _active_workers: &Arc<Mutex<BTreeSet<String>>>,
    _context: &ProviderStartContext,
) -> Result<ProviderRuntimeRecord, String> {
    let channels = spec.scripted.take().ok_or("scripted channels missing")?;
    let instrument = channels.instrument;
    let (history, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (controls, controls_rx) = mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
    let (events_tx, events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let (display_controls, display_rx) = mpsc::sync_channel(RITHMIC_REALTIME_CONTROL_CAPACITY);
    let (_display_tx, display_events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let cancellation = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&cancellation);
    let worker = thread::Builder::new()
        .name("aeris-scripted-provider".into())
        .spawn(move || {
            run_scripted_worker(
                &instrument,
                &WorkerChannels {
                    quotes: channels.quotes,
                    controls: controls_rx,
                    display: display_rx,
                    history: history_rx,
                    events: events_tx,
                },
                &wake,
                &stop,
            );
        })
        .map_err(|error| error.to_string())?;
    Ok(ProviderRuntimeRecord {
        descriptor: spec.descriptor,
        history,
        cancellation,
        lifecycle: Arc::new(ProviderRuntimeLifecycle::default()),
        hyperliquid: Some(HyperliquidChannels {
            controls,
            events,
            display_controls,
            display_events,
        }),
        rithmic: None,
        catalog: ProviderCatalogChannels {
            enabled: false,
            channels: Box::new(UnavailableCatalog),
        },
        workers: vec![worker],
    })
}

struct UnavailableCatalog;

impl ProviderCatalogChannel for UnavailableCatalog {
    fn try_recv(&self) -> Option<super::ProviderCatalogEvent> {
        None
    }

    fn send(&self, _: &str, _: ProviderCatalogCommand) -> Result<(), String> {
        Err("scripted fixture catalog is fixed at composition".into())
    }
}

struct WorkerChannels {
    quotes: Receiver<ScriptedRequest>,
    controls: Receiver<HyperliquidRealtimeControl>,
    display: Receiver<crate::hyperliquid_display_depth::HyperliquidDisplayDepthControl>,
    history: Receiver<super::HistoryRequest>,
    events: SyncSender<Fenced<ProviderTag, HyperliquidRealtimeEvent>>,
}

fn run_scripted_worker(
    instrument: &InstallProviderInstrument,
    channels: &WorkerChannels,
    wake: &ProviderCoordinatorWake,
    stop: &AtomicBool,
) {
    let mut sink: Option<super::provider_session::ProviderEventSink> = None;
    let mut demand = HyperliquidDemand::default();
    while !stop.load(Ordering::Acquire) {
        while let Ok(request) = channels.quotes.try_recv() {
            match request {
                ScriptedRequest::Subscriptions(reply) => {
                    let _ = reply.send((demand.quotes.len(), demand.candles.len()));
                }
                ScriptedRequest::Quote(quote) => {
                    let subscribed = demand
                        .quotes
                        .iter()
                        .any(|key| key.instrument_id == instrument.instrument_id);
                    let result = sink.filter(|_| subscribed).map_or_else(
                        || Err("scripted quote has no active instrument lease".into()),
                        |sink| {
                            channels
                                .events
                                .try_send(sink.stamp(quote_event(instrument, &quote, sink)))
                                .map_err(|_| {
                                    "scripted realtime queue is full or unavailable".to_string()
                                })
                                .map(|()| wake.notify())
                        },
                    );
                    let _ = quote.reply.send(result);
                }
            }
        }
        // A chart would require history. Do not silently serve an empty page.
        if channels.history.try_recv().is_ok() {
            break;
        }
        while channels.display.try_recv().is_ok() {}
        match channels.controls.recv_timeout(Duration::from_millis(2)) {
            Ok(HyperliquidRealtimeControl::Connect(next)) => {
                let generation = next.epoch.get().get();
                sink = Some(next);
                for event in [
                    HyperliquidRealtimeEvent::Connecting(generation),
                    HyperliquidRealtimeEvent::Connected(generation),
                ] {
                    if channels.events.try_send(next.stamp(event)).is_ok() {
                        wake.notify();
                    }
                }
            }
            Ok(HyperliquidRealtimeControl::Subscribe(next)) => demand = next,
            Ok(HyperliquidRealtimeControl::Stop) => {
                sink = None;
                demand = HyperliquidDemand::default();
            }
            #[cfg(feature = "probe")]
            Ok(HyperliquidRealtimeControl::FailWebSocket { .. }) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn quote_event(
    instrument: &InstallProviderInstrument,
    quote: &ScriptedQuote,
    sink: super::provider_session::ProviderEventSink,
) -> HyperliquidRealtimeEvent {
    let generation = sink.epoch.get().get();
    HyperliquidRealtimeEvent::Quote(
        generation,
        TopOfBookQuote {
            metadata: EventMetadata {
                provider_id: instrument.provider.clone(),
                instrument_id: instrument.instrument_id.clone(),
                entitlement_id: instrument.entitlement_id.clone(),
                source_sequence: quote.sequence,
                session_generation: generation,
                timestamps: QualifiedTimestamp {
                    exchange_unix_nanos: None,
                    provider_unix_nanos: None,
                    received_unix_nanos: quote.received_unix_nanos,
                },
            },
            bid: Some(QuoteLevel {
                price: quote.bid,
                quantity: None,
                order_count: None,
            }),
            ask: Some(QuoteLevel {
                price: quote.ask,
                quantity: None,
                order_count: None,
            }),
        },
    )
}
