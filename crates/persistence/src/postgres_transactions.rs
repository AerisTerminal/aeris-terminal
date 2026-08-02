//! `PostgreSQL` transactions behind the outbox and inbox contracts.
//!
//! One `PostgresTransaction` wraps one live database transaction. Staged outbox
//! records become visible only when the caller commits the same transaction that
//! carried the authoritative change, and inbox receipts deduplicate inside the
//! transaction applying the effect. Recovery is the unpublished-outbox query:
//! after any crash, everything committed but unpublished is redeliverable in
//! commit order.

use crate::errors::PersistenceError;
use crate::{OutboxRecord, TransactionalInbox, TransactionalOutbox};
use postgres::Transaction;

/// Largest outbox payload accepted.
pub const MAXIMUM_OUTBOX_PAYLOAD_BYTES: usize = 1_048_576;
/// Largest event/topic identifier accepted.
const MAXIMUM_IDENTIFIER_BYTES: usize = 256;

/// One live `PostgreSQL` transaction behind the persistence contracts.
pub struct PostgresTransaction<'client> {
    transaction: Transaction<'client>,
}

impl<'client> PostgresTransaction<'client> {
    /// Wraps one already-open transaction.
    #[must_use]
    pub const fn new(transaction: Transaction<'client>) -> Self {
        Self { transaction }
    }

    /// Commits the transaction, making staged outbox records deliverable.
    ///
    /// # Errors
    ///
    /// Returns an error when the commit fails.
    pub fn commit(self) -> Result<(), PersistenceError> {
        self.transaction
            .commit()
            .map_err(|error| PersistenceError::Query(error.to_string()))
    }

    /// Rolls the transaction back explicitly; staged records never become visible.
    ///
    /// # Errors
    ///
    /// Returns an error when the rollback fails.
    pub fn rollback(self) -> Result<(), PersistenceError> {
        self.transaction
            .rollback()
            .map_err(|error| PersistenceError::Query(error.to_string()))
    }
}

impl TransactionalOutbox for PostgresTransaction<'_> {
    type Error = PersistenceError;

    fn insert(&mut self, record: OutboxRecord) -> Result<(), Self::Error> {
        if record.event_id.is_empty()
            || record.event_id.len() > MAXIMUM_IDENTIFIER_BYTES
            || record.topic.is_empty()
            || record.topic.len() > MAXIMUM_IDENTIFIER_BYTES
        {
            return Err(PersistenceError::InvalidIdentifier);
        }
        if record.payload.len() > MAXIMUM_OUTBOX_PAYLOAD_BYTES {
            return Err(PersistenceError::PayloadTooLarge(record.payload.len()));
        }
        let staged_at = unix_nanos_now();
        self.transaction
            .execute(
                "insert into axiusflow_outbox
                    (event_id, topic, partition_key, payload, staged_at_unix_nanos)
                values ($1, $2, $3, $4, $5)",
                &[
                    &record.event_id,
                    &record.topic,
                    &record.partition_key,
                    &record.payload,
                    &staged_at,
                ],
            )
            .map_err(|error| PersistenceError::Query(error.to_string()))?;
        Ok(())
    }
}

impl TransactionalInbox for PostgresTransaction<'_> {
    type Error = PersistenceError;

    fn record_once(&mut self, event_id: &str) -> Result<bool, Self::Error> {
        if event_id.is_empty() || event_id.len() > MAXIMUM_IDENTIFIER_BYTES {
            return Err(PersistenceError::InvalidIdentifier);
        }
        let received_at = unix_nanos_now();
        let affected = self
            .transaction
            .execute(
                "insert into axiusflow_inbox (event_id, received_at_unix_nanos)
                values ($1, $2)
                on conflict (event_id) do nothing",
                &[&event_id, &received_at],
            )
            .map_err(|error| PersistenceError::Query(error.to_string()))?;
        Ok(affected == 1)
    }
}

/// One unpublished outbox record in commit order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingOutboxRecord {
    pub sequence: i64,
    pub event_id: String,
    pub topic: String,
    pub partition_key: Vec<u8>,
    pub payload: Vec<u8>,
}

/// Reads every committed-but-unpublished outbox record in commit order.
///
/// # Errors
///
/// Returns an error when the query fails.
pub fn read_pending_outbox(
    client: &mut postgres::Client,
) -> Result<Vec<PendingOutboxRecord>, PersistenceError> {
    let rows = client
        .query(
            "select sequence, event_id, topic, partition_key, payload
            from axiusflow_outbox
            where published_at_unix_nanos is null
            order by sequence",
            &[],
        )
        .map_err(|error| PersistenceError::Query(error.to_string()))?;
    Ok(rows
        .iter()
        .map(|row| PendingOutboxRecord {
            sequence: row.get(0),
            event_id: row.get(1),
            topic: row.get(2),
            partition_key: row.get(3),
            payload: row.get(4),
        })
        .collect())
}

/// Marks one outbox record published after its delivery evidence exists.
///
/// # Errors
///
/// Returns an error when the update fails.
pub fn mark_outbox_published(
    client: &mut postgres::Client,
    sequence: i64,
    published_at_unix_nanos: i64,
) -> Result<(), PersistenceError> {
    let affected = client
        .execute(
            "update axiusflow_outbox
            set published_at_unix_nanos = $2
            where sequence = $1 and published_at_unix_nanos is null",
            &[&sequence, &published_at_unix_nanos],
        )
        .map_err(|error| PersistenceError::Query(error.to_string()))?;
    if affected != 1 {
        return Err(PersistenceError::Query(format!(
            "outbox sequence {sequence} was not unpublished"
        )));
    }
    Ok(())
}

fn unix_nanos_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// Connects with an explicit timeout-bounded configuration.
///
/// # Errors
///
/// Returns an error for invalid fields or a failed connection.
pub fn connect_postgres(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    database: &str,
) -> Result<postgres::Client, PersistenceError> {
    for value in [host, user, password, database] {
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(PersistenceError::InvalidIdentifier);
        }
    }
    postgres::Client::connect(
        &format!("host={host} port={port} user={user} password={password} dbname={database} connect_timeout=10"),
        postgres::NoTls,
    )
    .map_err(|error| PersistenceError::Connection(error.to_string()))
}
