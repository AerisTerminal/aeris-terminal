//! Paged order history served by one reader thread on a read-only SQLite connection (M5.11).
//!
//! The owner never reads history for a client: `HistoryReader` opens its own connection with
//! `SQLITE_OPEN_READ_ONLY`, so a write attempt on it fails, and pages keyset-scan the orders
//! table by rowid (insertion order), at most [`MAXIMUM_HISTORY_PAGE`] rows per page.

use crate::store::order_history_page;
use crate::{Order, TradingAccountId};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};

/// A history page holds at most this many rows.
pub const MAXIMUM_HISTORY_PAGE: usize = 500;
/// History requests waiting on the single reader thread.
const HISTORY_REQUEST_CAPACITY: usize = 32;

/// A paged query over the durable order history.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HistoryQuery {
    /// Restrict to one account; `None` reads every account.
    pub account_id: Option<TradingAccountId>,
    /// Continue after this cursor in the selected direction.
    pub after: Option<HistoryCursor>,
    /// Start at the newest order and page toward older orders when true.
    pub newest_first: bool,
    /// Page size, clamped to [`MAXIMUM_HISTORY_PAGE`] (the default).
    pub limit: Option<usize>,
}

/// The row cursor continuing a paged history read.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct HistoryCursor(pub i64);

/// One page of order history.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryPage {
    pub orders: Vec<Order>,
    /// Pass as `HistoryQuery::after` for the next page; `None` when the history is exhausted.
    pub next: Option<HistoryCursor>,
}

enum HistoryRequest {
    Query(HistoryQuery, SyncSender<Result<HistoryPage, String>>),
    Stop,
}

/// The single history reader thread holding a read-only connection to the trading store.
pub struct HistoryReader {
    requests: SyncSender<HistoryRequest>,
    worker: Option<JoinHandle<()>>,
}

impl HistoryReader {
    /// Opens the store read-only and starts the reader thread.
    ///
    /// # Errors
    /// Returns an error when the database cannot be opened read-only or the thread fails.
    pub fn start(path: &Path) -> Result<Self, String> {
        let connection = crate::store::open_read_only(path)?;
        let (requests, receiver) = mpsc::sync_channel(HISTORY_REQUEST_CAPACITY);
        let worker = thread::Builder::new()
            .name("aeris-trading-history".to_string())
            .spawn(move || run_reader(&connection, &receiver))
            .map_err(|error| format!("trading history reader could not start: {error}"))?;
        Ok(Self {
            requests,
            worker: Some(worker),
        })
    }

    /// Reads one page of order history.
    ///
    /// # Errors
    /// Returns an error when the reader queue is full, the reader stopped, or the read fails.
    pub fn query(&self, query: HistoryQuery) -> Result<HistoryPage, String> {
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        self.requests
            .try_send(HistoryRequest::Query(query, reply_tx))
            .map_err(|error| match error {
                TrySendError::Full(_) => "trading history queue is full".to_string(),
                TrySendError::Disconnected(_) => {
                    "trading history reader is unavailable".to_string()
                }
            })?;
        reply_rx
            .recv()
            .map_err(|_| "trading history reader stopped before replying".to_string())?
    }

    fn stop(&mut self) {
        let _ = self.requests.try_send(HistoryRequest::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for HistoryReader {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_reader(connection: &rusqlite::Connection, requests: &Receiver<HistoryRequest>) {
    while let Ok(request) = requests.recv() {
        match request {
            HistoryRequest::Query(query, reply) => {
                let page = order_history_page(connection, &query);
                if reply.send(page).is_err() {
                    return;
                }
            }
            HistoryRequest::Stop => return,
        }
    }
}

/// The database path a history reader serves, when the store is file-backed. The in-memory
/// store of tests has no second connection, so it has no history reader.
pub(crate) fn history_database_path(database_path: &Path) -> Option<PathBuf> {
    if database_path == Path::new(":memory:") {
        None
    } else {
        Some(database_path.to_path_buf())
    }
}
