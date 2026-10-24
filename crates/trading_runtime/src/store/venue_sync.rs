//! Broker venue recovery facts that outlive a restart: contracts whose deals or positions
//! the owner could not account, which keep their account's new orders blocked and floor its
//! deal replay, and reverses that were waiting for their exit to flatten the position.

use super::{
    BTreeMap, InstrumentId, StepTx, TradingAccountId, TradingStore, database_error, params,
};

/// The unresolved contracts of every broker account: for each instrument, the execution time
/// of its earliest deal the owner could not account, or `None` when only a position report
/// named it.
pub(crate) type UnresolvedContracts =
    BTreeMap<TradingAccountId, BTreeMap<InstrumentId, Option<i64>>>;

impl TradingStore {
    fn venue_sync_tables_present(&self) -> Result<bool, String> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'broker_unresolved_contracts'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)
            .map_err(database_error)
    }

    /// Every unresolved contract. A gated pre-group store has no table yet and holds none.
    pub(super) fn load_unresolved_contracts(&self) -> Result<UnresolvedContracts, String> {
        if !self.venue_sync_tables_present()? {
            return Ok(BTreeMap::new());
        }
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, instrument_id, first_deal_unix_nanos
                 FROM broker_unresolved_contracts",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(database_error)?;
        let mut contracts = UnresolvedContracts::new();
        for row in rows {
            let (account, instrument, first_deal) = row.map_err(database_error)?;
            contracts
                .entry(TradingAccountId::try_new(account).map_err(|error| error.to_string())?)
                .or_default()
                .insert(
                    InstrumentId::try_new(instrument).map_err(|error| error.to_string())?,
                    first_deal,
                );
        }
        Ok(contracts)
    }

    /// Records an unresolved contract, keeping the earliest unaccounted deal time.
    pub(crate) fn put_unresolved_contract(
        &self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
        first_deal_unix_nanos: Option<i64>,
    ) -> Result<(), String> {
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO broker_unresolved_contracts(account_id, instrument_id,
                    first_deal_unix_nanos) VALUES (?1, ?2, ?3)
                 ON CONFLICT(account_id, instrument_id) DO UPDATE SET first_deal_unix_nanos =
                    CASE WHEN first_deal_unix_nanos IS NULL THEN excluded.first_deal_unix_nanos
                         WHEN excluded.first_deal_unix_nanos IS NULL THEN first_deal_unix_nanos
                         ELSE MIN(first_deal_unix_nanos, excluded.first_deal_unix_nanos) END",
                params![
                    account_id.as_str(),
                    instrument_id.as_str(),
                    first_deal_unix_nanos
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    /// Drops the contracts of one account that a reconcile accounted.
    pub(crate) fn delete_unresolved_contracts(
        &self,
        account_id: &TradingAccountId,
        instruments: &[InstrumentId],
    ) -> Result<(), String> {
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        for instrument_id in instruments {
            transaction
                .execute(
                    "DELETE FROM broker_unresolved_contracts
                     WHERE account_id = ?1 AND instrument_id = ?2",
                    params![account_id.as_str(), instrument_id.as_str()],
                )
                .map_err(database_error)?;
        }
        transaction.commit().map_err(database_error)
    }

    /// Marks a reverse whose exit was requested and whose opening order waits for the flat
    /// position.
    pub(crate) fn put_pending_reverse(
        &self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
        requested_unix_nanos: i64,
    ) -> Result<(), String> {
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO broker_pending_reverses(account_id, instrument_id,
                    requested_unix_nanos) VALUES (?1, ?2, ?3)
                 ON CONFLICT(account_id, instrument_id) DO UPDATE SET
                    requested_unix_nanos = excluded.requested_unix_nanos",
                params![
                    account_id.as_str(),
                    instrument_id.as_str(),
                    requested_unix_nanos
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    pub(crate) fn delete_pending_reverse(
        &self,
        account_id: &TradingAccountId,
        instrument_id: &InstrumentId,
    ) -> Result<(), String> {
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        transaction
            .execute(
                "DELETE FROM broker_pending_reverses WHERE account_id = ?1 AND instrument_id = ?2",
                params![account_id.as_str(), instrument_id.as_str()],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    /// Removes and returns every reverse an earlier process left waiting. Its opening order
    /// is never placed after a restart.
    pub(crate) fn take_pending_reverses(
        &mut self,
    ) -> Result<Vec<(TradingAccountId, InstrumentId)>, String> {
        if !self.venue_sync_tables_present()? {
            return Ok(Vec::new());
        }
        let transaction = self.connection.transaction().map_err(database_error)?;
        let reverses = {
            let mut statement = transaction
                .prepare(
                    "SELECT account_id, instrument_id FROM broker_pending_reverses
                     ORDER BY requested_unix_nanos, account_id, instrument_id",
                )
                .map_err(database_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(database_error)?;
            rows.map(|row| {
                let (account, instrument) = row.map_err(database_error)?;
                Ok((
                    TradingAccountId::try_new(account).map_err(|error| error.to_string())?,
                    InstrumentId::try_new(instrument).map_err(|error| error.to_string())?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?
        };
        transaction
            .execute("DELETE FROM broker_pending_reverses", [])
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(reverses)
    }
}
