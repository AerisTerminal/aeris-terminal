//! Single in-process owner for bounded public economic and fundamentals context.
//!
//! The owner performs official-source network and credential work on one background worker.
//! Desktop surfaces receive immutable snapshots through [`ContextView`] and never fetch, cache,
//! schedule, or retain API keys themselves.

mod model;
mod sources;

pub use model::{
    ContextDataset, ContextMetric, ContextProvenance, ContextSnapshot, ContextSource,
    ContextSourceStatus, CotPosition, DegreeDayMetric, EconomicEvent, EventImportance,
    MAXIMUM_CONTEXT_ITEMS, SourceAvailability, SourcePublication,
};
pub use sources::OfficialContextFetcher;

use aeris_platform_runtime::{CredentialVault, NativeCredentialVault};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

const COMMAND_CAPACITY: usize = 64;
const REPLY_CAPACITY: usize = 1;
const MAXIMUM_STATUS_DETAIL_BYTES: usize = 256;
const CONTEXT_VAULT_SERVICE: &str = "aeris-context-data";
type Reply<T> = SyncSender<Result<T, String>>;
type PublicationWake = Arc<dyn Fn() + Send + Sync>;
type SharedPublicationWake = Arc<Mutex<Option<PublicationWake>>>;

/// Bounded source refresh cadence. Calendar schedules change more often than weekly reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextRefreshPolicy {
    pub calendar_seconds: u64,
    pub data_seconds: u64,
    pub retry_seconds: u64,
}

impl Default for ContextRefreshPolicy {
    fn default() -> Self {
        Self {
            calendar_seconds: 6 * 60 * 60,
            data_seconds: 60 * 60,
            retry_seconds: 5 * 60,
        }
    }
}

impl ContextRefreshPolicy {
    fn validate(self) -> Result<Self, String> {
        if self.calendar_seconds < 60 || self.data_seconds < 60 || self.retry_seconds < 30 {
            return Err("context refresh intervals are below their safe minimum".to_string());
        }
        Ok(self)
    }

    const fn interval_for(self, source: ContextSource) -> u64 {
        match source {
            ContextSource::Bls | ContextSource::Bea | ContextSource::FederalReserve => {
                self.calendar_seconds
            }
            ContextSource::Eia
            | ContextSource::Noaa
            | ContextSource::Cftc
            | ContextSource::Usda
            | ContextSource::UsdaWasde
            | ContextSource::UsdaFas
            | ContextSource::Fred => self.data_seconds,
        }
    }
}

/// Startup settings for the one context owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextServiceConfig {
    pub refresh: ContextRefreshPolicy,
    pub request_timeout: Duration,
}

impl Default for ContextServiceConfig {
    fn default() -> Self {
        Self {
            refresh: ContextRefreshPolicy::default(),
            request_timeout: Duration::from_secs(8),
        }
    }
}

impl ContextServiceConfig {
    fn validate(self) -> Result<Self, String> {
        self.refresh.validate()?;
        if !(Duration::from_secs(1)..=Duration::from_secs(30)).contains(&self.request_timeout) {
            return Err(
                "context request timeout must be between one and thirty seconds".to_string(),
            );
        }
        Ok(self)
    }
}

/// Classified fetch failure. Detail must be redacted and safe for presentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContextFetchError {
    pub availability: SourceAvailability,
    pub detail: String,
}

impl ContextFetchError {
    #[must_use]
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            availability: SourceAvailability::Unavailable,
            detail: bounded_detail(detail.into()),
        }
    }

    #[must_use]
    pub fn missing_credential(source: ContextSource) -> Self {
        Self {
            availability: SourceAvailability::MissingCredential,
            detail: format!("{} API key is not configured", source.label()),
        }
    }
}

/// Adapter boundary used only by the context worker.
pub trait ContextFetcher: Send + Sync + 'static {
    /// Fetches and validates one official source publication.
    ///
    /// # Errors
    /// Returns a redacted classified failure for missing credentials, transport, or wire errors.
    fn fetch(
        &self,
        source: ContextSource,
        credential: Option<&str>,
        fetched_unix_seconds: i64,
    ) -> Result<SourcePublication, ContextFetchError>;
}

/// Lock-bounded immutable read handle for GPUI and other consumers.
#[derive(Clone)]
pub struct ContextView {
    current: Arc<Mutex<Arc<ContextSnapshot>>>,
}

impl ContextView {
    #[must_use]
    pub fn snapshot(&self) -> Arc<ContextSnapshot> {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Cloneable command handle for the one context owner.
#[derive(Clone)]
pub struct ContextService {
    commands: SyncSender<Command>,
    runtime: Arc<ContextRuntime>,
    view: ContextView,
}

struct ContextRuntime {
    stopping: Arc<AtomicBool>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    publication_wake: SharedPublicationWake,
}

enum Command {
    Refresh(ContextSource),
    StoreCredential(ContextSource, Zeroizing<String>, Reply<()>),
    DeleteCredential(ContextSource, Reply<()>),
    Shutdown,
}

trait ContextCredentialStore: Send + 'static {
    fn load(&self, key: &str) -> Result<Option<Zeroizing<String>>, String>;
    fn store(&self, key: &str, value: &str) -> Result<(), String>;
    fn delete(&self, key: &str) -> Result<(), String>;
}

struct NativeContextCredentialStore {
    vault: NativeCredentialVault,
}

impl NativeContextCredentialStore {
    fn new() -> Result<Self, String> {
        NativeCredentialVault::new(CONTEXT_VAULT_SERVICE)
            .map(|vault| Self { vault })
            .map_err(|_| "context credential storage is unavailable".to_string())
    }
}

impl ContextCredentialStore for NativeContextCredentialStore {
    fn load(&self, key: &str) -> Result<Option<Zeroizing<String>>, String> {
        let secret = self
            .vault
            .load(key)
            .map_err(|_| "context credential storage is unavailable".to_string())?;
        secret
            .map(|bytes| {
                String::from_utf8(bytes)
                    .map(Zeroizing::new)
                    .map_err(|_| "stored context credential is invalid".to_string())
            })
            .transpose()
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        self.vault
            .store(key, value.as_bytes())
            .map_err(|_| "context credential storage is unavailable".to_string())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        self.vault
            .delete(key)
            .map_err(|_| "context credential storage is unavailable".to_string())
    }
}

impl ContextService {
    /// Starts the production official-source owner and its bounded worker.
    ///
    /// # Errors
    /// Returns an error when configuration or native credential storage is unavailable.
    pub fn start(config: ContextServiceConfig) -> Result<Self, String> {
        let config = config.validate()?;
        let fetcher = OfficialContextFetcher::new(config.request_timeout)?;
        let credentials = NativeContextCredentialStore::new()?;
        Self::start_with(config, Box::new(fetcher), Box::new(credentials))
    }

    fn start_with(
        config: ContextServiceConfig,
        fetcher: Box<dyn ContextFetcher>,
        credentials: Box<dyn ContextCredentialStore>,
    ) -> Result<Self, String> {
        let config = config.validate()?;
        let now = now_unix_seconds()?;
        let current = Arc::new(Mutex::new(Arc::new(ContextSnapshot::empty(now))));
        let view = ContextView {
            current: Arc::clone(&current),
        };
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let publication_wake = Arc::new(Mutex::new(None));
        let worker_wake = Arc::clone(&publication_wake);
        let worker = thread::Builder::new()
            .name("aeris-context-runtime".to_string())
            .spawn(move || {
                Coordinator::new(
                    config,
                    current,
                    fetcher,
                    credentials,
                    worker_stopping,
                    worker_wake,
                )
                .run(&receiver);
            })
            .map_err(|error| format!("failed to start context runtime: {error}"))?;
        Ok(Self {
            commands,
            runtime: Arc::new(ContextRuntime {
                stopping,
                worker: Mutex::new(Some(worker)),
                publication_wake,
            }),
            view,
        })
    }

    #[must_use]
    pub fn view(&self) -> ContextView {
        self.view.clone()
    }

    /// Installs the desktop scheduling wake used after immutable publications.
    pub fn set_publication_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self
            .runtime
            .publication_wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(wake);
    }

    /// Coalesces an explicit refresh request into the bounded owner queue.
    ///
    /// # Errors
    /// Returns an overload or stopped error rather than blocking the caller.
    pub fn request_refresh(&self, source: ContextSource) -> Result<(), String> {
        match self.commands.try_send(Command::Refresh(source)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err("context command queue is full".to_string()),
            Err(TrySendError::Disconnected(_)) => Err("context runtime is stopped".to_string()),
        }
    }

    /// Stores one user-owned API key in the native vault and schedules its source immediately.
    /// Call this blocking method only from background work.
    ///
    /// # Errors
    /// Returns validation, queue, vault, or worker errors without exposing the key.
    pub fn store_api_key(&self, source: ContextSource, value: String) -> Result<(), String> {
        if source.credential_key().is_none() {
            return Err(format!("{} does not use an API key", source.label()));
        }
        let value = Zeroizing::new(value);
        if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
            return Err("context API key is invalid".to_string());
        }
        self.request_reply(|reply| Command::StoreCredential(source, value, reply))
    }

    /// Deletes one user-owned API key. Call only from background work.
    ///
    /// # Errors
    /// Returns queue, vault, or worker errors.
    pub fn delete_api_key(&self, source: ContextSource) -> Result<(), String> {
        if source.credential_key().is_none() {
            return Err(format!("{} does not use an API key", source.label()));
        }
        self.request_reply(|reply| Command::DeleteCredential(source, reply))
    }

    fn request_reply<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (sender, receiver) = mpsc::sync_channel(REPLY_CAPACITY);
        self.commands
            .send(command(sender))
            .map_err(|_| "context runtime is stopped".to_string())?;
        receiver
            .recv()
            .map_err(|_| "context runtime stopped before replying".to_string())?
    }

    /// Stops the worker within an explicit caller-owned deadline.
    ///
    /// # Errors
    /// Returns an error when an in-flight official-source request has not reached its bounded
    /// timeout before the shutdown deadline. The worker remains fenced and exits afterward.
    pub fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        if !self.runtime.stopping.swap(true, Ordering::AcqRel) {
            let _ = self.commands.try_send(Command::Shutdown);
        }
        let deadline = Instant::now() + timeout;
        let mut worker = self
            .runtime
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while worker.as_ref().is_some_and(|worker| !worker.is_finished()) {
            if Instant::now() >= deadline {
                return Err("context runtime did not stop before its deadline".to_string());
            }
            thread::sleep(Duration::from_millis(5));
        }
        if let Some(worker) = worker.take() {
            worker
                .join()
                .map_err(|_| "context runtime worker panicked during shutdown".to_string())?;
        }
        Ok(())
    }
}

impl Drop for ContextService {
    fn drop(&mut self) {
        if Arc::strong_count(&self.runtime) == 1 {
            let _ = self.shutdown(Duration::from_secs(2));
        }
    }
}

struct Coordinator {
    config: ContextServiceConfig,
    current: Arc<Mutex<Arc<ContextSnapshot>>>,
    fetcher: Box<dyn ContextFetcher>,
    credentials: Box<dyn ContextCredentialStore>,
    snapshot: ContextSnapshot,
    due: BTreeMap<ContextSource, i64>,
    requested: HashSet<ContextSource>,
    stopping: Arc<AtomicBool>,
    publication_wake: SharedPublicationWake,
}

impl Coordinator {
    fn new(
        config: ContextServiceConfig,
        current: Arc<Mutex<Arc<ContextSnapshot>>>,
        fetcher: Box<dyn ContextFetcher>,
        credentials: Box<dyn ContextCredentialStore>,
        stopping: Arc<AtomicBool>,
        publication_wake: SharedPublicationWake,
    ) -> Self {
        let now = now_unix_seconds().unwrap_or(1);
        Self {
            config,
            current,
            fetcher,
            credentials,
            snapshot: ContextSnapshot::empty(now),
            due: ContextSource::ALL
                .into_iter()
                .enumerate()
                .map(|(index, source)| {
                    let stagger = i64::try_from(index).unwrap_or(0) * 2;
                    (source, now.saturating_add(stagger))
                })
                .collect(),
            requested: HashSet::new(),
            stopping,
            publication_wake,
        }
    }

    fn run(mut self, receiver: &Receiver<Command>) {
        loop {
            if self.stopping.load(Ordering::Acquire) {
                return;
            }
            if let Some(source) = self.next_due_source() {
                self.refresh(source);
                continue;
            }
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(Command::Refresh(source)) => {
                    self.requested.insert(source);
                }
                Ok(Command::StoreCredential(source, value, reply)) => {
                    let result = source.credential_key().map_or_else(
                        || Err(format!("{} does not use an API key", source.label())),
                        |key| self.credentials.store(key, &value),
                    );
                    if result.is_ok() {
                        self.requested.insert(source);
                    }
                    let _ = reply.send(result);
                }
                Ok(Command::DeleteCredential(source, reply)) => {
                    let result = source.credential_key().map_or_else(
                        || Err(format!("{} does not use an API key", source.label())),
                        |key| self.credentials.delete(key),
                    );
                    if result.is_ok() {
                        self.requested.insert(source);
                    }
                    let _ = reply.send(result);
                }
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn next_due_source(&mut self) -> Option<ContextSource> {
        if let Some(source) = ContextSource::ALL
            .into_iter()
            .find(|source| self.requested.contains(source))
        {
            self.requested.remove(&source);
            return Some(source);
        }
        let now = now_unix_seconds().ok()?;
        ContextSource::ALL
            .into_iter()
            .find(|source| self.due.get(source).is_some_and(|due| *due <= now))
    }

    fn refresh(&mut self, source: ContextSource) {
        let now = now_unix_seconds().unwrap_or(self.snapshot.published_unix_seconds.max(1));
        let credential = source
            .credential_key()
            .map_or(Ok(None), |key| self.credentials.load(key));
        let outcome = match credential {
            Ok(credential) => {
                self.fetcher
                    .fetch(source, credential.as_deref().map(String::as_str), now)
            }
            Err(detail) => Err(ContextFetchError::unavailable(detail)),
        };
        let status_index = self
            .snapshot
            .source_statuses
            .iter()
            .position(|status| status.source == source);
        let (availability, detail, next_interval) = match outcome {
            Ok(publication) => match publication.validate() {
                Ok(()) => {
                    self.merge(publication);
                    (
                        SourceAvailability::Available,
                        None,
                        self.config.refresh.interval_for(source),
                    )
                }
                Err(detail) => (
                    SourceAvailability::Unavailable,
                    Some(bounded_detail(detail)),
                    self.config.refresh.retry_seconds,
                ),
            },
            Err(error) => (
                error.availability,
                Some(bounded_detail(error.detail)),
                self.config.refresh.retry_seconds,
            ),
        };
        let next = now.saturating_add(i64::try_from(next_interval).unwrap_or(i64::MAX));
        self.due.insert(source, next);
        if let Some(index) = status_index {
            let status = &mut self.snapshot.source_statuses[index];
            status.availability = availability;
            status.last_attempt_unix_seconds = Some(now);
            if availability == SourceAvailability::Available {
                status.last_success_unix_seconds = Some(now);
            }
            status.next_refresh_unix_seconds = Some(next);
            status.detail = detail;
        }
        self.publish(now);
    }

    fn merge(&mut self, mut publication: SourcePublication) {
        retain_other_sources(
            &mut self.snapshot.economic_events,
            publication.source,
            |value| value.provenance.source,
        );
        retain_other_sources(&mut self.snapshot.energy, publication.source, |value| {
            value.provenance.source
        });
        retain_other_sources(&mut self.snapshot.weather, publication.source, |value| {
            value.provenance.source
        });
        retain_other_sources(
            &mut self.snapshot.commitments,
            publication.source,
            |value| value.provenance.source,
        );
        retain_other_sources(
            &mut self.snapshot.agriculture,
            publication.source,
            |value| value.provenance.source,
        );
        retain_other_sources(
            &mut self.snapshot.macro_observations,
            publication.source,
            |value| value.provenance.source,
        );
        self.snapshot
            .economic_events
            .append(&mut publication.economic_events);
        self.snapshot.energy.append(&mut publication.energy);
        self.snapshot.weather.append(&mut publication.weather);
        self.snapshot
            .commitments
            .append(&mut publication.commitments);
        self.snapshot
            .agriculture
            .append(&mut publication.agriculture);
        self.snapshot
            .macro_observations
            .append(&mut publication.macro_observations);
        self.snapshot
            .economic_events
            .sort_by_key(|event| event.scheduled_unix_seconds);
        self.snapshot
            .commitments
            .sort_by_key(|position| position.report_date_unix_seconds);
        bound_vec(&mut self.snapshot.economic_events);
        bound_vec(&mut self.snapshot.energy);
        bound_vec(&mut self.snapshot.weather);
        bound_vec(&mut self.snapshot.commitments);
        bound_vec(&mut self.snapshot.agriculture);
        bound_vec(&mut self.snapshot.macro_observations);
    }

    fn publish(&mut self, now: i64) {
        self.snapshot.revision = self.snapshot.revision.wrapping_add(1).max(1);
        self.snapshot.published_unix_seconds = now;
        *self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(self.snapshot.clone());
        let wake = self
            .publication_wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

fn retain_other_sources<T>(
    values: &mut Vec<T>,
    source: ContextSource,
    source_of: impl Fn(&T) -> ContextSource,
) {
    values.retain(|value| source_of(value) != source);
}

fn bound_vec<T>(values: &mut Vec<T>) {
    if values.len() > MAXIMUM_CONTEXT_ITEMS {
        values.drain(..values.len() - MAXIMUM_CONTEXT_ITEMS);
    }
}

fn bounded_detail(mut detail: String) -> String {
    if detail.len() > MAXIMUM_STATUS_DETAIL_BYTES {
        detail.truncate(MAXIMUM_STATUS_DETAIL_BYTES);
        while !detail.is_char_boundary(detail.len()) {
            detail.pop();
        }
    }
    detail
}

fn now_unix_seconds() -> Result<i64, String> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?;
    i64::try_from(elapsed.as_secs()).map_err(|_| "system clock exceeds supported range".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemoryCredentials(Mutex<HashMap<String, String>>);

    impl ContextCredentialStore for MemoryCredentials {
        fn load(&self, key: &str) -> Result<Option<Zeroizing<String>>, String> {
            Ok(self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(key)
                .cloned()
                .map(Zeroizing::new))
        }

        fn store(&self, key: &str, value: &str) -> Result<(), String> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(key.to_string(), value.to_string());
            Ok(())
        }

        fn delete(&self, key: &str) -> Result<(), String> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(key);
            Ok(())
        }
    }

    struct FixtureFetcher;

    impl ContextFetcher for FixtureFetcher {
        fn fetch(
            &self,
            source: ContextSource,
            credential: Option<&str>,
            fetched_unix_seconds: i64,
        ) -> Result<SourcePublication, ContextFetchError> {
            if source.credential_key().is_some() && credential.is_none() {
                return Err(ContextFetchError::missing_credential(source));
            }
            let mut publication = SourcePublication::empty(source);
            if source == ContextSource::Bls {
                publication.economic_events.push(EconomicEvent {
                    id: "bls:employment-situation:fixture".to_string(),
                    title: "Employment Situation".to_string(),
                    scheduled_unix_seconds: fetched_unix_seconds + 3_600,
                    importance: EventImportance::High,
                    provenance: ContextProvenance {
                        source,
                        source_url: "https://www.bls.gov/schedule/news_release/bls.ics".to_string(),
                        release_unix_seconds: fetched_unix_seconds,
                        fetched_unix_seconds,
                    },
                });
            }
            Ok(publication)
        }
    }

    fn service() -> ContextService {
        ContextService::start_with(
            ContextServiceConfig::default(),
            Box::new(FixtureFetcher),
            Box::new(MemoryCredentials::default()),
        )
        .expect("fixture context service starts")
    }

    fn wait_for(service: &ContextService, predicate: impl Fn(&ContextSnapshot) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let snapshot = service.view().snapshot();
            if predicate(&snapshot) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("context snapshot did not reach expected state");
    }

    #[test]
    fn owner_publishes_immutable_bounded_source_views() {
        let service = service();
        service
            .request_refresh(ContextSource::Bls)
            .expect("refresh accepted");
        wait_for(&service, |snapshot| !snapshot.economic_events.is_empty());
        let snapshot = service.view().snapshot();
        assert_eq!(snapshot.economic_events.len(), 1);
        assert_eq!(
            snapshot.economic_events[0].provenance.source,
            ContextSource::Bls
        );
        assert!(snapshot.revision > 0);
        service
            .shutdown(Duration::from_secs(1))
            .expect("context service stops");
    }

    #[test]
    fn missing_api_key_is_explicit_and_storing_one_retries_source() {
        let service = service();
        service
            .request_refresh(ContextSource::Fred)
            .expect("refresh accepted");
        wait_for(&service, |snapshot| {
            snapshot.source_statuses.iter().any(|status| {
                status.source == ContextSource::Fred
                    && status.availability == SourceAvailability::MissingCredential
            })
        });
        service
            .store_api_key(ContextSource::Fred, "fixture-key".to_string())
            .expect("key stored");
        wait_for(&service, |snapshot| {
            snapshot.source_statuses.iter().any(|status| {
                status.source == ContextSource::Fred
                    && status.availability == SourceAvailability::Available
            })
        });
        service
            .shutdown(Duration::from_secs(1))
            .expect("context service stops");
    }

    #[test]
    fn credential_validation_rejects_public_sources_and_control_bytes() {
        let service = service();
        assert!(
            service
                .store_api_key(ContextSource::Bls, "unused".to_string())
                .is_err()
        );
        assert!(
            service
                .store_api_key(ContextSource::Eia, "bad\nkey".to_string())
                .is_err()
        );
        service
            .shutdown(Duration::from_secs(1))
            .expect("context service stops");
    }
}
