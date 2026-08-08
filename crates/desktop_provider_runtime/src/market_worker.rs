use crate::{
    ConnectTrigger, DesktopProviderConfig, DesktopProviderError, DesktopProviderEvent,
    DesktopProviderMetrics, DesktopProviderRuntime, DesktopProviderState, NetworkEvent,
    ProviderInvalidationReason, ProviderSessionDriver, SessionGeneration,
};
use axiusflow_application::MarketStreamPublication;
use axiusflow_desktop_history::{
    ChartId, DesktopHistoryError, HistoryDecoder, HistoryPublication, HistoryWorker,
    HistoryWorkerConfig, HydrationOutcome, HydrationRequest, StartupCacheState, WorkerMetrics,
};
use axiusflow_desktop_storage::{
    CatalogKey, HistorySeriesIdentity, PublicationRequest, SegmentEncryptionKey, SegmentIdentity,
};
use axiusflow_observability::{
    DiagnosticsQueue, FeedDiagnosticsSnapshot, LatencyTimestampChain, LocalLatencyMetric,
};
use axiusflow_platform_runtime::{CredentialVault, PowerEvent};
use axiusflow_provider_history::{
    Completion, DataClass, HistoryItem, RequestInterest, SequencedHistory, VerifiedHistorySnapshot,
};
use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    num::{NonZeroU64, NonZeroUsize},
    path::Path,
    sync::Arc,
    thread::ThreadId,
};

/// Redacted failure classes at the composed provider and history boundary.
#[derive(Debug)]
pub enum DesktopMarketWorkerError {
    Provider(DesktopProviderError),
    HistoryConfiguration,
    HistoryThreadOwnership,
    HistoryDecode,
    HistoryResourceLimit,
    HistoryHandoffState,
    HistoryStorage,
    HistoryContinuity,
    HistoryCompletionMismatch,
    HistoryCompletionIncomplete,
    HistoryEmptyCutoverMissing,
    HandoffLimitReached { maximum: usize },
    HandoffNotTracked,
    HandoffGenerationMismatch,
    ProviderNotStreaming,
    CallbackSourceMismatch,
}

impl fmt::Display for DesktopMarketWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(error) => write!(formatter, "desktop market provider failed: {error}"),
            Self::HistoryConfiguration => {
                formatter.write_str("desktop market history configuration failed")
            }
            Self::HistoryThreadOwnership => {
                formatter.write_str("desktop market history thread ownership failed")
            }
            Self::HistoryDecode => formatter.write_str("desktop market history decode failed"),
            Self::HistoryResourceLimit => {
                formatter.write_str("desktop market history reached a configured bound")
            }
            Self::HistoryHandoffState => {
                formatter.write_str("desktop market history handoff state is invalid")
            }
            Self::HistoryStorage => formatter.write_str("desktop market history storage failed"),
            Self::HistoryContinuity => {
                formatter.write_str("desktop market history continuity failed")
            }
            Self::HistoryCompletionMismatch => {
                formatter.write_str("desktop market history completion identity mismatched")
            }
            Self::HistoryCompletionIncomplete => {
                formatter.write_str("desktop market history completion is not a complete page")
            }
            Self::HistoryEmptyCutoverMissing => {
                formatter.write_str("desktop market empty history has no cutover watermark")
            }
            Self::HandoffLimitReached { maximum } => write!(
                formatter,
                "desktop market worker reached its {maximum}-handoff bound"
            ),
            Self::HandoffNotTracked => formatter.write_str("history handoff is not tracked"),
            Self::HandoffGenerationMismatch => {
                formatter.write_str("history callback belongs to a stale provider generation")
            }
            Self::ProviderNotStreaming => formatter.write_str("provider session is not streaming"),
            Self::CallbackSourceMismatch => {
                formatter.write_str("provider callback source mismatched")
            }
        }
    }
}

impl Error for DesktopMarketWorkerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider(error) => Some(error),
            Self::HistoryConfiguration
            | Self::HistoryThreadOwnership
            | Self::HistoryDecode
            | Self::HistoryResourceLimit
            | Self::HistoryHandoffState
            | Self::HistoryStorage
            | Self::HistoryContinuity
            | Self::HistoryCompletionMismatch
            | Self::HistoryCompletionIncomplete
            | Self::HistoryEmptyCutoverMissing
            | Self::HandoffLimitReached { .. }
            | Self::HandoffNotTracked
            | Self::HandoffGenerationMismatch
            | Self::ProviderNotStreaming
            | Self::CallbackSourceMismatch => None,
        }
    }
}

impl From<DesktopProviderError> for DesktopMarketWorkerError {
    fn from(error: DesktopProviderError) -> Self {
        Self::Provider(error)
    }
}

impl From<DesktopHistoryError> for DesktopMarketWorkerError {
    fn from(error: DesktopHistoryError) -> Self {
        match error {
            DesktopHistoryError::InvalidConfiguration(_) => Self::HistoryConfiguration,
            DesktopHistoryError::UiThreadWorkForbidden
            | DesktopHistoryError::WorkerThreadMismatch => Self::HistoryThreadOwnership,
            DesktopHistoryError::Decode(_) => Self::HistoryDecode,
            DesktopHistoryError::DecodedHistoryTooLarge { .. }
            | DesktopHistoryError::CacheFull { .. }
            | DesktopHistoryError::ChartLimitReached { .. }
            | DesktopHistoryError::HandoffLimitReached { .. } => Self::HistoryResourceLimit,
            DesktopHistoryError::HandoffAlreadyStarted | DesktopHistoryError::MissingHandoff => {
                Self::HistoryHandoffState
            }
            DesktopHistoryError::Storage(_) => Self::HistoryStorage,
            DesktopHistoryError::Provider(_) => Self::HistoryContinuity,
        }
    }
}

/// Bounds for the composed provider and history worker.
#[derive(Clone, Debug)]
pub struct DesktopMarketWorkerConfig {
    pub provider: DesktopProviderConfig,
    pub history: HistoryWorkerConfig,
    pub maximum_catalog_entries: usize,
}

/// Snapshot metadata required when installing one completed provider page.
#[derive(Clone, Copy, Debug)]
pub struct HistoryCompletionInstall {
    pub binding: HistoryCompletionBinding,
    pub snapshot_generation: NonZeroU64,
    pub empty_cutover_watermark: Option<u64>,
    pub startup_cache_state: StartupCacheState,
}

/// Worker-issued, non-reusable binding between one scheduler request and handoff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryCompletionBinding(NonZeroU64);

/// Worker-local composition of direct-provider lifecycle and desktop history.
pub struct DesktopMarketWorker<T, V, D: ProviderSessionDriver> {
    provider: DesktopProviderRuntime<V, D>,
    history: HistoryWorker<T>,
    handoff_generations: BTreeMap<SegmentIdentity, SessionGeneration>,
    history_completion_bindings:
        BTreeMap<SegmentIdentity, (RequestInterest, HistoryCompletionBinding)>,
    next_history_completion_binding: NonZeroU64,
    maximum_handoffs: NonZeroUsize,
}

impl<T: Clone, V, D> DesktopMarketWorker<T, V, D>
where
    V: CredentialVault,
    D: ProviderSessionDriver,
{
    /// Opens the provider and history owners on the current worker thread.
    ///
    /// # Errors
    ///
    /// Returns a redacted provider configuration or history-open failure.
    pub fn try_open(
        vault: V,
        driver: D,
        credential_key: impl Into<String>,
        history_root: impl AsRef<Path>,
        catalog_key: CatalogKey,
        ui_thread: ThreadId,
        config: DesktopMarketWorkerConfig,
    ) -> Result<Self, DesktopMarketWorkerError> {
        let maximum_handoffs = config.history.maximum_handoffs;
        let provider =
            DesktopProviderRuntime::try_new(vault, driver, credential_key, config.provider)?;
        let history = HistoryWorker::try_open(
            history_root,
            catalog_key,
            config.maximum_catalog_entries,
            ui_thread,
            config.history,
        )?;
        Ok(Self {
            provider,
            history,
            handoff_generations: BTreeMap::new(),
            history_completion_bindings: BTreeMap::new(),
            next_history_completion_binding: NonZeroU64::MIN,
            maximum_handoffs,
        })
    }

    /// Returns the provider lifecycle state.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access.
    pub fn provider_state(&self) -> Result<DesktopProviderState, DesktopMarketWorkerError> {
        self.provider.state().map_err(Into::into)
    }

    /// Verifies that an adapter-owned callback receiver belongs to this worker's driver.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access.
    pub fn callback_source_matches(
        &self,
        predicate: impl FnOnce(&D) -> bool,
    ) -> Result<bool, DesktopMarketWorkerError> {
        self.provider.driver_matches(predicate).map_err(Into::into)
    }

    /// Returns redacted provider metrics.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access.
    pub fn provider_metrics(&self) -> Result<DesktopProviderMetrics, DesktopMarketWorkerError> {
        self.provider.metrics().map_err(Into::into)
    }

    /// Publishes the production feed diagnostics snapshot when cadence allows.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread, clock, or diagnostics failure.
    pub fn try_diagnostics_snapshot(
        &mut self,
    ) -> Result<Option<FeedDiagnosticsSnapshot>, DesktopMarketWorkerError> {
        self.provider.try_diagnostics_snapshot().map_err(Into::into)
    }

    pub const fn history_metrics(&self) -> WorkerMetrics {
        self.history.metrics()
    }

    /// Persists one validated provider segment through the worker-owned encrypted store.
    ///
    /// # Errors
    ///
    /// Returns a redacted wrong-thread, identity, rights, bounds, or storage failure.
    pub fn persist_history_segment(
        &mut self,
        request: PublicationRequest<'_>,
    ) -> Result<(), DesktopMarketWorkerError> {
        self.history.persist_segment(request).map_err(Into::into)
    }

    /// Returns the newest retained identity for one exact history series revision.
    ///
    /// # Errors
    ///
    /// Returns a redacted wrong-thread, invalid-dimension, or catalog failure.
    pub fn latest_history_identity(
        &self,
        series: HistorySeriesIdentity<'_>,
        now_unix_seconds: i64,
    ) -> Result<Option<SegmentIdentity>, DesktopMarketWorkerError> {
        self.history
            .latest_identity(series, now_unix_seconds)
            .map_err(Into::into)
    }

    /// Starts a fresh provider generation.
    ///
    /// # Errors
    ///
    /// Returns a redacted lifecycle, vault, driver, or bound failure.
    pub fn connect(
        &mut self,
        trigger: ConnectTrigger,
    ) -> Result<SessionGeneration, DesktopMarketWorkerError> {
        self.provider.connect(trigger).map_err(Into::into)
    }

    /// Records connection intent and starts when environmental fences allow.
    ///
    /// # Errors
    ///
    /// Returns a redacted lifecycle, driver, vault, or queue failure.
    pub fn request_connection(
        &mut self,
    ) -> Result<Option<SessionGeneration>, DesktopMarketWorkerError> {
        self.provider.request_connection().map_err(Into::into)
    }

    /// Accepts provider establishment for the active generation.
    ///
    /// # Errors
    ///
    /// Returns an error for stale, invalid, wrong-thread, or bounded-queue use.
    pub fn session_established(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), DesktopMarketWorkerError> {
        self.provider
            .session_established(generation)
            .map_err(Into::into)
    }

    /// Records one live trade callback in production feed diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for stale generation, wrong-thread, or diagnostics failure.
    pub fn record_trade_diagnostics(
        &mut self,
        generation: SessionGeneration,
        provider_timestamp_unix_nanos: Option<i64>,
    ) -> Result<(), DesktopMarketWorkerError> {
        self.provider
            .record_trade_diagnostics(generation, provider_timestamp_unix_nanos)
            .map_err(Into::into)
    }

    /// Records one live heartbeat callback in production feed diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for stale generation, wrong-thread, or diagnostics failure.
    pub fn record_heartbeat_diagnostics(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), DesktopMarketWorkerError> {
        self.provider
            .record_heartbeat_diagnostics(generation)
            .map_err(Into::into)
    }

    /// Observes one fixed-capacity queue in production diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access or invalid queue evidence.
    pub fn observe_diagnostics_queue(
        &mut self,
        queue: DiagnosticsQueue,
        current_items: usize,
        item_capacity: usize,
        current_bytes: usize,
        byte_capacity: usize,
    ) -> Result<(), DesktopMarketWorkerError> {
        self.provider
            .observe_diagnostics_queue(
                queue,
                current_items,
                item_capacity,
                current_bytes,
                byte_capacity,
            )
            .map_err(Into::into)
    }

    /// Records one correctly labelled local processing interval.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access or invalid timestamp evidence.
    pub fn record_latency_diagnostics(
        &mut self,
        generation: SessionGeneration,
        metric: LocalLatencyMetric,
        chain: &LatencyTimestampChain,
    ) -> Result<(), DesktopMarketWorkerError> {
        self.provider
            .record_latency_diagnostics(generation, metric, chain)
            .map_err(Into::into)
    }

    /// Records one safely conflated UI update.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access or diagnostics failure.
    pub fn record_coalesced_ui_update_diagnostics(
        &mut self,
        generation: SessionGeneration,
        count: u64,
    ) -> Result<(), DesktopMarketWorkerError> {
        self.provider
            .record_coalesced_ui_update_diagnostics(generation, count)
            .map_err(Into::into)
    }

    /// Publishes one immutable market update for the active generation.
    ///
    /// # Errors
    ///
    /// Returns an error for stale, invalid, wrong-thread, or bounded-queue use.
    pub fn publish(
        &mut self,
        generation: SessionGeneration,
        publication: MarketStreamPublication,
    ) -> Result<Arc<MarketStreamPublication>, DesktopMarketWorkerError> {
        let was_active = matches!(
            self.provider.state()?,
            DesktopProviderState::Streaming { generation: active } if active == generation
        );
        let provider_result = self.provider.publish(generation, publication);
        let remains_active = matches!(
            self.provider.state()?,
            DesktopProviderState::Streaming { generation: active } if active == generation
        );
        let history_result = if was_active && !remains_active {
            self.retire_handoffs()
        } else {
            Ok(())
        };
        Self::finish_fence(provider_result, history_result)
    }

    /// Returns the last accepted market publication.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access.
    pub fn current_market_publication(
        &self,
    ) -> Result<Option<Arc<MarketStreamPublication>>, DesktopMarketWorkerError> {
        self.provider.current_publication().map_err(Into::into)
    }

    /// Receives at most one provider event without blocking.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access.
    pub fn try_recv_provider_event(
        &mut self,
    ) -> Result<Option<DesktopProviderEvent>, DesktopMarketWorkerError> {
        self.provider.try_recv_event().map_err(Into::into)
    }

    /// Fences an invalid provider generation and all of its history handoffs.
    ///
    /// # Errors
    ///
    /// Returns a redacted provider or history-retirement failure.
    pub fn session_invalid(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), DesktopMarketWorkerError> {
        let active = matches!(
            self.provider.state()?,
            DesktopProviderState::Connecting { generation: active, .. }
                | DesktopProviderState::Streaming { generation: active }
                if active == generation
        );
        let provider_result = self.provider.session_invalid(generation);
        let history_result = if active {
            self.retire_handoffs()
        } else {
            Ok(())
        };
        Self::finish_fence(provider_result, history_result)
    }

    /// Fences an invalid provider generation while retaining its coarse,
    /// non-secret invalidation reason in diagnostics.
    ///
    /// # Errors
    ///
    /// Returns a redacted provider or history-retirement failure.
    pub fn session_invalid_for_provider(
        &mut self,
        generation: SessionGeneration,
        reason: ProviderInvalidationReason,
    ) -> Result<(), DesktopMarketWorkerError> {
        let active = matches!(
            self.provider.state()?,
            DesktopProviderState::Connecting { generation: active, .. }
                | DesktopProviderState::Streaming { generation: active }
                if active == generation
        );
        let provider_result = self
            .provider
            .session_invalid_for_provider(generation, reason);
        let history_result = if active {
            self.retire_handoffs()
        } else {
            Ok(())
        };
        Self::finish_fence(provider_result, history_result)
    }

    /// Fences a provider generation after its adapter callback queue overflows.
    ///
    /// # Errors
    ///
    /// Returns a redacted provider or history-retirement failure.
    pub fn session_callback_queue_overflow(
        &mut self,
        generation: SessionGeneration,
    ) -> Result<(), DesktopMarketWorkerError> {
        let active = matches!(
            self.provider.state()?,
            DesktopProviderState::Connecting { generation: active, .. }
                | DesktopProviderState::Streaming { generation: active }
                if active == generation
        );
        let provider_result = self.provider.session_callback_queue_overflow(generation);
        let history_result = if active {
            self.retire_handoffs()
        } else {
            Ok(())
        };
        Self::finish_fence(provider_result, history_result)
    }

    /// Applies a native power event and retires history before suspension.
    ///
    /// # Errors
    ///
    /// Returns a redacted provider or history-retirement failure.
    pub fn handle_power_event(
        &mut self,
        event: PowerEvent,
    ) -> Result<Option<SessionGeneration>, DesktopMarketWorkerError> {
        let provider_result = self.provider.handle_power_event(event);
        let history_result = if event == PowerEvent::Suspending {
            self.retire_handoffs()
        } else {
            Ok(())
        };
        Self::finish_fence(provider_result, history_result)
    }

    /// Applies network availability and retires history on loss.
    ///
    /// # Errors
    ///
    /// Returns a redacted provider or history-retirement failure.
    pub fn handle_network_event(
        &mut self,
        event: NetworkEvent,
    ) -> Result<Option<SessionGeneration>, DesktopMarketWorkerError> {
        let provider_result = self.provider.handle_network_event(event);
        let history_result = if event == NetworkEvent::Unavailable {
            self.retire_handoffs()
        } else {
            Ok(())
        };
        Self::finish_fence(provider_result, history_result)
    }

    /// Retries an unconfirmed provider stop.
    ///
    /// # Errors
    ///
    /// Returns a redacted lifecycle, driver, vault, or queue failure.
    pub fn retry_unconfirmed_stop(
        &mut self,
    ) -> Result<Option<SessionGeneration>, DesktopMarketWorkerError> {
        self.provider.retry_unconfirmed_stop().map_err(Into::into)
    }

    /// Permanently stops the provider and retires all history handoffs.
    ///
    /// # Errors
    ///
    /// Returns a redacted provider or history-retirement failure.
    pub fn stop(&mut self) -> Result<(), DesktopMarketWorkerError> {
        let provider_result = self.provider.stop();
        let history_result = self.retire_handoffs();
        Self::finish_fence(provider_result, history_result)
    }

    /// Starts a bounded history handoff owned by one streaming generation.
    ///
    /// # Errors
    ///
    /// Returns an error for stale lifecycle, bounds, storage, or handoff state.
    pub fn begin_history_handoff(
        &mut self,
        generation: SessionGeneration,
        identity: SegmentIdentity,
        encryption_key: &SegmentEncryptionKey,
        now_unix_seconds: i64,
    ) -> Result<(), DesktopMarketWorkerError> {
        self.ensure_streaming_generation(generation)?;
        if self.handoff_generations.len() >= self.maximum_handoffs.get() {
            return Err(DesktopMarketWorkerError::HandoffLimitReached {
                maximum: self.maximum_handoffs.get(),
            });
        }
        self.history
            .begin_handoff(identity.clone(), encryption_key, now_unix_seconds)?;
        self.handoff_generations.insert(identity, generation);
        Ok(())
    }

    /// Starts a history handoff or binds recovery work to an active handoff.
    ///
    /// # Errors
    ///
    /// Returns an error for stale lifecycle, bounds, storage, or handoff state.
    pub fn begin_scheduled_history_handoff(
        &mut self,
        generation: SessionGeneration,
        identity: SegmentIdentity,
        interest: RequestInterest,
        encryption_key: &SegmentEncryptionKey,
        now_unix_seconds: i64,
    ) -> Result<HistoryCompletionBinding, DesktopMarketWorkerError> {
        self.ensure_streaming_generation(generation)?;
        match self.handoff_generations.get(&identity) {
            Some(tracked) if *tracked != generation => {
                return Err(DesktopMarketWorkerError::HandoffGenerationMismatch);
            }
            Some(_) if self.history_completion_bindings.contains_key(&identity) => {
                return Err(DesktopMarketWorkerError::HistoryHandoffState);
            }
            Some(_) => {}
            None => self.begin_history_handoff(
                generation,
                identity.clone(),
                encryption_key,
                now_unix_seconds,
            )?,
        }
        let binding = HistoryCompletionBinding(self.next_history_completion_binding);
        let next_binding = self
            .next_history_completion_binding
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or(DesktopMarketWorkerError::HistoryResourceLimit)?;
        self.next_history_completion_binding = next_binding;
        self.history_completion_bindings
            .insert(identity, (interest, binding));
        Ok(binding)
    }

    /// Retires one tracked history handoff.
    ///
    /// # Errors
    ///
    /// Returns an error for unknown, wrong-thread, or inconsistent handoff state.
    pub fn end_history_handoff(
        &mut self,
        generation: SessionGeneration,
        identity: &SegmentIdentity,
    ) -> Result<(), DesktopMarketWorkerError> {
        self.ensure_history_callback(generation, identity)?;
        self.history.end_handoff(identity)?;
        self.handoff_generations.remove(identity);
        self.history_completion_bindings.remove(identity);
        Ok(())
    }

    /// Applies one live history callback after provider-generation fencing.
    ///
    /// # Errors
    ///
    /// Returns an error for stale lifecycle, handoff, continuity, or memory bounds.
    pub fn push_history_live(
        &mut self,
        generation: SessionGeneration,
        identity: &SegmentIdentity,
        item: SequencedHistory<T>,
        decoded_item_bytes: usize,
    ) -> Result<Option<Arc<HistoryPublication<T>>>, DesktopMarketWorkerError> {
        self.ensure_history_callback(generation, identity)?;
        self.history
            .push_live(identity, item, decoded_item_bytes)
            .map_err(Into::into)
    }

    /// Installs one history snapshot after provider-generation fencing.
    ///
    /// # Errors
    ///
    /// Returns an error for stale lifecycle, handoff, continuity, or memory bounds.
    pub fn install_history_snapshot(
        &mut self,
        generation: SessionGeneration,
        identity: &SegmentIdentity,
        snapshot: VerifiedHistorySnapshot<T>,
        decoded_bytes: usize,
        startup_cache_state: StartupCacheState,
    ) -> Result<Arc<HistoryPublication<T>>, DesktopMarketWorkerError> {
        self.ensure_history_callback(generation, identity)?;
        self.history
            .install_snapshot(identity, snapshot, decoded_bytes, startup_cache_state)
            .map_err(Into::into)
    }

    /// Decodes and installs one scheduler-validated, non-paginated provider completion.
    ///
    /// The completion must describe the tracked segment exactly. Stale provider
    /// generations and identity mismatches are rejected before payload decoding.
    /// Empty pages require an explicit provider cutover watermark.
    ///
    /// # Errors
    ///
    /// Returns an error for stale lifecycle, mismatched or paginated work,
    /// decode/size failure, missing empty cutover evidence, or snapshot continuity.
    pub fn install_history_completion<E, F>(
        &mut self,
        generation: SessionGeneration,
        identity: &SegmentIdentity,
        completion: &Completion,
        install: HistoryCompletionInstall,
        mut retained_decoded_bytes: E,
        mut decode: F,
    ) -> Result<Arc<HistoryPublication<T>>, DesktopMarketWorkerError>
    where
        E: FnMut(&HistoryItem) -> Result<usize, String>,
        F: FnMut(&HistoryItem, usize) -> Result<(T, usize), String>,
    {
        self.ensure_history_callback(generation, identity)?;
        let Some((interest, tracked_binding)) = self.history_completion_bindings.get(identity)
        else {
            return Err(DesktopMarketWorkerError::HistoryCompletionMismatch);
        };
        if *tracked_binding != install.binding || !completion.interests().contains(interest) {
            return Err(DesktopMarketWorkerError::HistoryCompletionMismatch);
        }
        if !completion_matches_identity(completion, identity) {
            return Err(DesktopMarketWorkerError::HistoryCompletionMismatch);
        }
        if completion.continuation_scheduled()
            || completion.page().next.is_some()
            || completion.page().request.continuation.is_some()
            || completion.interests().is_empty()
            || completion.page().items.len() > completion.page().request.maximum_items.get()
        {
            return Err(DesktopMarketWorkerError::HistoryCompletionIncomplete);
        }
        let mut previous_sequence: Option<u64> = None;
        for item in &completion.page().items {
            if item.event_time_unix_nanos < identity.range_start_unix_nanos
                || item.event_time_unix_nanos >= identity.range_end_unix_nanos
                || previous_sequence
                    .is_some_and(|previous| previous.checked_add(1) != Some(item.sequence))
            {
                return Err(DesktopMarketWorkerError::HistoryContinuity);
            }
            let sequence = NonZeroU64::new(item.sequence)
                .ok_or(DesktopMarketWorkerError::HistoryContinuity)?;
            previous_sequence = Some(sequence.get());
        }
        let snapshot_watermark = match completion.page().items.last() {
            Some(item) => {
                if install.empty_cutover_watermark.is_some() {
                    return Err(DesktopMarketWorkerError::HistoryCompletionMismatch);
                }
                item.sequence
            }
            None => install
                .empty_cutover_watermark
                .ok_or(DesktopMarketWorkerError::HistoryEmptyCutoverMissing)?,
        };
        let mut estimated_item_bytes = Vec::with_capacity(completion.page().items.len());
        let mut decoded_bytes = 0_usize;
        for item in &completion.page().items {
            let item_bytes = retained_decoded_bytes(item)
                .map_err(|_| DesktopMarketWorkerError::HistoryDecode)?
                .max(size_of::<SequencedHistory<T>>());
            decoded_bytes = decoded_bytes
                .checked_add(item_bytes)
                .ok_or(DesktopMarketWorkerError::HistoryResourceLimit)?;
            estimated_item_bytes.push(item_bytes);
        }
        self.history
            .check_snapshot_capacity(identity, snapshot_watermark, decoded_bytes)?;
        let mut values = Vec::with_capacity(completion.page().items.len());
        for (item, maximum_item_bytes) in completion.page().items.iter().zip(estimated_item_bytes) {
            let sequence = NonZeroU64::new(item.sequence)
                .ok_or(DesktopMarketWorkerError::HistoryContinuity)?;
            let (value, item_bytes) = decode(item, maximum_item_bytes)
                .map_err(|_| DesktopMarketWorkerError::HistoryDecode)?;
            if item_bytes.max(size_of::<SequencedHistory<T>>()) > maximum_item_bytes {
                return Err(DesktopMarketWorkerError::HistoryResourceLimit);
            }
            values.push(SequencedHistory { sequence, value });
        }
        let snapshot = if values.is_empty() {
            VerifiedHistorySnapshot::empty_with_watermark(
                install.snapshot_generation,
                install
                    .empty_cutover_watermark
                    .ok_or(DesktopMarketWorkerError::HistoryEmptyCutoverMissing)?,
            )
        } else {
            VerifiedHistorySnapshot::try_new(install.snapshot_generation, values)
                .map_err(|_| DesktopMarketWorkerError::HistoryContinuity)?
        };
        let publication = self.install_history_snapshot(
            generation,
            identity,
            snapshot,
            decoded_bytes,
            install.startup_cache_state,
        )?;
        self.history_completion_bindings.remove(identity);
        Ok(publication)
    }

    /// Hydrates visible history independently of provider lifecycle.
    ///
    /// # Errors
    ///
    /// Returns a redacted storage, decode, authorization, or memory-bound failure.
    pub fn hydrate_visible<H: HistoryDecoder<T>>(
        &mut self,
        request: HydrationRequest<'_>,
        decoder: &mut H,
    ) -> Result<HydrationOutcome<T>, DesktopMarketWorkerError> {
        self.history
            .hydrate_visible(request, decoder)
            .map_err(Into::into)
    }

    /// Binds a chart to an authorized immutable history publication.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread, authorization, or chart bounds.
    pub fn bind_chart(
        &mut self,
        chart_id: ChartId,
        identity: &SegmentIdentity,
        encryption_key: &SegmentEncryptionKey,
        now_unix_seconds: i64,
    ) -> Result<Option<Arc<HistoryPublication<T>>>, DesktopMarketWorkerError> {
        self.history
            .bind_chart(chart_id, identity, encryption_key, now_unix_seconds)
            .map_err(Into::into)
    }

    /// Removes one chart binding.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access.
    pub fn unbind_chart(&mut self, chart_id: ChartId) -> Result<(), DesktopMarketWorkerError> {
        self.history.unbind_chart(chart_id).map_err(Into::into)
    }

    /// Returns the current authorized history publication.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread, storage, or authorization failure.
    pub fn current_history_publication(
        &mut self,
        identity: &SegmentIdentity,
        encryption_key: &SegmentEncryptionKey,
        now_unix_seconds: i64,
    ) -> Result<Option<Arc<HistoryPublication<T>>>, DesktopMarketWorkerError> {
        self.history
            .current_publication(identity, encryption_key, now_unix_seconds)
            .map_err(Into::into)
    }

    /// Returns the bounded history cache entry count.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access.
    pub fn cached_history_entries(&self) -> Result<usize, DesktopMarketWorkerError> {
        self.history.cached_entries().map_err(Into::into)
    }

    /// Verifies that an adapter callback belongs to the active streaming generation.
    ///
    /// # Errors
    ///
    /// Returns a redacted lifecycle or generation mismatch.
    pub fn ensure_streaming_generation(
        &self,
        generation: SessionGeneration,
    ) -> Result<(), DesktopMarketWorkerError> {
        match self.provider.state()? {
            DesktopProviderState::Streaming { generation: active } if active == generation => {
                Ok(())
            }
            DesktopProviderState::Streaming { .. } => {
                Err(DesktopMarketWorkerError::HandoffGenerationMismatch)
            }
            _ => Err(DesktopMarketWorkerError::ProviderNotStreaming),
        }
    }

    /// Verifies that a history callback belongs to its tracked streaming handoff.
    ///
    /// # Errors
    ///
    /// Returns a redacted lifecycle, generation, or handoff mismatch.
    pub fn ensure_history_callback(
        &self,
        generation: SessionGeneration,
        identity: &SegmentIdentity,
    ) -> Result<(), DesktopMarketWorkerError> {
        self.ensure_streaming_generation(generation)?;
        match self.handoff_generations.get(identity) {
            Some(tracked) if *tracked == generation => Ok(()),
            Some(_) => Err(DesktopMarketWorkerError::HandoffGenerationMismatch),
            None => Err(DesktopMarketWorkerError::HandoffNotTracked),
        }
    }

    fn retire_handoffs(&mut self) -> Result<(), DesktopHistoryError> {
        let identities = self.handoff_generations.keys().cloned().collect::<Vec<_>>();
        let mut first_error = None;
        for identity in identities {
            match self.history.end_handoff(&identity) {
                Ok(()) => {
                    self.handoff_generations.remove(&identity);
                    self.history_completion_bindings.remove(&identity);
                }
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn finish_fence<R>(
        provider_result: Result<R, DesktopProviderError>,
        history_result: Result<(), DesktopHistoryError>,
    ) -> Result<R, DesktopMarketWorkerError> {
        match (provider_result, history_result) {
            (Err(error), _) => Err(error.into()),
            (Ok(_), Err(error)) => Err(error.into()),
            (Ok(value), Ok(())) => Ok(value),
        }
    }
}

fn completion_matches_identity(completion: &Completion, identity: &SegmentIdentity) -> bool {
    let request = &completion.page().request;
    request.provider_id == identity.scope.provider_id
        && request.account_id == identity.scope.account_id
        && request.entitlement_revision == identity.scope.entitlement_revision
        && request.instrument_id == identity.instrument_id
        && request.resolution == identity.resolution
        && request.range.start_unix_nanos == identity.range_start_unix_nanos
        && request.range.end_unix_nanos == identity.range_end_unix_nanos
        && matches!(
            (request.data_class, identity.data_kind),
            (DataClass::Bars, axiusflow_desktop_storage::DataKind::Bars)
                | (DataClass::Ticks, axiusflow_desktop_storage::DataKind::Ticks)
                | (DataClass::Depth, axiusflow_desktop_storage::DataKind::Depth)
        )
}
