use super::{
    Arc, AtomicBool, BTreeMap, BTreeSet, BarSeriesKey, COMMAND_CAPACITY, ClientId, Command,
    ConsumerId, ConsumerIdentity, ConsumerResourceClass, DisplayDepthRequest, Duration,
    GenerationId, HISTORY_CAPACITY, HistoryRequest, InstallProviderInstrument, Instant,
    InstrumentStreamKey, LeaseOwner, MarketDataLeaseId, MarketEngine, MarketRuntime, MarketService,
    MarketServiceError, MarketServiceProviders, MarketServiceStatus, Mutex,
    NativeStudyRegistration, Ordering, OwnedCoordinatorChannels, ProviderCatalogCommand,
    ProviderCoordinatorWake, ProviderDispatch, ProviderDispatchRecord, ProviderEventLane,
    ProviderRuntimeEvent, ProviderRuntimeLifecycle, ProviderRuntimeRecord, ProviderRuntimeRegistry,
    ProviderRuntimeSpec, ProviderStartHandle, QuoteSubscription, Reply, ScanProviderMarkets,
    ScreenProviderMarkets, SearchProviderInstruments, SelectProviderInstrument,
    StartedProviderRuntime, StreamRequirements, StudyInstanceId, SyncSender, TrySendError,
    Viewport, WorkspaceId, id, mpsc, spawn_coordinator, spawn_history_worker, thread,
    validate_provider_instrument, validate_provider_scan, validate_provider_screen,
    validate_provider_search, validate_provider_selection,
};
use crate::ProviderPresentationDescriptor;
use aeris_trading::market::{MarketFeedError, QuoteInterest, QuoteInterestSink};

struct RuntimeChannels {
    releases: Arc<Mutex<Vec<MarketDataLeaseId>>>,
    latest: Arc<Mutex<super::quotes::QuoteBook>>,
    trading_feed: Arc<super::trading_feed::TradingFeedOutbox>,
    outbox: Arc<super::outbox::ConsumerOutbox>,
    recording: Arc<super::recording::MarketRecording>,
}

struct MarketQuoteInterestSink(MarketService);

impl QuoteInterestSink for MarketQuoteInterestSink {
    fn set_interest(&self, interest: QuoteInterest) -> Result<(), MarketFeedError> {
        interest.validate()?;
        self.0
            .request(|reply| Ok(Command::SetTradingInterest(interest, reply)))
            .map_err(|_| MarketFeedError::Unavailable)
    }
}

impl ProviderRuntimeRegistry {
    pub(super) fn start(
        specs: Vec<ProviderRuntimeSpec>,
        completions: &SyncSender<Command>,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
        context: &super::ProviderStartContext,
    ) -> Result<Self, String> {
        let wake = ProviderCoordinatorWake::new(
            completions.clone(),
            specs.iter().map(|spec| spec.descriptor.id),
        );
        let mut registry = Self {
            wake: wake.clone(),
            records: BTreeMap::new(),
            order: Vec::new(),
        };
        for spec in specs {
            if registry.records.contains_key(spec.descriptor.id) {
                registry.cancel_and_join();
                return Err(format!(
                    "provider runtime {} is configured more than once",
                    spec.descriptor.id
                ));
            }
            let provider_id = spec.descriptor.id;
            let record = match Self::start_record(
                spec,
                completions,
                wake.clone(),
                engine,
                active_workers,
                context,
            ) {
                Ok(record) => record,
                Err(error) => {
                    registry.cancel_and_join();
                    return Err(error);
                }
            };
            registry.records.insert(provider_id, record);
            registry.order.push(provider_id);
        }
        Ok(registry)
    }

    fn start_record(
        mut spec: ProviderRuntimeSpec,
        completions: &SyncSender<Command>,
        wake: ProviderCoordinatorWake,
        engine: &MarketEngine,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
        context: &super::ProviderStartContext,
    ) -> Result<ProviderRuntimeRecord, String> {
        let handle = ProviderStartHandle {
            completions,
            wake,
            engine,
            active_workers,
            context,
        };
        let starter = std::mem::replace(
            &mut spec.starter,
            Box::new(|_, _| Err("provider runtime starter already consumed".to_string())),
        );
        starter(spec, &handle)
    }

    pub(super) fn start_history_runtime(
        spec: ProviderRuntimeSpec,
        completions: &SyncSender<Command>,
        active_workers: &Arc<Mutex<BTreeSet<String>>>,
    ) -> Result<StartedProviderRuntime, String> {
        let cancellation = Arc::new(AtomicBool::new(false));
        let lifecycle = Arc::new(ProviderRuntimeLifecycle::default());
        let (history_tx, history_rx) = mpsc::sync_channel(HISTORY_CAPACITY);
        let history_worker = spawn_history_worker(
            spec.descriptor.capabilities.history.worker,
            spec.history
                .ok_or_else(|| "Provider history source is unavailable".to_string())?,
            history_rx,
            completions.clone(),
            Arc::clone(&cancellation),
            Arc::clone(active_workers),
        )?;
        Ok(StartedProviderRuntime {
            history: history_tx,
            cancellation,
            lifecycle,
            workers: vec![history_worker],
        })
    }

    pub(super) fn dispatch(&self) -> ProviderDispatch<'_> {
        let records: BTreeMap<_, _> = self
            .records
            .iter()
            .map(|(provider_id, record)| {
                let catalog = record
                    .catalog
                    .enabled
                    .then_some(record.catalog.channels.as_ref());
                (
                    *provider_id,
                    ProviderDispatchRecord {
                        descriptor: record.descriptor,
                        history: &record.history,
                        lifecycle: Some(&record.lifecycle),
                        realtime: record.realtime.as_ref(),
                        display_depth: record.display_depth.as_ref(),
                        catalog,
                    },
                )
            })
            .collect();
        let mut lanes = Vec::new();
        for provider_id in &self.order {
            let Some(record) = records.get(provider_id) else {
                continue;
            };
            if record.realtime.is_some() {
                lanes.push(ProviderEventLane::Realtime(provider_id));
            }
            if record.catalog.is_some() {
                lanes.push(ProviderEventLane::Catalog(provider_id));
            }
            if record.display_depth.is_some() {
                lanes.push(ProviderEventLane::DisplayDepth(provider_id));
            }
        }
        ProviderDispatch {
            records,
            wake: Some(&self.wake),
            lanes,
        }
    }

    pub(super) fn cancel_and_join(&mut self) -> Vec<String> {
        for record in self.records.values() {
            record.cancellation.store(true, Ordering::Release);
        }
        let records = std::mem::take(&mut self.records);
        let mut panicked = Vec::new();
        for (_, record) in records {
            let ProviderRuntimeRecord {
                history,
                realtime,
                display_depth,
                catalog,
                workers,
                ..
            } = record;
            drop(realtime);
            drop(display_depth);
            drop(history);
            drop(catalog);
            for worker in workers {
                let name = worker.thread().name().unwrap_or("unnamed").to_string();
                if worker.join().is_err() {
                    panicked.push(name);
                }
            }
        }
        panicked
    }
}

impl Drop for ProviderRuntimeRegistry {
    fn drop(&mut self) {
        let _ = self.cancel_and_join();
    }
}

pub(super) fn join_runtime_workers(workers: Vec<thread::JoinHandle<()>>) {
    for worker in workers {
        let _ = worker.join();
    }
}

impl ProviderDispatch<'_> {
    pub(super) fn descriptor(&self, provider_id: &str) -> Option<super::ProviderDescriptor> {
        let registered = self
            .records
            .get(provider_id)
            .map(|record| record.descriptor);
        #[cfg(test)]
        {
            registered.or_else(|| {
                super::BUILT_IN_PROVIDER_DESCRIPTORS
                    .iter()
                    .copied()
                    .find(|descriptor| descriptor.id == provider_id)
            })
        }
        #[cfg(not(test))]
        {
            registered
        }
    }

    pub(super) fn live_model(&self, provider_id: &str) -> Option<super::LiveModel> {
        self.descriptor(provider_id)
            .map(|descriptor| descriptor.capabilities.live_model)
    }

    pub(super) fn validate_period(
        &self,
        provider_id: &str,
        period: super::BarPeriod,
    ) -> Result<(), String> {
        let descriptor = self
            .descriptor(provider_id)
            .ok_or_else(|| "market provider is unsupported".to_string())?;
        (descriptor.capabilities.history.supported_period)(period)
    }

    pub(super) fn history(
        &self,
        provider_id: &str,
    ) -> Result<&SyncSender<HistoryRequest>, &'static str> {
        self.records
            .get(provider_id)
            .map(|record| record.history)
            .ok_or("provider history runtime is unavailable")
    }

    pub(super) fn detail(&self, provider_id: &str) -> Option<String> {
        self.records
            .get(provider_id)
            .and_then(|record| record.lifecycle)
            .and_then(ProviderRuntimeLifecycle::detail)
    }

    pub(super) fn realtime_enabled(&self, provider_id: &str) -> bool {
        self.records
            .get(provider_id)
            .is_some_and(|record| record.realtime.is_some())
    }

    pub(super) fn observe_generation(
        &self,
        provider_id: &str,
        generation: u64,
        reconnecting: bool,
    ) {
        if let Some(lifecycle) = self
            .records
            .get(provider_id)
            .and_then(|record| record.lifecycle)
        {
            lifecycle.observe_generation(generation, reconnecting);
        }
    }

    pub(super) fn transport_rtt_nanos(&self, provider_id: &str) -> Option<u64> {
        self.records
            .get(provider_id)
            .and_then(|record| record.lifecycle)
            .and_then(ProviderRuntimeLifecycle::transport_rtt_nanos)
    }

    fn observe_transport_rtt(&self, provider_id: &str, generation: u64, transport_rtt_nanos: u64) {
        if let Some(lifecycle) = self
            .records
            .get(provider_id)
            .and_then(|record| record.lifecycle)
        {
            lifecycle.observe_transport_rtt(generation, transport_rtt_nanos);
        }
    }

    pub(super) fn take_event(&self, lane: usize) -> Option<ProviderRuntimeEvent> {
        let event = match *self.lanes.get(lane)? {
            ProviderEventLane::Realtime(provider_id) => self
                .records
                .get(provider_id)?
                .realtime?
                .take_event()
                .map(ProviderRuntimeEvent::BoundRealtime),
            ProviderEventLane::Catalog(provider_id) => self
                .records
                .get(provider_id)?
                .catalog?
                .try_recv()
                .map(|event| ProviderRuntimeEvent::Catalog(provider_id, event)),
            ProviderEventLane::DisplayDepth(provider_id) => self
                .records
                .get(provider_id)?
                .display_depth?
                .take_event()
                .map(|event| ProviderRuntimeEvent::DisplayDepth(provider_id, event)),
        }?;
        self.observe_event(&event);
        Some(event)
    }

    fn observe_event(&self, event: &ProviderRuntimeEvent) {
        let Some(metadata) = event.metadata() else {
            return;
        };
        // A retired queued event cannot overwrite out-of-band recovery health.
        if self
            .wake
            .is_some_and(|wake| wake.overflowed(metadata.provider, metadata.generation))
        {
            return;
        }
        self.observe_generation(
            metadata.provider,
            metadata.generation,
            metadata.reconnecting,
        );
        if let Some(rtt) = metadata.transport_rtt_nanos {
            self.observe_transport_rtt(metadata.provider, metadata.generation, rtt);
        }
    }

    pub(super) fn dispatch_catalog(
        &self,
        provider_id: &str,
        command: ProviderCatalogCommand,
    ) -> Result<(), String> {
        let record = self
            .records
            .get(provider_id)
            .ok_or_else(|| format!("{provider_id} catalog worker is unavailable"))?;
        record
            .catalog
            .ok_or_else(|| format!("{provider_id} catalog worker is unavailable"))?
            .send(provider_id, command)
    }

    pub(super) fn send_display_depth(
        &self,
        provider_id: &str,
        requests: &[DisplayDepthRequest],
    ) -> Result<bool, String> {
        let Some(display_depth) = self
            .records
            .get(provider_id)
            .ok_or_else(|| "Provider display-depth worker is unavailable".to_string())?
            .display_depth
        else {
            return Ok(true);
        };
        display_depth.send_demand(requests)
    }
}

impl MarketService {
    /// Returns the runtime-owned dated session projection for one installed instrument.
    #[must_use]
    pub fn market_session_status(
        &self,
        instrument: &aeris_contracts::InstallProviderInstrument,
        now_unix_nanos: i64,
    ) -> crate::MarketSessionStatus {
        Self::calendar_for(instrument).status(
            &instrument.instrument_id,
            now_unix_nanos.div_euclid(1_000_000),
        )
    }

    /// Trading days of one installed instrument's recurring calendar overlapping
    /// `[from_unix_nanos, to_unix_nanos)`, oldest first and at most the newest `limit`.
    /// A pure projection: dated provider calendar updates the coordinator holds are not
    /// part of it, and an instrument without a known calendar has no sessions.
    #[must_use]
    pub fn trading_sessions(
        instrument: &aeris_contracts::InstallProviderInstrument,
        from_unix_nanos: i64,
        to_unix_nanos: i64,
        limit: usize,
    ) -> Vec<crate::MarketTradingSession> {
        Self::calendar_for(instrument)
            .trading_sessions(
                &instrument.instrument_id,
                from_unix_nanos.div_euclid(1_000_000),
                to_unix_nanos.div_euclid(1_000_000),
                limit,
            )
            .into_iter()
            .map(|window| crate::MarketTradingSession {
                open_unix_nanos: window.open_utc_ms.saturating_mul(1_000_000),
                close_unix_nanos: window.close_utc_ms.saturating_mul(1_000_000),
            })
            .collect()
    }

    fn calendar_for(
        instrument: &aeris_contracts::InstallProviderInstrument,
    ) -> super::calendar::MarketCalendarService {
        use super::calendar::{CalendarSource, MarketCalendarService};
        let mut calendar = MarketCalendarService::default();
        calendar.resolve(instrument);
        if super::is_recording_replay_provider(&instrument.provider) {
            calendar.insert(instrument.instrument_id.clone(), CalendarSource::AlwaysOpen);
        }
        calendar
    }

    /// Returns the requested streams supported by the authoritative provider configuration.
    /// # Errors
    /// Returns coordinator failure.
    pub fn available_streams(
        &self,
        provider: &str,
        requested: StreamRequirements,
    ) -> Result<StreamRequirements, String> {
        self.request(|reply| Ok(Command::AvailableStreams(provider.into(), requested, reply)))
    }

    /// Returns provider-neutral metadata for catalog and chart presentation.
    #[must_use]
    pub fn provider_presentations(&self) -> Vec<ProviderPresentationDescriptor> {
        self.runtime.provider_presentations.clone()
    }

    /// Returns whether `provider` registered an open-interest source.
    #[must_use]
    pub fn supports_open_interest(&self, provider: &str) -> bool {
        self.runtime.open_interest_sources.contains_key(provider)
    }

    /// Fetches historical and current open interest for one installed instrument and chart
    /// interval through its registered provider source, reusing the runtime's bounded TTL cache.
    ///
    /// # Errors
    /// Returns an error when the interval cannot be resolved for the provider or the upstream
    /// fetcher fails. Providers without an open-interest source return an empty list.
    pub fn fetch_open_interest(
        &self,
        instrument: &InstallProviderInstrument,
        interval: aeris_market_data::ChartInterval,
        maximum_points: usize,
        stop: &Arc<AtomicBool>,
    ) -> Result<Vec<aeris_provider_spi::OpenInterestSample>, String> {
        let Some(source) = self
            .runtime
            .open_interest_sources
            .get(instrument.provider.as_str())
            .cloned()
        else {
            return Ok(Vec::new());
        };
        if stop.load(Ordering::Acquire) || self.runtime.shutdown.load(Ordering::Acquire) {
            return Err("open interest request cancelled".to_string());
        }
        let presentation = self
            .runtime
            .provider_presentations
            .iter()
            .find(|descriptor| descriptor.id == instrument.provider)
            .ok_or_else(|| format!("unknown provider {}", instrument.provider))?;
        let series_key = crate::resolve_series_key(instrument, interval, presentation)
            .map_err(|error| error.to_string())?;
        let cache_key = super::OpenInterestCacheKey {
            provider: instrument.provider.clone(),
            instrument_id: instrument.instrument_id.clone(),
            period: series_key.period,
        };
        let now = Instant::now();
        if let Some(cached) = self
            .runtime
            .open_interest_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&cache_key, now)
        {
            return Ok(cached);
        }
        let request = aeris_provider_spi::OpenInterestRequest {
            instrument: super::history_instrument_from_install(instrument),
            period: series_key.period,
            maximum_points: maximum_points
                .clamp(1, aeris_provider_spi::MAXIMUM_OPEN_INTEREST_POINTS),
        };
        let raw = source(&request, stop)?;
        if stop.load(Ordering::Acquire) || self.runtime.shutdown.load(Ordering::Acquire) {
            return Err("open interest request cancelled".to_string());
        }
        let mut by_timestamp = BTreeMap::new();
        for sample in raw {
            if sample.contracts.is_finite() && sample.contracts >= 0.0 {
                let high_contracts = sample
                    .high_contracts
                    .filter(|value| value.is_finite() && *value >= 0.0);
                let low_contracts = sample
                    .low_contracts
                    .filter(|value| value.is_finite() && *value >= 0.0);
                let notional_usd = sample
                    .notional_usd
                    .filter(|value| value.is_finite() && *value >= 0.0);
                by_timestamp.insert(
                    sample.timestamp_unix_nanos,
                    aeris_provider_spi::OpenInterestSample {
                        timestamp_unix_nanos: sample.timestamp_unix_nanos,
                        contracts: sample.contracts,
                        high_contracts,
                        low_contracts,
                        notional_usd,
                    },
                );
            }
        }
        let samples = by_timestamp.into_values().collect::<Vec<_>>();
        self.runtime
            .open_interest_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(cache_key, Instant::now(), samples.clone());
        Ok(samples)
    }

    /// Returns whether `provider` registered a liquidation source.
    #[must_use]
    pub fn supports_liquidations(&self, provider: &str) -> bool {
        self.runtime.liquidation_sources.contains_key(provider)
    }

    /// Fetches bar-aligned liquidation volumes for one installed instrument and chart interval
    /// through its registered provider source, reusing the runtime's bounded short-TTL cache.
    ///
    /// # Errors
    /// Returns an error when the interval cannot be resolved for the provider or the upstream
    /// fetcher fails. Providers without a liquidation source return an empty list.
    pub fn fetch_liquidations(
        &self,
        instrument: &InstallProviderInstrument,
        interval: aeris_market_data::ChartInterval,
        maximum_points: usize,
        stop: &Arc<AtomicBool>,
    ) -> Result<Vec<aeris_provider_spi::LiquidationSample>, String> {
        let Some(source) = self
            .runtime
            .liquidation_sources
            .get(instrument.provider.as_str())
            .cloned()
        else {
            return Ok(Vec::new());
        };
        if stop.load(Ordering::Acquire) || self.runtime.shutdown.load(Ordering::Acquire) {
            return Err("liquidation request cancelled".to_string());
        }
        let presentation = self
            .runtime
            .provider_presentations
            .iter()
            .find(|descriptor| descriptor.id == instrument.provider)
            .ok_or_else(|| format!("unknown provider {}", instrument.provider))?;
        let series_key = crate::resolve_series_key(instrument, interval, presentation)
            .map_err(|error| error.to_string())?;
        let cache_key = super::OpenInterestCacheKey {
            provider: instrument.provider.clone(),
            instrument_id: instrument.instrument_id.clone(),
            period: series_key.period,
        };
        let now = Instant::now();
        if let Some(cached) = self
            .runtime
            .liquidation_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&cache_key, now)
        {
            return Ok(cached);
        }
        let request = aeris_provider_spi::LiquidationRequest {
            instrument: super::history_instrument_from_install(instrument),
            period: series_key.period,
            maximum_points: maximum_points.clamp(1, aeris_provider_spi::MAXIMUM_LIQUIDATION_POINTS),
        };
        let raw = source(&request, stop)?;
        if stop.load(Ordering::Acquire) || self.runtime.shutdown.load(Ordering::Acquire) {
            return Err("liquidation request cancelled".to_string());
        }
        let mut by_timestamp = BTreeMap::new();
        for sample in raw {
            if sample.long_usd.is_finite()
                && sample.long_usd >= 0.0
                && sample.short_usd.is_finite()
                && sample.short_usd >= 0.0
                && sample.long_contracts.is_finite()
                && sample.long_contracts >= 0.0
                && sample.short_contracts.is_finite()
                && sample.short_contracts >= 0.0
            {
                by_timestamp.insert(sample.timestamp_unix_nanos, sample);
            }
        }
        let samples = by_timestamp.into_values().collect::<Vec<_>>();
        self.runtime
            .liquidation_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(cache_key, Instant::now(), samples.clone());
        Ok(samples)
    }
}

/// Composition-root builder for [`MarketService`].
///
/// Allows registering provider plugins, local recording replay, auth adapters, and
/// broker venue runners before starting the single in-process coordinator.
#[derive(Default)]
pub struct MarketServiceBuilder {
    providers: Vec<ProviderRuntimeSpec>,
    auth_adapters: Vec<(
        &'static str,
        Arc<dyn super::provider_auth::ProviderAuthAdapter>,
    )>,
    broker_venues: Vec<(String, super::BrokerVenueRunner)>,
}

impl MarketServiceBuilder {
    /// Registers a provider plugin runtime specification.
    #[must_use]
    pub fn register_provider(mut self, spec: ProviderRuntimeSpec) -> Self {
        self.providers.push(spec);
        self
    }

    /// Adds the local recording-replay provider source.
    #[must_use]
    pub fn with_recording_replay(mut self) -> Self {
        self.providers.push(ProviderRuntimeSpec::recording_replay());
        self
    }

    /// Registers a provider-neutral authentication adapter for `provider_id`.
    #[must_use]
    pub fn register_auth(
        mut self,
        provider_id: &'static str,
        adapter: Arc<dyn super::provider_auth::ProviderAuthAdapter>,
    ) -> Self {
        self.auth_adapters.push((provider_id, adapter));
        self
    }

    /// Registers a pluggable broker venue worker runner for `venue_id`.
    #[must_use]
    pub fn register_broker_venue(
        mut self,
        venue_id: impl Into<String>,
        runner: super::BrokerVenueRunner,
    ) -> Self {
        self.broker_venues.push((venue_id.into(), runner));
        self
    }

    #[cfg(test)]
    pub(super) fn register_provider_spec(self, spec: ProviderRuntimeSpec) -> Self {
        self.register_provider(spec)
    }

    /// Starts the composed [`MarketService`].
    ///
    /// # Errors
    /// Returns [`MarketServiceError`] when the engine, any provider runtime, or a broker venue
    /// registration fails.
    pub fn start(self) -> Result<MarketService, MarketServiceError> {
        MarketService::start_from_builder(self)
    }
}

impl MarketService {
    /// Returns a fresh composition-root builder for configuring and starting a [`MarketService`].
    #[must_use]
    pub fn builder() -> MarketServiceBuilder {
        MarketServiceBuilder::default()
    }

    #[cfg(any(test, feature = "replay"))]
    pub(super) fn start_composed(providers: Vec<ProviderRuntimeSpec>) -> Result<Self, String> {
        Self::start_from_builder(MarketServiceBuilder {
            providers,
            auth_adapters: Vec::new(),
            broker_venues: Vec::new(),
        })
        .map_err(String::from)
    }

    fn start_from_builder(builder: MarketServiceBuilder) -> Result<Self, MarketServiceError> {
        let MarketServiceBuilder {
            providers,
            auth_adapters,
            broker_venues,
        } = builder;
        let provider_presentations = providers
            .iter()
            .map(|provider| *provider.descriptor.presentation)
            .collect();
        let open_interest_sources = providers
            .iter()
            .filter_map(|provider| {
                provider
                    .open_interest
                    .as_ref()
                    .map(|source| (provider.descriptor.id, Arc::clone(source)))
            })
            .collect();
        let liquidation_sources = providers
            .iter()
            .filter_map(|provider| {
                provider
                    .liquidations
                    .as_ref()
                    .map(|source| (provider.descriptor.id, Arc::clone(source)))
            })
            .collect();
        let descriptors = providers
            .iter()
            .map(|provider| provider.descriptor)
            .collect::<Vec<_>>();
        let engine = super::configured_engine_from_descriptors(&descriptors)
            .map_err(MarketServiceError::Detail)?;
        let (command_tx, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let quote_releases = Arc::new(Mutex::new(Vec::new()));
        let quotes = Arc::new(Mutex::new(super::quotes::QuoteBook::default()));
        let trading_feed = Arc::new(super::trading_feed::TradingFeedOutbox::default());
        let outbox = Arc::new(super::outbox::ConsumerOutbox::default());
        let recording = Arc::new(super::recording::MarketRecording::new(Arc::new(
            super::clock::SystemClock,
        )));
        let recording_replays = Arc::new(super::recording::RecordingReplays::new(
            Arc::clone(&recording),
            Arc::new(super::clock::SystemClock),
        ));
        let shutdown = Arc::new(AtomicBool::new(false));
        let active_provider_workers = Arc::new(Mutex::new(BTreeSet::new()));
        let start_context = super::ProviderStartContext { recording_replays };
        let provider_registry = ProviderRuntimeRegistry::start(
            providers,
            &command_tx,
            &engine,
            &active_provider_workers,
            &start_context,
        )
        .map_err(MarketServiceError::Detail)?;
        let workers = vec![
            spawn_coordinator(
                engine,
                OwnedCoordinatorChannels {
                    commands: command_rx,
                    providers: provider_registry,
                    quote_releases: Arc::clone(&quote_releases),
                    quotes: Arc::clone(&quotes),
                    trading_feed: Arc::clone(&trading_feed),
                    outbox: Arc::clone(&outbox),
                    recording: Arc::clone(&recording),
                },
                Arc::clone(&shutdown),
                Arc::new(super::clock::SystemClock),
            )
            .map_err(MarketServiceError::WorkerSpawnFailed)?,
        ];
        let (provider_auth, broker_venue_runners) =
            Self::build_registries(auth_adapters, broker_venues)?;
        let market = Self::build_market_service(
            command_tx,
            shutdown,
            active_provider_workers,
            workers,
            start_context,
            provider_auth,
            broker_venue_runners,
            MarketServiceProviders {
                presentations: provider_presentations,
                open_interest_sources,
                liquidation_sources,
            },
            RuntimeChannels {
                releases: quote_releases,
                latest: quotes,
                trading_feed,
                outbox,
                recording,
            },
        );
        Ok(market)
    }

    fn build_registries(
        auth_adapters: Vec<(
            &'static str,
            Arc<dyn super::provider_auth::ProviderAuthAdapter>,
        )>,
        broker_venues: Vec<(String, super::BrokerVenueRunner)>,
    ) -> Result<
        (
            super::provider_auth::ProviderAuthRegistry,
            super::broker_venue::BrokerVenueRunnerRegistry,
        ),
        MarketServiceError,
    > {
        let mut provider_auth = super::provider_auth::ProviderAuthRegistry::default();
        let mut broker_venue_runners = super::broker_venue::BrokerVenueRunnerRegistry::default();
        for (provider_id, adapter) in auth_adapters {
            provider_auth.register(provider_id, adapter);
        }
        for (venue_id, runner) in broker_venues {
            broker_venue_runners.register(venue_id, runner)?;
        }
        Ok((provider_auth, broker_venue_runners))
    }

    #[allow(clippy::too_many_arguments)]
    fn build_market_service(
        commands: SyncSender<Command>,
        shutdown: Arc<AtomicBool>,
        active_provider_workers: Arc<Mutex<BTreeSet<String>>>,
        workers: Vec<thread::JoinHandle<()>>,
        context: super::ProviderStartContext,
        provider_auth: super::provider_auth::ProviderAuthRegistry,
        broker_venue_runners: super::broker_venue::BrokerVenueRunnerRegistry,
        providers: MarketServiceProviders,
        channels: RuntimeChannels,
    ) -> Self {
        let RuntimeChannels {
            releases: quote_releases,
            latest: quotes,
            trading_feed,
            outbox,
            recording,
        } = channels;
        let super::ProviderStartContext {
            recording_replays, ..
        } = context;
        let shutdown_wake = commands.clone();
        let MarketServiceProviders {
            presentations: provider_presentations,
            open_interest_sources,
            liquidation_sources,
        } = providers;
        Self {
            commands,
            runtime: Arc::new(MarketRuntime {
                shutdown,
                shutdown_wake,
                provider_presentations,
                open_interest_sources,
                open_interest_cache: Mutex::new(super::OpenInterestCache::default()),
                liquidation_sources,
                liquidation_cache: Mutex::new(super::LiquidationCache::default()),
                provider_auth,
                broker_venue_runners: Mutex::new(broker_venue_runners),
                attached_broker_venues: Mutex::new(BTreeSet::new()),
                active_provider_workers,
                workers: Mutex::new(Some(workers)),
                quote_releases,
                quotes,
                trading_feed,
                outbox,
                recording,
                recording_replays,
            }),
        }
    }

    /// Cancels provider work, drains accepted provider events, and joins owned workers.
    ///
    /// # Errors
    /// Returns an error when a worker panics or the complete shutdown exceeds `timeout`.
    pub fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        self.runtime.shutdown.store(true, Ordering::Release);
        self.runtime.trading_feed.wake();
        self.runtime.outbox.notify_attached();
        // A full queue already makes the coordinator runnable.
        let _ = self.commands.try_send(Command::ProviderWake);
        let mut workers = self
            .runtime
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(MarketServiceError::ShutdownInProgress)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(MarketServiceError::ShutdownDeadlineOverflow)?;
        let mut panicked = Vec::new();
        loop {
            let mut index = 0;
            while index < workers.len() {
                if workers[index].is_finished() {
                    let worker = workers.swap_remove(index);
                    let name = worker.thread().name().unwrap_or("unnamed").to_string();
                    if worker.join().is_err() {
                        panicked.push(name);
                    }
                } else {
                    index += 1;
                }
            }
            if workers.is_empty() {
                return if panicked.is_empty() {
                    Ok(())
                } else {
                    Err(MarketServiceError::ShutdownWorkersPanicked(panicked.join(", ")).into())
                };
            }
            let now = Instant::now();
            if now >= deadline {
                let mut pending = workers
                    .iter()
                    .map(|worker| worker.thread().name().unwrap_or("unnamed"))
                    .map(str::to_string)
                    .collect::<BTreeSet<_>>();
                pending.extend(
                    self.runtime
                        .active_provider_workers
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .iter()
                        .cloned(),
                );
                let pending = pending.into_iter().collect::<Vec<_>>().join(", ");
                return Err(MarketServiceError::ShutdownTimedOut(pending).into());
            }
            thread::sleep(Duration::from_millis(5).min(deadline.duration_since(now)));
        }
    }

    /// Attaches a client identity to in-process market state.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn attach(&self, client_id: u64) -> Result<(), String> {
        self.request(|reply| Ok(Command::Attach(id(client_id).map(ClientId)?, reply)))
    }

    /// Detaches a client and all of its consumers.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn detach(&self, client_id: u64) -> Result<(), String> {
        self.request(|reply| Ok(Command::Detach(id(client_id).map(ClientId)?, reply)))
    }

    /// Returns one bounded coordinator-owned lifecycle and memory snapshot.
    ///
    /// # Errors
    /// Returns an error when the coordinator is unavailable.
    pub fn status(&self) -> Result<MarketServiceStatus, String> {
        self.request(|reply| Ok(Command::Status(reply)))
    }

    /// Injects a WebSocket-only failure into the active public feed. HTTP catalog
    /// and history remain untouched. Available only to explicitly built probes.
    ///
    /// # Errors
    /// Returns an error outside validation mode or when the bounded provider
    /// control channel is unavailable.
    #[cfg(feature = "probe")]
    pub fn probe_fail_provider_websocket(
        &self,
        provider_id: &str,
        subsequent_connect_failures: u8,
    ) -> Result<(), String> {
        if !aeris_platform_runtime::data_root::active_data_root()
            .is_ok_and(aeris_platform_runtime::data_root::DataRoot::is_validation)
        {
            return Err("probe WebSocket injection requires an isolated validation root".into());
        }
        if subsequent_connect_failures > 4 {
            return Err("probe reconnect attempt limit exceeded".into());
        }
        self.request(|reply| {
            Ok(Command::ProbeFailWebSocket(
                provider_id.to_string(),
                subsequent_connect_failures,
                reply,
            ))
        })
    }

    /// Registers one market consumer owned by an attached client.
    ///
    /// # Errors
    /// Returns an error for invalid identity, ownership, bounds, or coordinator failure.
    pub fn register_consumer(
        &self,
        client_id: u64,
        workspace_id: u64,
        consumer_id: u64,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Register(
                ConsumerIdentity {
                    client_id: ClientId(id(client_id)?),
                    workspace_id: WorkspaceId(id(workspace_id)?),
                    consumer_id: ConsumerId(id(consumer_id)?),
                },
                reply,
            ))
        })
    }

    /// Removes one market consumer.
    ///
    /// # Errors
    /// Returns an error for zero identity or coordinator failure.
    pub fn remove_consumer(&self, client_id: u64, consumer_id: u64) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Remove(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                reply,
            ))
        })
    }

    /// Applies a generation-fenced viewport to one consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, range, generation, or coordinator failure.
    pub fn set_viewport(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        start_unix_nanos: i64,
        end_unix_nanos: i64,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Viewport(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                Viewport::try_new(start_unix_nanos, end_unix_nanos)
                    .map_err(|error| error.to_string())?,
                reply,
            ))
        })
    }

    /// Updates one consumer's presentation priority.
    ///
    /// # Errors
    /// Returns an error for invalid identity, missing consumer, or coordinator failure.
    pub fn set_visibility(
        &self,
        client_id: u64,
        consumer_id: u64,
        visible: bool,
    ) -> Result<(), String> {
        self.set_resource_class(
            client_id,
            consumer_id,
            if visible {
                ConsumerResourceClass::Foreground
            } else {
                ConsumerResourceClass::Background
            },
        )
    }

    /// Updates one consumer's exact resource class.
    ///
    /// # Errors
    /// Returns an error for invalid identity, missing consumer, or coordinator failure.
    pub fn set_resource_class(
        &self,
        client_id: u64,
        consumer_id: u64,
        resource_class: ConsumerResourceClass,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::ResourceClass(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                resource_class,
                reply,
            ))
        })
    }

    /// Replaces one consumer's exact upstream stream set without changing its
    /// selected market generation or restarting history.
    ///
    /// # Errors
    /// Returns an error for invalid identity, stale selection, unsupported
    /// streams, or coordinator failure.
    pub fn set_streams(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        streams: StreamRequirements,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Streams(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                streams,
                reply,
            ))
        })
    }

    /// Accepts one generation-fenced series demand without waiting for provider I/O.
    ///
    /// # Errors
    /// Returns an error for invalid demand or an unavailable coordinator.
    pub fn set_demand(
        &self,
        client_id: u64,
        consumer_id: u64,
        generation: u64,
        series: &BarSeriesKey,
        streams: StreamRequirements,
    ) -> Result<(), String> {
        self.request(|reply| {
            Ok(Command::Demand(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                GenerationId(id(generation)?),
                series.clone(),
                streams,
                reply,
            ))
        })
    }

    /// Resolves a catalog instrument and installs its series in one coordinator turn.
    /// A rejected selection leaves the previous generation's demand installed.
    ///
    /// # Errors
    /// Returns an error only when the coordinator command cannot be delivered.
    pub fn select_and_demand(
        &self,
        client_id: u64,
        request: super::SelectionRequest,
    ) -> Result<(), String> {
        id(request.consumer_id)?;
        id(request.generation)?;
        self.request(|reply| {
            Ok(Command::SelectAndDemand(
                ClientId(id(client_id)?),
                request,
                reply,
            ))
        })
    }

    /// Registers one native study instance anchored to an owned market consumer.
    ///
    /// Study dependencies may request several canonical series, but those data
    /// requirements are reconciled through runtime-owned engine leases rather
    /// than additional presentation consumers.
    ///
    /// # Errors
    /// Returns an error for invalid identity, ownership, study definition,
    /// provider capability, bounded capacity, or coordinator failure.
    pub fn register_study(
        &self,
        client_id: u64,
        consumer_id: u64,
        registration: NativeStudyRegistration,
    ) -> Result<StudyInstanceId, String> {
        self.request(|reply| {
            Ok(Command::RegisterStudy(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                registration,
                reply,
            ))
        })
    }

    /// Reinitializes one owned study in place after a declared input or setting
    /// change while preserving its stable instance/output identities.
    ///
    /// # Errors
    /// Returns an error for ownership, definition, provider-demand, execution,
    /// or coordinator failure. Runtime state rolls back when preparation fails.
    pub fn reinitialize_study(
        &self,
        client_id: u64,
        study_id: StudyInstanceId,
        registration: NativeStudyRegistration,
    ) -> Result<Vec<StudyInstanceId>, String> {
        self.request(|reply| {
            Ok(Command::ReinitializeStudy(
                ClientId(id(client_id)?),
                study_id,
                registration,
                reply,
            ))
        })
    }

    /// Removes one owned study and every downstream study that depends on it.
    ///
    /// Released market dependencies are removed from the same authoritative
    /// provider subscription set used by charts.
    ///
    /// # Errors
    /// Returns an error for invalid identity, ownership, unknown study, or
    /// coordinator failure.
    pub fn remove_study(
        &self,
        client_id: u64,
        study_id: StudyInstanceId,
    ) -> Result<Vec<StudyInstanceId>, String> {
        self.request(|reply| {
            Ok(Command::RemoveStudy(
                ClientId(id(client_id)?),
                study_id,
                reply,
            ))
        })
    }

    /// Schedules one bounded exact provider-instrument search for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, unsupported input, ownership, or coordinator failure.
    pub fn search_provider_instruments(
        &self,
        client_id: u64,
        search: SearchProviderInstruments,
    ) -> Result<(), String> {
        validate_provider_search(&search)?;
        self.request(|reply| {
            Ok(Command::SearchProviderInstruments(
                ClientId(id(client_id)?),
                search,
                reply,
            ))
        })
    }

    /// Schedules one bounded listed-market statistics screen for an owned consumer. The
    /// result arrives as `MarketRuntimeEvent::ProviderMarketScreen`, fenced to the latest
    /// screen generation of that consumer and provider.
    ///
    /// # Errors
    /// Returns an error for invalid identity, a provider without a market screen, ownership,
    /// or coordinator failure.
    pub fn screen_provider_markets(
        &self,
        client_id: u64,
        screen: ScreenProviderMarkets,
    ) -> Result<(), String> {
        validate_provider_screen(&screen).map_err(|error| error.to_string())?;
        self.request(|reply| {
            Ok(Command::ScreenProviderMarkets(
                ClientId(id(client_id)?),
                screen,
                reply,
            ))
        })
    }

    /// Schedules one bounded market scan across a provider's most traded listed markets for
    /// an owned consumer. Progress and the final answer arrive as
    /// `MarketRuntimeEvent::ProviderMarketScan`, each replacing the previous one and fenced to
    /// the latest scan generation of that consumer and provider; a newer scan supersedes the
    /// running pass.
    ///
    /// # Errors
    /// Returns an error for invalid identity or definition, out-of-range bounds, a provider
    /// without a market scan, ownership, or coordinator failure.
    pub fn scan_provider_markets(
        &self,
        client_id: u64,
        scan: ScanProviderMarkets,
    ) -> Result<(), String> {
        validate_provider_scan(&scan).map_err(|error| error.to_string())?;
        self.request(|reply| {
            Ok(Command::ScanProviderMarkets(
                ClientId(id(client_id)?),
                scan,
                reply,
            ))
        })
    }

    /// Schedules one exact provider-instrument selection for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid identity, unsupported input, ownership, or coordinator failure.
    pub fn select_provider_instrument(
        &self,
        client_id: u64,
        selection: SelectProviderInstrument,
    ) -> Result<(), String> {
        validate_provider_selection(&selection)?;
        self.request(|reply| {
            Ok(Command::SelectProviderInstrument(
                ClientId(id(client_id)?),
                selection,
                reply,
            ))
        })
    }

    /// Installs one bounded adapter-resolved instrument in the runtime catalog.
    ///
    /// # Errors
    /// Returns an error for invalid identity, stale generations, capacity, or coordinator failure.
    pub fn install_provider_instrument(
        &self,
        instrument: &InstallProviderInstrument,
    ) -> Result<(), String> {
        validate_provider_instrument(instrument)?;
        self.request(|reply| {
            Ok(Command::InstallProviderInstrument(
                instrument.clone(),
                reply,
            ))
        })
    }

    /// Transactionally replaces one consumer's bounded price-alert set.
    ///
    /// Alert conditions are evaluated by the coordinator against accepted
    /// trade prices. Provider demand is shared with ordinary chart demand.
    ///
    /// # Errors
    /// Returns an error for invalid ownership, instrument metadata, capacity,
    /// or an unavailable coordinator.
    pub fn replace_price_alerts(
        &self,
        client_id: u64,
        consumer_id: u64,
        alerts: &[crate::MarketPriceAlert],
    ) -> Result<(), String> {
        for alert in alerts {
            validate_provider_instrument(&alert.instrument)?;
        }
        self.request(|reply| {
            Ok(Command::ReplacePriceAlerts(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                alerts.to_vec(),
                reply,
            ))
        })
    }

    /// Acquires a reference-counted quote interest without chart demand.
    /// Dropping the returned guard queues a bounded nonblocking release.
    ///
    /// # Errors
    /// Returns a provider, identity, capacity, or coordinator error.
    pub fn subscribe_quotes(
        &self,
        owner: LeaseOwner,
        key: InstrumentStreamKey,
    ) -> Result<QuoteSubscription, String> {
        let lease_id = self.request(|reply| Ok(Command::AcquireQuote(key.clone(), reply)))?;
        Ok(QuoteSubscription {
            lease_id,
            owner,
            key,
            runtime: Arc::clone(&self.runtime),
        })
    }

    /// Publishes trading-owned quote interest into the single engine lease
    /// registry. Revisions are checked on the coordinator before mutation.
    #[must_use]
    pub fn trading_quote_interest_sink(&self) -> Arc<dyn QuoteInterestSink> {
        Arc::new(MarketQuoteInterestSink(self.clone()))
    }

    /// Attaches the trading owner's feed on a bounded background worker.
    /// Events are latest-wins per instrument and kind while the owner is busy.
    ///
    /// # Errors
    /// Returns an error when the feed was already attached or the worker cannot start.
    pub fn attach_trading_feed(&self, link: super::TradingFeedLink) -> Result<(), String> {
        let mut workers = self
            .runtime
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let workers = workers
            .as_mut()
            .ok_or_else(|| "market engine shutdown is in progress".to_string())?;
        if self.runtime.trading_feed.attached() {
            return Err("trading feed is already attached".into());
        }
        let inbox = Arc::clone(&self.runtime.trading_feed);
        let shutdown = Arc::clone(&self.runtime.shutdown);
        let worker = thread::Builder::new()
            .name("aeris-trading-feed".into())
            .spawn(move || {
                while !shutdown.load(Ordering::Acquire) {
                    if let Some(event) = inbox.receive_until_shutdown(&shutdown)
                        && !shutdown.load(Ordering::Acquire)
                    {
                        (link.events)(event);
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        workers.push(worker);
        self.runtime.trading_feed.attach();
        // If the coordinator is already busy, its queued work will discover
        // the attachment at the start of the next turn.
        let _ = self.commands.try_send(Command::ProviderWake);
        Ok(())
    }

    /// Drains at most one bounded market publication for an owned consumer.
    ///
    /// # Errors
    /// Returns an error for invalid ownership or coordinator failure.
    pub fn poll_event(
        &self,
        client_id: u64,
        consumer_id: u64,
    ) -> Result<Option<crate::MarketConsumerEvent>, String> {
        self.request(|reply| {
            Ok(Command::Drain(
                ClientId(id(client_id)?),
                ConsumerId(id(consumer_id)?),
                reply,
            ))
        })
    }

    /// Drains one bounded fair batch directly from this client's canonical
    /// consumer outboxes. No intermediate desktop event queue is created.
    ///
    /// # Errors
    /// Returns an error for an unattached client or coordinator failure.
    pub fn drain_events(
        &self,
        client_id: u64,
        consumer_budgets: &[(u64, usize)],
    ) -> Result<Vec<(u64, crate::MarketConsumerEvent)>, String> {
        let consumer_budgets = consumer_budgets
            .iter()
            .map(|(consumer_id, maximum)| Ok((ConsumerId(id(*consumer_id)?), *maximum)))
            .collect::<Result<Vec<_>, String>>()?;
        self.request(|reply| {
            Ok(Command::DrainClient(
                ClientId(id(client_id)?),
                consumer_budgets,
                reply,
            ))
        })
    }

    /// Waits for a coordinator-owned consumer publication or a bridge command.
    /// Only background bridge workers may call this; no GPUI thread waits.
    #[must_use]
    pub fn wait_for_events(&self, client_id: u64, timeout: Duration) -> bool {
        id(client_id).is_ok_and(|client| self.runtime.outbox.wait(ClientId(client), timeout))
    }

    /// Signals the background bridge when its ordered command queue changes.
    pub fn wake_bridge(&self, client_id: u64) {
        if let Ok(client) = id(client_id) {
            self.runtime.outbox.notify(ClientId(client));
        }
    }

    /// Wakes attached bridge workers after a UI command enters an ordered queue.
    /// Attachment is bounded by the engine's maximum consumer count.
    pub fn wake_bridges(&self) {
        self.runtime.outbox.notify_attached();
    }

    pub(super) fn request<T>(
        &self,
        build: impl FnOnce(Reply<T>) -> Result<Command, String>,
    ) -> Result<T, String> {
        self.request_with_timeout(build, Duration::from_secs(5))
    }

    fn request_with_timeout<T>(
        &self,
        build: impl FnOnce(Reply<T>) -> Result<Command, String>,
        timeout: Duration,
    ) -> Result<T, String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let state = Arc::new(super::RequestState(std::sync::atomic::AtomicU8::new(0)));
        let command = Command::Request(Box::new(build(reply_tx)?), Arc::clone(&state));
        let deadline = Instant::now() + timeout;
        let mut command = command;
        loop {
            match self.commands.try_send(command) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => {
                    return Err(MarketServiceError::CoordinatorUnavailable.into());
                }
                Err(TrySendError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        return Err(MarketServiceError::CoordinatorBusy.into());
                    }
                    command = returned;
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
        match reply_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(MarketServiceError::CoordinatorStopped.into())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if state.cancel() {
                    Err(MarketServiceError::DeadlineCancelled.into())
                } else {
                    // Never automatically retry a mutation whose execution has started.
                    Err(MarketServiceError::DeadlineUnknown.into())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        HistorySnapshot, HistorySource, ProviderCatalogChannel, ProviderCatalogChannelImpl,
        providers::{
            hyperliquid::dispatch_catalog as dispatch_hyperliquid_catalog,
            rithmic::dispatch_catalog as dispatch_rithmic_catalog,
        },
    };
    use super::*;
    use crate::hyperliquid_realtime::HyperliquidCatalogControl;
    use crate::rithmic_realtime::RithmicCatalogControl;
    use crate::study::{
        NativeStudyProgram, NativeStudyRegistration, StudyDefinition, StudyDependency,
        StudyExecutionContext, StudyInvalidationPolicy, StudyMarketInput, StudyOutputSpec,
        StudyPaneTarget, StudyPlotKind, StudyPointStyle, StudyScaleTarget, StudySettings,
    };
    use aeris_market_data::{BarPeriod, MarketBar};

    #[test]
    fn catalog_channel_dispatches_and_converts_without_provider_enum() {
        let (control_tx, control_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (event_tx, event_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let channel = super::super::ProviderCatalogChannelImpl::new(
            control_tx,
            event_rx,
            dispatch_rithmic_catalog,
        );
        channel
            .send(
                "rithmic",
                ProviderCatalogCommand::Search(SearchProviderInstruments::default()),
            )
            .expect("bounded catalog request");
        assert!(matches!(
            control_rx.try_recv(),
            Ok(RithmicCatalogControl::Search(()))
        ));
        event_tx
            .send(
                crate::rithmic_realtime::RithmicCatalogEvent::SearchCompleted(
                    crate::ProviderInstrumentSearchResult::default(),
                ),
            )
            .expect("bounded catalog response");
        assert!(matches!(
            channel.try_recv(),
            Some(super::super::ProviderCatalogEvent::SearchCompleted(_))
        ));
    }

    #[test]
    fn catalog_channel_preserves_provider_screen_and_capacity_rules() {
        let (rithmic_tx, rithmic_rx) = mpsc::sync_channel(1);
        let (_, rithmic_events) =
            mpsc::sync_channel::<crate::rithmic_realtime::RithmicCatalogEvent>(1);
        let rithmic =
            ProviderCatalogChannelImpl::new(rithmic_tx, rithmic_events, dispatch_rithmic_catalog);
        assert!(
            rithmic
                .send(
                    "rithmic",
                    ProviderCatalogCommand::Screen(ScreenProviderMarkets::default())
                )
                .is_err()
        );
        assert!(
            rithmic
                .send(
                    "rithmic",
                    ProviderCatalogCommand::Scan(ScanProviderMarkets::default())
                )
                .is_err()
        );
        rithmic
            .send(
                "rithmic",
                ProviderCatalogCommand::Search(SearchProviderInstruments::default()),
            )
            .expect("first control fits");
        assert!(
            rithmic
                .send(
                    "rithmic",
                    ProviderCatalogCommand::Select(SelectProviderInstrument::default())
                )
                .is_err(),
            "a full bounded catalog channel refuses another control"
        );
        assert!(matches!(
            rithmic_rx.try_recv(),
            Ok(RithmicCatalogControl::Search(()))
        ));

        let (hyperliquid_tx, hyperliquid_rx) = mpsc::sync_channel(1);
        let (_, hyperliquid_events) =
            mpsc::sync_channel::<crate::hyperliquid_realtime::HyperliquidCatalogEvent>(1);
        let hyperliquid = ProviderCatalogChannelImpl::new(
            hyperliquid_tx,
            hyperliquid_events,
            dispatch_hyperliquid_catalog,
        );
        hyperliquid
            .send(
                "hyperliquid",
                ProviderCatalogCommand::Screen(ScreenProviderMarkets::default()),
            )
            .expect("public provider supports market screens");
        assert!(matches!(
            hyperliquid_rx.try_recv(),
            Ok(HyperliquidCatalogControl::Screen(()))
        ));
        let mut unoffered = ScanProviderMarkets::default();
        unoffered.definition.timeframe = aeris_market_data::ChartInterval::Second5;
        assert!(
            hyperliquid
                .send("hyperliquid", ProviderCatalogCommand::Scan(unoffered))
                .is_err(),
            "an interval the provider does not offer is refused before dispatch"
        );
        let mut weekly = ScanProviderMarkets::default();
        weekly.definition.timeframe = aeris_market_data::ChartInterval::Week1;
        hyperliquid
            .send("hyperliquid", ProviderCatalogCommand::Scan(weekly))
            .expect("public provider supports market scans");
        assert!(matches!(
            hyperliquid_rx.try_recv(),
            Ok(HyperliquidCatalogControl::Scan {
                period: aeris_market_data::BarPeriod::Week { weeks: 1 },
                ..
            })
        ));
    }

    struct RecordingStudyHistory {
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl HistorySource for RecordingStudyHistory {
        fn fetch(&mut self, request: &HistoryRequest) -> Result<HistorySnapshot, String> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request.series.instrument_id.clone());
            Ok(HistorySnapshot {
                price_scale: 2,
                quantity_scale: 0,
                bars: vec![
                    MarketBar {
                        source_sequence: 1,
                        exchange_timestamp_seconds: 1_700_000_000,
                        exchange_timestamp_unix_nanos: 1_700_000_000_000_000_000,
                        open: 10_000,
                        high: 10_200,
                        low: 9_900,
                        close: 10_100,
                        volume: 100,
                    },
                    MarketBar {
                        source_sequence: 2,
                        exchange_timestamp_seconds: 1_700_000_060,
                        exchange_timestamp_unix_nanos: 1_700_000_060_000_000_000,
                        open: 10_100,
                        high: 10_300,
                        low: 10_000,
                        close: 10_200,
                        volume: 120,
                    },
                ],
                forming: None,
                handoff_boundary_unix_nanos: Some(1_700_000_060_000_000_000),
                backwards_exhausted: false,
            })
        }
    }

    fn study_instrument() -> InstallProviderInstrument {
        InstallProviderInstrument {
            provider: "rithmic".to_string(),
            session_generation: 1,
            selection_generation: 1,
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            provider_symbol: "MNQU6".to_string(),
            display_symbol: "MNQ Sep 2026".to_string(),
            venue_id: "CME".to_string(),
            price_scale: 2,
            quantity_scale: 0,
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            price_increment: Some(25),
            ..Default::default()
        }
    }

    fn study_series() -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "rithmic".to_string(),
            instrument_id: "instrument:rithmic:CME:MNQU6".to_string(),
            entitlement_id: "rithmic-test:CME:MNQU6".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        }
    }

    fn study_definition(series: BarSeriesKey) -> StudyDefinition {
        StudyDefinition {
            identifier: "runtime.history.study".to_string(),
            dependencies: vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams: StreamRequirements::BARS,
            })],
            settings: Vec::new(),
            outputs: vec![StudyOutputSpec {
                identifier: "value".to_string(),
                title: "Runtime History Study".to_string(),
                legend_label: None,
                plot: StudyPlotKind::Line,
                pane: StudyPaneTarget::Price,
                scale: StudyScaleTarget::Primary,
                threshold_region: None,
                point_style: StudyPointStyle::default(),
            }],
            invalidation: StudyInvalidationPolicy::SameRange,
        }
    }

    fn calculate_test_study(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let output = context
            .output(0)
            .ok_or_else(|| "test study output is unavailable".to_string())?;
        if !output.is_empty() {
            output.set(0, None)?;
        }
        Ok(())
    }

    fn study_registration(series: BarSeriesKey) -> NativeStudyRegistration {
        let definition = study_definition(series);
        NativeStudyRegistration {
            settings: StudySettings::defaults(&definition.settings).expect("valid defaults"),
            definition,
            program: NativeStudyProgram {
                calculate: calculate_test_study,
                state_factory: None,
            },
        }
    }

    #[test]
    fn queued_request_deadline_cancels_execution_without_waiting_for_coordinator() {
        let (commands, receiver) = mpsc::sync_channel(1);
        let service = MarketService::build_market_service(
            commands,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(BTreeSet::new())),
            Vec::new(),
            super::super::ProviderStartContext {
                recording_replays: Arc::new(super::super::recording::RecordingReplays::new(
                    Arc::new(super::super::recording::MarketRecording::new(Arc::new(
                        super::super::clock::SystemClock,
                    ))),
                    Arc::new(super::super::clock::SystemClock),
                )),
            },
            super::super::provider_auth::ProviderAuthRegistry::default(),
            super::super::broker_venue::BrokerVenueRunnerRegistry::default(),
            MarketServiceProviders {
                presentations: vec![*super::super::RITHMIC_DESCRIPTOR.presentation],
            },
            RuntimeChannels {
                releases: Arc::new(Mutex::new(Vec::new())),
                latest: Arc::new(Mutex::new(super::super::quotes::QuoteBook::default())),
                trading_feed: Arc::new(super::super::trading_feed::TradingFeedOutbox::default()),
                outbox: Arc::new(super::super::outbox::ConsumerOutbox::default()),
                recording: Arc::new(super::super::recording::MarketRecording::new(Arc::new(
                    super::super::clock::SystemClock,
                ))),
            },
        );
        assert_eq!(service.provider_presentations()[0].id, "rithmic");
        let result =
            service.request_with_timeout(|reply| Ok(Command::Status(reply)), Duration::ZERO);
        assert!(result.unwrap_err().contains("cancelled before execution"));
        let Command::Request(_, state) = receiver.try_recv().unwrap() else {
            panic!("request envelope");
        };
        assert!(!state.start());
        service
            .shutdown(Duration::from_secs(1))
            .expect("test runtime stops");
    }

    #[test]
    fn quote_subscription_drop_queues_bounded_nonblocking_release() {
        let (commands, receiver) = mpsc::sync_channel(2);
        let releases = Arc::new(Mutex::new(Vec::new()));
        let service = MarketService::build_market_service(
            commands,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(BTreeSet::new())),
            Vec::new(),
            super::super::ProviderStartContext {
                recording_replays: Arc::new(super::super::recording::RecordingReplays::new(
                    Arc::new(super::super::recording::MarketRecording::new(Arc::new(
                        super::super::clock::SystemClock,
                    ))),
                    Arc::new(super::super::clock::SystemClock),
                )),
            },
            super::super::provider_auth::ProviderAuthRegistry::default(),
            super::super::broker_venue::BrokerVenueRunnerRegistry::default(),
            MarketServiceProviders {
                presentations: vec![],
            },
            RuntimeChannels {
                releases: Arc::clone(&releases),
                latest: Arc::new(Mutex::new(super::super::quotes::QuoteBook::default())),
                trading_feed: Arc::new(super::super::trading_feed::TradingFeedOutbox::default()),
                outbox: Arc::new(super::super::outbox::ConsumerOutbox::default()),
                recording: Arc::new(super::super::recording::MarketRecording::new(Arc::new(
                    super::super::clock::SystemClock,
                ))),
            },
        );
        let service_for_thread = service.clone();
        let subscriber = thread::spawn(move || {
            service_for_thread.subscribe_quotes(
                LeaseOwner::Trading,
                InstrumentStreamKey {
                    provider: "hyperliquid".into(),
                    instrument: aeris_instruments::InstrumentId::try_new("BTC").unwrap(),
                    entitlement: None,
                },
            )
        });
        let Command::Request(command, state) = receiver.recv().unwrap() else {
            panic!("quote request must use the cancellation envelope");
        };
        assert!(state.start());
        let Command::AcquireQuote(_, reply) = *command else {
            panic!("expected quote acquisition");
        };
        let lease = MarketDataLeaseId(std::num::NonZeroU64::new(1).unwrap());
        reply.send(Ok(lease)).unwrap();
        let guard = subscriber.join().unwrap().unwrap();
        assert_eq!(guard.owner(), LeaseOwner::Trading);
        drop(guard);
        assert_eq!(
            *releases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![lease]
        );
        assert!(matches!(receiver.try_recv(), Ok(Command::ProviderWake)));
        service.shutdown(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn fourth_descriptor_starts_without_coordinator_registration_edits() {
        const FOURTH_PRESENTATION: crate::ProviderPresentationDescriptor =
            crate::ProviderPresentationDescriptor {
                id: "fourth-test-provider",
                ..super::super::RITHMIC_PRESENTATION
            };
        let mut descriptor = super::super::RITHMIC_DESCRIPTOR;
        descriptor.id = "fourth-test-provider";
        descriptor.presentation = &FOURTH_PRESENTATION;
        descriptor.capabilities.account_id = "fourth-test-account";
        descriptor.capabilities.history.worker = "aeris-fourth-history";
        let spec = super::super::providers::rithmic::mock_history_only_spec(
            descriptor,
            Box::new(super::super::providers::rithmic::MockHistorySource),
        );
        let service = MarketService::start_composed(vec![spec]).expect("fourth runtime starts");
        assert!(
            service
                .status()
                .expect("runtime status")
                .providers
                .iter()
                .any(|provider| provider.provider == "fourth-test-provider")
        );
        service
            .shutdown(Duration::from_secs(1))
            .expect("fourth runtime stops");
    }

    #[test]
    fn pluggable_third_provider_auth_and_broker_venue_compose_without_runtime_edits() {
        use aeris_provider_spi::{
            ConnectionId, LoginFieldValue, ProviderAuthAdapter, ProviderAuthError,
            ProviderLoginServer, SavedProviderLogin,
        };
        use aeris_trading::venue::{BrokerVenueLink, VenueCapabilities};
        use std::sync::atomic::AtomicUsize;

        struct MockThirdAuth;
        impl ProviderAuthAdapter for MockThirdAuth {
            fn servers(&self) -> Vec<ProviderLoginServer> {
                vec![ProviderLoginServer {
                    id: "mock-sim",
                    label: "Mock Sim",
                }]
            }
            fn discover_servers(
                &self,
                server_id: &str,
                _stop: Arc<AtomicBool>,
            ) -> Result<Vec<String>, ProviderAuthError> {
                Ok(vec![format!("{server_id}:MockSim")])
            }
            fn saved_login(&self) -> Result<Option<SavedProviderLogin>, ProviderAuthError> {
                Ok(None)
            }
            fn sign_in(
                &self,
                _fields: &[LoginFieldValue<'_>],
                _stop: Arc<AtomicBool>,
            ) -> Result<(), ProviderAuthError> {
                Err(ProviderAuthError::InvalidFields("mock"))
            }
            fn forget_login(&self) -> Result<(), ProviderAuthError> {
                Ok(())
            }
        }

        const MOCK_PRESENTATION: crate::ProviderPresentationDescriptor =
            crate::ProviderPresentationDescriptor {
                id: "mock-third-provider",
                ..super::super::RITHMIC_PRESENTATION
            };
        let mut descriptor = super::super::RITHMIC_DESCRIPTOR;
        descriptor.id = "mock-third-provider";
        descriptor.presentation = &MOCK_PRESENTATION;
        descriptor.capabilities.account_id = "mock-third-account";
        descriptor.capabilities.history.worker = "aeris-mock-third-history";
        let spec = super::super::providers::rithmic::mock_history_only_spec(
            descriptor,
            Box::new(super::super::providers::rithmic::MockHistorySource),
        );
        let broker_started = Arc::new(AtomicUsize::new(0));
        let broker_started_flag = Arc::clone(&broker_started);
        let service = MarketService::builder()
            .register_provider_spec(spec)
            .register_auth("mock-third-provider", Arc::new(MockThirdAuth))
            .register_broker_venue(
                "mock-broker",
                Arc::new(move |mut link, shutdown| {
                    let _ = (link.attach)(VenueCapabilities {
                        bracket_levels: 0,
                        client_order_id_limit: 32,
                    });
                    broker_started_flag.fetch_add(1, Ordering::AcqRel);
                    while !shutdown.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(5));
                    }
                }),
            )
            .start()
            .expect("composed service starts");

        assert!(
            service
                .status()
                .expect("status")
                .providers
                .iter()
                .any(|p| p.provider == "mock-third-provider")
        );
        let systems = service
            .discover_provider_servers("mock-third-provider", "mock-sim")
            .expect("mock auth servers");
        assert_eq!(systems, vec!["mock-sim:MockSim".to_string()]);
        let link = BrokerVenueLink {
            attach: Box::new(|_| Ok((1, aeris_trading::venue_outbox::bounded_venue_requests(4).1))),
            events: Box::new(|_| Ok(())),
        };
        let conn = ConnectionId::try_new("sim-1").expect("valid connection id");
        service
            .attach_broker_venue_for("mock-broker", &conn, link)
            .expect("mock broker attaches");
        let deadline = Instant::now() + Duration::from_secs(2);
        while broker_started.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(broker_started.load(Ordering::Acquire), 1);
        service
            .shutdown(Duration::from_secs(2))
            .expect("composed service shuts down");
    }

    #[test]
    fn uncached_study_dependency_uses_existing_bounded_history_worker() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = MarketService::start_composed(vec![
            super::super::providers::rithmic::mock_history_only_spec(
                super::super::RITHMIC_DESCRIPTOR,
                Box::new(RecordingStudyHistory {
                    calls: Arc::clone(&calls),
                }),
            ),
        ])
        .expect("test runtime starts");
        service.attach(1).expect("client attaches");
        service
            .register_consumer(1, 1, 1)
            .expect("consumer registers");
        service
            .install_provider_instrument(&study_instrument())
            .expect("instrument installs");

        let study_id = service
            .register_study(1, 1, study_registration(study_series()))
            .expect("study registers without waiting for provider history");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let called = !calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty();
            let status = service.status().expect("runtime status");
            if called && status.retained_bars >= 2 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "study dependency did not complete through provider history worker"
            );
            thread::sleep(Duration::from_millis(5));
        }

        assert_eq!(
            *calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["instrument:rithmic:CME:MNQU6".to_string()]
        );
        assert_eq!(
            service.remove_study(1, study_id).expect("study removes"),
            vec![study_id]
        );
        service.remove_consumer(1, 1).expect("consumer removes");
        service.detach(1).expect("client detaches");
        service
            .shutdown(Duration::from_secs(2))
            .expect("test runtime shuts down");
    }
}
