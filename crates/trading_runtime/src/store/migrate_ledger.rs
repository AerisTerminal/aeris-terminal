//! The V25 data migration: one deposit entry per funded simulated account, one realized
//! entry per recorded fill accounting row, the exact integer cost basis behind every open
//! position's displayed average, and the instrument class/grid query columns decoded from
//! the contract document. Fees stay zero: no fee schedule existed before this migration.
//! The whole backfill runs once, in one transaction, behind a metadata marker, so an
//! interrupted start retries without duplicating rows.

use aeris_instruments::InstrumentId;
use rusqlite::params;

use super::TradingStore;
use super::database_error;
use super::ledger::split_i128;
use super::orders::{decode_contract, encode_price_grid, instrument_class_str};

const BACKFILL_MARKER: &str = "ledger_backfill_v25";

impl TradingStore {
    pub(super) fn backfill_ledger(&mut self) -> Result<(), String> {
        if !self.ledger_tables_present()? || self.ledger_backfilled()? {
            return Ok(());
        }
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| format!("trading ledger migration could not start: {error}"))?;
        backfill_deposits(&transaction)?;
        backfill_realized_entries(&transaction)?;
        backfill_cost_basis(&transaction)?;
        backfill_instrument_columns(&transaction)?;
        transaction
            .execute(
                "INSERT INTO metadata(key, value) VALUES (?1, 1)",
                [BACKFILL_MARKER],
            )
            .map_err(database_error)?;
        transaction
            .commit()
            .map_err(|error| format!("trading ledger migration could not commit: {error}"))
    }

    pub(super) fn ledger_tables_present(&self) -> Result<bool, String> {
        self.connection
            .query_row(
                "SELECT count(*) = 2 FROM sqlite_master
                 WHERE type = 'table' AND name IN ('ledger_entries', 'fee_schedules')",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)
    }

    fn ledger_backfilled(&self) -> Result<bool, String> {
        self.connection
            .query_row(
                "SELECT count(*) > 0 FROM metadata WHERE key = ?1",
                [BACKFILL_MARKER],
                |row| row.get(0),
            )
            .map_err(database_error)
    }
}

/// One deposit per funded simulated account carries its starting equity into the ledger, so
/// the balance the migration computes matches the balance the owner reported before.
fn backfill_deposits(transaction: &rusqlite::Transaction<'_>) -> Result<(), String> {
    let mut statement = transaction
        .prepare(
            "SELECT id, starting_equity_units, starting_equity_scale FROM accounts
             WHERE environment = 'simulated' AND starting_equity_units IS NOT NULL
             ORDER BY id",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    drop(statement);
    for (account, units, scale) in rows {
        transaction
            .execute(
                "INSERT INTO ledger_entries
                    (id, account_id, kind, amount_units, amount_scale, fill_id, at_unix_nanos)
                 VALUES (?1, ?2, 'deposit', ?3, ?4, NULL, 1)",
                params![
                    format!("migration-deposit-{account}"),
                    account,
                    units,
                    scale
                ],
            )
            .map_err(database_error)?;
    }
    Ok(())
}

/// One realized entry per fill accounting row keeps every historical fill's contribution
/// attributable; the row timestamps order the replay.
fn backfill_realized_entries(transaction: &rusqlite::Transaction<'_>) -> Result<(), String> {
    let mut statement = transaction
        .prepare(
            "SELECT a.fill_id, a.account_id, a.realized_gross_units, a.realized_gross_scale,
                f.execution_unix_nanos
             FROM fill_accounting a
             JOIN fills f ON f.id = a.fill_id
             WHERE a.realized_gross_units IS NOT NULL
             ORDER BY f.execution_unix_nanos, a.fill_id",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    drop(statement);
    for (fill_id, account, units, scale, at) in rows {
        transaction
            .execute(
                "INSERT INTO ledger_entries
                    (id, account_id, kind, amount_units, amount_scale, fill_id, at_unix_nanos)
                 VALUES (?1, ?2, 'realized_pnl', ?3, ?4, ?5, ?6)",
                params![
                    format!("migration-pnl-{fill_id}"),
                    account,
                    units,
                    scale,
                    fill_id,
                    at
                ],
            )
            .map_err(database_error)?;
    }
    Ok(())
}

/// The exact basis of an open position is the displayed average price times the open
/// quantity, computed in integer space so no rounding enters the historical record.
fn backfill_cost_basis(transaction: &rusqlite::Transaction<'_>) -> Result<(), String> {
    let mut statement = transaction
        .prepare(
            "SELECT account_id, instrument_id, net_units, net_scale,
                average_units, average_scale FROM positions
             WHERE average_units IS NOT NULL AND net_units <> 0",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    drop(statement);
    for (account, instrument, net_units, net_scale, average_units, average_scale) in rows {
        let basis = i128::from(average_units) * i128::from(net_units.unsigned_abs());
        let (hi, lo) = split_i128(basis)?;
        transaction
            .execute(
                "UPDATE positions SET cost_basis_hi = ?3, cost_basis_lo = ?4,
                    cost_price_scale = ?5, cost_quantity_scale = ?6
                 WHERE account_id = ?1 AND instrument_id = ?2",
                params![account, instrument, hi, lo, average_scale, net_scale],
            )
            .map_err(database_error)?;
    }
    Ok(())
}

/// The query columns decode from the same contract document the runtime loads; a row whose
/// document predates the current codec keeps NULL columns instead of blocking the
/// migration, and the primary loader still validates the document itself.
fn backfill_instrument_columns(transaction: &rusqlite::Transaction<'_>) -> Result<(), String> {
    let mut statement = transaction
        .prepare("SELECT id, contract_json FROM instruments ORDER BY id")
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    drop(statement);
    for (id, contract_json) in rows {
        let instrument_id = InstrumentId::try_new(id.clone()).map_err(|error| error.to_string())?;
        let Ok(contract) = decode_contract(&contract_json, &instrument_id) else {
            continue;
        };
        transaction
            .execute(
                "UPDATE instruments SET instrument_class = ?2, price_grid_json = ?3 WHERE id = ?1",
                params![
                    id,
                    instrument_class_str(contract.instrument_class),
                    encode_price_grid(&contract.price_grid).to_string()
                ],
            )
            .map_err(database_error)?;
    }
    Ok(())
}
