use super::{CATALOG_ENTRIES, ORDER_INTENTS, RawRun, WORKSPACE_UPDATES, deterministic_payload};
use rusqlite::{Connection, Error as SqliteError, ErrorCode, params};
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process,
    time::Instant,
};

pub(super) fn run_once(path: &Path) -> Result<RawRun, Box<dyn Error>> {
    let mut connection = open(path)?;
    migrate(&connection)?;

    let workspace_start = Instant::now();
    for revision in 0..WORKSPACE_UPDATES {
        connection.execute(
            "insert into workspace_snapshot(workspace_id, revision, payload) values (?1, ?2, ?3)
             on conflict(workspace_id) do update set revision=excluded.revision, payload=excluded.payload",
            params!["primary", i64::try_from(revision)?, deterministic_payload(revision, 4_096)],
        )?;
    }
    let workspace_nanos = elapsed_nanos(workspace_start);

    let catalog_start = Instant::now();
    {
        let transaction = connection.transaction()?;
        {
            let mut statement = transaction.prepare(
                "insert into cache_manifest(cache_key, checksum, byte_len, rights_revision)
                 values (?1, ?2, ?3, ?4)",
            )?;
            for index in 0..CATALOG_ENTRIES {
                statement.execute(params![
                    format!("coinbase:btc-usd:minute:{index}"),
                    deterministic_payload(index, 32),
                    65_536_i64,
                    "crypto_public_realtime_v1"
                ])?;
            }
        }
        transaction.commit()?;
    }
    let catalog_nanos = elapsed_nanos(catalog_start);

    let mut order_commit_nanos = Vec::with_capacity(ORDER_INTENTS);
    for index in 0..ORDER_INTENTS {
        let start = Instant::now();
        let transaction = connection.transaction()?;
        transaction.execute(
            "insert into order_intent(client_order_id, state, payload) values (?1, ?2, ?3)",
            params![
                format!("paper-order-{index}"),
                "prepared",
                deterministic_payload(index, 512)
            ],
        )?;
        transaction.commit()?;
        order_commit_nanos.push(elapsed_nanos(start));
    }
    verify_counts(&connection)?;
    verify_rollback(&mut connection)?;
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(connection);

    let reopen_start = Instant::now();
    let reopened = open(path)?;
    verify_counts(&reopened)?;
    let reopen_nanos = elapsed_nanos(reopen_start);
    drop(reopened);

    Ok(RawRun {
        workspace_nanos,
        catalog_nanos,
        order_commit_nanos,
        reopen_nanos,
        file_bytes: sqlite_file_bytes(path)?,
    })
}

pub(super) fn crash_child(path: &Path) -> Result<(), Box<dyn Error>> {
    let mut connection = open(path)?;
    migrate(&connection)?;
    connection.execute(
        "insert into order_intent(client_order_id, state, payload) values (?1, ?2, ?3)",
        params!["crash-committed", "prepared", b"committed"],
    )?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "insert into order_intent(client_order_id, state, payload) values (?1, ?2, ?3)",
        params!["crash-uncommitted", "prepared", b"uncommitted"],
    )?;
    process::exit(91);
}

pub(super) fn verify_after_crash(path: &Path) -> Result<(), Box<dyn Error>> {
    let connection = open(path)?;
    let committed = count_key(&connection, "crash-committed")?;
    let uncommitted = count_key(&connection, "crash-uncommitted")?;
    if committed != 1 || uncommitted != 0 {
        return Err(format!(
            "SQLite crash recovery diverged: committed={committed} uncommitted={uncommitted}"
        )
        .into());
    }
    Ok(())
}

pub(super) fn verify_corruption_detection(path: &Path) -> Result<&'static str, Box<dyn Error>> {
    let connection = open(path)?;
    migrate(&connection)?;
    connection.execute(
        "insert into workspace_snapshot(workspace_id, revision, payload) values (?1, ?2, ?3)",
        params!["corrupt-me", 1_i64, deterministic_payload(7, 32_768)],
    )?;
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(connection);

    let original_length = fs::metadata(path)?.len();
    let file = fs::OpenOptions::new().write(true).open(path)?;
    file.set_len((original_length / 3).max(128))?;
    drop(file);

    let outcome = match Connection::open(path) {
        Err(_) => "controlled_open_error",
        Ok(connection) => match connection
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        {
            Err(_) => "controlled_integrity_error",
            Ok(result) if result != "ok" => "integrity_check_rejected",
            Ok(_) => return Err("SQLite truncation corruption was not detected".into()),
        },
    };
    Ok(outcome)
}

pub(super) fn verify_disk_full(path: &Path) -> Result<(), Box<dyn Error>> {
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "PRAGMA page_size=4096;
         PRAGMA journal_mode=WAL;
         PRAGMA synchronous=FULL;
         PRAGMA wal_autocheckpoint=1;
         PRAGMA max_page_count=16;
         CREATE TABLE limited_payload(id INTEGER PRIMARY KEY, payload BLOB NOT NULL);",
    )?;
    let payload = deterministic_payload(11, 16_384);
    let mut committed = 0_u32;
    let mut failed_index = None;
    for index in 0..1_024_u32 {
        match connection.execute(
            "insert into limited_payload(id, payload) values (?1, ?2)",
            params![index, &payload],
        ) {
            Ok(_) => committed = committed.saturating_add(1),
            Err(SqliteError::SqliteFailure(error, _)) if error.code == ErrorCode::DiskFull => {
                failed_index = Some(index);
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let failed_index =
        failed_index.ok_or("SQLite did not surface SQLITE_FULL under max_page_count")?;
    drop(connection);

    let reopened = Connection::open(path)?;
    let persisted: i64 =
        reopened.query_row("select count(*) from limited_payload", [], |row| row.get(0))?;
    let failed_persisted: i64 = reopened.query_row(
        "select count(*) from limited_payload where id=?1",
        [failed_index],
        |row| row.get(0),
    )?;
    let integrity: String = reopened.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if persisted != i64::from(committed) || failed_persisted != 0 || integrity != "ok" {
        return Err(format!(
            "SQLite disk-full recovery diverged: committed={committed} persisted={persisted} failed_persisted={failed_persisted} integrity={integrity}"
        )
        .into());
    }
    Ok(())
}

fn open(path: &Path) -> Result<Connection, Box<dyn Error>> {
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=FULL;
         PRAGMA foreign_keys=ON;
         PRAGMA busy_timeout=2500;",
    )?;
    Ok(connection)
}

fn migrate(connection: &Connection) -> Result<(), Box<dyn Error>> {
    let version = schema_version(connection)?;
    if version == 0 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE workspace_snapshot(
                 workspace_id TEXT PRIMARY KEY,
                 revision INTEGER NOT NULL,
                 payload BLOB NOT NULL
             ) STRICT;
             CREATE TABLE cache_manifest(
                 cache_key TEXT PRIMARY KEY,
                 checksum BLOB NOT NULL,
                 byte_len INTEGER NOT NULL CHECK(byte_len >= 0),
                 rights_revision TEXT NOT NULL
             ) STRICT;
             PRAGMA user_version=1;
             COMMIT;",
        )?;
    }
    if schema_version(connection)? == 1 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE order_intent(
                 client_order_id TEXT PRIMARY KEY,
                 state TEXT NOT NULL,
                 payload BLOB NOT NULL
             ) STRICT;
             PRAGMA user_version=2;
             COMMIT;",
        )?;
    }
    let observed = schema_version(connection)?;
    if observed != 2 {
        return Err(format!("SQLite migration version diverged: {observed}").into());
    }
    Ok(())
}

fn schema_version(connection: &Connection) -> Result<u32, Box<dyn Error>> {
    Ok(connection.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

fn verify_counts(connection: &Connection) -> Result<(), Box<dyn Error>> {
    let workspace: i64 = connection.query_row(
        "select count(*) from workspace_snapshot where workspace_id='primary'",
        [],
        |row| row.get(0),
    )?;
    let catalog: i64 =
        connection.query_row("select count(*) from cache_manifest", [], |row| row.get(0))?;
    let orders: i64 =
        connection.query_row("select count(*) from order_intent", [], |row| row.get(0))?;
    if workspace != 1
        || catalog != i64::try_from(CATALOG_ENTRIES)?
        || orders != i64::try_from(ORDER_INTENTS)?
    {
        return Err(format!(
            "SQLite workload counts diverged: workspace={workspace} catalog={catalog} orders={orders}"
        )
        .into());
    }
    Ok(())
}

fn verify_rollback(connection: &mut Connection) -> Result<(), Box<dyn Error>> {
    let transaction = connection.transaction()?;
    transaction.execute(
        "insert into order_intent(client_order_id, state, payload) values (?1, ?2, ?3)",
        params!["rolled-back", "prepared", b"rollback"],
    )?;
    transaction.rollback()?;
    if count_key(connection, "rolled-back")? != 0 {
        return Err("SQLite exposed a rolled-back intent".into());
    }
    Ok(())
}

fn count_key(connection: &Connection, key: &str) -> Result<i64, Box<dyn Error>> {
    Ok(connection.query_row(
        "select count(*) from order_intent where client_order_id=?1",
        [key],
        |row| row.get(0),
    )?)
}

fn sqlite_file_bytes(path: &Path) -> Result<u64, Box<dyn Error>> {
    let mut total = fs::metadata(path)?.len();
    for suffix in ["-wal", "-shm"] {
        let sibling = PathBuf::from(format!("{}{suffix}", path.display()));
        if let Ok(metadata) = fs::metadata(sibling) {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

fn elapsed_nanos(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
