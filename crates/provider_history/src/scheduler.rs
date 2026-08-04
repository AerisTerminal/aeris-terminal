use crate::{
    Continuation, HistoryCapabilities, HistoryPage, HistoryPageRequest, HistoryRange,
    ProviderHistoryError,
    page_validation::{continuation_progresses, validate_page},
    rate_gate::RateGates,
    scheduling_model::{
        CancelOutcome, Completion, Dispatch, DispatchOutcome, ExpiredRequest, FetchFailureOutcome,
        InflightEntry, QueueEntry, RequestInterest, RequestPriority, SchedulerConfig, Submission,
        class_inflight,
    },
};
use std::collections::{BTreeMap, BTreeSet};

/// Deterministic bounded scheduler for one provider capability matrix.
pub struct HistoryScheduler {
    capabilities: HistoryCapabilities,
    config: SchedulerConfig,
    queued: BTreeMap<HistoryPageRequest, QueueEntry>,
    inflight: BTreeMap<u64, InflightEntry>,
    interest_requests: BTreeMap<RequestInterest, BTreeSet<HistoryPageRequest>>,
    rate_gates: RateGates,
    next_order: u64,
    next_dispatch_id: u64,
}

impl HistoryScheduler {
    /// Creates a scheduler with explicit finite bounds.
    ///
    /// # Errors
    ///
    /// Returns an error when adjacent prefetch exceeds the supported pair.
    pub fn try_new(
        capabilities: HistoryCapabilities,
        config: SchedulerConfig,
    ) -> Result<Self, ProviderHistoryError> {
        if config.adjacent_prefetch_windows > 2 {
            return Err(ProviderHistoryError::InvalidConfiguration(
                "adjacent prefetch windows must be within 0..=2",
            ));
        }
        Ok(Self {
            capabilities,
            config,
            queued: BTreeMap::new(),
            inflight: BTreeMap::new(),
            interest_requests: BTreeMap::new(),
            rate_gates: RateGates::default(),
            next_order: 1,
            next_dispatch_id: 1,
        })
    }

    /// Adds a request, deduplicating exact work and upgrading its priority.
    ///
    /// # Errors
    ///
    /// Returns an error for capability violations, duplicate interest use, or bounds.
    pub fn submit(
        &mut self,
        request: HistoryPageRequest,
        interest: RequestInterest,
        priority: RequestPriority,
        now_unix_nanos: i64,
    ) -> Result<Submission, ProviderHistoryError> {
        if self.interest_requests.contains_key(&interest) {
            return Err(ProviderHistoryError::InterestAlreadyScheduled {
                interest: interest.get(),
            });
        }
        self.capabilities
            .validate_request(&request, now_unix_nanos)?;
        let continuations = initial_continuations(&request);
        let deduplicated = usize::from(self.add_request(
            request,
            interest,
            priority,
            &continuations,
            now_unix_nanos,
        )?);
        Ok(Submission {
            queued_new: 1 - deduplicated,
            deduplicated,
            adjacent_prefetches: 0,
        })
    }

    /// Adds a visible range plus bounded previous/next windows under one interest.
    ///
    /// # Errors
    ///
    /// Returns an error for capability violations, duplicate interest use, or bounds.
    pub fn submit_visible_with_prefetch(
        &mut self,
        request: &HistoryPageRequest,
        interest: RequestInterest,
        now_unix_nanos: i64,
    ) -> Result<Submission, ProviderHistoryError> {
        if self.interest_requests.contains_key(&interest) {
            return Err(ProviderHistoryError::InterestAlreadyScheduled {
                interest: interest.get(),
            });
        }
        self.capabilities
            .validate_request(request, now_unix_nanos)?;
        let mut requests = vec![(request.clone(), RequestPriority::Visible)];
        let span = i64::try_from(request.range.span_nanos()?)
            .map_err(|_| ProviderHistoryError::RequestSpanExceeded)?;
        if self.config.adjacent_prefetch_windows >= 1
            && let (Some(start), Some(end)) = (
                request.range.start_unix_nanos.checked_sub(span),
                request.range.end_unix_nanos.checked_sub(span),
            )
        {
            let mut previous = request.clone();
            previous.range = HistoryRange {
                start_unix_nanos: start,
                end_unix_nanos: end,
            };
            previous.continuation = None;
            if self
                .capabilities
                .validate_request(&previous, now_unix_nanos)
                .is_ok()
            {
                requests.push((previous, RequestPriority::AdjacentPrefetch));
            }
        }
        if self.config.adjacent_prefetch_windows >= 2 {
            let end = request
                .range
                .end_unix_nanos
                .saturating_add(span)
                .min(now_unix_nanos);
            let start = request.range.end_unix_nanos;
            if start < end {
                let mut next = request.clone();
                next.range = HistoryRange {
                    start_unix_nanos: start,
                    end_unix_nanos: end,
                };
                next.continuation = None;
                if self
                    .capabilities
                    .validate_request(&next, now_unix_nanos)
                    .is_ok()
                {
                    requests.push((next, RequestPriority::AdjacentPrefetch));
                }
            }
        }
        let required_new = requests
            .iter()
            .filter(|(candidate, _)| !self.contains_request(candidate))
            .count();
        if self.queued.len().saturating_add(required_new)
            > self.config.maximum_queued_requests.get()
        {
            return Err(ProviderHistoryError::QueueFull {
                maximum: self.config.maximum_queued_requests.get(),
            });
        }
        let incoming = BTreeSet::from([interest]);
        for (candidate, _) in &requests {
            self.ensure_interest_capacity(candidate, &incoming)?;
        }
        let adjacent_prefetches = requests.len().saturating_sub(1);
        let mut deduplicated = 0;
        for (candidate, priority) in requests {
            let continuations = initial_continuations(&candidate);
            deduplicated += usize::from(self.add_request(
                candidate,
                interest,
                priority,
                &continuations,
                now_unix_nanos,
            )?);
        }
        Ok(Submission {
            queued_new: adjacent_prefetches
                .saturating_add(1)
                .saturating_sub(deduplicated),
            deduplicated,
            adjacent_prefetches,
        })
    }

    /// Returns the highest-priority rate-eligible request and reports expired work.
    /// Unix time validates provider lookback while monotonic time gates request quotas.
    ///
    /// # Errors
    ///
    /// Returns an error if a stored non-time-based capability becomes unavailable.
    pub fn dispatch_next(
        &mut self,
        now_unix_nanos: i64,
        now_monotonic_nanos: u64,
    ) -> Result<DispatchOutcome, ProviderHistoryError> {
        let mut candidates = self
            .queued
            .values()
            .map(|entry| (entry.priority, entry.order, entry.request.clone()))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(priority, order, _)| (*priority, *order));
        let mut expired = Vec::new();
        let mut valid = Vec::with_capacity(candidates.len());
        for (priority, order, request) in candidates {
            match self.capabilities.validate_request(&request, now_unix_nanos) {
                Ok(()) => valid.push((priority, order, request)),
                Err(ProviderHistoryError::LookbackExceeded) => {
                    let entry = self.queued.remove(&request).ok_or(
                        ProviderHistoryError::InvalidConfiguration("queued request disappeared"),
                    )?;
                    self.detach_interests(&entry.request, &entry.interests);
                    expired.push(ExpiredRequest {
                        request: entry.request,
                        interests: entry.interests.into_iter().collect(),
                    });
                }
                Err(error) => return Err(error),
            }
        }
        if self.inflight.len() >= self.config.maximum_total_inflight.get() {
            return Ok(DispatchOutcome {
                dispatch: None,
                expired,
            });
        }
        let mut selected = None;
        for (_, _, request) in valid {
            if self.dispatch_allowed(request.data_class, now_monotonic_nanos)? {
                selected = Some(request);
                break;
            }
        }
        let Some(request) = selected else {
            return Ok(DispatchOutcome {
                dispatch: None,
                expired,
            });
        };
        let dispatch_id = self.next_dispatch_id;
        let next_dispatch_id = self.next_dispatch_id.checked_add(1).ok_or(
            ProviderHistoryError::InvalidConfiguration("dispatch id exhausted"),
        )?;
        let entry =
            self.queued
                .remove(&request)
                .ok_or(ProviderHistoryError::InvalidConfiguration(
                    "queued request disappeared",
                ))?;
        let attempts =
            entry
                .attempts
                .checked_add(1)
                .ok_or(ProviderHistoryError::InvalidConfiguration(
                    "fetch attempt count exhausted",
                ))?;
        self.rate_gates.consume(
            &self.capabilities,
            entry.request.data_class,
            now_monotonic_nanos,
        )?;
        self.next_dispatch_id = next_dispatch_id;
        self.inflight.insert(
            dispatch_id,
            InflightEntry {
                request: entry.request.clone(),
                priority: entry.priority,
                interests: entry.interests,
                continuations: entry.continuations,
                accepted_at_unix_nanos: now_unix_nanos,
                order: entry.order,
                attempts,
                aborting: false,
            },
        );
        Ok(DispatchOutcome {
            dispatch: Some(Dispatch {
                dispatch_id,
                request: entry.request,
                priority: entry.priority,
            }),
            expired,
        })
    }

    /// Validates a provider completion and schedules its next page when present.
    ///
    /// # Errors
    ///
    /// Returns an error for stale/mismatched dispatches or malformed pages.
    pub fn complete(
        &mut self,
        dispatch_id: u64,
        page: HistoryPage,
        _completion_unix_nanos: i64,
    ) -> Result<Completion, ProviderHistoryError> {
        let inflight = self
            .inflight
            .get(&dispatch_id)
            .cloned()
            .ok_or(ProviderHistoryError::UnknownDispatch { dispatch_id })?;
        if page.request != inflight.request {
            return Err(ProviderHistoryError::CompletionMismatch);
        }
        validate_page(&self.capabilities, &page, inflight.accepted_at_unix_nanos)?;
        let next_request = (!inflight.interests.is_empty())
            .then(|| page.next.clone())
            .flatten()
            .map(|next| {
                let mut request = inflight.request.clone();
                request.continuation = Some(next);
                request
            });
        let mut next_continuations = inflight.continuations.clone();
        if let Some(request) = &next_request {
            self.capabilities
                .validate_request(request, inflight.accepted_at_unix_nanos)?;
            let continuation = request
                .continuation
                .as_ref()
                .ok_or(ProviderHistoryError::InvalidContinuation)?;
            if !next_continuations.insert(continuation.clone())
                || next_continuations.len() > self.config.maximum_continuations_per_request.get()
                || !continuation_progresses(&inflight.request, continuation)
            {
                return Err(ProviderHistoryError::InvalidContinuation);
            }
            if !self.contains_request(request)
                && self.queued.len() >= self.config.maximum_queued_requests.get()
            {
                return Err(ProviderHistoryError::QueueFull {
                    maximum: self.config.maximum_queued_requests.get(),
                });
            }
            self.ensure_interest_capacity(request, &inflight.interests)?;
            self.ensure_continuation_capacity(request, &next_continuations)?;
        }
        self.inflight.remove(&dispatch_id);
        let interests = inflight.interests.iter().copied().collect::<Vec<_>>();
        self.detach_interests(&inflight.request, &inflight.interests);
        let continuation_scheduled = if let Some(request) = next_request {
            for interest in &interests {
                self.add_request(
                    request.clone(),
                    *interest,
                    inflight.priority,
                    &next_continuations,
                    inflight.accepted_at_unix_nanos,
                )?;
            }
            true
        } else {
            false
        };
        Ok(Completion {
            page,
            interests,
            continuation_scheduled,
        })
    }

    /// Cancels all queued and inflight requests owned by one interest.
    #[must_use]
    pub fn cancel(&mut self, interest: RequestInterest) -> CancelOutcome {
        let Some(requests) = self.interest_requests.remove(&interest) else {
            return CancelOutcome {
                queued_removed: 0,
                provider_aborts: Vec::new(),
            };
        };
        let mut queued_removed = 0;
        for request in requests {
            if let Some(entry) = self.queued.get_mut(&request) {
                entry.interests.remove(&interest);
                if entry.interests.is_empty() {
                    self.queued.remove(&request);
                    queued_removed += 1;
                }
            }
        }
        let mut provider_aborts = Vec::new();
        for (dispatch_id, entry) in &mut self.inflight {
            if entry.interests.remove(&interest) && entry.interests.is_empty() {
                provider_aborts.push(*dispatch_id);
                entry.aborting = true;
            }
        }
        CancelOutcome {
            queued_removed,
            provider_aborts,
        }
    }

    /// Finalizes a provider fetch failure without leaving its dispatch in flight.
    ///
    /// The request is retried only while its attempt bound and queue capacity permit it.
    /// Otherwise all remaining consumers are returned for terminal failure reporting.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown dispatch.
    pub fn fail_fetch(
        &mut self,
        dispatch_id: u64,
    ) -> Result<FetchFailureOutcome, ProviderHistoryError> {
        let entry = self
            .inflight
            .remove(&dispatch_id)
            .ok_or(ProviderHistoryError::UnknownDispatch { dispatch_id })?;
        let can_retry = !entry.aborting
            && !entry.interests.is_empty()
            && entry.attempts < self.config.maximum_fetch_attempts.get()
            && self.queued.len() < self.config.maximum_queued_requests.get();
        if can_retry {
            let attempts = entry.attempts;
            self.queued.insert(
                entry.request.clone(),
                QueueEntry {
                    request: entry.request,
                    priority: entry.priority,
                    order: entry.order,
                    interests: entry.interests,
                    continuations: entry.continuations,
                    accepted_at_unix_nanos: entry.accepted_at_unix_nanos,
                    attempts,
                },
            );
            return Ok(FetchFailureOutcome::Requeued { attempts });
        }
        let interests = entry.interests.iter().copied().collect::<Vec<_>>();
        self.detach_interests(&entry.request, &entry.interests);
        Ok(FetchFailureOutcome::Terminal {
            attempts: entry.attempts,
            interests,
        })
    }

    #[must_use]
    pub fn queued_len(&self) -> usize {
        self.queued.len()
    }

    #[must_use]
    pub fn inflight_len(&self) -> usize {
        self.inflight.len()
    }

    /// Releases one provider concurrency slot after an abort has completed.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown dispatch or one that is not aborting.
    pub fn acknowledge_abort(&mut self, dispatch_id: u64) -> Result<(), ProviderHistoryError> {
        let entry = self
            .inflight
            .get(&dispatch_id)
            .ok_or(ProviderHistoryError::UnknownDispatch { dispatch_id })?;
        if !entry.aborting {
            return Err(ProviderHistoryError::AbortNotPending { dispatch_id });
        }
        self.inflight.remove(&dispatch_id);
        Ok(())
    }

    fn add_request(
        &mut self,
        request: HistoryPageRequest,
        interest: RequestInterest,
        priority: RequestPriority,
        continuations: &BTreeSet<Continuation>,
        accepted_at_unix_nanos: i64,
    ) -> Result<bool, ProviderHistoryError> {
        if let Some(entry) = self
            .inflight
            .values_mut()
            .find(|entry| !entry.aborting && entry.request == request)
        {
            if entry.interests.len() >= self.config.maximum_interests_per_request.get()
                && !entry.interests.contains(&interest)
            {
                return Err(ProviderHistoryError::InvalidConfiguration(
                    "request interest bound reached",
                ));
            }
            entry.priority = entry.priority.min(priority);
            entry.interests.insert(interest);
            entry.continuations.extend(continuations.iter().cloned());
            entry.accepted_at_unix_nanos = entry.accepted_at_unix_nanos.min(accepted_at_unix_nanos);
            self.interest_requests
                .entry(interest)
                .or_default()
                .insert(request);
            return Ok(true);
        }
        if let Some(entry) = self.queued.get_mut(&request) {
            if entry.interests.len() >= self.config.maximum_interests_per_request.get()
                && !entry.interests.contains(&interest)
            {
                return Err(ProviderHistoryError::InvalidConfiguration(
                    "request interest bound reached",
                ));
            }
            entry.priority = entry.priority.min(priority);
            entry.interests.insert(interest);
            entry.continuations.extend(continuations.iter().cloned());
            entry.accepted_at_unix_nanos = entry.accepted_at_unix_nanos.min(accepted_at_unix_nanos);
            self.interest_requests
                .entry(interest)
                .or_default()
                .insert(request);
            return Ok(true);
        }
        if self.queued.len() >= self.config.maximum_queued_requests.get() {
            return Err(ProviderHistoryError::QueueFull {
                maximum: self.config.maximum_queued_requests.get(),
            });
        }
        let order = self.next_order;
        self.next_order =
            self.next_order
                .checked_add(1)
                .ok_or(ProviderHistoryError::InvalidConfiguration(
                    "request order exhausted",
                ))?;
        self.queued.insert(
            request.clone(),
            QueueEntry {
                request: request.clone(),
                priority,
                order,
                interests: BTreeSet::from([interest]),
                continuations: continuations.clone(),
                accepted_at_unix_nanos,
                attempts: 0,
            },
        );
        self.interest_requests
            .entry(interest)
            .or_default()
            .insert(request);
        Ok(false)
    }

    fn contains_request(&self, request: &HistoryPageRequest) -> bool {
        self.queued.contains_key(request)
            || self
                .inflight
                .values()
                .any(|entry| !entry.aborting && entry.request == *request)
    }

    fn ensure_interest_capacity(
        &self,
        request: &HistoryPageRequest,
        incoming: &BTreeSet<RequestInterest>,
    ) -> Result<(), ProviderHistoryError> {
        let existing = self
            .queued
            .get(request)
            .map(|entry| &entry.interests)
            .or_else(|| {
                self.inflight
                    .values()
                    .find(|entry| !entry.aborting && entry.request == *request)
                    .map(|entry| &entry.interests)
            });
        let combined = existing.map_or(incoming.len(), |existing| existing.union(incoming).count());
        if combined > self.config.maximum_interests_per_request.get() {
            return Err(ProviderHistoryError::InvalidConfiguration(
                "request interest bound reached",
            ));
        }
        Ok(())
    }

    fn ensure_continuation_capacity(
        &self,
        request: &HistoryPageRequest,
        incoming: &BTreeSet<Continuation>,
    ) -> Result<(), ProviderHistoryError> {
        let existing = self
            .queued
            .get(request)
            .map(|entry| &entry.continuations)
            .or_else(|| {
                self.inflight
                    .values()
                    .find(|entry| !entry.aborting && entry.request == *request)
                    .map(|entry| &entry.continuations)
            });
        let combined = existing.map_or(incoming.len(), |existing| existing.union(incoming).count());
        if combined > self.config.maximum_continuations_per_request.get() {
            return Err(ProviderHistoryError::InvalidContinuation);
        }
        Ok(())
    }

    fn dispatch_allowed(
        &self,
        data_class: crate::DataClass,
        now_monotonic_nanos: u64,
    ) -> Result<bool, ProviderHistoryError> {
        let inflight = class_inflight(
            self.inflight.values().map(|entry| entry.request.data_class),
            data_class,
        );
        self.rate_gates.allowed(
            &self.capabilities,
            data_class,
            inflight,
            now_monotonic_nanos,
        )
    }

    fn detach_interests(
        &mut self,
        request: &HistoryPageRequest,
        interests: &BTreeSet<RequestInterest>,
    ) {
        for interest in interests {
            if let Some(requests) = self.interest_requests.get_mut(interest) {
                requests.remove(request);
                if requests.is_empty() {
                    self.interest_requests.remove(interest);
                }
            }
        }
    }
}

fn initial_continuations(request: &HistoryPageRequest) -> BTreeSet<Continuation> {
    request.continuation.iter().cloned().collect()
}
