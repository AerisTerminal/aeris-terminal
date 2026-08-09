use crate::{
    CacheSource, ControlPlaneState, DesktopHistoryError, HistoryPublication, HydrationOutcome,
    HydrationRequest, ProviderConnectionState, StartupCacheState, WorkerMetrics,
    cache::SharedHistoryCache,
};
use axiusflow_desktop_storage::{
    AuthorizedHistoryRead, AvailabilityReason, CatalogKey, DesktopStorageError,
    HistorySeriesIdentity, HistoryStore, MAXIMUM_SEGMENT_BYTES, PublicationRequest, RecoveryAction,
    RetainedRange, SegmentIdentity,
};
use axiusflow_provider_history::{
    CoverageSnapshot, HandoffCoordinator, LiveAcceptance, SequencedHistory, VerifiedHistorySnapshot,
};
use std::{
    collections::BTreeMap,
    marker::PhantomData,
    num::NonZeroUsize,
    ops::Bound::{Excluded, Unbounded},
    path::Path,
    rc::Rc,
    sync::Arc,
    thread::{self, ThreadId},
};

/// Worker-side decoder for one authenticated local segment.
pub trait HistoryDecoder<T> {
    /// Computes the exact retained decoded-memory charge without retaining
    /// decoded values.
    ///
    /// # Errors
    ///
    /// Returns a redacted failure class when the charge cannot be derived.
    fn retained_decoded_bytes(&mut self, payload: &[u8]) -> Result<usize, String>;

    /// Decodes one bounded payload and reports its retained decoded-memory charge.
    ///
    /// # Errors
    ///
    /// Returns a redacted failure class for malformed or incompatible bytes.
    fn decode(
        &mut self,
        payload: &[u8],
        maximum_decoded_bytes: usize,
    ) -> Result<(Vec<SequencedHistory<T>>, usize), String>;
}

/// Bounds for one desktop history worker.
#[derive(Clone, Copy, Debug)]
pub struct HistoryWorkerConfig {
    pub maximum_cache_entries: NonZeroUsize,
    pub maximum_decoded_bytes: NonZeroUsize,
    pub maximum_charts: NonZeroUsize,
    pub maximum_segment_read_bytes: NonZeroUsize,
    pub maximum_buffered_live: NonZeroUsize,
    pub maximum_handoffs: NonZeroUsize,
}

/// Blocking storage/decode and provider-handoff owner for one dedicated thread.
///
/// ```compile_fail
/// use axiusflow_desktop_history::HistoryWorker;
/// fn require_send<T: Send>() {}
/// require_send::<HistoryWorker<u64>>();
/// ```
pub struct HistoryWorker<T> {
    owner_thread: ThreadId,
    store: HistoryStore,
    cache: SharedHistoryCache<T>,
    maximum_segment_read_bytes: NonZeroUsize,
    maximum_decoded_bytes: NonZeroUsize,
    maximum_buffered_live: NonZeroUsize,
    maximum_handoffs: NonZeroUsize,
    buffered_live_bytes: usize,
    handoffs: BTreeMap<SegmentIdentity, HandoffEntry<T>>,
    metrics: WorkerMetrics,
    _not_send: PhantomData<Rc<()>>,
}

struct HandoffEntry<T> {
    coordinator: HandoffCoordinator<T>,
    buffered_item_bytes: BTreeMap<u64, usize>,
}

impl<T: Clone> HistoryWorker<T> {
    /// Creates a worker only when called outside the declared GPUI thread.
    ///
    /// # Errors
    ///
    /// Returns an error when construction occurs on the UI thread or read bytes
    /// can exceed the total decoded-memory budget.
    pub fn try_open(
        root: impl AsRef<Path>,
        catalog_key: CatalogKey,
        maximum_catalog_entries: usize,
        ui_thread: ThreadId,
        config: HistoryWorkerConfig,
    ) -> Result<Self, DesktopHistoryError> {
        let owner_thread = thread::current().id();
        if owner_thread == ui_thread {
            return Err(DesktopHistoryError::UiThreadWorkForbidden);
        }
        if config.maximum_segment_read_bytes.get() > config.maximum_decoded_bytes.get() {
            return Err(DesktopHistoryError::InvalidConfiguration(
                "segment read bound exceeds decoded-memory bound",
            ));
        }
        if config.maximum_segment_read_bytes.get() > MAXIMUM_SEGMENT_BYTES {
            return Err(DesktopHistoryError::InvalidConfiguration(
                "segment read bound exceeds the storage limit",
            ));
        }
        let store = HistoryStore::open(root, catalog_key, maximum_catalog_entries)?;
        Ok(Self {
            owner_thread,
            store,
            cache: SharedHistoryCache::new(
                config.maximum_cache_entries,
                config.maximum_decoded_bytes,
                config.maximum_charts,
            ),
            maximum_segment_read_bytes: config.maximum_segment_read_bytes,
            maximum_decoded_bytes: config.maximum_decoded_bytes,
            maximum_buffered_live: config.maximum_buffered_live,
            maximum_handoffs: config.maximum_handoffs,
            buffered_live_bytes: 0,
            handoffs: BTreeMap::new(),
            metrics: WorkerMetrics::default(),
            _not_send: PhantomData,
        })
    }

    #[must_use]
    pub const fn metrics(&self) -> WorkerMetrics {
        self.metrics
    }

    /// Persists one validated provider segment through the worker-owned encrypted store.
    ///
    /// An already-present immutable identity is accepted without replacement.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread, identity, rights, bounds, or storage failures.
    pub fn persist_segment(
        &mut self,
        request: PublicationRequest<'_>,
    ) -> Result<(), DesktopHistoryError> {
        self.ensure_owner()?;
        match self.store.publish(request) {
            Ok(_) | Err(DesktopStorageError::SegmentAlreadyExists) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Returns the newest retained identity for one exact history series revision.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread, invalid dimensions, or catalog failure.
    pub fn latest_identity(
        &self,
        series: HistorySeriesIdentity<'_>,
        now_unix_seconds: i64,
    ) -> Result<Option<SegmentIdentity>, DesktopHistoryError> {
        self.ensure_owner()?;
        self.store
            .latest_identity(series, now_unix_seconds)
            .map_err(Into::into)
    }

    /// Returns normalized durable coverage facts for one exact series revision.
    ///
    /// # Errors
    /// Returns an error for wrong-thread access, invalid dimensions, or catalog failure.
    pub fn coverage_snapshot(
        &self,
        series: HistorySeriesIdentity<'_>,
        now_unix_seconds: i64,
    ) -> Result<CoverageSnapshot, DesktopHistoryError> {
        self.ensure_owner()?;
        self.store
            .series_coverage_snapshot(series, now_unix_seconds)
            .map_err(Into::into)
    }

    /// Records provider-proven empty coverage through the worker-owned store.
    ///
    /// # Errors
    /// Returns an error for wrong-thread access, invalid dimensions, or catalog failure.
    pub fn record_confirmed_empty(
        &mut self,
        series: HistorySeriesIdentity<'_>,
        range: axiusflow_desktop_storage::RetainedRange,
        now_unix_seconds: i64,
    ) -> Result<(), DesktopHistoryError> {
        self.ensure_owner()?;
        self.store
            .record_confirmed_empty(series, range, now_unix_seconds)
            .map_err(Into::into)
    }

    /// Resolves invalidated or quarantined evidence after a provider repair.
    ///
    /// # Errors
    /// Returns an error for wrong-thread access, invalid dimensions, or storage failure.
    pub fn resolve_repaired_range(
        &mut self,
        series: HistorySeriesIdentity<'_>,
        range: axiusflow_desktop_storage::RetainedRange,
        confirmed_empty: bool,
        now_unix_seconds: i64,
    ) -> Result<(), DesktopHistoryError> {
        self.ensure_owner()?;
        self.store
            .resolve_repaired_range(series, range, confirmed_empty, now_unix_seconds)
            .map_err(Into::into)
    }

    /// Returns active immutable segments overlapping one visible range.
    ///
    /// # Errors
    /// Returns an error for wrong-thread access, invalid dimensions, or catalog failure.
    pub fn retained_identities_in_range(
        &self,
        series: HistorySeriesIdentity<'_>,
        requested: RetainedRange,
        now_unix_seconds: i64,
    ) -> Result<Vec<SegmentIdentity>, DesktopHistoryError> {
        self.ensure_owner()?;
        self.store
            .retained_identities_in_range(series, requested, now_unix_seconds)
            .map_err(Into::into)
    }

    /// Returns the current bounded cache entry count.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access, a mismatched local-segment key,
    /// expired local state cleanup failure, or a handoff bound violation.
    pub fn cached_entries(&self) -> Result<usize, DesktopHistoryError> {
        self.ensure_owner()?;
        Ok(self.cache.len())
    }

    /// Returns the current decoded-memory charge.
    ///
    /// # Errors
    ///
    /// Returns an error when called outside the owning worker thread.
    pub fn cached_decoded_bytes(&mut self) -> Result<usize, DesktopHistoryError> {
        self.ensure_owner()?;
        Ok(self
            .cache
            .decoded_bytes()
            .saturating_add(self.buffered_live_bytes))
    }

    /// Binds one chart to a shared immutable history generation.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access, a mismatched local-segment key,
    /// or when the chart bound is full. Expired local publications are removed.
    pub fn bind_chart(
        &mut self,
        chart_id: crate::ChartId,
        identity: &SegmentIdentity,
        encryption_key: &axiusflow_desktop_storage::SegmentEncryptionKey,
        now_unix_seconds: i64,
    ) -> Result<Option<Arc<HistoryPublication<T>>>, DesktopHistoryError> {
        self.ensure_owner()?;
        if self
            .authorized_cached_publication(identity, encryption_key, now_unix_seconds)?
            .is_none()
        {
            self.cache.unbind_chart(chart_id);
            return Ok(None);
        }
        self.cache.bind_chart(chart_id, identity)
    }

    /// Removes one chart binding without mutating any published generation.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access or a mismatched local-segment
    /// key. Expired local publications are removed and returned as absent.
    pub fn unbind_chart(&mut self, chart_id: crate::ChartId) -> Result<(), DesktopHistoryError> {
        self.ensure_owner()?;
        self.cache.unbind_chart(chart_id);
        Ok(())
    }

    /// Returns the current shared immutable generation for one identity.
    ///
    /// # Errors
    ///
    /// Returns an error when called outside the owning worker thread.
    pub fn current_publication(
        &mut self,
        identity: &SegmentIdentity,
        encryption_key: &axiusflow_desktop_storage::SegmentEncryptionKey,
        now_unix_seconds: i64,
    ) -> Result<Option<Arc<HistoryPublication<T>>>, DesktopHistoryError> {
        self.ensure_owner()?;
        self.authorized_cached_publication(identity, encryption_key, now_unix_seconds)
    }

    /// Hydrates one exact visible range from memory or authenticated local storage.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access, storage/authentication failure,
    /// malformed decoded bytes, or a configured memory-bound violation.
    pub fn hydrate_visible<D: HistoryDecoder<T>>(
        &mut self,
        request: HydrationRequest<'_>,
        decoder: &mut D,
    ) -> Result<HydrationOutcome<T>, DesktopHistoryError> {
        self.ensure_owner()?;
        if request.control_plane_state == ControlPlaneState::Unavailable {
            self.metrics.control_plane_unavailable_requests = self
                .metrics
                .control_plane_unavailable_requests
                .saturating_add(1);
        }
        if let Some((publication, access_policy)) = self.cache.get(request.identity) {
            if let Some(access_policy) = access_policy
                && let Some((reason, recovery)) =
                    access_policy.validate(request.encryption_key, request.now_unix_seconds)?
            {
                self.cache.remove(request.identity);
                if reason == AvailabilityReason::Expired {
                    self.store
                        .remove_if_expired(request.identity, request.now_unix_seconds)?;
                }
                return Ok(unavailable_outcome(
                    reason,
                    recovery,
                    request.provider_state,
                ));
            }
            return Ok(HydrationOutcome::Ready {
                publication,
                memory_cache_hit: true,
            });
        }
        self.metrics.storage_reads = self.metrics.storage_reads.saturating_add(1);
        let read = self
            .store
            .read_bounded_authorized(
                request.identity,
                request.encryption_key,
                request.now_unix_seconds,
                request.missing_recovery,
                self.maximum_segment_read_bytes.get(),
            )
            .map_err(|error| match error {
                axiusflow_desktop_storage::DesktopStorageError::SegmentTooLarge {
                    requested,
                    maximum,
                } => DesktopHistoryError::DecodedHistoryTooLarge { requested, maximum },
                error => DesktopHistoryError::Storage(error),
            })?;
        match read {
            AuthorizedHistoryRead::Hit {
                payload,
                access_policy,
            } => {
                self.metrics.storage_bytes_read = self
                    .metrics
                    .storage_bytes_read
                    .saturating_add(payload.len() as u64);
                self.metrics.decode_operations = self.metrics.decode_operations.saturating_add(1);
                let maximum_decoded_bytes = decoder
                    .retained_decoded_bytes(&payload)
                    .map_err(DesktopHistoryError::Decode)?;
                self.cache.reserve_publish_capacity(
                    request.identity,
                    maximum_decoded_bytes,
                    self.buffered_live_bytes,
                )?;
                let (values, decoded_bytes, watermark) =
                    self.decode_segment(&payload, decoder, maximum_decoded_bytes)?;
                let publication = self.cache.publish(
                    request.identity.clone(),
                    HistoryPublication {
                        generation: 1,
                        watermark,
                        source: CacheSource::LocalSegment,
                        startup_cache_state: request.startup_cache_state,
                        values,
                    },
                    decoded_bytes,
                    self.buffered_live_bytes,
                    Some(access_policy),
                )?;
                Ok(HydrationOutcome::Ready {
                    publication,
                    memory_cache_hit: false,
                })
            }
            AuthorizedHistoryRead::Unavailable { reason, recovery } => Ok(unavailable_outcome(
                reason,
                recovery,
                request.provider_state,
            )),
        }
    }

    /// Starts one bounded cached/backfill/live cutover.
    ///
    /// # Errors
    ///
    /// Returns an error when called outside the owning worker thread.
    pub fn begin_handoff(
        &mut self,
        identity: SegmentIdentity,
        encryption_key: &axiusflow_desktop_storage::SegmentEncryptionKey,
        now_unix_seconds: i64,
    ) -> Result<(), DesktopHistoryError> {
        self.ensure_owner()?;
        identity.validate()?;
        if self.handoffs.contains_key(&identity) {
            return Err(DesktopHistoryError::HandoffAlreadyStarted);
        }
        if self.handoffs.len() >= self.maximum_handoffs.get() {
            return Err(DesktopHistoryError::HandoffLimitReached {
                maximum: self.maximum_handoffs.get(),
            });
        }
        let publication =
            self.authorized_cached_publication(&identity, encryption_key, now_unix_seconds)?;
        let mut coordinator = HandoffCoordinator::new(self.maximum_buffered_live);
        if let Some(publication) = publication {
            match publication.source {
                CacheSource::LocalSegment => coordinator.require_snapshot(publication.watermark),
                CacheSource::ProviderSnapshot | CacheSource::ProviderLive => coordinator
                    .require_snapshot_after(publication.generation, publication.watermark),
            }
        }
        self.cache.pin(identity.clone());
        self.handoffs.insert(
            identity,
            HandoffEntry {
                coordinator,
                buffered_item_bytes: BTreeMap::new(),
            },
        );
        Ok(())
    }

    /// Retires an abandoned or disconnected handoff and releases its cache pin.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access or an unknown handoff identity.
    pub fn end_handoff(&mut self, identity: &SegmentIdentity) -> Result<(), DesktopHistoryError> {
        self.ensure_owner()?;
        let entry = self
            .handoffs
            .remove(identity)
            .ok_or(DesktopHistoryError::MissingHandoff)?;
        self.release_buffered_bytes(&entry);
        self.cache.unpin(identity);
        Ok(())
    }

    /// Buffers or atomically publishes one provider live item.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access, missing handoff state, sequence
    /// discontinuity, or a cache-bound violation. Loss errors retain the last
    /// published generation while latching snapshot-required recovery.
    pub fn push_live(
        &mut self,
        identity: &SegmentIdentity,
        item: SequencedHistory<T>,
        decoded_item_bytes: usize,
    ) -> Result<Option<Arc<HistoryPublication<T>>>, DesktopHistoryError> {
        self.ensure_owner()?;
        let observed_sequence = item.sequence.get();
        let mut candidate = self
            .handoffs
            .remove(identity)
            .ok_or(DesktopHistoryError::MissingHandoff)?;
        let acceptance = match candidate.coordinator.push_live(item) {
            Ok(acceptance) => acceptance,
            Err(error) => {
                candidate.coordinator.require_snapshot(observed_sequence);
                self.release_buffered_bytes(&candidate);
                candidate.buffered_item_bytes.clear();
                self.handoffs.insert(identity.clone(), candidate);
                return Err(error.into());
            }
        };
        match acceptance {
            LiveAcceptance::Buffered => {
                let buffered_bytes = self.buffered_live_bytes.saturating_add(decoded_item_bytes);
                let requested = self.cache.decoded_bytes().saturating_add(buffered_bytes);
                if requested > self.maximum_decoded_bytes.get() {
                    candidate.coordinator.require_snapshot(observed_sequence);
                    self.release_buffered_bytes(&candidate);
                    candidate.buffered_item_bytes.clear();
                    self.handoffs.insert(identity.clone(), candidate);
                    return Err(DesktopHistoryError::DecodedHistoryTooLarge {
                        requested,
                        maximum: self.maximum_decoded_bytes.get(),
                    });
                }
                candidate
                    .buffered_item_bytes
                    .insert(observed_sequence, decoded_item_bytes);
                self.buffered_live_bytes = buffered_bytes;
                self.handoffs.insert(identity.clone(), candidate);
                Ok(None)
            }
            LiveAcceptance::Duplicate => {
                self.metrics.duplicate_live_items =
                    self.metrics.duplicate_live_items.saturating_add(1);
                self.handoffs.insert(identity.clone(), candidate);
                Ok(None)
            }
            LiveAcceptance::Accepted(item) => {
                let Some((previous, access_policy)) = self.cache.get(identity) else {
                    self.handoffs.insert(identity.clone(), candidate);
                    return Err(DesktopHistoryError::MissingHandoff);
                };
                let Some(previous_decoded_bytes) = self.cache.decoded_bytes_for(identity) else {
                    self.handoffs.insert(identity.clone(), candidate);
                    return Err(DesktopHistoryError::MissingHandoff);
                };
                let decoded_bytes = previous_decoded_bytes.saturating_add(decoded_item_bytes);
                if let Err(error) = self.cache.reserve_publish_capacity(
                    identity,
                    decoded_bytes,
                    self.buffered_live_bytes,
                ) {
                    candidate.coordinator.require_snapshot(observed_sequence);
                    candidate.buffered_item_bytes.clear();
                    self.handoffs.insert(identity.clone(), candidate);
                    return Err(error);
                }
                let generation = previous.generation;
                let startup_cache_state = previous.startup_cache_state;
                let mut values = previous.values.clone();
                values.push(item);
                drop(previous);
                let publication = match self.cache.publish(
                    identity.clone(),
                    HistoryPublication {
                        generation,
                        watermark: values.last().map_or(0, |value| value.sequence.get()),
                        source: CacheSource::ProviderLive,
                        startup_cache_state,
                        values,
                    },
                    decoded_bytes,
                    self.buffered_live_bytes,
                    access_policy,
                ) {
                    Ok(publication) => publication,
                    Err(error) => {
                        candidate.coordinator.require_snapshot(observed_sequence);
                        candidate.buffered_item_bytes.clear();
                        self.handoffs.insert(identity.clone(), candidate);
                        return Err(error);
                    }
                };
                self.metrics.live_items = self.metrics.live_items.saturating_add(1);
                self.handoffs.insert(identity.clone(), candidate);
                Ok(Some(publication))
            }
        }
    }

    /// Atomically publishes a verified snapshot with its contiguous live suffix.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access, missing handoff state, invalid
    /// cutover continuity, or a cache-bound violation. Invalid recovery
    /// snapshots retain the last publication and the snapshot-required floor.
    pub fn install_snapshot(
        &mut self,
        identity: &SegmentIdentity,
        snapshot: VerifiedHistorySnapshot<T>,
        decoded_bytes: usize,
        startup_cache_state: StartupCacheState,
    ) -> Result<Arc<HistoryPublication<T>>, DesktopHistoryError> {
        self.ensure_owner()?;
        let mut candidate = self
            .handoffs
            .remove(identity)
            .ok_or(DesktopHistoryError::MissingHandoff)?;
        let retained_live_bytes = candidate
            .buffered_item_bytes
            .range((Excluded(snapshot.watermark()), Unbounded))
            .fold(0_usize, |total, (_, bytes)| total.saturating_add(*bytes));
        let decoded_bytes = decoded_bytes.saturating_add(retained_live_bytes);
        let candidate_buffered_bytes = candidate
            .buffered_item_bytes
            .values()
            .fold(0_usize, |total, bytes| total.saturating_add(*bytes));
        let other_buffered_bytes = self
            .buffered_live_bytes
            .saturating_sub(candidate_buffered_bytes);
        let requested = decoded_bytes.saturating_add(other_buffered_bytes);
        if requested > self.maximum_decoded_bytes.get() {
            self.handoffs.insert(identity.clone(), candidate);
            return Err(DesktopHistoryError::DecodedHistoryTooLarge {
                requested,
                maximum: self.maximum_decoded_bytes.get(),
            });
        }
        let batch = match candidate.coordinator.install_snapshot(snapshot) {
            Ok(batch) => batch,
            Err(error) => {
                self.release_buffered_bytes(&candidate);
                if matches!(
                    candidate.coordinator.state(),
                    axiusflow_provider_history::HandoffState::SnapshotRequired { .. }
                ) {
                    latch_snapshot_required(&mut candidate);
                }
                self.handoffs.insert(identity.clone(), candidate);
                return Err(error.into());
            }
        };
        let generation = batch.snapshot.generation();
        let snapshot_watermark = batch.snapshot.watermark();
        self.buffered_live_bytes = other_buffered_bytes;
        candidate.buffered_item_bytes.clear();
        let mut values = batch.snapshot.into_items();
        values.extend(batch.live);
        let watermark = values
            .last()
            .map_or(snapshot_watermark, |value| value.sequence.get());
        let publication = match self.cache.publish(
            identity.clone(),
            HistoryPublication {
                generation,
                watermark,
                source: CacheSource::ProviderSnapshot,
                startup_cache_state,
                values,
            },
            decoded_bytes,
            self.buffered_live_bytes,
            None,
        ) {
            Ok(publication) => publication,
            Err(error) => {
                candidate.coordinator.require_snapshot(watermark);
                self.handoffs.insert(identity.clone(), candidate);
                return Err(error);
            }
        };
        self.metrics.provider_snapshots = self.metrics.provider_snapshots.saturating_add(1);
        self.handoffs.insert(identity.clone(), candidate);
        Ok(publication)
    }

    /// Proves cache capacity for a provider snapshot before decoding it.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong-thread access or when the decoded snapshot
    /// and buffered live data cannot fit within the configured cache bounds.
    pub fn check_snapshot_capacity(
        &self,
        identity: &SegmentIdentity,
        snapshot_watermark: u64,
        decoded_bytes: usize,
    ) -> Result<(), DesktopHistoryError> {
        self.ensure_owner()?;
        let candidate = self
            .handoffs
            .get(identity)
            .ok_or(DesktopHistoryError::MissingHandoff)?;
        let retained_live_bytes = candidate
            .buffered_item_bytes
            .range((Excluded(snapshot_watermark), Unbounded))
            .fold(0_usize, |total, (_, bytes)| total.saturating_add(*bytes));
        let candidate_buffered_bytes = candidate
            .buffered_item_bytes
            .values()
            .fold(0_usize, |total, bytes| total.saturating_add(*bytes));
        let other_buffered_bytes = self
            .buffered_live_bytes
            .saturating_sub(candidate_buffered_bytes);
        self.cache.check_publish_capacity(
            identity,
            decoded_bytes.saturating_add(retained_live_bytes),
            other_buffered_bytes,
        )
    }

    fn ensure_owner(&self) -> Result<(), DesktopHistoryError> {
        if thread::current().id() != self.owner_thread {
            return Err(DesktopHistoryError::WorkerThreadMismatch);
        }
        Ok(())
    }

    fn authorized_cached_publication(
        &mut self,
        identity: &SegmentIdentity,
        encryption_key: &axiusflow_desktop_storage::SegmentEncryptionKey,
        now_unix_seconds: i64,
    ) -> Result<Option<Arc<HistoryPublication<T>>>, DesktopHistoryError> {
        let Some((publication, access_policy)) = self.cache.get(identity) else {
            return Ok(None);
        };
        if let Some(access_policy) = access_policy
            && let Some((reason, _)) = access_policy.validate(encryption_key, now_unix_seconds)?
        {
            self.cache.remove(identity);
            if reason == AvailabilityReason::Expired {
                self.store.remove_if_expired(identity, now_unix_seconds)?;
            }
            return Ok(None);
        }
        Ok(Some(publication))
    }

    fn decode_segment<D: HistoryDecoder<T>>(
        &mut self,
        payload: &[u8],
        decoder: &mut D,
        maximum_decoded_bytes: usize,
    ) -> Result<(Vec<SequencedHistory<T>>, usize, u64), DesktopHistoryError> {
        if maximum_decoded_bytes == 0 {
            return Err(DesktopHistoryError::DecodedHistoryTooLarge {
                requested: 1,
                maximum: 0,
            });
        }
        let (values, decoded_bytes) = decoder
            .decode(payload, maximum_decoded_bytes)
            .map_err(DesktopHistoryError::Decode)?;
        if decoded_bytes > maximum_decoded_bytes {
            return Err(DesktopHistoryError::DecodedHistoryTooLarge {
                requested: decoded_bytes,
                maximum: maximum_decoded_bytes,
            });
        }
        let watermark = validate_decoded_history(&values)?;
        self.metrics.decoded_bytes = self
            .metrics
            .decoded_bytes
            .saturating_add(decoded_bytes as u64);
        Ok((values, decoded_bytes, watermark))
    }

    fn release_buffered_bytes(&mut self, entry: &HandoffEntry<T>) {
        let released = entry
            .buffered_item_bytes
            .values()
            .fold(0_usize, |total, bytes| total.saturating_add(*bytes));
        self.buffered_live_bytes = self.buffered_live_bytes.saturating_sub(released);
    }
}

fn validate_decoded_history<T>(values: &[SequencedHistory<T>]) -> Result<u64, DesktopHistoryError> {
    let Some(last) = values.last() else {
        return Err(DesktopHistoryError::Decode(
            "decoded segment contains no history values".to_string(),
        ));
    };
    if values
        .windows(2)
        .any(|pair| pair[0].sequence.get().checked_add(1) != Some(pair[1].sequence.get()))
    {
        return Err(DesktopHistoryError::Decode(
            "decoded segment sequence is not contiguous".to_string(),
        ));
    }
    Ok(last.sequence.get())
}

fn unavailable_outcome<T>(
    reason: AvailabilityReason,
    recovery: RecoveryAction,
    provider_state: ProviderConnectionState,
) -> HydrationOutcome<T> {
    match recovery {
        RecoveryAction::ProviderRefetch if provider_state == ProviderConnectionState::Online => {
            HydrationOutcome::ProviderFetchRequired { reason }
        }
        RecoveryAction::ProviderRefetch => {
            HydrationOutcome::OfflineUnavailable { reason, recovery }
        }
        RecoveryAction::LiveOnly => HydrationOutcome::LiveOnly { reason },
    }
}

fn latch_snapshot_required<T>(entry: &mut HandoffEntry<T>) {
    if let axiusflow_provider_history::HandoffState::SnapshotRequired {
        minimum_watermark, ..
    } = entry.coordinator.state()
    {
        entry.coordinator.require_snapshot(minimum_watermark);
    }
    entry.buffered_item_bytes.clear();
}
