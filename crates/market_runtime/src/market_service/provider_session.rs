//! Coordinator-owned provider connections and epoch reservation.
//!
//! A provider session translates commands to its bounded worker channels.
//! The registry, not the adapter worker, decides when another connection may
//! be attempted. Adapter-owned transport replacement uses `rebind` when migrated.

use std::{
    collections::BTreeMap,
    sync::mpsc::{SyncSender, TrySendError},
    time::Instant,
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
}

pub(super) trait ProviderSession: Send {
    fn catalog_uses_session(&self) -> bool {
        false
    }
    fn connect(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError>;
    fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError>;
    fn request_history(&self, request: HistoryRequest) -> Result<bool, ProviderSessionError>;
    fn release_idle(&self) -> Result<bool, ProviderSessionError>;
    fn authorization_changed(&self, _ready: bool) -> Result<bool, ProviderSessionError> {
        Err(ProviderSessionError::Unavailable)
    }
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
}

pub(super) struct ProviderRegistry {
    sequence: EpochSequence<ProviderTag>,
    connections: BTreeMap<&'static str, ProviderConnection>,
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

pub(super) struct CtraderSession {
    pub(super) controls: SyncSender<super::ctrader::RealtimeControl>,
    pub(super) history: SyncSender<HistoryRequest>,
}

impl ProviderSession for CtraderSession {
    fn catalog_uses_session(&self) -> bool {
        true
    }
    fn connect(
        &self,
        epoch: Epoch<ProviderTag>,
        sink: ProviderEventSink,
    ) -> Result<bool, ProviderSessionError> {
        debug_assert_eq!(sink.epoch, epoch);
        try_control(
            &self.controls,
            super::ctrader::RealtimeControl::Connect(sink),
        )
    }

    fn apply_demand(&self, demand: &ProviderDemand) -> Result<bool, ProviderSessionError> {
        try_control(
            &self.controls,
            super::ctrader::RealtimeControl::Subscribe(demand.ctrader_wire()),
        )
    }

    fn request_history(&self, request: HistoryRequest) -> Result<bool, ProviderSessionError> {
        try_control(&self.history, request)
    }

    fn release_idle(&self) -> Result<bool, ProviderSessionError> {
        try_control(&self.controls, super::ctrader::RealtimeControl::Stop)
    }

    fn authorization_changed(&self, ready: bool) -> Result<bool, ProviderSessionError> {
        try_control(
            &self.controls,
            super::ctrader::RealtimeControl::AuthorizationChanged(ready),
        )
    }

    fn shutdown(&self) {
        let _ = self.release_idle();
    }
}

impl ProviderRegistry {
    pub(super) fn catalog_connect_ids(&self) -> Vec<&'static str> {
        self.connections
            .iter()
            .filter_map(|(id, connection)| connection.session.catalog_uses_session().then_some(*id))
            .collect()
    }

    pub(super) fn catalog_uses_session(&self, id: &str) -> bool {
        self.connections
            .get(id)
            .is_some_and(|connection| connection.session.catalog_uses_session())
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
    pub(super) fn new() -> Self {
        Self {
            sequence: EpochSequence::default(),
            connections: BTreeMap::new(),
        }
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
        if connection
            .session
            .connect(epoch, ProviderEventSink { epoch })?
        {
            connection.epoch = Some(epoch);
            connection.state = ConnectionState::Connecting;
            connection.last_demand = None;
            connection.online_since = None;
            Ok(Some(epoch))
        } else {
            Ok(None)
        }
    }

    pub(super) fn connected(&mut self, id: &str, epoch: Epoch<ProviderTag>, now: Instant) {
        if let Some(connection) = self.connections.get_mut(id)
            && connection.epoch == Some(epoch)
        {
            connection.state = ConnectionState::Online;
            connection.online_since = Some(now);
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
        {
            if connection.online_since.take().is_some_and(|since| {
                now.saturating_duration_since(since)
                    >= connection.backoff.policy().healthy_reset_after()
            }) {
                connection.backoff.reset();
            }
            connection.state = match connection.backoff.failure(class) {
                RetryDecision::Wait(delay) => ConnectionState::RetryAt(now + delay),
                RetryDecision::Failed { .. } | RetryDecision::Fatal => ConnectionState::Failed,
            };
        }
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
        if connection.last_demand.as_ref() == Some(demand) {
            return Ok(true);
        }
        if connection.session.apply_demand(demand)? {
            connection.last_demand = Some(demand.clone());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(super) fn request_history(
        &self,
        id: &str,
        request: HistoryRequest,
    ) -> Result<bool, ProviderSessionError> {
        self.connections
            .get(id)
            .ok_or(ProviderSessionError::Unavailable)?
            .session
            .request_history(request)
    }

    pub(super) fn authorization_changed(
        &self,
        id: &str,
        ready: bool,
    ) -> Result<bool, ProviderSessionError> {
        self.connections
            .get(id)
            .ok_or(ProviderSessionError::Unavailable)?
            .session
            .authorization_changed(ready)
    }

    pub(super) fn release_idle(&mut self, id: &str) -> Result<bool, ProviderSessionError> {
        let connection = self
            .connections
            .get_mut(id)
            .ok_or(ProviderSessionError::Unavailable)?;
        if connection.session.release_idle()? {
            connection.state = ConnectionState::Idle;
            connection.epoch = None;
            connection.backoff.reset();
            connection.last_demand = None;
            connection.online_since = None;
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
    use crate::market_service::clock::ScriptedClock;
    use std::sync::{Arc, Mutex};

    struct ScriptedSession(Arc<Mutex<(usize, Vec<ProviderDemand>)>>);

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
            Ok(true)
        }

        fn shutdown(&self) {}
    }

    #[test]
    fn scripted_registry_changes_demand_without_connecting_again() {
        let clock = ScriptedClock::new(12);
        let calls = Arc::new(Mutex::new((0, Vec::new())));
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
    fn scripted_registry_reserves_next_epoch_only_when_retry_is_due() {
        let clock = ScriptedClock::new(34);
        let calls = Arc::new(Mutex::new((0, Vec::new())));
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
        assert_ne!(retry[0].1, first);
        assert_eq!(registry.epoch("hyperliquid"), Some(retry[0].1));
        assert_eq!(calls.lock().unwrap().0, 2);
    }

    #[test]
    fn scripted_ctrader_registry_applies_five_diffs_without_a_second_connection() {
        let clock = ScriptedClock::new(42);
        let calls = Arc::new(Mutex::new((0, Vec::new())));
        let mut registry = ProviderRegistry::new();
        registry.register(
            "ctrader",
            Box::new(ScriptedSession(Arc::clone(&calls))),
            ReconnectPolicy::ctrader(),
            &clock,
        );
        let epoch = registry
            .connect("ctrader", clock.monotonic())
            .unwrap()
            .unwrap();
        registry.connected("ctrader", epoch, clock.monotonic());
        for revision in 0..5 {
            let mut demand = ProviderDemand::default();
            demand
                .explicit_trade_ids
                .insert(format!("symbol-or-interval-{revision}"));
            assert!(registry.apply_demand("ctrader", &demand).unwrap());
            assert!(registry.apply_demand("ctrader", &demand).unwrap());
            assert_eq!(
                registry.connect("ctrader", clock.monotonic()).unwrap(),
                None
            );
        }
        assert_eq!(calls.lock().unwrap().0, 1);
        assert_eq!(calls.lock().unwrap().1.len(), 5);
        assert_eq!(registry.epoch("ctrader"), Some(epoch));
    }

    #[test]
    fn scripted_ctrader_registry_honors_server_wait_and_retry_budget() {
        let clock = ScriptedClock::new(43);
        let calls = Arc::new(Mutex::new((0, Vec::new())));
        let mut registry = ProviderRegistry::new();
        registry.register(
            "ctrader",
            Box::new(ScriptedSession(Arc::clone(&calls))),
            ReconnectPolicy::ctrader(),
            &clock,
        );
        let mut epoch = registry
            .connect("ctrader", clock.monotonic())
            .unwrap()
            .unwrap();
        registry.connected("ctrader", epoch, clock.monotonic());
        let now = clock.monotonic();
        registry.disconnected_with_class(
            "ctrader",
            epoch,
            now,
            RetryClass::RateLimited {
                retry_after: std::time::Duration::from_secs(120),
            },
        );
        assert_eq!(
            registry.next_retry(),
            Some(now + std::time::Duration::from_secs(120))
        );
        assert_eq!(
            registry
                .retry_due(now + std::time::Duration::from_secs(119))
                .len(),
            0
        );
        epoch = registry.retry_due(now + std::time::Duration::from_secs(120))[0].1;
        for attempt in 1..=5 {
            registry.connected("ctrader", epoch, now);
            registry.disconnected("ctrader", epoch, now);
            if attempt < 5 {
                let due = registry.next_retry().expect("remaining retry");
                epoch = registry.retry_due(due)[0].1;
            }
        }
        assert_eq!(registry.next_retry(), None);
        assert_eq!(
            registry.connections["ctrader"].state,
            ConnectionState::Failed
        );
        assert_eq!(calls.lock().unwrap().0, 6);
    }

    #[test]
    fn ctrader_catalog_and_market_controls_share_the_reserved_epoch() {
        let clock = ScriptedClock::new(44);
        let (controls, queued) = std::sync::mpsc::sync_channel(4);
        let (history, _) = std::sync::mpsc::sync_channel(4);
        let mut registry = ProviderRegistry::new();
        registry.register(
            "ctrader",
            Box::new(CtraderSession { controls, history }),
            ReconnectPolicy::ctrader(),
            &clock,
        );
        assert_eq!(registry.catalog_connect_ids(), ["ctrader"]);
        assert!(!registry.catalog_ready("ctrader"));
        let epoch = registry
            .connect("ctrader", clock.monotonic())
            .unwrap()
            .unwrap();
        assert!(registry.catalog_ready("ctrader"));
        let super::super::ctrader::RealtimeControl::Connect(sink) = queued.try_recv().unwrap()
        else {
            panic!("catalog must reserve the provider epoch before connecting");
        };
        assert_eq!(sink.epoch, epoch);
        registry
            .apply_demand("ctrader", &ProviderDemand::default())
            .unwrap();
        assert!(matches!(
            queued.try_recv().unwrap(),
            super::super::ctrader::RealtimeControl::Subscribe(_)
        ));
        assert_eq!(
            registry.connect("ctrader", clock.monotonic()).unwrap(),
            None
        );
        assert!(registry.release_idle("ctrader").unwrap());
        assert!(!registry.catalog_ready("ctrader"));
        assert!(matches!(
            queued.try_recv().unwrap(),
            super::super::ctrader::RealtimeControl::Stop
        ));
    }
}
