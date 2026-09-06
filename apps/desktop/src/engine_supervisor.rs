//! Desktop-owned recovery for authenticated resident-engine IPC sessions.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

use axiusflow_engine_protocol::{
    ConsumerResourceClass, InstallProviderInstrument, SearchProviderInstruments,
    SelectProviderInstrument, SeriesKey, envelope,
};
use axiusflow_local_engine_client::{
    EngineClient, connect_or_start_engine, connect_or_start_engine_until, sibling_engine_executable,
};

const RESTORE_DEADLINE: Duration = Duration::from_secs(4);
const RESTORE_RETRY_INTERVAL: Duration = Duration::from_millis(25);
const MAX_PENDING_EVENTS: usize = 256;

#[derive(Clone)]
struct ConsumerRestore {
    workspace_id: u64,
    demand: Option<(u64, SeriesKey)>,
    viewport: Option<(u64, i64, i64)>,
    resource_class: ConsumerResourceClass,
    pending_search: Option<SearchProviderInstruments>,
    pending_selection: Option<SelectProviderInstrument>,
}

/// Result of one supervised pushed market event.
pub(super) struct SupervisedEvent {
    pub consumer_id: Option<u64>,
    pub event: Option<envelope::Payload>,
    pub reconnected: bool,
}

/// One bounded desktop IPC owner that reconstructs its consumers after engine restart.
pub(super) struct EngineSupervisor {
    executable: PathBuf,
    client_id: u64,
    client: EngineClient,
    consumers: BTreeMap<u64, ConsumerRestore>,
    instruments: BTreeMap<(String, String), InstallProviderInstrument>,
    pending_instruments: BTreeSet<(String, String)>,
    pending_events: VecDeque<(u64, envelope::Payload)>,
    #[cfg(test)]
    reconnect_fixture: Option<Box<dyn FnMut() -> Result<EngineClient, String>>>,
}

impl EngineSupervisor {
    pub fn connect(client_id: u64) -> Result<Self, String> {
        let executable = sibling_engine_executable()?;
        let mut client = connect_or_start_engine(&executable)?;
        client.attach_client(client_id)?;
        Ok(Self {
            executable,
            client_id,
            client,
            consumers: BTreeMap::new(),
            instruments: BTreeMap::new(),
            pending_instruments: BTreeSet::new(),
            pending_events: VecDeque::new(),
            #[cfg(test)]
            reconnect_fixture: None,
        })
    }

    pub fn register_consumer(&mut self, workspace_id: u64, consumer_id: u64) -> Result<(), String> {
        self.client
            .register_consumer(self.client_id, workspace_id, consumer_id)?;
        self.consumers.insert(
            consumer_id,
            ConsumerRestore {
                workspace_id,
                demand: None,
                viewport: None,
                resource_class: ConsumerResourceClass::Foreground,
                pending_search: None,
                pending_selection: None,
            },
        );
        Ok(())
    }

    pub fn install_provider_instrument(
        &mut self,
        instrument: InstallProviderInstrument,
    ) -> Result<(), String> {
        self.client
            .install_provider_instrument(instrument.clone())?;
        self.record_provider_instrument(instrument);
        Ok(())
    }

    pub fn set_series_demand(
        &mut self,
        consumer_id: u64,
        generation: u64,
        series: SeriesKey,
    ) -> Result<(), String> {
        self.client
            .set_series_demand(consumer_id, generation, series.clone())?;
        self.record_series_demand(consumer_id, generation, series)
    }

    pub fn set_series_demand_until(
        &mut self,
        consumer_id: u64,
        generation: u64,
        series: SeriesKey,
        deadline: Instant,
    ) -> Result<(), String> {
        self.client
            .set_series_demand_until(consumer_id, generation, series.clone(), deadline)?;
        self.record_series_demand(consumer_id, generation, series)
    }

    fn record_series_demand(
        &mut self,
        consumer_id: u64,
        generation: u64,
        series: SeriesKey,
    ) -> Result<(), String> {
        let previous_provider = self
            .consumers
            .get(&consumer_id)
            .and_then(|consumer| consumer.demand.as_ref())
            .map(|(_, current)| current.provider.clone());
        let provider = series.provider.clone();
        self.pending_instruments
            .remove(&(provider.clone(), series.instrument_id.clone()));
        self.consumer_mut(consumer_id)?.demand = Some((generation, series));
        if let Some(previous_provider) = previous_provider
            && previous_provider != provider
        {
            self.prune_provider_instruments(&previous_provider);
        }
        self.prune_provider_instruments(&provider);
        Ok(())
    }

    pub fn set_market_viewport(
        &mut self,
        consumer_id: u64,
        generation: u64,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), String> {
        self.client.set_market_viewport(
            consumer_id,
            generation,
            start_unix_nanos,
            end_unix_nanos,
        )?;
        self.consumer_mut(consumer_id)?.viewport =
            Some((generation, start_unix_nanos, end_unix_nanos));
        Ok(())
    }

    pub fn set_market_resource_class(
        &mut self,
        consumer_id: u64,
        resource_class: ConsumerResourceClass,
    ) -> Result<(), String> {
        self.client
            .set_market_resource_class(consumer_id, resource_class)?;
        self.consumer_mut(consumer_id)?.resource_class = resource_class;
        Ok(())
    }

    pub fn search_provider_instruments(
        &mut self,
        request: SearchProviderInstruments,
    ) -> Result<(), String> {
        let consumer_id = request.consumer_id;
        self.client.search_provider_instruments(request.clone())?;
        self.consumer_mut(consumer_id)?.pending_search = Some(request);
        Ok(())
    }

    pub fn select_provider_instrument(
        &mut self,
        request: SelectProviderInstrument,
    ) -> Result<(), String> {
        let consumer_id = request.consumer_id;
        self.client.select_provider_instrument(request.clone())?;
        self.consumer_mut(consumer_id)?.pending_selection = Some(request);
        Ok(())
    }

    pub fn abandon_provider_search(&mut self, consumer_id: u64, provider: &str, generation: u64) {
        if let Some(consumer) = self.consumers.get_mut(&consumer_id) {
            abandon_matching_search(consumer, provider, generation);
        }
    }

    pub fn abandon_provider_selection(
        &mut self,
        consumer_id: u64,
        provider: &str,
        generation: u64,
    ) {
        if let Some(consumer) = self.consumers.get_mut(&consumer_id) {
            abandon_matching_selection(consumer, provider, generation);
        }
    }

    pub fn receive_market_event(&mut self, timeout: Duration) -> Result<SupervisedEvent, String> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(self.finish_event(event));
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| "market receive deadline overflowed".to_string())?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        match self.client.receive_market_event_timeout(remaining) {
            Ok(Some(event)) => Ok(self.finish_event(event)),
            Ok(None) => Ok(SupervisedEvent {
                consumer_id: None,
                event: None,
                reconnected: false,
            }),
            Err(disconnected) => {
                let restore_deadline = Instant::now()
                    .checked_add(RESTORE_DEADLINE)
                    .ok_or_else(|| "engine recovery deadline overflowed".to_string())?;
                self.reconnect_and_restore_until(restore_deadline).map_err(|restore| {
                    format!("resident engine connection failed: {disconnected}; recovery failed: {restore}")
                })?;
                Ok(SupervisedEvent {
                    consumer_id: None,
                    event: None,
                    reconnected: true,
                })
            }
        }
    }

    pub fn receive_market_event_for(
        &mut self,
        consumer_id: u64,
        timeout: Duration,
    ) -> Result<SupervisedEvent, String> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| "market receive deadline overflowed".to_string())?;
        self.receive_market_event_for_until(consumer_id, deadline)
    }

    pub fn receive_market_event_for_until(
        &mut self,
        consumer_id: u64,
        deadline: Instant,
    ) -> Result<SupervisedEvent, String> {
        if let Some(index) = self
            .pending_events
            .iter()
            .position(|(target, _)| *target == consumer_id)
            && let Some(event) = self.pending_events.remove(index)
        {
            return Ok(self.finish_event(event));
        }
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(SupervisedEvent {
                    consumer_id: None,
                    event: None,
                    reconnected: false,
                });
            }
            match self.client.receive_market_event_timeout(remaining) {
                Ok(Some(event)) if event.0 == consumer_id || event.0 == 0 => {
                    return Ok(self.finish_event(event));
                }
                Ok(Some(event)) => {
                    if self.pending_events.len() >= MAX_PENDING_EVENTS {
                        return Err("desktop market event buffer exceeded its bound".to_string());
                    }
                    self.pending_events.push_back(event);
                }
                Ok(None) => {
                    return Ok(SupervisedEvent {
                        consumer_id: None,
                        event: None,
                        reconnected: false,
                    });
                }
                Err(disconnected) => {
                    self.reconnect_and_restore_until(deadline).map_err(|restore| {
                        format!("resident engine connection failed: {disconnected}; recovery failed: {restore}")
                    })?;
                    return Ok(SupervisedEvent {
                        consumer_id: None,
                        event: None,
                        reconnected: true,
                    });
                }
            }
        }
    }

    fn finish_event(&mut self, (consumer_id, event): (u64, envelope::Payload)) -> SupervisedEvent {
        if consumer_id == 0 {
            return SupervisedEvent {
                consumer_id: Some(0),
                event: Some(event),
                reconnected: false,
            };
        }
        if !self.complete_catalog_command(consumer_id, &event) {
            return SupervisedEvent {
                consumer_id: Some(consumer_id),
                event: None,
                reconnected: false,
            };
        }
        if let envelope::Payload::ProviderInstrumentSelection(selection) = &event
            && let Some(instrument) = selection.instrument.clone()
        {
            self.record_provider_instrument(instrument);
        }
        SupervisedEvent {
            consumer_id: Some(consumer_id),
            event: Some(event),
            reconnected: false,
        }
    }

    pub fn remove_market_consumer(&mut self, consumer_id: u64) -> Result<(), String> {
        self.consumers.remove(&consumer_id);
        self.prune_provider_instruments_for_current_demands();
        self.client.remove_market_consumer(consumer_id)
    }

    pub fn detach(mut self) -> Result<(), String> {
        self.detach_client()
    }

    pub fn detach_client(&mut self) -> Result<(), String> {
        self.consumers.clear();
        self.instruments.clear();
        self.pending_instruments.clear();
        self.pending_events.clear();
        self.client.detach_client(self.client_id)
    }

    fn consumer_mut(&mut self, consumer_id: u64) -> Result<&mut ConsumerRestore, String> {
        self.consumers
            .get_mut(&consumer_id)
            .ok_or_else(|| "desktop engine consumer is not registered".to_string())
    }

    fn complete_catalog_command(&mut self, consumer_id: u64, event: &envelope::Payload) -> bool {
        let Some(consumer) = self.consumers.get_mut(&consumer_id) else {
            return false;
        };
        match event {
            envelope::Payload::ProviderInstrumentSearchResult(result) => {
                let current = consumer.pending_search.as_ref().is_some_and(|request| {
                    request.provider == result.provider
                        && request.search_generation == result.search_generation
                });
                if current {
                    consumer.pending_search = None;
                }
                current
            }
            envelope::Payload::ProviderInstrumentSelection(selection) => {
                let current = consumer
                    .pending_selection
                    .as_ref()
                    .is_some_and(|request| selection_completion_matches(request, selection));
                if current {
                    consumer.pending_selection = None;
                }
                current
            }
            envelope::Payload::ProviderCatalogRejected(rejection) => {
                let search = consumer.pending_search.as_ref().is_some_and(|request| {
                    request.provider == rejection.provider
                        && request.search_generation == rejection.command_generation
                });
                let selection = consumer.pending_selection.as_ref().is_some_and(|request| {
                    request.provider == rejection.provider
                        && request.selection_generation == rejection.command_generation
                });
                if search {
                    consumer.pending_search = None;
                }
                if selection {
                    consumer.pending_selection = None;
                }
                search || selection
            }
            _ => true,
        }
    }

    fn record_provider_instrument(&mut self, instrument: InstallProviderInstrument) {
        let provider = instrument.provider.clone();
        let current_session = self
            .instruments
            .values()
            .filter(|installed| installed.provider == provider)
            .map(|installed| installed.session_generation)
            .max();
        let newer_session =
            current_session.is_none_or(|session| instrument.session_generation > session);
        if newer_session || provider == "rithmic" {
            self.instruments
                .retain(|_, installed| installed.provider != provider);
            self.pending_instruments
                .retain(|(installed_provider, _)| installed_provider != &provider);
        }
        if provider == "rithmic" {
            for consumer in self.consumers.values_mut() {
                let obsolete = consumer.demand.as_ref().is_some_and(|(_, series)| {
                    series.provider == provider
                        && (series.instrument_id != instrument.instrument_id
                            || series.entitlement_id != instrument.entitlement_id)
                });
                if obsolete {
                    consumer.demand = None;
                    consumer.viewport = None;
                }
                if consumer
                    .pending_selection
                    .as_ref()
                    .is_some_and(|selection| selection.provider == provider && newer_session)
                {
                    consumer.pending_selection = None;
                }
            }
        }
        let key = (provider.clone(), instrument.instrument_id.clone());
        let demanded = self.consumers.values().any(|consumer| {
            consumer.demand.as_ref().is_some_and(|(_, series)| {
                series.provider == instrument.provider
                    && series.instrument_id == instrument.instrument_id
                    && series.entitlement_id == instrument.entitlement_id
            })
        });
        self.instruments.insert(key.clone(), instrument);
        if demanded {
            self.pending_instruments.remove(&key);
        } else {
            self.pending_instruments.insert(key);
        }
        self.prune_provider_instruments(&provider);
    }

    fn prune_provider_instruments_for_current_demands(&mut self) {
        let providers = self
            .instruments
            .values()
            .map(|instrument| instrument.provider.clone())
            .collect::<Vec<_>>();
        for provider in providers {
            self.prune_provider_instruments(&provider);
        }
    }

    fn prune_provider_instruments(&mut self, provider: &str) {
        let pending = self.pending_instruments.clone();
        let demanded = self
            .consumers
            .values()
            .filter_map(|consumer| consumer.demand.as_ref())
            .map(|(_, series)| {
                (
                    series.provider.clone(),
                    series.instrument_id.clone(),
                    series.entitlement_id.clone(),
                )
            })
            .collect::<BTreeSet<_>>();
        let current_session = self
            .instruments
            .values()
            .filter(|instrument| instrument.provider == provider)
            .map(|instrument| instrument.session_generation)
            .max();
        let latest_selection = current_session.and_then(|session| {
            self.instruments
                .values()
                .filter(|instrument| {
                    instrument.provider == provider && instrument.session_generation == session
                })
                .map(|instrument| instrument.selection_generation)
                .max()
        });
        self.instruments.retain(|_, instrument| {
            if instrument.provider != provider {
                return true;
            }
            if Some(instrument.session_generation) != current_session {
                return false;
            }
            if provider == "rithmic" {
                return Some(instrument.selection_generation) == latest_selection
                    && (demanded.contains(&(
                        instrument.provider.clone(),
                        instrument.instrument_id.clone(),
                        instrument.entitlement_id.clone(),
                    )) || pending.contains(&(
                        instrument.provider.clone(),
                        instrument.instrument_id.clone(),
                    )));
            }
            demanded.contains(&(
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
                instrument.entitlement_id.clone(),
            )) || pending.contains(&(
                instrument.provider.clone(),
                instrument.instrument_id.clone(),
            ))
        });
        self.pending_instruments
            .retain(|key| self.instruments.contains_key(key));
    }

    fn reconnect_and_restore_until(&mut self, deadline: Instant) -> Result<(), String> {
        let mut last_error = "resident engine recovery did not start".to_string();
        while Instant::now() < deadline {
            match self
                .connect_replacement_until(deadline)
                .and_then(|mut client| {
                    self.restore_into_until(&mut client, deadline)
                        .map(|()| client)
                }) {
                Ok(client) => {
                    self.client = client;
                    self.pending_events.clear();
                    return Ok(());
                }
                Err(error) => last_error = error,
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            thread::sleep(remaining.min(RESTORE_RETRY_INTERVAL));
        }
        Err(last_error)
    }

    fn connect_replacement_until(&mut self, deadline: Instant) -> Result<EngineClient, String> {
        #[cfg(test)]
        if let Some(connect) = self.reconnect_fixture.as_mut() {
            return connect();
        }
        connect_or_start_engine_until(&self.executable, deadline)
    }

    fn restore_into_until(
        &self,
        client: &mut EngineClient,
        deadline: Instant,
    ) -> Result<(), String> {
        client.attach_client_until(self.client_id, deadline)?;
        for (consumer_id, consumer) in &self.consumers {
            client.register_consumer_until(
                self.client_id,
                consumer.workspace_id,
                *consumer_id,
                deadline,
            )?;
        }
        let mut instruments = self.instruments.values().cloned().collect::<Vec<_>>();
        instruments.sort_by_key(|instrument| {
            (
                instrument.provider.clone(),
                instrument.session_generation,
                instrument.selection_generation,
                instrument.instrument_id.clone(),
            )
        });
        for instrument in instruments {
            client.install_provider_instrument_until(instrument, deadline)?;
        }
        for (consumer_id, consumer) in &self.consumers {
            if let Some(search) = &consumer.pending_search {
                client.search_provider_instruments_until(search.clone(), deadline)?;
            }
            if let Some(selection) = &consumer.pending_selection {
                client.select_provider_instrument_until(selection.clone(), deadline)?;
            }
            if let Some((generation, series)) = &consumer.demand {
                client.set_series_demand_until(
                    *consumer_id,
                    *generation,
                    series.clone(),
                    deadline,
                )?;
            }
            if let Some((generation, start, end)) = consumer.viewport {
                client.set_market_viewport_until(*consumer_id, generation, start, end, deadline)?;
            }
            client.set_market_resource_class_until(
                *consumer_id,
                consumer.resource_class,
                deadline,
            )?;
        }
        Ok(())
    }
}

fn selection_completion_matches(
    request: &SelectProviderInstrument,
    selection: &axiusflow_engine_protocol::ProviderInstrumentSelection,
) -> bool {
    selection.instrument.as_ref().is_some_and(|instrument| {
        request.provider == instrument.provider
            && request.selection_generation == selection.command_generation
    })
}

fn abandon_matching_search(consumer: &mut ConsumerRestore, provider: &str, generation: u64) {
    if consumer.pending_search.as_ref().is_some_and(|request| {
        request.provider == provider && request.search_generation == generation
    }) {
        consumer.pending_search = None;
    }
}

fn abandon_matching_selection(consumer: &mut ConsumerRestore, provider: &str, generation: u64) {
    if consumer.pending_selection.as_ref().is_some_and(|request| {
        request.provider == provider && request.selection_generation == generation
    }) {
        consumer.pending_selection = None;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet, VecDeque},
        io::{Read, Write},
        path::PathBuf,
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    use axiusflow_engine_protocol::{
        EngineReady, Envelope, EnvelopeDecoder, InstallProviderInstrument, MarketBar,
        PROTOCOL_VERSION, ProviderInstrumentInstalled, ProviderInstrumentSelection,
        SearchProviderInstruments, SelectProviderInstrument, SeriesCadence, SeriesKey,
        SeriesSnapshot, encode_envelope, envelope,
    };
    use axiusflow_local_engine_client::EngineClient;
    use interprocess::local_socket::{
        GenericNamespaced, ListenerOptions, ToNsName as _, prelude::*,
    };

    use super::{
        ConsumerRestore, EngineSupervisor, abandon_matching_search, abandon_matching_selection,
        selection_completion_matches,
    };
    use axiusflow_engine_protocol::ConsumerResourceClass;

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn catalog_timeout_cleanup_is_generation_fenced() {
        let mut consumer = ConsumerRestore {
            workspace_id: 1,
            demand: None,
            viewport: None,
            resource_class: ConsumerResourceClass::Foreground,
            pending_search: Some(SearchProviderInstruments {
                consumer_id: 9,
                search_generation: 4,
                provider: "rithmic".to_string(),
                query: "ES".to_string(),
                maximum_results: 20,
            }),
            pending_selection: Some(SelectProviderInstrument {
                consumer_id: 9,
                selection_generation: 8,
                search_generation: 4,
                provider: "rithmic".to_string(),
                symbol: "ESZ6".to_string(),
                exchange: "CME".to_string(),
                entitlement_id: "fixture".to_string(),
            }),
        };

        abandon_matching_search(&mut consumer, "rithmic", 3);
        abandon_matching_selection(&mut consumer, "rithmic", 7);
        assert!(consumer.pending_search.is_some());
        assert!(consumer.pending_selection.is_some());

        abandon_matching_search(&mut consumer, "rithmic", 4);
        abandon_matching_selection(&mut consumer, "rithmic", 8);
        assert!(consumer.pending_search.is_none());
        assert!(consumer.pending_selection.is_none());
    }

    #[test]
    fn provider_selection_completion_uses_command_generation_not_provider_generation() {
        let request = SelectProviderInstrument {
            consumer_id: 9,
            selection_generation: 4,
            search_generation: 3,
            provider: "hyperliquid".to_string(),
            symbol: "ETH".to_string(),
            exchange: "Hyperliquid".to_string(),
            entitlement_id: "hyperliquid-public".to_string(),
        };
        let selection = ProviderInstrumentSelection {
            consumer_id: 9,
            command_generation: 4,
            instrument: Some(provider_instrument(
                "hyperliquid",
                "hyperliquid:perp:ETH",
                7,
                29,
            )),
        };
        assert!(selection_completion_matches(&request, &selection));

        let stale_command = ProviderInstrumentSelection {
            command_generation: 3,
            ..selection
        };
        assert!(!selection_completion_matches(&request, &stale_command));
    }

    struct FixtureServer {
        command: LocalSocketStream,
        event: LocalSocketStream,
        decoder: EnvelopeDecoder,
        pending: VecDeque<Envelope>,
    }

    impl FixtureServer {
        fn receive(&mut self) -> envelope::Payload {
            loop {
                if let Some(envelope) = self.pending.pop_front() {
                    return envelope.payload.expect("fixture payload");
                }
                let mut bytes = [0_u8; 16 * 1024];
                let count = self.command.read(&mut bytes).expect("fixture read");
                assert!(count > 0, "fixture client remains connected");
                self.pending.extend(
                    self.decoder
                        .push(&bytes[..count])
                        .expect("decode fixture frame"),
                );
            }
        }

        fn send(&mut self, payload: envelope::Payload) {
            self.send_to(0, payload);
        }

        fn send_to(&mut self, target_consumer_id: u64, payload: envelope::Payload) {
            let frame = encode_envelope(&Envelope {
                protocol_version: PROTOCOL_VERSION,
                target_consumer_id,
                payload: Some(payload),
            })
            .expect("encode fixture frame");
            self.event.write_all(&frame).expect("fixture write");
            self.event.flush().expect("fixture flush");
        }

        fn send_ready(&mut self, epoch: u64) {
            let frame = encode_envelope(&Envelope {
                protocol_version: PROTOCOL_VERSION,
                target_consumer_id: 0,
                payload: Some(envelope::Payload::EngineReady(EngineReady {
                    protocol_version: PROTOCOL_VERSION,
                    engine_epoch: epoch,
                    workspace_revision: 0,
                    lifecycle_contract_revision:
                        axiusflow_engine_protocol::LIFECYCLE_CONTRACT_REVISION,
                    release_identity: axiusflow_platform_runtime::current_release_identity()
                        .release_identity,
                    install_generation: axiusflow_platform_runtime::current_release_identity()
                        .install_generation,
                })),
            })
            .expect("encode fixture readiness");
            self.command
                .write_all(&frame)
                .expect("fixture readiness write");
            self.command.flush().expect("fixture readiness flush");
        }
    }

    /// Accepts both session streams in any arrival order and pairs them by
    /// nonce, mirroring the resident engine pairing contract.
    fn accept_fixture_pair(
        listener: &LocalSocketListener,
    ) -> (LocalSocketStream, LocalSocketStream) {
        let mut command = None;
        let mut event = None;
        let mut nonce = None;
        for _ in 0..2 {
            let mut stream = listener.accept().expect("accept fixture client");
            let mut decoder = EnvelopeDecoder::try_new().expect("fixture decoder");
            let hello = loop {
                let mut bytes = [0_u8; 16 * 1024];
                let count = stream.read(&mut bytes).expect("fixture hello read");
                assert!(count > 0, "fixture client sends its hello");
                let mut envelopes = decoder.push(&bytes[..count]).expect("decode fixture hello");
                if let Some(envelope) = envelopes.pop() {
                    break match envelope.payload {
                        Some(envelope::Payload::ClientHello(hello)) => hello,
                        _ => panic!("fixture expects a client hello"),
                    };
                }
            };
            if let Some(previous) = nonce.replace(hello.session_nonce) {
                assert_eq!(
                    previous, hello.session_nonce,
                    "paired fixture streams share one nonce"
                );
            }
            if hello.stream_role == axiusflow_engine_protocol::StreamRole::Command as i32 {
                assert!(command.replace(stream).is_none());
            } else if hello.stream_role == axiusflow_engine_protocol::StreamRole::Event as i32 {
                assert!(event.replace(stream).is_none());
            } else {
                panic!("fixture expects a directed stream hello");
            }
        }
        (
            command.expect("paired fixture command stream"),
            event.expect("paired fixture event stream"),
        )
    }

    fn start_fixture(
        epoch: u64,
        disconnect_on_poll: bool,
        snapshot_series: SeriesKey,
    ) -> (
        String,
        Arc<Mutex<Vec<envelope::Payload>>>,
        thread::JoinHandle<()>,
    ) {
        let name = format!(
            "axiusflow-supervisor-test-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        );
        let socket = name
            .as_str()
            .to_ns_name::<GenericNamespaced>()
            .expect("fixture socket name");
        let listener = ListenerOptions::new()
            .name(socket)
            .create_sync()
            .expect("bind fixture server");
        let commands = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&commands);
        let worker = thread::spawn(move || {
            let (command, event) = accept_fixture_pair(&listener);
            let mut server = FixtureServer {
                command,
                event,
                decoder: EnvelopeDecoder::try_new().expect("fixture decoder"),
                pending: VecDeque::new(),
            };
            server.send_ready(epoch);
            let mut registered_consumers = 0_usize;
            let mut visibility_demands = 0_usize;
            loop {
                let payload = server.receive();
                if matches!(payload, envelope::Payload::RegisterConsumer(_)) {
                    registered_consumers = registered_consumers.saturating_add(1);
                }
                if matches!(payload, envelope::Payload::VisibilityDemand(_)) {
                    visibility_demands = visibility_demands.saturating_add(1);
                }
                if let envelope::Payload::InstallProviderInstrument(instrument) = &payload {
                    server.send(envelope::Payload::ProviderInstrumentInstalled(
                        ProviderInstrumentInstalled {
                            provider: instrument.provider.clone(),
                            session_generation: instrument.session_generation,
                            selection_generation: instrument.selection_generation,
                            instrument_id: instrument.instrument_id.clone(),
                        },
                    ));
                }
                let generation_restore_complete = matches!(
                    &payload,
                    envelope::Payload::SeriesDemand(demand)
                        if registered_consumers >= 4
                            && demand.consumer_id == 14
                            && demand.generation == 9
                );
                recorded
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(payload);
                if disconnect_on_poll && (visibility_demands > 0 || generation_restore_complete) {
                    return;
                }
                if registered_consumers > 0 && visibility_demands == registered_consumers {
                    server.send_to(
                        11,
                        envelope::Payload::SeriesSnapshot(SeriesSnapshot {
                            consumer_id: 11,
                            generation: 7,
                            series: Some(snapshot_series),
                            provider_generation: 2,
                            price_scale: 2,
                            quantity_scale: 8,
                            bars: vec![MarketBar {
                                source_sequence: 1,
                                exchange_timestamp_seconds: 60,
                                open: 100,
                                high: 110,
                                low: 90,
                                close: 105,
                                volume: 7,
                                exchange_timestamp_unix_nanos: 60_000_000_000,
                            }],
                            publication_generation: 1,
                            forming: false,
                        }),
                    );
                    return;
                }
            }
        });
        (name, commands, worker)
    }

    fn series() -> SeriesKey {
        SeriesKey {
            provider: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:btc:usd".to_string(),
            cadence_value: 60,
            definition_revision: 1,
            entitlement_id: "crypto_public_realtime".to_string(),
            cadence: SeriesCadence::FixedSeconds as i32,
        }
    }

    fn instrument() -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: "rithmic".to_string(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: "instrument:rithmic:btc:usd".to_string(),
            provider_symbol: "BTC-USD".to_string(),
            display_symbol: "BTC/USD".to_string(),
            venue_id: "rithmic".to_string(),
            price_scale: 2,
            quantity_scale: 8,
            entitlement_id: "crypto_public_realtime".to_string(),
        }
    }

    fn provider_instrument(
        provider: &str,
        instrument_id: &str,
        session_generation: u64,
        selection_generation: u64,
    ) -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: provider.to_string(),
            session_generation,
            selection_generation,
            instrument_id: instrument_id.to_string(),
            provider_symbol: instrument_id.to_string(),
            display_symbol: instrument_id.to_string(),
            venue_id: provider.to_string(),
            price_scale: 2,
            quantity_scale: 8,
            entitlement_id: if provider == "hyperliquid" {
                "hyperliquid-public".to_string()
            } else {
                format!("{provider}:{instrument_id}")
            },
        }
    }

    fn provider_series(instrument: &InstallProviderInstrument, cadence_value: u32) -> SeriesKey {
        SeriesKey {
            provider: instrument.provider.clone(),
            instrument_id: instrument.instrument_id.clone(),
            cadence_value,
            definition_revision: 1,
            entitlement_id: instrument.entitlement_id.clone(),
            cadence: SeriesCadence::FixedSeconds as i32,
        }
    }

    fn configure_restore_state(supervisor: &mut EngineSupervisor, requested: &SeriesKey) {
        supervisor
            .register_consumer(101, 11)
            .expect("register visible consumer");
        supervisor
            .register_consumer(202, 12)
            .expect("register hidden consumer");
        supervisor
            .install_provider_instrument(instrument())
            .expect("install instrument");
        supervisor
            .search_provider_instruments(SearchProviderInstruments {
                consumer_id: 11,
                search_generation: 3,
                provider: "rithmic".to_string(),
                query: "MNQ".to_string(),
                maximum_results: 8,
            })
            .expect("queue catalog search");
        supervisor
            .select_provider_instrument(SelectProviderInstrument {
                consumer_id: 12,
                selection_generation: 4,
                search_generation: 3,
                provider: "rithmic".to_string(),
                symbol: "MNQU6".to_string(),
                exchange: "CME".to_string(),
                entitlement_id: "rithmic-test".to_string(),
            })
            .expect("queue catalog selection");
        for consumer_id in [11, 12] {
            supervisor
                .set_series_demand(consumer_id, 7, requested.clone())
                .expect("set demand");
            supervisor
                .set_market_viewport(consumer_id, 7, 1_000, 2_000)
                .expect("set viewport");
        }
        supervisor
            .set_market_resource_class(12, ConsumerResourceClass::Background)
            .expect("hide second consumer");
    }

    fn configure_generation_ordered_restore(supervisor: &mut EngineSupervisor) {
        // Two providers restore together: the multiplexed Hyperliquid catalog
        // accumulates installed instruments across sessions, while the
        // single-selection Rithmic session keeps only its latest install.
        let hyperliquid_old_z = provider_instrument("hyperliquid", "hyperliquid:perp:BTC", 4, 1);
        let hyperliquid_old_a =
            provider_instrument("hyperliquid", "hyperliquid:spot:1:BTC/USDC", 4, 2);
        let hyperliquid_current_z =
            provider_instrument("hyperliquid", "hyperliquid:perp:BTC", 5, 1);
        let hyperliquid_current_a =
            provider_instrument("hyperliquid", "hyperliquid:spot:1:BTC/USDC", 5, 2);
        let rithmic_old_z = provider_instrument("rithmic", "z-obsolete", 7, 1);
        let rithmic_old_a = provider_instrument("rithmic", "a-obsolete", 7, 2);
        let rithmic_new_z = provider_instrument("rithmic", "z-replaced-session", 8, 1);
        let rithmic_current = provider_instrument("rithmic", "a-current", 8, 2);
        for consumer_id in 11..=14 {
            supervisor
                .register_consumer(100 + consumer_id, consumer_id)
                .expect("register restore consumer");
        }
        supervisor
            .install_provider_instrument(hyperliquid_old_z)
            .expect("install first concurrent instrument");
        supervisor
            .set_series_demand(11, 1, provider_series(&hyperliquid_current_z, 60))
            .expect("set first concurrent demand");
        supervisor
            .install_provider_instrument(hyperliquid_old_a)
            .expect("install second concurrent instrument");
        supervisor
            .set_series_demand(12, 1, provider_series(&hyperliquid_current_a, 300))
            .expect("set second concurrent demand");
        supervisor
            .install_provider_instrument(hyperliquid_current_z)
            .expect("advance concurrent provider session");
        supervisor
            .install_provider_instrument(hyperliquid_current_a)
            .expect("restore second current-session instrument");
        for instrument in [rithmic_old_z, rithmic_old_a] {
            supervisor
                .install_provider_instrument(instrument)
                .expect("advance Rithmic selection state");
        }
        supervisor
            .select_provider_instrument(SelectProviderInstrument {
                consumer_id: 14,
                selection_generation: 3,
                search_generation: 2,
                provider: "rithmic".to_string(),
                symbol: "OBSOLETE".to_string(),
                exchange: "CME".to_string(),
                entitlement_id: "rithmic:obsolete".to_string(),
            })
            .expect("queue old-session selection");
        supervisor
            .install_provider_instrument(rithmic_new_z)
            .expect("advance Rithmic provider session");
        supervisor
            .install_provider_instrument(rithmic_current.clone())
            .expect("install authoritative Rithmic selection");
        supervisor
            .set_series_demand(13, 4, provider_series(&rithmic_current, 60))
            .expect("set current Rithmic chart demand");
        supervisor
            .set_series_demand(14, 9, provider_series(&rithmic_current, 300))
            .expect("set concurrent current Rithmic demand");
        assert_eq!(supervisor.instruments.len(), 3);
    }

    fn assert_generation_ordered_restore(commands: &[envelope::Payload]) {
        let installed = commands
            .iter()
            .filter_map(|payload| match payload {
                envelope::Payload::InstallProviderInstrument(instrument) => Some((
                    instrument.provider.as_str(),
                    instrument.session_generation,
                    instrument.selection_generation,
                    instrument.instrument_id.as_str(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            installed,
            vec![
                ("hyperliquid", 5, 1, "hyperliquid:perp:BTC"),
                ("hyperliquid", 5, 2, "hyperliquid:spot:1:BTC/USDC"),
                ("rithmic", 8, 2, "a-current"),
            ]
        );
        let demands = commands
            .iter()
            .filter_map(|payload| match payload {
                envelope::Payload::SeriesDemand(demand) => demand.series.as_ref().map(|series| {
                    (
                        demand.consumer_id,
                        demand.generation,
                        series.provider.as_str(),
                        series.instrument_id.as_str(),
                    )
                }),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            demands,
            vec![
                (11, 1, "hyperliquid", "hyperliquid:perp:BTC"),
                (12, 1, "hyperliquid", "hyperliquid:spot:1:BTC/USDC"),
                (13, 4, "rithmic", "a-current"),
                (14, 9, "rithmic", "a-current"),
            ]
        );
        assert!(!commands.iter().any(|payload| matches!(
            payload,
            envelope::Payload::InstallProviderInstrument(instrument)
                if instrument.provider == "rithmic" && instrument.instrument_id != "a-current"
        )));
        assert!(
            !commands
                .iter()
                .any(|payload| matches!(payload, envelope::Payload::SelectProviderInstrument(_)))
        );
    }

    #[test]
    fn restart_restores_every_consumer_and_resumes_a_covering_snapshot() {
        let token = [23_u8; 32];
        let requested = series();
        let (first_name, _, first_server) = start_fixture(1, true, requested.clone());
        let (second_name, restored, second_server) = start_fixture(2, false, requested.clone());
        let first = EngineClient::connect(&first_name, &token).expect("connect first engine");
        let replacement_name = second_name.clone();
        let mut supervisor = EngineSupervisor {
            executable: PathBuf::from("unused-test-engine"),
            client_id: 9,
            client: first,
            consumers: BTreeMap::default(),
            instruments: BTreeMap::default(),
            pending_instruments: BTreeSet::default(),
            pending_events: VecDeque::default(),
            reconnect_fixture: Some(Box::new(move || {
                EngineClient::connect(&replacement_name, &token)
            })),
        };
        supervisor
            .client
            .attach_client(9)
            .expect("attach first client");
        configure_restore_state(&mut supervisor, &requested);

        let recovered = supervisor
            .receive_market_event_for(11, Duration::from_secs(1))
            .expect("recover engine");
        assert!(recovered.reconnected);
        assert_eq!(supervisor.client.ready().engine_epoch, 2);
        let resumed = supervisor
            .receive_market_event_for(11, Duration::from_secs(1))
            .expect("poll restored engine");
        assert!(matches!(
            resumed.event,
            Some(envelope::Payload::SeriesSnapshot(snapshot))
                if snapshot.consumer_id == 11 && snapshot.generation == 7
        ));
        first_server.join().expect("join first fixture");
        second_server.join().expect("join second fixture");

        let commands = restored
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            commands
                .iter()
                .filter(|payload| matches!(payload, envelope::Payload::RegisterConsumer(_)))
                .count(),
            2
        );
        assert_eq!(
            commands
                .iter()
                .filter(|payload| matches!(payload, envelope::Payload::SeriesDemand(_)))
                .count(),
            2
        );
        assert_eq!(
            commands
                .iter()
                .filter(|payload| matches!(payload, envelope::Payload::ViewportDemand(_)))
                .count(),
            2
        );
        assert_eq!(
            commands
                .iter()
                .filter(|payload| matches!(payload, envelope::Payload::VisibilityDemand(_)))
                .count(),
            2
        );
        assert!(
            commands
                .iter()
                .any(|payload| matches!(payload, envelope::Payload::SearchProviderInstruments(_)))
        );
        assert!(
            commands
                .iter()
                .any(|payload| matches!(payload, envelope::Payload::SelectProviderInstrument(_)))
        );
        assert!(
            commands
                .iter()
                .any(|payload| matches!(payload, envelope::Payload::InstallProviderInstrument(_)))
        );
    }

    #[test]
    fn reconnect_restores_only_current_generation_ordered_provider_state() {
        let token = [29_u8; 32];
        let requested = series();
        let (first_name, _, first_server) = start_fixture(1, true, requested.clone());
        let (second_name, restored, second_server) = start_fixture(2, false, requested.clone());
        let first = EngineClient::connect(&first_name, &token).expect("connect first engine");
        let reconnects = Arc::new(AtomicUsize::new(0));
        let reconnect_count = Arc::clone(&reconnects);
        let replacement_name = second_name.clone();
        let mut supervisor = EngineSupervisor {
            executable: PathBuf::from("unused-test-engine"),
            client_id: 9,
            client: first,
            consumers: BTreeMap::default(),
            instruments: BTreeMap::default(),
            pending_instruments: BTreeSet::default(),
            pending_events: VecDeque::default(),
            reconnect_fixture: Some(Box::new(move || {
                reconnect_count.fetch_add(1, Ordering::Relaxed);
                EngineClient::connect(&replacement_name, &token)
            })),
        };
        supervisor
            .client
            .attach_client(9)
            .expect("attach first client");
        configure_generation_ordered_restore(&mut supervisor);

        let recovered = supervisor
            .receive_market_event_for(11, Duration::from_secs(1))
            .expect("recover engine");
        assert!(recovered.reconnected);
        assert_eq!(reconnects.load(Ordering::Relaxed), 1);
        let _ = supervisor
            .receive_market_event_for(11, Duration::from_secs(1))
            .expect("poll restored current demand");
        first_server.join().expect("join first fixture");
        second_server.join().expect("join second fixture");

        let commands = restored
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_generation_ordered_restore(&commands);
    }
}
