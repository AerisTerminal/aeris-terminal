//! Checksummed, ordered, transactionally applied schema migrations.
//!
//! Migration files are embedded at compile time, applied in version order inside
//! one transaction each, and recorded in `axiusflow_schema_migrations` with their
//! SHA-256 checksum. A re-run verifies every recorded checksum, so a mutated
//! migration file fails as drift instead of silently diverging the schema.

use crate::errors::PersistenceError;
use postgres::Client;
use sha2::{Digest, Sha256};

/// One embedded migration in application order.
struct EmbeddedMigration {
    version: i32,
    name: &'static str,
    sql: &'static str,
}

const MIGRATIONS: &[EmbeddedMigration] = &[EmbeddedMigration {
    version: 1,
    name: "0001_outbox_inbox",
    sql: include_str!("../migrations/0001_outbox_inbox.sql"),
}];

/// Applies every pending migration in order and verifies recorded checksums.
///
/// # Errors
///
/// Returns an error on drift, ordering violations, or query failures.
pub fn migrate(client: &mut Client, applied_at_unix_nanos: i64) -> Result<usize, PersistenceError> {
    client
        .batch_execute(
            "create table if not exists axiusflow_schema_migrations (
                version integer primary key,
                name text not null,
                checksum text not null,
                applied_at_unix_nanos bigint not null
            )",
        )
        .map_err(|error| PersistenceError::Query(error.to_string()))?;
    let recorded: Vec<(i32, String)> = client
        .query(
            "select version, checksum from axiusflow_schema_migrations order by version",
            &[],
        )
        .map_err(|error| PersistenceError::Query(error.to_string()))?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();

    let mut applied = 0_usize;
    for (index, migration) in MIGRATIONS.iter().enumerate() {
        let expected_order = i32::try_from(index + 1).unwrap_or(i32::MAX);
        if migration.version != expected_order {
            return Err(PersistenceError::MigrationOrder {
                expected: expected_order,
                actual: migration.version,
            });
        }
        let checksum = checksum_of(migration.sql);
        if let Some((version, recorded_checksum)) = recorded
            .iter()
            .find(|(version, _)| *version == migration.version)
        {
            if recorded_checksum != &checksum {
                return Err(PersistenceError::MigrationDrift { version: *version });
            }
            continue;
        }
        let mut transaction = client
            .transaction()
            .map_err(|error| PersistenceError::Query(error.to_string()))?;
        transaction
            .batch_execute(migration.sql)
            .map_err(|error| PersistenceError::Query(error.to_string()))?;
        transaction
            .execute(
                "insert into axiusflow_schema_migrations
                    (version, name, checksum, applied_at_unix_nanos)
                values ($1, $2, $3, $4)",
                &[
                    &migration.version,
                    &migration.name,
                    &checksum,
                    &applied_at_unix_nanos,
                ],
            )
            .map_err(|error| PersistenceError::Query(error.to_string()))?;
        transaction
            .commit()
            .map_err(|error| PersistenceError::Query(error.to_string()))?;
        applied += 1;
    }
    Ok(applied)
}

fn checksum_of(sql: &str) -> String {
    let digest = Sha256::digest(sql.as_bytes());
    let mut output = String::with_capacity(64);
    for byte in digest {
        let _ = std::fmt::Write::write_fmt(&mut output, format_args!("{byte:02x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{MIGRATIONS, checksum_of};

    #[test]
    fn migrations_are_strictly_ordered() {
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(usize::try_from(migration.version), Ok(index + 1));
        }
    }

    #[test]
    fn checksum_is_stable_and_content_bound() {
        let first = checksum_of(MIGRATIONS[0].sql);
        assert_eq!(first, checksum_of(MIGRATIONS[0].sql));
        assert_ne!(first, checksum_of("select 1"));
    }
}
