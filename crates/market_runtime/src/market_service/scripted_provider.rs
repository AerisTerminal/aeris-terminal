//! Offline composition of the production coordinator and one scripted provider.
//! This is deliberately absent from ordinary market-runtime builds.

use super::{
    Arc, AtomicBool, Duration, HISTORY_CAPACITY, HistoryRequest, InstallProviderInstrument,
    MarketService, Ordering, ProviderCatalogChannel, ProviderCatalogChannels,
    ProviderCatalogCommand, ProviderCoordinatorWake, ProviderDemand, ProviderEvent,
    ProviderEventKind, ProviderRealtimeChannels, ProviderRuntimeLifecycle, ProviderRuntimeRecord,
    ProviderRuntimeSpec, ProviderStartHandle, REALTIME_CAPACITY, Receiver, RecvTimeoutError,
    SyncSender, TEST_CANDLE_DESCRIPTOR, TopOfBookQuote, mpsc,
    provider_session::{ProviderEventSink, ProviderSession, ProviderSessionError, try_control},
    thread, validate_provider_instrument,
};
use aeris_market_data::{EventMetadata, QualifiedTimestamp, QuoteLevel};
use aeris_market_engine::MarketStream;

const SCRIPTED_CONTROL_CAPACITY: usize = 4;

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

enum ScriptedControl {
    Connect(ProviderEventSink),
    Subscribe { quotes: Vec<String>, candles: usize },
    Stop,
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
        if instrument.provider != TEST_CANDLE_DESCRIPTOR.id
            || instrument.entitlement_id != TEST_CANDLE_DESCRIPTOR.capabilities.account_id
            || instrument.session_generation == 0
        {
            return Err("scripted instrument must use the fixture's provider route".into());
        }
        validate_provider_instrument(&instrument)?;
        let installed = instrument.clone();
        let (quotes, receiver) = mpsc::sync_channel(8);
        let spec = ProviderRuntimeSpec::new(
            TEST_CANDLE_DESCRIPTOR,
            None,
            true,
            Box::new(move |spec, handle| {
                start_scripted_runtime(&spec, handle, receiver, instrument)
            }),
        );
        // Install the fixture's verified Terms before trading asks for its
        // route. Demand and subsequent quotes still use the production lanes.
        let service = Self::start_composed(vec![spec])?;
        service.install_provider_instrument(&installed)?;
        Ok((service, ScriptedQuoteProvider { quotes }))
    }
}

struct ScriptedSession {
    controls: SyncSender<ScriptedControl>,
    history: SyncSender<HistoryRequest>,
}

impl ProviderSession for ScriptedSession {
    fn connect(
        &self,
        _epoch: aeris_market_engine::fence::Epoch<aeris_market_engine::fence::ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        try_control(&self.controls, ScriptedControl::Connect(sink))
    }

    fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError> {
        let quotes = demand
            .instruments
            .iter()
            .filter(|requested| requested.streams.contains(MarketStream::Quotes))
            .map(|requested| requested.instrument.instrument_id.clone())
            .collect();
        let candles = demand
            .candle_series
            .iter()
            .filter(|(_, streams, _)| streams.contains(MarketStream::Bars))
            .count();
        try_control(
            &self.controls,
            ScriptedControl::Subscribe { quotes, candles },
        )
    }

    fn request_history(&self, request: HistoryRequest) -> Result<bool, ProviderSessionError> {
        try_control(&self.history, request)
    }

    fn release_idle(&self) -> Result<bool, ProviderSessionError> {
        try_control(&self.controls, ScriptedControl::Stop)
    }

    fn shutdown(&self) {
        let _ = try_control(&self.controls, ScriptedControl::Stop);
    }
}

fn start_scripted_runtime(
    spec: &ProviderRuntimeSpec,
    handle: &ProviderStartHandle<'_>,
    quotes: Receiver<ScriptedRequest>,
    instrument: InstallProviderInstrument,
) -> Result<ProviderRuntimeRecord, String> {
    let wake = handle.wake();
    let (history, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
    let (controls, controls_rx) = mpsc::sync_channel(SCRIPTED_CONTROL_CAPACITY);
    let (events_tx, events) = mpsc::sync_channel(REALTIME_CAPACITY);
    let cancellation = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&cancellation);
    let worker = thread::Builder::new()
        .name("aeris-scripted-provider".into())
        .spawn(move || {
            run_scripted_worker(
                &instrument,
                &WorkerChannels {
                    quotes,
                    controls: controls_rx,
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
        realtime: Some(ProviderRealtimeChannels::new(
            move |history| {
                Box::new(ScriptedSession {
                    controls: controls.clone(),
                    history: history.clone(),
                })
            },
            move || events.try_recv().ok(),
        )),
        display_depth: None,
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
    controls: Receiver<ScriptedControl>,
    history: Receiver<HistoryRequest>,
    events: SyncSender<ProviderEvent>,
}

fn run_scripted_worker(
    instrument: &InstallProviderInstrument,
    channels: &WorkerChannels,
    wake: &ProviderCoordinatorWake,
    stop: &AtomicBool,
) {
    let mut sink: Option<ProviderEventSink> = None;
    let mut subscribed_quotes: Vec<String> = Vec::new();
    let mut subscribed_candles: usize = 0;
    while !stop.load(Ordering::Acquire) {
        while let Ok(request) = channels.quotes.try_recv() {
            match request {
                ScriptedRequest::Subscriptions(reply) => {
                    let _ = reply.send((subscribed_quotes.len(), subscribed_candles));
                }
                ScriptedRequest::Quote(quote) => {
                    let subscribed = subscribed_quotes
                        .iter()
                        .any(|id| id == &instrument.instrument_id);
                    let result = sink.filter(|_| subscribed).map_or_else(
                        || Err("scripted quote has no active instrument lease".into()),
                        |sink| {
                            channels
                                .events
                                .try_send(bound_event(sink, quote_event(instrument, &quote, sink)))
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
        match channels.controls.recv_timeout(Duration::from_millis(2)) {
            Ok(ScriptedControl::Connect(next)) => {
                sink = Some(next);
                for kind in [ProviderEventKind::Connecting, ProviderEventKind::Connected] {
                    if channels.events.try_send(bound_event(next, kind)).is_ok() {
                        wake.notify();
                    }
                }
            }
            Ok(ScriptedControl::Subscribe { quotes, candles }) => {
                subscribed_quotes = quotes;
                subscribed_candles = candles;
            }
            Ok(ScriptedControl::Stop) => {
                sink = None;
                subscribed_quotes.clear();
                subscribed_candles = 0;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn bound_event(sink: ProviderEventSink, kind: ProviderEventKind) -> ProviderEvent {
    ProviderEvent {
        provider: TEST_CANDLE_DESCRIPTOR.id,
        generation: sink.epoch,
        wire_generation: sink.epoch.get().get(),
        kind,
    }
}

fn quote_event(
    instrument: &InstallProviderInstrument,
    quote: &ScriptedQuote,
    sink: ProviderEventSink,
) -> ProviderEventKind {
    let generation = sink.epoch.get().get();
    ProviderEventKind::Quote(TopOfBookQuote {
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
    })
}
