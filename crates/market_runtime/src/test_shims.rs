//! Adapter-free mock provider harnesses used only by `aeris_market_runtime`'s
//! internal unit tests. Production builds never compile this module and
//! `aeris_market_runtime` has zero Cargo dependencies on any adapter crate.

#![cfg(test)]

use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
};

use aeris_contracts::InstallProviderInstrument;
use aeris_market_data::{BarPeriod, MarketBar, MarketTrade, OrderLevelEvent};
use aeris_market_engine::{
    MarketStream, StreamRequirements,
    fence::{Epoch, Fenced, ProviderTag},
};

use crate::{
    ProviderInstrumentSearchResult,
    market_service::{
        COMMAND_CAPACITY, DisplayDepthRequest, HYPERLIQUID_DESCRIPTOR, HistoryRequest,
        HistorySnapshot, HistorySource, ProviderCatalogChannelImpl, ProviderCatalogChannels,
        ProviderCatalogCommand, ProviderCatalogEvent, ProviderDemand, ProviderDisplayDepthChannels,
        ProviderDisplayDepthEvent, ProviderRealtimeChannels, ProviderRuntimeRecord,
        ProviderRuntimeSpec, ProviderStartHandle, RITHMIC_DESCRIPTOR,
        provider_event::{
            ProviderCandleSymbol, ProviderDisconnect, ProviderEvent, ProviderEventKind,
            ProviderFault, ProviderFaultKind, ProviderTradeBatch, WireProviderEvent,
        },
        provider_session::{ProviderEventSink, ProviderSession, ProviderSessionError, try_control},
        try_send_catalog_control,
    },
};

pub(crate) mod hyperliquid_display_depth {
    use super::hyperliquid_realtime::HyperliquidInstrumentDemand;

    #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
    pub(crate) struct HyperliquidDisplayBookDemand {
        pub(crate) instrument: HyperliquidInstrumentDemand,
        pub(crate) provider_generation: u64,
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub(crate) struct HyperliquidDisplayDepthDemand {
        pub(crate) books: Vec<HyperliquidDisplayBookDemand>,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) enum HyperliquidDisplayDepthControl {
        Subscribe(HyperliquidDisplayDepthDemand),
        Stop,
    }

    pub(crate) enum HyperliquidDisplayDepthEvent {}
}

pub(crate) mod hyperliquid_realtime {
    use super::*;

    #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
    pub(crate) struct HyperliquidInstrumentDemand {
        pub(crate) wire_coin: String,
        pub(crate) instrument_id: String,
        pub(crate) entitlement_id: String,
        pub(crate) price_scale: u8,
        pub(crate) quantity_scale: u8,
    }

    #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
    pub(crate) struct HyperliquidCandleDemand {
        pub(crate) instrument: HyperliquidInstrumentDemand,
        pub(crate) interval: String,
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub(crate) struct HyperliquidDemand {
        pub(crate) trades: Vec<HyperliquidInstrumentDemand>,
        pub(crate) quotes: Vec<HyperliquidInstrumentDemand>,
        pub(crate) depths: Vec<HyperliquidInstrumentDemand>,
        pub(crate) candles: Vec<HyperliquidCandleDemand>,
    }

    pub(crate) enum HyperliquidRealtimeControl {
        Connect(ProviderEventSink),
        Subscribe(()),
        Stop,
        #[cfg(feature = "probe")]
        FailWebSocket,
    }

    pub(crate) enum HyperliquidCatalogControl {
        Search(()),
        Select(()),
        Screen(()),
        Scan { period: BarPeriod },
    }

    pub(crate) enum HyperliquidCatalogEvent {}

    impl From<HyperliquidCatalogEvent> for ProviderCatalogEvent {
        fn from(event: HyperliquidCatalogEvent) -> Self {
            match event {}
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) struct MockLiveCandle {
        pub(crate) open_nanos: i64,
        pub(crate) open: i64,
        pub(crate) high: i64,
        pub(crate) low: i64,
        pub(crate) close: i64,
        pub(crate) volume: i64,
    }

    pub(crate) enum HyperliquidRealtimeEvent {
        Connecting(u64),
        Connected(u64),
        Heartbeat(u64, Option<u64>),
        Disconnected(u64),
        Candle(u64, String, String, MockLiveCandle),
    }

    pub(crate) fn into_wire_provider_event(event: HyperliquidRealtimeEvent) -> WireProviderEvent {
        let (generation, kind) = match event {
            HyperliquidRealtimeEvent::Connecting(generation) => {
                (generation, ProviderEventKind::Connecting)
            }
            HyperliquidRealtimeEvent::Connected(generation) => {
                (generation, ProviderEventKind::Connected)
            }
            HyperliquidRealtimeEvent::Heartbeat(generation, rtt) => {
                (generation, ProviderEventKind::Heartbeat(rtt))
            }
            HyperliquidRealtimeEvent::Disconnected(generation) => (
                generation,
                ProviderEventKind::Disconnected(ProviderDisconnect::Recover {
                    detail: "Mock candle feed disconnected",
                    auto_recover: true,
                    retry_after: None,
                    provider_detail: None,
                }),
            ),
            HyperliquidRealtimeEvent::Candle(generation, coin, interval, candle) => (
                generation,
                ProviderEventKind::Candle {
                    symbol: ProviderCandleSymbol { coin, interval },
                    bar: MarketBar {
                        exchange_timestamp_seconds: candle.open_nanos.div_euclid(1_000_000_000),
                        exchange_timestamp_unix_nanos: candle.open_nanos,
                        open: candle.open,
                        high: candle.high,
                        low: candle.low,
                        close: candle.close,
                        volume: candle.volume,
                        source_sequence: 0,
                    },
                },
            ),
        };
        WireProviderEvent {
            provider: HYPERLIQUID_DESCRIPTOR.id,
            generation,
            kind,
        }
    }
}

pub(crate) mod rithmic_realtime {
    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct RithmicInstrumentDemand {
        pub(crate) instrument: InstallProviderInstrument,
        pub(crate) streams: StreamRequirements,
        pub(crate) order_levels: bool,
    }

    impl RithmicInstrumentDemand {
        pub(crate) const fn trades(&self) -> bool {
            self.streams.contains(MarketStream::Bars) || self.streams.contains(MarketStream::Trades)
        }

        pub(crate) const fn quotes(&self) -> bool {
            self.streams.contains(MarketStream::Quotes)
        }

        pub(crate) const fn order_book(&self) -> bool {
            self.streams.contains(MarketStream::Depth)
        }
    }

    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub(crate) struct RithmicRealtimeDemand {
        pub(crate) instruments: Vec<RithmicInstrumentDemand>,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) enum RithmicRealtimeControl {
        Subscribe(RithmicRealtimeDemand),
        Stop,
    }

    pub(crate) enum RithmicCatalogControl {
        Search(()),
        Select(()),
    }

    pub(crate) enum RithmicCatalogEvent {
        SearchCompleted(ProviderInstrumentSearchResult),
    }

    impl From<RithmicCatalogEvent> for ProviderCatalogEvent {
        fn from(event: RithmicCatalogEvent) -> Self {
            match event {
                RithmicCatalogEvent::SearchCompleted(result) => Self::SearchCompleted(result),
            }
        }
    }

    pub(crate) enum RithmicRealtimeEvent {
        Connecting(u64),
        Connected(u64),
        Heartbeat(u64, Option<u64>),
        Disconnected(u64, Option<ProviderFault>),
        Trade(u64, MarketTrade),
        OrderLevels(u64, OrderLevelEvent),
    }

    pub(crate) fn into_wire_provider_event(event: RithmicRealtimeEvent) -> WireProviderEvent {
        let default_fault = || ProviderFault {
            kind: ProviderFaultKind::Transport,
            auto_recovers: true,
            detail: "Mock provider transport disconnected",
        };
        let (generation, kind) = match event {
            RithmicRealtimeEvent::Connecting(generation) => {
                (generation, ProviderEventKind::Connecting)
            }
            RithmicRealtimeEvent::Connected(generation) => {
                (generation, ProviderEventKind::Connected)
            }
            RithmicRealtimeEvent::Heartbeat(generation, rtt) => {
                (generation, ProviderEventKind::Heartbeat(rtt))
            }
            RithmicRealtimeEvent::Disconnected(generation, fault) => (
                generation,
                ProviderEventKind::Disconnected(ProviderDisconnect::from_fault(
                    fault.unwrap_or_else(default_fault),
                )),
            ),
            RithmicRealtimeEvent::Trade(generation, trade) => (
                generation,
                ProviderEventKind::Trades(ProviderTradeBatch::One(trade)),
            ),
            RithmicRealtimeEvent::OrderLevels(generation, levels) => {
                (generation, ProviderEventKind::OrderLevels(levels))
            }
        };
        WireProviderEvent {
            provider: RITHMIC_DESCRIPTOR.id,
            generation,
            kind,
        }
    }
}

pub(crate) mod providers {
    use super::*;

    pub(crate) mod hyperliquid {
        use super::super::hyperliquid_display_depth::{
            HyperliquidDisplayBookDemand, HyperliquidDisplayDepthControl,
            HyperliquidDisplayDepthDemand, HyperliquidDisplayDepthEvent,
        };
        use super::super::hyperliquid_realtime::{
            HyperliquidCandleDemand, HyperliquidCatalogControl, HyperliquidDemand,
            HyperliquidInstrumentDemand, HyperliquidRealtimeControl, HyperliquidRealtimeEvent,
        };
        use super::*;

        pub(crate) fn dispatch_catalog(
            controls: &SyncSender<HyperliquidCatalogControl>,
            provider: &str,
            command: ProviderCatalogCommand,
        ) -> Result<(), String> {
            let control = match command {
                ProviderCatalogCommand::Search(_) => HyperliquidCatalogControl::Search(()),
                ProviderCatalogCommand::Select(_) => HyperliquidCatalogControl::Select(()),
                ProviderCatalogCommand::Screen(_) => HyperliquidCatalogControl::Screen(()),
                ProviderCatalogCommand::Scan(scan) => {
                    let interval = scan.definition.timeframe;
                    let period = crate::resolve_cadence(
                        interval,
                        HYPERLIQUID_DESCRIPTOR.presentation,
                        aeris_instruments::CalendarSourceKind::AlwaysOpen,
                    )
                    .map_err(|_| format!("{provider} does not offer a {interval} market scan"))?;
                    HyperliquidCatalogControl::Scan { period }
                }
            };
            try_send_catalog_control(controls, control, provider)
        }

        pub(crate) fn hyperliquid_wire(demand: &ProviderDemand) -> HyperliquidDemand {
            let mut trades = BTreeSet::new();
            let mut quotes = BTreeSet::new();
            let mut depths = BTreeSet::new();
            let mut candles = BTreeSet::new();
            for requested in &demand.instruments {
                let (Ok(price_scale), Ok(quantity_scale)) = (
                    u8::try_from(requested.instrument.price_scale),
                    u8::try_from(requested.instrument.quantity_scale),
                ) else {
                    continue;
                };
                let instrument = HyperliquidInstrumentDemand {
                    wire_coin: requested.instrument.provider_symbol.clone(),
                    instrument_id: requested.instrument.instrument_id.clone(),
                    entitlement_id: requested.instrument.entitlement_id.clone(),
                    price_scale,
                    quantity_scale,
                };
                if requested.streams.contains(MarketStream::Trades) {
                    trades.insert(instrument.clone());
                }
                if requested.streams.contains(MarketStream::Quotes) {
                    quotes.insert(instrument.clone());
                }
                if requested.streams.contains(MarketStream::Depth) {
                    depths.insert(instrument);
                }
            }
            for (series, streams, installed) in &demand.candle_series {
                let (Ok(price_scale), Ok(quantity_scale)) = (
                    u8::try_from(installed.price_scale),
                    u8::try_from(installed.quantity_scale),
                ) else {
                    continue;
                };
                let Some(interval) = HYPERLIQUID_DESCRIPTOR
                    .capabilities
                    .candle_wire_interval
                    .and_then(|resolve| resolve(series.period).ok())
                else {
                    continue;
                };
                let instrument = HyperliquidInstrumentDemand {
                    wire_coin: installed.provider_symbol.clone(),
                    instrument_id: installed.instrument_id.clone(),
                    entitlement_id: installed.entitlement_id.clone(),
                    price_scale,
                    quantity_scale,
                };
                if streams.contains(MarketStream::Bars) {
                    candles.insert(HyperliquidCandleDemand {
                        instrument: instrument.clone(),
                        interval,
                    });
                }
                if streams.contains(MarketStream::Trades) {
                    trades.insert(instrument.clone());
                }
                if streams.contains(MarketStream::Quotes) {
                    quotes.insert(instrument.clone());
                }
                if streams.contains(MarketStream::Depth) {
                    depths.insert(instrument);
                }
            }
            HyperliquidDemand {
                trades: trades.into_iter().collect(),
                quotes: quotes.into_iter().collect(),
                depths: depths.into_iter().collect(),
                candles: candles.into_iter().collect(),
            }
        }

        pub(crate) struct HyperliquidSession {
            pub(crate) controls: SyncSender<HyperliquidRealtimeControl>,
            pub(crate) display_controls: SyncSender<HyperliquidDisplayDepthControl>,
            pub(crate) history: SyncSender<HistoryRequest>,
        }

        impl ProviderSession for HyperliquidSession {
            #[cfg(feature = "probe")]
            fn probe_fail_websocket(
                &self,
                _subsequent_connect_failures: u8,
            ) -> Result<bool, ProviderSessionError> {
                try_control(&self.controls, HyperliquidRealtimeControl::FailWebSocket)
            }

            fn connect(
                &self,
                _epoch: Epoch<ProviderTag>,
                sink: ProviderEventSink,
            ) -> Result<bool, ProviderSessionError> {
                try_control(&self.controls, HyperliquidRealtimeControl::Connect(sink))
            }

            fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError> {
                let _ = hyperliquid_wire(demand);
                try_control(&self.controls, HyperliquidRealtimeControl::Subscribe(()))
            }

            fn request_history(
                &self,
                request: HistoryRequest,
            ) -> Result<bool, ProviderSessionError> {
                try_control(&self.history, request)
            }

            fn release_idle(&self) -> Result<bool, ProviderSessionError> {
                let _ = self
                    .display_controls
                    .try_send(HyperliquidDisplayDepthControl::Stop);
                try_control(&self.controls, HyperliquidRealtimeControl::Stop)
            }

            fn shutdown(&self) {
                let _ = self
                    .display_controls
                    .try_send(HyperliquidDisplayDepthControl::Stop);
                let _ = self.controls.try_send(HyperliquidRealtimeControl::Stop);
            }
        }

        pub(crate) fn realtime_channels(
            controls: SyncSender<HyperliquidRealtimeControl>,
            events: Receiver<Fenced<ProviderTag, HyperliquidRealtimeEvent>>,
            display_controls: SyncSender<HyperliquidDisplayDepthControl>,
        ) -> ProviderRealtimeChannels {
            ProviderRealtimeChannels::new(
                move |history| {
                    Box::new(HyperliquidSession {
                        controls: controls.clone(),
                        display_controls: display_controls.clone(),
                        history: history.clone(),
                    })
                },
                move || {
                    events.try_recv().ok().map(|fenced| {
                        let wire = super::super::hyperliquid_realtime::into_wire_provider_event(
                            fenced.value,
                        );
                        ProviderEvent::bound(
                            wire.provider,
                            fenced.epoch,
                            wire.generation,
                            wire.kind,
                        )
                    })
                },
            )
        }

        pub(crate) fn display_depth_channels(
            controls: SyncSender<HyperliquidDisplayDepthControl>,
            events: Receiver<HyperliquidDisplayDepthEvent>,
        ) -> ProviderDisplayDepthChannels {
            ProviderDisplayDepthChannels::new(
                move |requests: &[DisplayDepthRequest]| {
                    let mut books = BTreeSet::new();
                    for request in requests {
                        let (Ok(price_scale), Ok(quantity_scale)) = (
                            u8::try_from(request.instrument.price_scale),
                            u8::try_from(request.instrument.quantity_scale),
                        ) else {
                            continue;
                        };
                        books.insert(HyperliquidDisplayBookDemand {
                            instrument: HyperliquidInstrumentDemand {
                                wire_coin: request.instrument.provider_symbol.clone(),
                                instrument_id: request.instrument.instrument_id.clone(),
                                entitlement_id: request.instrument.entitlement_id.clone(),
                                price_scale,
                                quantity_scale,
                            },
                            provider_generation: request.provider_generation,
                        });
                    }
                    match controls.try_send(HyperliquidDisplayDepthControl::Subscribe(
                        HyperliquidDisplayDepthDemand {
                            books: books.into_iter().collect(),
                        },
                    )) {
                        Ok(()) => Ok(true),
                        Err(TrySendError::Full(_)) => Ok(false),
                        Err(TrySendError::Disconnected(_)) => {
                            Err("Provider display-depth worker is unavailable".to_string())
                        }
                    }
                },
                move || {
                    events
                        .try_recv()
                        .ok()
                        .map(|event| -> ProviderDisplayDepthEvent { match event {} })
                },
            )
        }
    }

    pub(crate) mod rithmic {
        use super::super::rithmic_realtime::{
            RithmicCatalogControl, RithmicCatalogEvent, RithmicInstrumentDemand,
            RithmicRealtimeControl, RithmicRealtimeDemand, RithmicRealtimeEvent,
        };
        use super::*;

        pub(crate) struct MockHistorySource;

        impl HistorySource for MockHistorySource {
            fn fetch(&mut self, _request: &HistoryRequest) -> Result<HistorySnapshot, String> {
                Err("mock history source".to_string())
            }
        }

        pub(crate) fn rithmic_supported_period(period: BarPeriod) -> Result<(), String> {
            match period {
                BarPeriod::Tick { trades: 0 } => Err("tick period must be non-zero".to_string()),
                _ => Ok(()),
            }
        }

        pub(crate) fn mock_history_only_spec(
            descriptor: crate::market_service::ProviderDescriptor,
            history: Box<dyn HistorySource>,
        ) -> ProviderRuntimeSpec {
            ProviderRuntimeSpec::new(
                descriptor,
                Some(history),
                false,
                Box::new(|spec, handle: &ProviderStartHandle<'_>| {
                    let descriptor = spec.descriptor;
                    let started = handle.start_history_runtime(spec)?;
                    let (catalog_controls, _catalog_rx) =
                        mpsc::sync_channel::<RithmicCatalogControl>(COMMAND_CAPACITY);
                    let (_catalog_tx, catalog_events) =
                        mpsc::sync_channel::<RithmicCatalogEvent>(COMMAND_CAPACITY);
                    Ok(ProviderRuntimeRecord {
                        descriptor,
                        history: started.history,
                        cancellation: started.cancellation,
                        lifecycle: started.lifecycle,
                        realtime: None,
                        display_depth: None,
                        catalog: ProviderCatalogChannels {
                            enabled: false,
                            channels: Box::new(ProviderCatalogChannelImpl::new(
                                catalog_controls,
                                catalog_events,
                                dispatch_catalog,
                            )),
                        },
                        workers: started.workers,
                    })
                }),
            )
        }

        pub(crate) fn dispatch_catalog(
            controls: &SyncSender<RithmicCatalogControl>,
            provider: &str,
            command: ProviderCatalogCommand,
        ) -> Result<(), String> {
            let control = match command {
                ProviderCatalogCommand::Search(value) => {
                    drop(value);
                    RithmicCatalogControl::Search(())
                }
                ProviderCatalogCommand::Select(value) => {
                    drop(value);
                    RithmicCatalogControl::Select(())
                }
                ProviderCatalogCommand::Screen(_) => {
                    return Err(format!("{provider} does not provide a market screen"));
                }
                ProviderCatalogCommand::Scan(_) => {
                    return Err(format!("{provider} does not provide a market scan"));
                }
            };
            try_send_catalog_control(controls, control, provider)
        }

        pub(crate) fn rithmic_wire(demand: &ProviderDemand) -> RithmicRealtimeDemand {
            let mut instruments = Vec::new();
            for requested in &demand.instruments {
                let has_stream = requested.streams.contains(MarketStream::Bars)
                    || requested.streams.contains(MarketStream::Trades)
                    || requested.streams.contains(MarketStream::Quotes)
                    || requested.streams.contains(MarketStream::Depth);
                if has_stream || requested.order_levels {
                    instruments.push(RithmicInstrumentDemand {
                        instrument: requested.instrument.clone(),
                        streams: requested.streams,
                        order_levels: requested.order_levels,
                    });
                }
            }
            RithmicRealtimeDemand { instruments }
        }

        #[derive(Default)]
        pub(crate) struct RithmicBinding {
            pub(crate) sink: Option<ProviderEventSink>,
            pub(crate) wire_generation: Option<u64>,
            pub(crate) pending: Option<RithmicRealtimeEvent>,
        }

        impl RithmicBinding {
            pub(crate) fn take(
                &mut self,
                events: &Receiver<RithmicRealtimeEvent>,
            ) -> Option<ProviderEvent> {
                let sink = self.sink?;
                let event = match self.pending.take() {
                    Some(event) => event,
                    None => events.try_recv().ok()?,
                };
                let wire_generation = match &event {
                    RithmicRealtimeEvent::Connecting(g)
                    | RithmicRealtimeEvent::Connected(g)
                    | RithmicRealtimeEvent::Heartbeat(g, _)
                    | RithmicRealtimeEvent::Disconnected(g, _)
                    | RithmicRealtimeEvent::Trade(g, _)
                    | RithmicRealtimeEvent::OrderLevels(g, _) => *g,
                };
                match self.wire_generation {
                    Some(current) if wire_generation < current => None,
                    Some(current) if wire_generation > current => {
                        self.pending = Some(event);
                        Some(ProviderEvent::bound(
                            RITHMIC_DESCRIPTOR.id,
                            sink.epoch,
                            wire_generation,
                            ProviderEventKind::TransportReplaced,
                        ))
                    }
                    _ => {
                        self.wire_generation = Some(wire_generation);
                        let wire = super::super::rithmic_realtime::into_wire_provider_event(event);
                        Some(ProviderEvent::bound(
                            wire.provider,
                            sink.epoch,
                            wire_generation,
                            wire.kind,
                        ))
                    }
                }
            }
        }

        pub(crate) struct RithmicSession {
            pub(crate) controls: SyncSender<RithmicRealtimeControl>,
            pub(crate) history: SyncSender<HistoryRequest>,
            pub(crate) binding: Arc<Mutex<RithmicBinding>>,
        }

        impl ProviderSession for RithmicSession {
            fn adapter_retries_transport(&self) -> bool {
                true
            }

            fn adapter_labels_sessions(&self) -> bool {
                true
            }

            fn connect(
                &self,
                _epoch: Epoch<ProviderTag>,
                sink: ProviderEventSink,
            ) -> Result<bool, ProviderSessionError> {
                let mut binding = self
                    .binding
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                binding.sink = Some(sink);
                binding.wire_generation = None;
                Ok(true)
            }

            fn rebind(
                &self,
                _epoch: Epoch<ProviderTag>,
                sink: ProviderEventSink,
            ) -> Result<bool, ProviderSessionError> {
                let mut binding = self
                    .binding
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                binding.sink = Some(sink);
                binding.wire_generation = None;
                Ok(true)
            }

            fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError> {
                let wire = rithmic_wire(demand);
                try_control(&self.controls, RithmicRealtimeControl::Subscribe(wire))
            }

            fn request_history(
                &self,
                request: HistoryRequest,
            ) -> Result<bool, ProviderSessionError> {
                try_control(&self.history, request)
            }

            fn release_idle(&self) -> Result<bool, ProviderSessionError> {
                try_control(&self.controls, RithmicRealtimeControl::Stop)
            }

            fn shutdown(&self) {
                let _ = self.controls.try_send(RithmicRealtimeControl::Stop);
            }
        }

        pub(crate) fn realtime_channels(
            controls: SyncSender<RithmicRealtimeControl>,
            events: Receiver<RithmicRealtimeEvent>,
            binding: Arc<Mutex<RithmicBinding>>,
        ) -> ProviderRealtimeChannels {
            let factory_binding = Arc::clone(&binding);
            ProviderRealtimeChannels::new(
                move |history| {
                    Box::new(RithmicSession {
                        controls: controls.clone(),
                        history: history.clone(),
                        binding: Arc::clone(&factory_binding),
                    })
                },
                move || {
                    binding
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take(&events)
                },
            )
        }
    }
}
