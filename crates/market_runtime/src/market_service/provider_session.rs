//! Coordinator-owned provider connections and epoch reservation.
//!
//! A provider session translates commands to its bounded worker channels.
//! The registry, not the adapter worker, decides when another connection may
//! be attempted. Adapter-owned transport replacement uses `rebind` when migrated.

use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        mpsc::{Receiver, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};

use aeris_market_engine::fence::{Epoch, EpochSequence, ProviderTag};

use super::{
    HistoryRequest, HyperliquidDisplayDepthControl, HyperliquidRealtimeControl, ProviderDemand,
    clock::Clock,
    reconnect::{Backoff, ReconnectPolicy, RetryClass, RetryDecision},
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ProviderSessionError {
    Unavailable,
    InvalidDemand,
}

pub(super) trait ProviderSession: Send {
    #[cfg(feature = "probe")]
    fn probe_fail_websocket(
        &self,
        _subsequent_connect_failures: u8,
    ) -> Result<bool, ProviderSessionError> {
        Err(ProviderSessionError::Unavailable)
    }
    fn adapter_retries_transport(&self) -> bool {
        false
    }
    fn connect(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError>;
    /// Adopt an adapter-owned replacement without opening another transport.
    fn rebind(
        &self,
        _epoch: Epoch<ProviderTag>,
        _sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        Err(ProviderSessionError::Unavailable)
    }
    fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError>;
    fn request_history(&self, request: HistoryRequest) -> Result<bool, ProviderSessionError>;
    fn release_idle(&self) -> Result<bool, ProviderSessionError>;
    fn shutdown(&self);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ConnectionState {
    Idle,
    Connecting,
    Online,
    RetryAt(Instant),
    Failed,
}

pub(super) struct ProviderConnection {
    pub(super) state: ConnectionState,
    pub(super) epoch: Option<Epoch<ProviderTag>>,
    pub(super) backoff: Backoff,
    pub(super) session: Box<dyn ProviderSession>,
    last_demand: Option<ProviderDemand>,
    online_since: Option<Instant>,
    failure_reason: Option<&'static str>,
    failure_published: bool,
    #[cfg(feature = "probe")]
    successful_connections: u64,
}

const WORKER_DISCONNECTED: &str = "provider control worker disconnected";

impl ProviderConnection {
    fn failed(&mut self, reason: &'static str) {
        self.state = ConnectionState::Failed;
        self.failure_reason = Some(reason);
        self.failure_published = false;
    }

    fn control_full(&mut self, now: Instant) {
        // Full jitter can return zero. An elapsed deadline would make the
        // coordinator immediately re-enter the same retry turn.
        let delay = self.backoff.transient_delay();
        self.state = ConnectionState::RetryAt(now + delay.max(Duration::from_millis(1)));
    }
}

pub(super) struct ProviderRegistry {
    sequence: EpochSequence<ProviderTag>,
    connections: BTreeMap<&'static str, ProviderConnection>,
    controls: BTreeMap<&'static str, super::ProviderControlState>,
    #[cfg(feature = "probe")]
    retry_records: std::collections::VecDeque<RetryRecord>,
}

#[cfg(feature = "probe")]
struct RetryRecord {
    provider: String,
    failed_at: Instant,
    attempt: u32,
    delay: std::time::Duration,
    resumed_at: Option<Instant>,
}

#[derive(Clone, Copy)]
pub(crate) struct ProviderEventSink {
    pub(crate) epoch: Epoch<ProviderTag>,
}

impl ProviderEventSink {
    pub(crate) fn stamp<T>(self, value: T) -> aeris_market_engine::fence::Fenced<ProviderTag, T> {
        aeris_market_engine::fence::Fenced {
            epoch: self.epoch,
            value,
        }
    }
}

pub(super) struct HyperliquidSession {
    pub(super) controls: SyncSender<HyperliquidRealtimeControl>,
    pub(super) display_controls: SyncSender<HyperliquidDisplayDepthControl>,
    pub(super) history: SyncSender<HistoryRequest>,
}

fn try_control<T>(sender: &SyncSender<T>, control: T) -> Result<bool, ProviderSessionError> {
    match sender.try_send(control) {
        Ok(()) => Ok(true),
        Err(TrySendError::Full(_)) => Ok(false),
        Err(TrySendError::Disconnected(_)) => Err(ProviderSessionError::Unavailable),
    }
}

impl ProviderSession for HyperliquidSession {
    #[cfg(feature = "probe")]
    fn probe_fail_websocket(
        &self,
        subsequent_connect_failures: u8,
    ) -> Result<bool, ProviderSessionError> {
        try_control(
            &self.controls,
            HyperliquidRealtimeControl::FailWebSocket {
                subsequent_connect_failures,
            },
        )
    }
    fn connect(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        debug_assert_eq!(sink.epoch, epoch);
        try_control(&self.controls, HyperliquidRealtimeControl::Connect(sink))
    }

    fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError> {
        try_control(
            &self.controls,
            HyperliquidRealtimeControl::Subscribe(demand.hyperliquid_wire()),
        )
    }

    fn request_history(&self, request: HistoryRequest) -> Result<bool, ProviderSessionError> {
        try_control(&self.history, request)
    }

    fn release_idle(&self) -> Result<bool, ProviderSessionError> {
        let main = try_control(&self.controls, HyperliquidRealtimeControl::Stop)?;
        let display = try_control(&self.display_controls, HyperliquidDisplayDepthControl::Stop)?;
        Ok(main && display)
    }

    fn shutdown(&self) {
        let _ = self.release_idle();
    }
}

pub(super) struct RithmicSession {
    pub(super) controls: SyncSender<super::RithmicRealtimeControl>,
    pub(super) history: SyncSender<HistoryRequest>,
    pub(super) binding: std::sync::Arc<Mutex<RithmicBinding>>,
}

#[derive(Default)]
pub(super) struct RithmicBinding {
    pub(super) sink: Option<ProviderEventSink>,
    pub(super) wire_generation: Option<u64>,
    pub(super) pending: Option<crate::rithmic_realtime::RithmicRealtimeEvent>,
}

impl ProviderSession for RithmicSession {
    fn adapter_retries_transport(&self) -> bool {
        true
    }
    fn connect(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        debug_assert_eq!(epoch, sink.epoch);
        self.binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sink = Some(sink);
        Ok(true)
    }

    fn rebind(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        debug_assert_eq!(epoch, sink.epoch);
        let mut binding = self
            .binding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        binding.sink = Some(sink);
        binding.wire_generation = binding
            .pending
            .as_ref()
            .map(crate::rithmic_realtime::RithmicRealtimeEvent::generation);
        Ok(true)
    }

    fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError> {
        let wire = demand
            .rithmic_wire()
            .map_err(|_| ProviderSessionError::InvalidDemand)?;
        try_control(
            &self.controls,
            super::RithmicRealtimeControl::Subscribe(wire),
        )
    }

    fn request_history(&self, request: HistoryRequest) -> Result<bool, ProviderSessionError> {
        try_control(&self.history, request)
    }

    fn release_idle(&self) -> Result<bool, ProviderSessionError> {
        try_control(&self.controls, super::RithmicRealtimeControl::Stop)
    }

    fn shutdown(&self) {
        let _ = self.release_idle();
    }
}

impl RithmicBinding {
    pub(super) fn take(
        &mut self,
        events: &Receiver<crate::rithmic_realtime::RithmicRealtimeEvent>,
    ) -> Option<super::ProviderEvent> {
        let sink = self.sink?;
        let event = match self.pending.take() {
            Some(event) => event,
            None => events.try_recv().ok()?,
        };
        let wire_generation = event.generation();
        match self.wire_generation {
            Some(current) if wire_generation < current => None,
            Some(current) if wire_generation > current => {
                self.pending = Some(event);
                Some(super::ProviderEvent {
                    provider: super::RITHMIC_DESCRIPTOR.id,
                    generation: sink.epoch,
                    wire_generation,
                    kind: super::ProviderEventKind::TransportReplaced,
                })
            }
            _ => {
                self.wire_generation = Some(wire_generation);
                let wire = super::ProviderEvent::rithmic(event);
                Some(super::ProviderEvent {
                    provider: wire.provider,
                    generation: sink.epoch,
                    wire_generation,
                    kind: wire.kind,
                })
            }
        }
    }
}

impl ProviderRegistry {
    #[cfg(feature = "probe")]
    pub(super) fn probe_fail_websocket(
        &self,
        id: &str,
        count: u8,
    ) -> Result<bool, ProviderSessionError> {
        self.connections
            .get(id)
            .ok_or(ProviderSessionError::Unavailable)?
            .session
            .probe_fail_websocket(count)
    }
    pub(super) fn catalog_ready(&self, id: &str) -> bool {
        self.connections.get(id).is_some_and(|connection| {
            connection.epoch.is_some()
                && matches!(
                    connection.state,
                    ConnectionState::Connecting | ConnectionState::Online
                )
        })
    }
    pub(super) fn is_failed(&self, id: &str) -> bool {
        self.connections
            .get(id)
            .is_some_and(|connection| matches!(connection.state, ConnectionState::Failed))
    }
    pub(super) fn new() -> Self {
        Self {
            sequence: EpochSequence::default(),
            connections: BTreeMap::new(),
            controls: BTreeMap::new(),
            #[cfg(feature = "probe")]
            retry_records: std::collections::VecDeque::new(),
        }
    }

    pub(super) fn register_control(&mut self, id: &'static str) {
        self.controls.entry(id).or_default();
    }

    pub(super) fn control(&self, id: &str) -> &super::ProviderControlState {
        self.controls
            .get(id)
            .expect("registered provider has control state")
    }

    pub(super) fn control_mut(&mut self, id: &str) -> &mut super::ProviderControlState {
        self.controls
            .get_mut(id)
            .expect("registered provider has control state")
    }

    pub(super) fn has_control(&self, id: &str) -> bool {
        self.controls.contains_key(id)
    }

    pub(super) fn controls(&self) -> impl Iterator<Item = &super::ProviderControlState> {
        self.controls.values()
    }

    pub(super) fn controls_mut(
        &mut self,
    ) -> impl Iterator<Item = &mut super::ProviderControlState> {
        self.controls.values_mut()
    }

    pub(super) fn register(
        &mut self,
        id: &'static str,
        session: Box<dyn ProviderSession>,
        policy: ReconnectPolicy,
        clock: &dyn Clock,
    ) {
        self.connections.insert(
            id,
            ProviderConnection {
                state: ConnectionState::Idle,
                epoch: None,
                backoff: Backoff::new(policy, clock, 0x5052_4f56),
                session,
                last_demand: None,
                online_since: None,
                failure_reason: None,
                failure_published: false,
                #[cfg(feature = "probe")]
                successful_connections: 0,
            },
        );
    }

    /// The only production call to `EpochSequence<ProviderTag>::next`.
    pub(super) fn reserve(&mut self) -> Epoch<ProviderTag> {
        self.sequence.next()
    }

    pub(super) fn connect(
        &mut self,
        id: &str,
        now: Instant,
    ) -> Result<Option<Epoch<ProviderTag>>, ProviderSessionError> {
        let Some(connection) = self.connections.get(id) else {
            return Err(ProviderSessionError::Unavailable);
        };
        if !matches!(connection.state, ConnectionState::Idle)
            && !matches!(connection.state, ConnectionState::RetryAt(at) if at <= now)
        {
            return Ok(None);
        }
        let epoch = self.reserve();
        let connection = self
            .connections
            .get_mut(id)
            .ok_or(ProviderSessionError::Unavailable)?;
        match connection
            .session
            .connect(epoch, ProviderEventSink { epoch })
        {
            Ok(true) => {
                connection.epoch = Some(epoch);
                connection.state = ConnectionState::Connecting;
                connection.failure_reason = None;
                connection.last_demand = None;
                connection.online_since = None;
                #[cfg(feature = "probe")]
                if let Some(record) = self
                    .retry_records
                    .iter_mut()
                    .rev()
                    .find(|record| record.provider == id && record.resumed_at.is_none())
                {
                    record.resumed_at = Some(now);
                }
                Ok(Some(epoch))
            }
            Ok(false) => {
                // The reserved epoch never left this registry, so the
                // preceding epoch stays current until the retry connects.
                connection.control_full(now);
                Ok(None)
            }
            Err(error) => {
                connection.failed(WORKER_DISCONNECTED);
                if connection.epoch.is_none() {
                    // No worker received this epoch. Use it solely to publish
                    // the terminal failure; it cannot authorize worker events.
                    connection.epoch = Some(epoch);
                }
                Err(error)
            }
        }
    }

    pub(super) fn connected(&mut self, id: &str, epoch: Epoch<ProviderTag>, now: Instant) {
        if let Some(connection) = self.connections.get_mut(id)
            && connection.epoch == Some(epoch)
            && !matches!(connection.state, ConnectionState::Failed)
        {
            #[cfg(feature = "probe")]
            if !matches!(connection.state, ConnectionState::Online) {
                connection.successful_connections =
                    connection.successful_connections.saturating_add(1);
            }
            connection.state = ConnectionState::Online;
            connection.online_since = Some(now);
        }
    }

    #[cfg(feature = "probe")]
    pub(super) fn successful_connections(&self, id: &str) -> u64 {
        self.connections
            .get(id)
            .map_or(0, |connection| connection.successful_connections)
    }

    pub(super) fn rebind(
        &mut self,
        id: &str,
    ) -> Result<Option<Epoch<ProviderTag>>, ProviderSessionError> {
        if !self.connections.contains_key(id) {
            return Err(ProviderSessionError::Unavailable);
        }
        let epoch = self.reserve();
        let connection = self
            .connections
            .get_mut(id)
            .ok_or(ProviderSessionError::Unavailable)?;
        if matches!(connection.state, ConnectionState::Failed) {
            return Err(ProviderSessionError::Unavailable);
        }
        match connection
            .session
            .rebind(epoch, ProviderEventSink { epoch })
        {
            Ok(true) => {
                connection.epoch = Some(epoch);
                connection.state = ConnectionState::Connecting;
                connection.last_demand = None;
                Ok(Some(epoch))
            }
            Ok(false) => {
                connection.failed("provider transport rebind was not accepted");
                Ok(None)
            }
            Err(error) => {
                connection.failed(WORKER_DISCONNECTED);
                Err(error)
            }
        }
    }

    pub(super) fn disconnected(&mut self, id: &str, epoch: Epoch<ProviderTag>, now: Instant) {
        self.disconnected_with_class(id, epoch, now, RetryClass::Transient);
    }

    pub(super) fn disconnected_with_class(
        &mut self,
        id: &str,
        epoch: Epoch<ProviderTag>,
        now: Instant,
        class: RetryClass,
    ) {
        if let Some(connection) = self.connections.get_mut(id)
            && connection.epoch == Some(epoch)
            && !matches!(connection.state, ConnectionState::Failed)
        {
            if connection.online_since.take().is_some_and(|since| {
                now.saturating_duration_since(since)
                    >= connection.backoff.policy().healthy_reset_after()
            }) {
                connection.backoff.reset();
            }
            connection.state = match connection.backoff.failure(class) {
                RetryDecision::Wait(delay) => {
                    ConnectionState::RetryAt(now + delay.max(Duration::from_millis(1)))
                }
                RetryDecision::Fatal => ConnectionState::Failed,
            };
            #[cfg(feature = "probe")]
            if let ConnectionState::RetryAt(at) = connection.state {
                if self.retry_records.len() == 16 {
                    self.retry_records.pop_front();
                }
                self.retry_records.push_back(RetryRecord {
                    provider: id.to_owned(),
                    failed_at: now,
                    attempt: connection.backoff.attempts(),
                    delay: at.saturating_duration_since(now),
                    resumed_at: None,
                });
            }
        }
    }

    #[cfg(feature = "probe")]
    pub(super) fn probe_retries(&self, now: Instant) -> Vec<super::ProbeRetry> {
        self.retry_records
            .iter()
            .filter(|record| record.provider == "hyperliquid")
            .map(|record| super::ProbeRetry {
                attempt: record.attempt,
                delay_ms: record.delay.as_millis().try_into().unwrap_or(u64::MAX),
                failed_age_ms: now
                    .saturating_duration_since(record.failed_at)
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
                resumed_age_ms: record.resumed_at.map(|at| {
                    now.saturating_duration_since(at)
                        .as_millis()
                        .try_into()
                        .unwrap_or(u64::MAX)
                }),
            })
            .collect()
    }

    pub(super) fn next_retry(&self) -> Option<Instant> {
        self.connections
            .values()
            .filter_map(|connection| match connection.state {
                ConnectionState::RetryAt(at) => Some(at),
                _ => None,
            })
            .min()
    }

    pub(super) fn retry_due(&mut self, now: Instant) -> Vec<(&'static str, Epoch<ProviderTag>)> {
        let due = self.due_ids(now);
        due.into_iter()
            .filter_map(|id| {
                self.connect(id, now)
                    .ok()
                    .flatten()
                    .map(|epoch| (id, epoch))
            })
            .collect()
    }

    pub(super) fn take_failures(
        &mut self,
    ) -> Vec<(&'static str, Epoch<ProviderTag>, &'static str)> {
        self.connections
            .iter_mut()
            .filter_map(|(id, connection)| {
                if connection.failure_published {
                    return None;
                }
                let (Some(epoch), Some(reason)) = (connection.epoch, connection.failure_reason)
                else {
                    return None;
                };
                connection.failure_published = true;
                Some((*id, epoch, reason))
            })
            .collect()
    }

    pub(super) fn due_ids(&self, now: Instant) -> Vec<&'static str> {
        self.connections
            .iter()
            .filter_map(|(id, connection)| {
                matches!(connection.state, ConnectionState::RetryAt(at) if at <= now).then_some(*id)
            })
            .collect()
    }

    pub(super) fn apply_demand(
        &mut self,
        id: &str,
        demand: &ProviderDemand,
    ) -> Result<bool, ProviderSessionError> {
        let connection = self
            .connections
            .get_mut(id)
            .ok_or(ProviderSessionError::Unavailable)?;
        if matches!(connection.state, ConnectionState::Failed) {
            return Err(ProviderSessionError::Unavailable);
        }
        if connection.last_demand.as_ref() == Some(demand) {
            return Ok(true);
        }
        match connection.session.apply_demand(demand) {
            Ok(true) => {
                connection.last_demand = Some(demand.clone());
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(error) => {
                connection.failed(match error {
                    ProviderSessionError::Unavailable => WORKER_DISCONNECTED,
                    ProviderSessionError::InvalidDemand => "provider demand cannot be encoded",
                });
                Err(error)
            }
        }
    }

    pub(super) fn request_history(
        &mut self,
        id: &str,
        request: HistoryRequest,
    ) -> Result<bool, ProviderSessionError> {
        let connection = self
            .connections
            .get_mut(id)
            .ok_or(ProviderSessionError::Unavailable)?;
        if matches!(connection.state, ConnectionState::Failed) {
            return Err(ProviderSessionError::Unavailable);
        }
        match connection.session.request_history(request) {
            Err(error) => {
                connection.failed(WORKER_DISCONNECTED);
                Err(error)
            }
            result => result,
        }
    }

    pub(super) fn release_idle(&mut self, id: &str) -> Result<bool, ProviderSessionError> {
        let connection = self
            .connections
            .get_mut(id)
            .ok_or(ProviderSessionError::Unavailable)?;
        if matches!(connection.state, ConnectionState::Failed) {
            return Err(ProviderSessionError::Unavailable);
        }
        if matches!(connection.state, ConnectionState::Idle) {
            return Ok(true);
        }
        let released = match connection.session.release_idle() {
            Ok(released) => released,
            Err(error) => {
                connection.failed(WORKER_DISCONNECTED);
                return Err(error);
            }
        };
        if released {
            connection.state = ConnectionState::Idle;
            connection.epoch = None;
            connection.backoff.reset();
            connection.last_demand = None;
            connection.online_since = None;
            connection.failure_reason = None;
            connection.failure_published = false;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(super) fn epoch(&self, id: &str) -> Option<Epoch<ProviderTag>> {
        self.connections
            .get(id)
            .and_then(|connection| connection.epoch)
    }

    pub(super) fn contains(&self, id: &str) -> bool {
        self.connections.contains_key(id)
    }

    pub(super) fn adapter_retries_transport(&self, id: &str) -> bool {
        self.connections
            .get(id)
            .is_some_and(|connection| connection.session.adapter_retries_transport())
    }
}

impl Drop for ProviderRegistry {
    fn drop(&mut self) {
        for connection in self.connections.values() {
            connection.session.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::market_service::{
        BarPeriod, BarSeriesKey, InstallProviderInstrument, MarketStream, ProviderInstrumentDemand,
        StreamRequirements, clock::ScriptedClock,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct ScriptedSession(Arc<Mutex<(usize, Vec<ProviderDemand>, usize)>>);

    impl ProviderSession for ScriptedSession {
        fn connect(
            &self,
            _: Epoch<ProviderTag>,
            _: ProviderEventSink,
        ) -> Result<bool, ProviderSessionError> {
            self.0.lock().unwrap().0 += 1;
            Ok(true)
        }

        fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError> {
            self.0.lock().unwrap().1.push(demand.clone());
            Ok(true)
        }

        fn request_history(&self, _: HistoryRequest) -> Result<bool, ProviderSessionError> {
            Ok(true)
        }

        fn release_idle(&self) -> Result<bool, ProviderSessionError> {
            self.0.lock().unwrap().2 += 1;
            Ok(true)
        }

        fn shutdown(&self) {}
    }

    fn scripted_instrument(provider: &str, symbol: &str) -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: provider.into(),
            instrument_id: symbol.into(),
            provider_symbol: symbol.into(),
            ..Default::default()
        }
    }

    fn scripted_provider_b_series(symbol: &str, seconds: u32) -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "provider_b".into(),
            instrument_id: symbol.into(),
            entitlement_id: String::new(),
            period: BarPeriod::time(seconds).unwrap(),
            definition_version: 1,
        }
    }

    fn provider_b_demand_changes() -> (
        ProviderInstrumentDemand,
        ProviderDemand,
        Vec<(&'static str, ProviderDemand)>,
    ) {
        let book = ProviderInstrumentDemand {
            instrument: scripted_instrument("provider_b", "book"),
            streams: StreamRequirements::NONE.with(MarketStream::Depth),
            display_depth: true,
        };
        let mut demand = ProviderDemand {
            instruments: vec![book.clone()],
            candle_series: vec![(
                scripted_provider_b_series("chart", 60),
                StreamRequirements::BARS,
                scripted_instrument("provider_b", "chart"),
            )],
            ..Default::default()
        };
        let initial = demand.clone();
        let mut changes = Vec::new();
        // Interval changes the live candle subscription's cadence.
        demand.candle_series[0].0 = scripted_provider_b_series("chart", 300);
        changes.push(("interval", demand.clone()));
        // A viewport-driven visible quote interest adds another instrument.
        demand.instruments.push(ProviderInstrumentDemand {
            instrument: scripted_instrument("provider_b", "visible-quote"),
            streams: StreamRequirements::NONE.with(MarketStream::Quotes),
            display_depth: false,
        });
        changes.push(("viewport", demand.clone()));
        // Switching tabs changes the selected live chart.
        demand.candle_series[0] = (
            scripted_provider_b_series("other-chart", 300),
            StreamRequirements::BARS,
            scripted_instrument("provider_b", "other-chart"),
        );
        changes.push(("tab", demand.clone()));
        // Adding a layout pane adds a second live series.
        demand.candle_series.push((
            scripted_provider_b_series("second-pane", 60),
            StreamRequirements::BARS,
            scripted_instrument("provider_b", "second-pane"),
        ));
        changes.push(("layout", demand));
        (book, initial, changes)
    }

    #[test]
    fn scripted_registry_changes_demand_without_connecting_again() {
        let clock = ScriptedClock::new(12);
        let calls = Arc::new(Mutex::new((0, Vec::new(), 0)));
        let mut registry = ProviderRegistry::new();
        registry.register(
            "hyperliquid",
            Box::new(ScriptedSession(Arc::clone(&calls))),
            ReconnectPolicy::hyperliquid(),
            &clock,
        );
        let first = registry
            .connect("hyperliquid", clock.monotonic())
            .unwrap()
            .unwrap();
        registry.connected("hyperliquid", first, clock.monotonic());
        for revision in 0..5 {
            let mut demand = ProviderDemand::default();
            demand
                .explicit_trade_ids
                .insert(format!("symbol-{revision}"));
            registry.apply_demand("hyperliquid", &demand).unwrap();
            registry.apply_demand("hyperliquid", &demand).unwrap();
            assert_eq!(
                registry.connect("hyperliquid", clock.monotonic()).unwrap(),
                None
            );
        }
        assert_eq!(calls.lock().unwrap().0, 1);
        assert_eq!(calls.lock().unwrap().1.len(), 5);
    }

    #[test]
    fn scripted_registry_keeps_each_provider_connected_across_five_demand_changes() {
        let clock = ScriptedClock::new(48);
        let mut registry = ProviderRegistry::new();
        let providers = [
            ("hyperliquid", ReconnectPolicy::hyperliquid()),
            ("rithmic", ReconnectPolicy::rithmic_recovery()),
        ];
        let records = providers
            .into_iter()
            .map(|(id, policy)| {
                let calls = Arc::new(Mutex::new((0, Vec::new(), 0)));
                registry.register(
                    id,
                    Box::new(ScriptedSession(Arc::clone(&calls))),
                    policy,
                    &clock,
                );
                let epoch = registry.connect(id, clock.monotonic()).unwrap().unwrap();
                registry.connected(id, epoch, clock.monotonic());
                (id, epoch, calls)
            })
            .collect::<Vec<_>>();
        for change in ["symbol", "interval", "viewport", "tab", "layout"] {
            for (id, epoch, _) in &records {
                let mut demand = ProviderDemand::default();
                demand.explicit_trade_ids.insert(change.to_string());
                assert!(registry.apply_demand(id, &demand).unwrap());
                assert_eq!(registry.connect(id, clock.monotonic()).unwrap(), None);
                assert_eq!(registry.epoch(id), Some(*epoch));
            }
        }
        for (id, _, calls) in records {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.0, 1, "{id} connected more than once");
            assert_eq!(calls.1.len(), 5, "{id} missed a demand change");
        }
    }

    #[test]
    fn scripted_registry_releases_only_idle_provider_and_diffs_concrete_demands() {
        let clock = ScriptedClock::new(54);
        let a_calls = Arc::new(Mutex::new((0, Vec::new(), 0)));
        let b_calls = Arc::new(Mutex::new((0, Vec::new(), 0)));
        let mut registry = ProviderRegistry::new();
        for (id, calls) in [("provider_a", &a_calls), ("provider_b", &b_calls)] {
            registry.register(
                id,
                Box::new(ScriptedSession(Arc::clone(calls))),
                ReconnectPolicy::hyperliquid(),
                &clock,
            );
            let epoch = registry.connect(id, clock.monotonic()).unwrap().unwrap();
            registry.connected(id, epoch, clock.monotonic());
        }
        let b_epoch = registry.epoch("provider_b").unwrap();
        let a_demand = ProviderDemand {
            instruments: vec![ProviderInstrumentDemand {
                instrument: scripted_instrument("provider_a", "quote"),
                streams: StreamRequirements::NONE.with(MarketStream::Quotes),
                display_depth: false,
            }],
            ..Default::default()
        };
        let (book, b_demand, changes) = provider_b_demand_changes();
        assert!(registry.apply_demand("provider_a", &a_demand).unwrap());
        assert!(registry.apply_demand("provider_b", &b_demand).unwrap());
        assert!(
            registry
                .apply_demand("provider_a", &ProviderDemand::default())
                .unwrap()
        );
        assert!(registry.release_idle("provider_a").unwrap());
        assert_eq!(registry.epoch("provider_a"), None);
        assert_eq!(
            registry.connections["provider_a"].state,
            ConnectionState::Idle
        );
        assert_eq!(registry.epoch("provider_b"), Some(b_epoch));
        assert_eq!(
            registry.connections["provider_b"].state,
            ConnectionState::Online
        );
        assert_eq!(a_calls.lock().unwrap().2, 1);
        assert_eq!(b_calls.lock().unwrap().2, 0);

        for (index, (change, demand)) in changes.into_iter().enumerate() {
            assert!(
                demand.instruments.contains(&book),
                "{change} lost order book"
            );
            assert!(registry.apply_demand("provider_b", &demand).unwrap());
            assert!(registry.apply_demand("provider_b", &demand).unwrap());
            assert_eq!(
                registry.connect("provider_b", clock.monotonic()).unwrap(),
                None
            );
            assert_eq!(registry.epoch("provider_b"), Some(b_epoch));
            let calls = b_calls.lock().unwrap();
            assert_eq!(calls.0, 1, "{change} reconnected provider B");
            assert_eq!(calls.1.len(), index + 2, "{change} missed a demand diff");
            assert_eq!(calls.1.last(), Some(&demand), "{change} payload was lost");
            assert_eq!(calls.2, 0, "{change} released active provider B");
        }
        let calls = a_calls.lock().unwrap();
        assert_eq!(calls.0, 1);
        assert_eq!(calls.1, vec![a_demand, ProviderDemand::default()]);
        assert_eq!(calls.2, 1);
    }

    #[test]
    fn scripted_registry_reserves_next_epoch_only_when_retry_is_due() {
        let clock = ScriptedClock::new(34);
        let calls = Arc::new(Mutex::new((0, Vec::new(), 0)));
        let mut registry = ProviderRegistry::new();
        registry.register(
            "hyperliquid",
            Box::new(ScriptedSession(Arc::clone(&calls))),
            ReconnectPolicy::hyperliquid(),
            &clock,
        );
        let first = registry
            .connect("hyperliquid", clock.monotonic())
            .unwrap()
            .unwrap();
        registry.connected("hyperliquid", first, clock.monotonic());
        registry.disconnected("hyperliquid", first, clock.monotonic());
        let at = registry
            .next_retry()
            .expect("transient disconnect schedules retry");
        assert_eq!(registry.retry_due(clock.monotonic()).len(), 0);
        let retry = registry.retry_due(at);
        assert_eq!(retry.len(), 1);
        let second = retry[0].1;
        assert_ne!(second, first);
        assert_eq!(registry.epoch("hyperliquid"), Some(second));
        assert_eq!(calls.lock().unwrap().0, 2);
    }

    struct HyperliquidChannels {
        session: Box<HyperliquidSession>,
        controls: Receiver<HyperliquidRealtimeControl>,
        _display: Receiver<HyperliquidDisplayDepthControl>,
        _history: Receiver<HistoryRequest>,
    }

    fn hyperliquid_channels(capacity: usize) -> HyperliquidChannels {
        let (controls, queued) = std::sync::mpsc::sync_channel(capacity);
        let (display_controls, display) = std::sync::mpsc::sync_channel(4);
        let (history, history_rx) = std::sync::mpsc::sync_channel(1);
        HyperliquidChannels {
            session: Box::new(HyperliquidSession {
                controls,
                display_controls,
                history,
            }),
            controls: queued,
            _display: display,
            _history: history_rx,
        }
    }

    #[test]
    fn full_retry_control_channel_schedules_future_bounded_attempt_without_spinning() {
        let clock = ScriptedClock::new(49);
        let channels = hyperliquid_channels(1);
        let mut registry = ProviderRegistry::new();
        registry.register(
            "hyperliquid",
            channels.session,
            ReconnectPolicy::rithmic_recovery(),
            &clock,
        );
        let first = registry
            .connect("hyperliquid", clock.monotonic())
            .unwrap()
            .unwrap();
        registry.disconnected("hyperliquid", first, clock.monotonic());
        let due = registry.next_retry().unwrap();
        clock.advance(due.duration_since(clock.monotonic()));
        // The original Connect remains queued, so the bounded control channel is full.
        assert_eq!(registry.retry_due(clock.monotonic()).len(), 0);
        let next = registry
            .next_retry()
            .expect("full channel must retain a retry");
        assert!(next > clock.monotonic(), "no elapsed deadline may remain");
        assert!(next.duration_since(clock.monotonic()) <= Duration::from_secs(6));
        assert_eq!(registry.epoch("hyperliquid"), Some(first));
        assert_eq!(registry.connections["hyperliquid"].backoff.attempts(), 2);

        let mut turns = 0;
        let interval_end = clock.monotonic() + Duration::from_secs(30);
        while clock.monotonic() < interval_end {
            turns += 1;
            assert!(turns <= 20, "coordinator must not spin on an elapsed retry");
            let at = registry
                .next_retry()
                .expect("a full channel keeps a scheduled retry");
            assert!(at > clock.monotonic());
            clock.advance(at.min(interval_end).duration_since(clock.monotonic()));
            if at <= clock.monotonic() {
                registry.retry_due(clock.monotonic());
            }
        }
        assert!(matches!(
            registry.connections["hyperliquid"].state,
            ConnectionState::RetryAt(_)
        ));
        assert_eq!(registry.epoch("hyperliquid"), Some(first));
        drop(channels.controls);
    }

    #[test]
    fn disconnected_retry_worker_fails_with_cause_and_no_deadline() {
        let clock = ScriptedClock::new(50);
        let channels = hyperliquid_channels(1);
        let mut registry = ProviderRegistry::new();
        registry.register(
            "hyperliquid",
            channels.session,
            ReconnectPolicy::hyperliquid(),
            &clock,
        );
        let first = registry
            .connect("hyperliquid", clock.monotonic())
            .unwrap()
            .unwrap();
        registry.disconnected("hyperliquid", first, clock.monotonic());
        drop(channels.controls);
        let due = registry.next_retry().unwrap();
        clock.advance(due.duration_since(clock.monotonic()));
        assert_eq!(registry.retry_due(clock.monotonic()).len(), 0);
        assert_eq!(
            registry.connections["hyperliquid"].state,
            ConnectionState::Failed
        );
        assert_eq!(
            registry.connections["hyperliquid"].failure_reason,
            Some(WORKER_DISCONNECTED)
        );
        assert_eq!(
            registry.take_failures(),
            vec![("hyperliquid", first, WORKER_DISCONNECTED)]
        );
        assert!(
            registry.take_failures().is_empty(),
            "failure publishes once"
        );
        assert_eq!(registry.next_retry(), None);
        assert_eq!(registry.epoch("hyperliquid"), Some(first));
        assert_eq!(registry.retry_due(clock.monotonic()).len(), 0);
    }

    #[test]
    fn full_demand_and_idle_controls_retain_work_until_channel_drains() {
        let clock = ScriptedClock::new(52);
        let channels = hyperliquid_channels(1);
        let mut registry = ProviderRegistry::new();
        registry.register(
            "hyperliquid",
            channels.session,
            ReconnectPolicy::hyperliquid(),
            &clock,
        );
        let epoch = registry
            .connect("hyperliquid", clock.monotonic())
            .unwrap()
            .unwrap();
        let mut demand = ProviderDemand::default();
        demand.explicit_trade_ids.insert("BTC".into());
        assert!(!registry.apply_demand("hyperliquid", &demand).unwrap());
        assert!(!registry.release_idle("hyperliquid").unwrap());
        assert_eq!(registry.epoch("hyperliquid"), Some(epoch));
        channels.controls.try_recv().unwrap();
        assert!(registry.apply_demand("hyperliquid", &demand).unwrap());
        assert_eq!(
            registry.connections["hyperliquid"].last_demand.as_ref(),
            Some(&demand)
        );
        channels.controls.try_recv().unwrap();
        assert!(registry.release_idle("hyperliquid").unwrap());
        assert_eq!(registry.epoch("hyperliquid"), None);
    }

    #[test]
    fn disconnected_demand_control_is_terminal_not_silently_retried() {
        let clock = ScriptedClock::new(53);
        let channels = hyperliquid_channels(1);
        let mut registry = ProviderRegistry::new();
        registry.register(
            "hyperliquid",
            channels.session,
            ReconnectPolicy::hyperliquid(),
            &clock,
        );
        let epoch = registry
            .connect("hyperliquid", clock.monotonic())
            .unwrap()
            .unwrap();
        drop(channels.controls);
        assert_eq!(
            registry.apply_demand("hyperliquid", &ProviderDemand::default()),
            Err(ProviderSessionError::Unavailable)
        );
        assert_eq!(
            registry.connections["hyperliquid"].state,
            ConnectionState::Failed
        );
        assert_eq!(
            registry.take_failures(),
            vec![("hyperliquid", epoch, WORKER_DISCONNECTED)]
        );
        assert_eq!(registry.next_retry(), None);
    }

    #[test]
    fn registry_owns_rate_limit_deadline_and_retires_old_epoch() {
        let clock = ScriptedClock::new(46);
        let channels = hyperliquid_channels(4);
        let mut registry = ProviderRegistry::new();
        registry.register(
            "hyperliquid",
            channels.session,
            ReconnectPolicy::hyperliquid(),
            &clock,
        );
        let first = registry
            .connect("hyperliquid", clock.monotonic())
            .unwrap()
            .unwrap();
        let HyperliquidRealtimeControl::Connect(first_sink) = channels.controls.try_recv().unwrap()
        else {
            panic!("first connect");
        };
        let now = clock.monotonic();
        registry.disconnected_with_class(
            "hyperliquid",
            first,
            now,
            RetryClass::RateLimited {
                retry_after: std::time::Duration::from_secs(17),
            },
        );
        assert_eq!(
            registry.next_retry(),
            Some(now + std::time::Duration::from_secs(17))
        );
        assert_eq!(
            registry
                .retry_due(now + std::time::Duration::from_secs(16))
                .len(),
            0
        );
        let second = registry.retry_due(now + std::time::Duration::from_secs(17))[0].1;
        assert_ne!(second, first);
        assert_eq!(first_sink.stamp(()).epoch, first);
        assert!(matches!(
            channels.controls.try_recv().unwrap(),
            HyperliquidRealtimeControl::Connect(sink) if sink.epoch == second
        ));
        registry.disconnected("hyperliquid", first, now);
        assert_eq!(registry.epoch("hyperliquid"), Some(second));
        assert_eq!(registry.next_retry(), None);
    }

    #[test]
    fn rithmic_rebind_retires_wire_epoch_without_connecting_again() {
        use crate::rithmic_realtime::RithmicRealtimeEvent as Wire;
        let clock = ScriptedClock::new(47);
        let (controls, queued) = std::sync::mpsc::sync_channel(8);
        let (history, _) = std::sync::mpsc::sync_channel(1);
        let (events_tx, events) = std::sync::mpsc::sync_channel(4);
        let binding = Arc::new(Mutex::new(RithmicBinding::default()));
        let mut registry = ProviderRegistry::new();
        registry.register(
            "rithmic",
            Box::new(RithmicSession {
                controls,
                history,
                binding: Arc::clone(&binding),
            }),
            ReconnectPolicy::rithmic_recovery(),
            &clock,
        );
        let first = registry
            .connect("rithmic", clock.monotonic())
            .unwrap()
            .unwrap();
        assert!(queued.try_recv().is_err(), "binding never opens a socket");
        events_tx.send(Wire::Connected(7)).unwrap();
        let initial = binding.lock().unwrap().take(&events).unwrap();
        assert_eq!(initial.generation, first);
        assert!(matches!(
            initial.kind,
            super::super::ProviderEventKind::Connected
        ));
        for revision in 0..5 {
            let mut demand = ProviderDemand::default();
            demand
                .explicit_trade_ids
                .insert(format!("selection-{revision}"));
            assert!(registry.apply_demand("rithmic", &demand).unwrap());
            assert!(matches!(
                queued.try_recv().unwrap(),
                super::super::RithmicRealtimeControl::Subscribe(_)
            ));
            assert!(
                registry
                    .connect("rithmic", clock.monotonic())
                    .unwrap()
                    .is_none()
            );
        }
        events_tx.send(Wire::Connecting(8)).unwrap();
        let replaced = binding.lock().unwrap().take(&events).unwrap();
        assert_eq!(replaced.generation, first);
        assert!(matches!(
            replaced.kind,
            super::super::ProviderEventKind::TransportReplaced
        ));
        let second = registry.rebind("rithmic").unwrap().unwrap();
        assert_ne!(first, second);
        assert!(queued.try_recv().is_err(), "rebind never opens a socket");
        let connecting = binding.lock().unwrap().take(&events).unwrap();
        assert_eq!(connecting.generation, second);
        assert_eq!(connecting.wire_generation, 8);
        assert!(matches!(
            connecting.kind,
            super::super::ProviderEventKind::Connecting
        ));
        events_tx.send(Wire::Heartbeat(7, None)).unwrap();
        assert!(
            binding.lock().unwrap().take(&events).is_none(),
            "old wire event is retired"
        );
    }
}
