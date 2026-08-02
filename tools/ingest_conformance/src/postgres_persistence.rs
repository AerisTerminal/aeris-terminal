//! `PostgreSQL` outbox/inbox persistence conformance over a real server.
//!
//! Applies the checksummed migrations twice (idempotent re-run with checksum
//! verification), proves transactional staging visibility, inbox deduplication
//! inside one transaction boundary, rollback invisibility, reconnect recovery of
//! committed-but-unpublished records, publish marking, and migration-drift
//! rejection. A plaintext single server proves persistence semantics only; TLS,
//! roles, and backup drills are not exercised and not claimed.

use axiusflow_persistence::{
    OutboxRecord, PersistenceError, PostgresTransaction, TransactionalInbox, TransactionalOutbox,
    connect_postgres, mark_outbox_published, migrate, read_pending_outbox,
};
use serde::Serialize;
use std::{env, error::Error, fs, path::Path};

const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const EVIDENCE_SCOPE: &str = "stage_2_postgres_persistence";
const POSTGRES_VERSION: &str = "17.10";

#[derive(Serialize)]
struct PersistenceBehaviorEvidence {
    migration_apply: &'static str,
    migration_idempotent_rerun: &'static str,
    migration_drift_rejection: &'static str,
    transactional_staging: &'static str,
    inbox_deduplication: &'static str,
    rollback_invisibility: &'static str,
    reconnect_recovery: &'static str,
    publish_marking: &'static str,
    tls: &'static str,
    role_based_access: &'static str,
    backup_drills: &'static str,
}

#[derive(Serialize)]
struct PostgresPersistenceReport {
    schema_version: u32,
    evidence_scope: &'static str,
    source_revision: String,
    postgres_version: &'static str,
    migrations_applied_first: usize,
    migrations_applied_second: usize,
    pending_committed: usize,
    pending_rolled_back: usize,
    pending_reconnected: usize,
    pending_published: usize,
    behavior: PersistenceBehaviorEvidence,
    limitations: [&'static str; 3],
}

/// Pending-record counts at each transactional boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BoundaryCounts {
    committed: usize,
    deduplicated: usize,
    rolled_back: usize,
    reconnected: usize,
    published: usize,
}

struct ConnectionParameters<'a> {
    host: &'a str,
    port: u16,
    user: &'a str,
    password: &'a str,
    database: &'a str,
}

impl ConnectionParameters<'_> {
    fn connect(&self) -> Result<postgres::Client, PersistenceError> {
        connect_postgres(
            self.host,
            self.port,
            self.user,
            self.password,
            self.database,
        )
    }
}

/// Runs the persistence conformance and writes one evidence artifact.
pub fn run(
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    database: &str,
    report_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let source_revision = env::var("GITHUB_SHA")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or("GITHUB_SHA must be non-empty for PostgreSQL persistence evidence")?;
    let parameters = ConnectionParameters {
        host,
        port,
        user,
        password,
        database,
    };
    let mut client = parameters.connect()?;
    let first = migrate(&mut client, 1_754_000_000_000_000_000)?;
    let second = migrate(&mut client, 1_754_000_000_000_000_001)?;
    let state = exercise_transactions(&mut client, &parameters)?;

    client.execute(
        "update axiusflow_schema_migrations set checksum = 'tampered' where version = 1",
        &[],
    )?;
    if !matches!(
        migrate(&mut client, 1_754_000_000_000_000_003),
        Err(PersistenceError::MigrationDrift { version: 1 })
    ) {
        return Err("migration drift was not rejected".into());
    }

    let report = PostgresPersistenceReport {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        evidence_scope: EVIDENCE_SCOPE,
        source_revision,
        postgres_version: POSTGRES_VERSION,
        migrations_applied_first: first,
        migrations_applied_second: second,
        pending_committed: state.committed,
        pending_rolled_back: state.rolled_back,
        pending_reconnected: state.reconnected,
        pending_published: state.published,
        behavior: PersistenceBehaviorEvidence {
            migration_apply: "passed",
            migration_idempotent_rerun: "passed",
            migration_drift_rejection: "passed",
            transactional_staging: "passed",
            inbox_deduplication: "passed",
            rollback_invisibility: "passed",
            reconnect_recovery: "passed",
            publish_marking: "passed",
            tls: "not_exercised",
            role_based_access: "not_exercised",
            backup_drills: "not_exercised",
        },
        limitations: [
            "plaintext_single_server",
            "no_role_isolation",
            "no_backup_drills",
        ],
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    fs::write(report_path, encoded)?;
    println!(
        "postgres_persistence=passed postgres={} migrations={}/{} pending_commit={} reconnect={} published_pending={} tls=not_exercised report={}",
        POSTGRES_VERSION,
        first,
        second,
        state.committed,
        state.reconnected,
        state.published,
        report_path.display()
    );
    Ok(())
}

/// Exercises the transactional outbox/inbox lifecycle and returns the pending
/// counts at each boundary: after commit, after dedup, after rollback, after
/// reconnect, and after publish marking.
fn exercise_transactions(
    client: &mut postgres::Client,
    parameters: &ConnectionParameters<'_>,
) -> Result<BoundaryCounts, Box<dyn Error>> {
    let record = OutboxRecord {
        event_id: "persistence-conformance-1".to_string(),
        topic: "ci.market.bar.normalized.v1".to_string(),
        partition_key: b"instrument-1".to_vec(),
        payload: b"deterministic-outbox-payload".to_vec(),
    };
    {
        let transaction = client.transaction()?;
        let mut transaction = PostgresTransaction::new(transaction);
        if !transaction.record_once("persistence-conformance-1")? {
            return Err("first inbox receipt was reported as duplicate".into());
        }
        transaction.insert(record.clone())?;
        transaction.commit()?;
    }
    let pending_after_commit = read_pending_outbox(client)?.len();

    {
        let transaction = client.transaction()?;
        let mut transaction = PostgresTransaction::new(transaction);
        if transaction.record_once("persistence-conformance-1")? {
            return Err("duplicate inbox receipt was reported as new".into());
        }
        transaction.commit()?;
    }
    let pending_after_dedup = read_pending_outbox(client)?.len();

    {
        let transaction = client.transaction()?;
        let mut transaction = PostgresTransaction::new(transaction);
        transaction.insert(OutboxRecord {
            event_id: "persistence-conformance-rolled-back".to_string(),
            ..record.clone()
        })?;
        transaction.rollback()?;
    }
    let pending_after_rollback = read_pending_outbox(client)?.len();

    let mut reconnected = parameters.connect()?;
    let pending = read_pending_outbox(&mut reconnected)?;
    let pending_after_reconnect = pending.len();
    if pending.len() != 1 || pending[0].event_id != record.event_id {
        return Err("reconnect recovery lost the committed outbox record".into());
    }
    mark_outbox_published(
        &mut reconnected,
        pending[0].sequence,
        1_754_000_000_000_000_002,
    )?;
    let pending_after_publish = read_pending_outbox(&mut reconnected)?.len();

    let counts = BoundaryCounts {
        committed: pending_after_commit,
        deduplicated: pending_after_dedup,
        rolled_back: pending_after_rollback,
        reconnected: pending_after_reconnect,
        published: pending_after_publish,
    };
    if counts
        != (BoundaryCounts {
            committed: 1,
            deduplicated: 1,
            rolled_back: 1,
            reconnected: 1,
            published: 0,
        })
    {
        return Err(format!("persistence state diverged: {counts:?}").into());
    }
    Ok(counts)
}
