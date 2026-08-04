use crate::{
    ConnectTrigger, DesktopProviderConfig, DesktopProviderError, DesktopProviderEvent,
    DesktopProviderMetrics, DesktopProviderRuntime, DesktopProviderState, NetworkEvent,
    ProviderSessionDriver, SessionGeneration,
};
use axiusflow_application::MarketStreamPublication;
use axiusflow_desktop_history::{
    ChartId, DesktopHistoryError, HistoryDecoder, HistoryPublication, HistoryWorker,
    HistoryWorkerConfig, HydrationOutcome, HydrationRequest, StartupCacheState, WorkerMetrics,
};
use axiusflow_desktop_storage::{CatalogKey, SegmentEncryptionKey, SegmentIdentity};
use axiusflow_platform_runtime::{CredentialVault, PowerEvent};
use axiusflow_provider_history::{SequencedHistory, VerifiedHistorySnapshot};
use std::{
    collections::BTreeMap, error::Error, fmt, num::NonZeroUsize, path::Path, sync::Arc,
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
    HandoffLimitReached { maximum: usize },
    HandoffNotTracked,
    HandoffGenerationMismatch,
    ProviderNotStreaming,
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
            Self::HandoffLimitReached { maximum } => write!(
                formatter,
                "desktop market worker reached its {maximum}-handoff bound"
            ),
            Self::HandoffNotTracked => formatter.write_str("history handoff is not tracked"),
            Self::HandoffGenerationMismatch => {
                formatter.write_str("history callback belongs to a stale provider generation")
            }
            Self::ProviderNotStreaming => formatter.write_str("provider session is not streaming"),
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
            | Self::HandoffLimitReached { .. }
            | Self::HandoffNotTracked
            | Self::HandoffGenerationMismatch
            | Self::ProviderNotStreaming => None,
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
#[derive(Clone, Copy, Debug)]
pub struct DesktopMarketWorkerConfig {
    pub provider: DesktopProviderConfig,
    pub history: HistoryWorkerConfig,
    pub maximum_catalog_entries: usize,
}

/// Worker-local composition of direct-provider lifecycle and desktop history.
pub struct DesktopMarketWorker<T, V, D: ProviderSessionDriver> {
    provider: DesktopProviderRuntime<V, D>,
    history: HistoryWorker<T>,
    handoff_generations: BTreeMap<SegmentIdentity, SessionGeneration>,
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

    /// Returns redacted provider metrics.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access.
    pub fn provider_metrics(&self) -> Result<DesktopProviderMetrics, DesktopMarketWorkerError> {
        self.provider.metrics().map_err(Into::into)
    }

    pub const fn history_metrics(&self) -> WorkerMetrics {
        self.history.metrics()
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

    fn ensure_streaming_generation(
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

    fn ensure_history_callback(
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
