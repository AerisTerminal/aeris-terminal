use super::{CATALOG_ENTRIES, ORDER_INTENTS, RawRun, WORKSPACE_UPDATES, deterministic_payload};
use redb::{
    Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, StorageBackend,
    TableDefinition, backends::FileBackend,
};
use std::{
    error::Error,
    fs::{self, OpenOptions},
    io,
    panic::{self, AssertUnwindSafe, catch_unwind},
    path::Path,
    process,
    time::Instant,
};

const METADATA: TableDefinition<&str, u64> = TableDefinition::new("metadata");
const WORKSPACE: TableDefinition<&str, &[u8]> = TableDefinition::new("workspace_snapshot");
const CATALOG: TableDefinition<&str, &[u8]> = TableDefinition::new("cache_manifest");
const ORDER_INTENT: TableDefinition<&str, &[u8]> = TableDefinition::new("order_intent");

pub(super) fn run_once(path: &Path) -> Result<RawRun, Box<dyn Error>> {
    let database = migrate(path)?;

    let workspace_start = Instant::now();
    for revision in 0..WORKSPACE_UPDATES {
        let transaction = database.begin_write()?;
        {
            let mut table = transaction.open_table(WORKSPACE)?;
            let mut payload = u64::try_from(revision)?.to_le_bytes().to_vec();
            payload.extend(deterministic_payload(revision, 4_096));
            table.insert("primary", payload.as_slice())?;
        }
        transaction.commit()?;
    }
    let workspace_nanos = elapsed_nanos(workspace_start);

    let catalog_start = Instant::now();
    let transaction = database.begin_write()?;
    {
        let mut table = transaction.open_table(CATALOG)?;
        for index in 0..CATALOG_ENTRIES {
            let key = format!("coinbase:btc-usd:minute:{index}");
            table.insert(key.as_str(), deterministic_payload(index, 64).as_slice())?;
        }
    }
    transaction.commit()?;
    let catalog_nanos = elapsed_nanos(catalog_start);

    let mut order_commit_nanos = Vec::with_capacity(ORDER_INTENTS);
    for index in 0..ORDER_INTENTS {
        let start = Instant::now();
        let transaction = database.begin_write()?;
        {
            let mut table = transaction.open_table(ORDER_INTENT)?;
            let key = format!("paper-order-{index}");
            table.insert(key.as_str(), deterministic_payload(index, 512).as_slice())?;
        }
        transaction.commit()?;
        order_commit_nanos.push(elapsed_nanos(start));
    }
    verify_counts(&database)?;
    verify_rollback(&database)?;
    drop(database);

    let reopen_start = Instant::now();
    let reopened = Database::open(path)?;
    verify_counts(&reopened)?;
    let reopen_nanos = elapsed_nanos(reopen_start);
    drop(reopened);

    Ok(RawRun {
        workspace_nanos,
        catalog_nanos,
        order_commit_nanos,
        reopen_nanos,
        file_bytes: fs::metadata(path)?.len(),
    })
}

pub(super) fn crash_child(path: &Path) -> Result<(), Box<dyn Error>> {
    let database = migrate(path)?;
    let committed = database.begin_write()?;
    {
        let mut table = committed.open_table(ORDER_INTENT)?;
        table.insert("crash-committed", b"committed".as_slice())?;
    }
    committed.commit()?;

    let uncommitted = database.begin_write()?;
    {
        let mut table = uncommitted.open_table(ORDER_INTENT)?;
        table.insert("crash-uncommitted", b"uncommitted".as_slice())?;
    }
    process::exit(91);
}

pub(super) fn verify_after_crash(path: &Path) -> Result<(), Box<dyn Error>> {
    let database = Database::open(path)?;
    let transaction = database.begin_read()?;
    let table = transaction.open_table(ORDER_INTENT)?;
    let committed = table.get("crash-committed")?.is_some();
    let uncommitted = table.get("crash-uncommitted")?.is_some();
    if !committed || uncommitted {
        return Err(format!(
            "redb crash recovery diverged: committed={committed} uncommitted={uncommitted}"
        )
        .into());
    }
    Ok(())
}

pub(super) fn verify_corruption_detection(path: &Path) -> Result<&'static str, Box<dyn Error>> {
    let database = migrate(path)?;
    let transaction = database.begin_write()?;
    {
        let mut table = transaction.open_table(WORKSPACE)?;
        table.insert("corrupt-me", deterministic_payload(7, 131_072).as_slice())?;
    }
    transaction.commit()?;
    drop(database);

    let original_length = fs::metadata(path)?.len();
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len((original_length / 3).max(128))?;
    drop(file);

    let panic_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let probe_result = catch_unwind(AssertUnwindSafe(|| {
        let database = Database::open(path)?;
        read_corruption_marker(&database)
    }));
    panic::set_hook(panic_hook);
    let outcome = match probe_result {
        Err(_) => "panic_caught_by_harness",
        Ok(Err(_)) => "controlled_probe_error",
        Ok(Ok(false)) => return Err("redb truncation caused silent record loss".into()),
        Ok(Ok(true)) => return Err("redb truncation corruption was not detected".into()),
    };
    Ok(outcome)
}

fn read_corruption_marker(database: &Database) -> Result<bool, Box<dyn Error>> {
    let transaction = database.begin_read()?;
    let table = transaction.open_table(WORKSPACE)?;
    Ok(table.get("corrupt-me")?.is_some())
}

pub(super) fn verify_disk_full(path: &Path) -> Result<(), Box<dyn Error>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    let backend = LimitedBackend {
        inner: FileBackend::new(file)?,
        maximum_len: 16 * 1024 * 1024,
    };
    let database = Database::builder()
        .set_cache_size(4 * 1024 * 1024)
        .create_with_backend(backend)?;
    let payload = deterministic_payload(11, 1_048_576);
    let mut committed = 0_u32;
    let mut failed_index = None;
    for index in 0..1_024_u32 {
        let attempt = (|| -> Result<(), Box<dyn Error>> {
            let transaction = database.begin_write()?;
            {
                let mut table = transaction.open_table(ORDER_INTENT)?;
                let key = format!("limited-{index}");
                table.insert(key.as_str(), payload.as_slice())?;
            }
            transaction.commit()?;
            Ok(())
        })();
        if let Err(error) = attempt {
            let message = error.to_string();
            if !message.contains("simulated storage capacity exhausted") {
                return Err(format!("redb disk-full probe failed unexpectedly: {message}").into());
            }
            failed_index = Some(index);
            break;
        }
        committed = committed.saturating_add(1);
    }
    let failed_index = failed_index.ok_or("redb did not surface a storage-full commit error")?;
    drop(database);

    let reopened = Database::open(path)?;
    let transaction = reopened.begin_read()?;
    let table = transaction.open_table(ORDER_INTENT)?;
    let failed_key = format!("limited-{failed_index}");
    let persisted = table.len()?;
    let failed_persisted = table.get(failed_key.as_str())?.is_some();
    if persisted != u64::from(committed) || failed_persisted {
        return Err(format!(
            "redb disk-full recovery diverged: committed={committed} persisted={persisted} failed_persisted={failed_persisted}"
        )
        .into());
    }
    Ok(())
}

fn open(path: &Path) -> Result<Database, Box<dyn Error>> {
    Ok(Database::builder()
        .set_cache_size(32 * 1024 * 1024)
        .create(path)?)
}

fn migrate(path: &Path) -> Result<Database, Box<dyn Error>> {
    let database = open(path)?;
    let transaction = database.begin_write()?;
    let version = {
        let table = transaction.open_table(METADATA)?;
        table
            .get("schema_version")?
            .map_or(0, |value| value.value())
    };
    if version == 0 {
        transaction.open_table(WORKSPACE)?;
        transaction.open_table(CATALOG)?;
        let mut metadata = transaction.open_table(METADATA)?;
        metadata.insert("schema_version", &1)?;
    }
    transaction.commit()?;
    drop(database);

    let database = open(path)?;
    let transaction = database.begin_write()?;
    let version = {
        let table = transaction.open_table(METADATA)?;
        table
            .get("schema_version")?
            .map_or(0, |value| value.value())
    };
    if version == 1 {
        transaction.open_table(ORDER_INTENT)?;
        let mut metadata = transaction.open_table(METADATA)?;
        metadata.insert("schema_version", &2)?;
    }
    let observed = {
        let table = transaction.open_table(METADATA)?;
        table
            .get("schema_version")?
            .map_or(0, |value| value.value())
    };
    if observed != 2 {
        return Err(format!("redb migration version diverged: {observed}").into());
    }
    transaction.commit()?;
    Ok(database)
}

fn verify_counts(database: &Database) -> Result<(), Box<dyn Error>> {
    let transaction = database.begin_read()?;
    let workspace = transaction.open_table(WORKSPACE)?;
    let catalog = transaction.open_table(CATALOG)?;
    let orders = transaction.open_table(ORDER_INTENT)?;
    if workspace.len()? != 1
        || catalog.len()? != u64::try_from(CATALOG_ENTRIES)?
        || orders.len()? != u64::try_from(ORDER_INTENTS)?
    {
        return Err(format!(
            "redb workload counts diverged: workspace={} catalog={} orders={}",
            workspace.len()?,
            catalog.len()?,
            orders.len()?
        )
        .into());
    }
    Ok(())
}

fn verify_rollback(database: &Database) -> Result<(), Box<dyn Error>> {
    let transaction = database.begin_write()?;
    {
        let mut table = transaction.open_table(ORDER_INTENT)?;
        table.insert("rolled-back", b"rollback".as_slice())?;
    }
    transaction.abort()?;
    let transaction = database.begin_read()?;
    if transaction
        .open_table(ORDER_INTENT)?
        .get("rolled-back")?
        .is_some()
    {
        return Err("redb exposed a rolled-back intent".into());
    }
    Ok(())
}

#[derive(Debug)]
struct LimitedBackend {
    inner: FileBackend,
    maximum_len: u64,
}

impl LimitedBackend {
    fn ensure_range(&self, offset: u64, bytes: usize) -> io::Result<()> {
        let end = offset
            .checked_add(u64::try_from(bytes).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("storage offset overflow"))?;
        if end > self.maximum_len {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "simulated storage capacity exhausted",
            ));
        }
        Ok(())
    }
}

impl StorageBackend for LimitedBackend {
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }

    fn read(&self, offset: u64, out: &mut [u8]) -> io::Result<()> {
        self.inner.read(offset, out)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        if len > self.maximum_len {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "simulated storage capacity exhausted",
            ));
        }
        self.inner.set_len(len)
    }

    fn sync_data(&self) -> io::Result<()> {
        self.inner.sync_data()
    }

    fn write(&self, offset: u64, data: &[u8]) -> io::Result<()> {
        self.ensure_range(offset, data.len())?;
        self.inner.write(offset, data)
    }

    fn close(&self) -> io::Result<()> {
        self.inner.close()
    }
}

fn elapsed_nanos(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
