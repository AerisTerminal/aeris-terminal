//! Offers the website download page to a build the launcher does not update (macOS and the
//! website installers). One background worker reads the published downloads; GPUI only
//! polls its result and decides when the next check is due.

use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

use aeris_observability::diagnostic;
use aeris_release_lifecycle::DownloadOfferError;

const CHECK_INTERVAL: Duration = Duration::from_hours(6);
const RETRY_INTERVAL: Duration = Duration::from_mins(30);

type CheckResult = Result<Option<String>, DownloadOfferError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum DownloadOfferState {
    Checking,
    Current,
    Available { latest_version: String },
    Unavailable,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct DownloadOfferPoll {
    pub changed: bool,
    /// A version offered for the first time this session.
    pub announce: Option<String>,
}

pub(super) struct DownloadOfferClient {
    requests: SyncSender<()>,
    results: Receiver<CheckResult>,
    state: DownloadOfferState,
    pending: bool,
    next_check_at: Instant,
    announced: Option<String>,
}

impl DownloadOfferClient {
    /// Starts the worker and its first check.
    pub(super) fn start() -> std::io::Result<Self> {
        let (request_tx, request_rx) = mpsc::sync_channel::<()>(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("aeris-download-offer".to_string())
            .spawn(move || {
                while request_rx.recv().is_ok() {
                    let result = aeris_release_lifecycle::newer_published_download(env!(
                        "CARGO_PKG_VERSION"
                    ));
                    if result_tx.send(result).is_err() {
                        return;
                    }
                }
            })?;
        Ok(Self::with_worker(request_tx, result_rx, Instant::now()))
    }

    fn with_worker(requests: SyncSender<()>, results: Receiver<CheckResult>, now: Instant) -> Self {
        Self {
            requests,
            results,
            state: DownloadOfferState::Checking,
            pending: false,
            next_check_at: now,
            announced: None,
        }
    }

    pub(super) const fn state(&self) -> &DownloadOfferState {
        &self.state
    }

    pub(super) fn poll(&mut self, now: Instant) -> DownloadOfferPoll {
        let mut poll = DownloadOfferPoll::default();
        if !self.pending && now >= self.next_check_at {
            match self.requests.try_send(()) {
                Ok(()) => self.pending = true,
                Err(mpsc::TrySendError::Full(())) => {}
                Err(mpsc::TrySendError::Disconnected(())) => {
                    diagnostic!("Aeris download offer worker stopped");
                    poll.changed = self.record_failure(now);
                }
            }
        }
        let result = match self.results.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return poll,
            Err(mpsc::TryRecvError::Disconnected) => {
                if self.pending {
                    self.pending = false;
                    diagnostic!("Aeris download offer worker stopped");
                    poll.changed |= self.record_failure(now);
                }
                return poll;
            }
        };
        self.pending = false;
        let state = match result {
            Ok(Some(latest_version)) => {
                self.next_check_at = now + CHECK_INTERVAL;
                if self.announced.as_ref() != Some(&latest_version) {
                    self.announced = Some(latest_version.clone());
                    poll.announce = Some(latest_version.clone());
                }
                DownloadOfferState::Available { latest_version }
            }
            Ok(None) => {
                self.next_check_at = now + CHECK_INTERVAL;
                DownloadOfferState::Current
            }
            Err(error) => {
                diagnostic!("Aeris download offer check degraded: {error}");
                poll.changed |= self.record_failure(now);
                return poll;
            }
        };
        poll.changed |= self.state != state;
        self.state = state;
        poll
    }

    /// A known newer version stays offered through a failed later check.
    fn record_failure(&mut self, now: Instant) -> bool {
        self.next_check_at = now + RETRY_INTERVAL;
        if matches!(self.state, DownloadOfferState::Available { .. }) {
            return false;
        }
        let changed = self.state != DownloadOfferState::Unavailable;
        self.state = DownloadOfferState::Unavailable;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> (
        DownloadOfferClient,
        Receiver<()>,
        SyncSender<CheckResult>,
        Instant,
    ) {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let now = Instant::now();
        (
            DownloadOfferClient::with_worker(request_tx, result_rx, now),
            request_rx,
            result_tx,
            now,
        )
    }

    #[test]
    fn a_newer_version_is_announced_once_and_rechecked_after_the_interval() {
        let (mut client, requests, results, start) = client();
        assert_eq!(client.poll(start), DownloadOfferPoll::default());
        requests.try_recv().expect("the first check starts at once");
        results.send(Ok(Some("1.3.0".to_string()))).expect("result");
        assert_eq!(
            client.poll(start),
            DownloadOfferPoll {
                changed: true,
                announce: Some("1.3.0".to_string()),
            }
        );
        assert_eq!(
            client.state(),
            &DownloadOfferState::Available {
                latest_version: "1.3.0".to_string()
            }
        );
        client.poll(start + CHECK_INTERVAL.saturating_sub(Duration::from_secs(1)));
        assert!(requests.try_recv().is_err(), "no check before the interval");
        client.poll(start + CHECK_INTERVAL);
        requests.try_recv().expect("the periodic check runs");
        results.send(Ok(Some("1.3.0".to_string()))).expect("result");
        assert_eq!(
            client.poll(start + CHECK_INTERVAL),
            DownloadOfferPoll::default(),
            "the same version is not announced again"
        );
    }

    #[test]
    fn a_failed_check_retries_sooner_and_keeps_a_known_offer() {
        let (mut client, requests, results, start) = client();
        client.poll(start);
        requests.try_recv().expect("first check");
        results
            .send(Err(DownloadOfferError::Malformed))
            .expect("result");
        assert!(client.poll(start).changed);
        assert_eq!(client.state(), &DownloadOfferState::Unavailable);
        client.poll(start + RETRY_INTERVAL);
        requests
            .try_recv()
            .expect("the retry runs after its interval");
        results.send(Ok(Some("1.3.0".to_string()))).expect("result");
        assert!(client.poll(start + RETRY_INTERVAL).announce.is_some());
        let later = start + RETRY_INTERVAL + CHECK_INTERVAL;
        client.poll(later);
        requests.try_recv().expect("periodic check");
        results
            .send(Err(DownloadOfferError::Malformed))
            .expect("result");
        assert_eq!(client.poll(later), DownloadOfferPoll::default());
        assert!(matches!(
            client.state(),
            DownloadOfferState::Available { .. }
        ));
    }

    #[test]
    fn an_up_to_date_build_shows_current() {
        let (mut client, requests, results, start) = client();
        client.poll(start);
        requests.try_recv().expect("first check");
        results.send(Ok(None)).expect("result");
        assert!(client.poll(start).changed);
        assert_eq!(client.state(), &DownloadOfferState::Current);
    }
}
