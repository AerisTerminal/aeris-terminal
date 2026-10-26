//! Closed-trade pages on the read-only history connection. A page selects exit identities
//! first, then projects each exit from at most the owner's 500 most recent fills in its cycle.
use super::{Connection, Fill, FillId, TradingAccountId, database_error, fixed, params};
use crate::core::ledger::FillAccounting;
use crate::history::{TradeHistoryCursor, TradeHistoryPage, TradeHistoryQuery};
use std::collections::BTreeMap;

pub(crate) fn trade_history_page(
    connection: &Connection,
    query: &TradeHistoryQuery,
) -> Result<TradeHistoryPage, String> {
    let transaction = connection.unchecked_transaction().map_err(database_error)?;
    let limit = query
        .limit
        .unwrap_or(crate::MAXIMUM_HISTORY_PAGE)
        .clamp(1, crate::MAXIMUM_HISTORY_PAGE);
    let mut statement = transaction
        .prepare(
            "SELECT f.id, f.execution_unix_nanos, f.rowid, a.cycle_id
         FROM fills f JOIN fill_accounting a ON a.fill_id = f.id
         WHERE f.account_id = ?1
           AND (a.realized_gross_units IS NOT NULL OR a.cycle_realized_units IS NOT NULL
                OR a.cycle_closed = 1)
           AND (?2 IS NULL OR (f.execution_unix_nanos, f.id) < (?2, ?3))
         ORDER BY f.execution_unix_nanos DESC, f.id DESC LIMIT ?4",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map(
            params![
                query.account_id.as_str(),
                query.after.as_ref().map(|cursor| cursor.exit_unix_nanos),
                query
                    .after
                    .as_ref()
                    .map(|cursor| cursor.exit_fill_id.as_str()),
                i64::try_from(limit + 1).map_err(|error| error.to_string())?,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .map_err(database_error)?;
    let mut exits = Vec::with_capacity(limit + 1);
    for row in rows {
        exits.push(row.map_err(database_error)?);
    }
    drop(statement);
    let has_more = exits.len() > limit;
    exits.truncate(limit);
    let mut trades = Vec::with_capacity(exits.len());
    for (id, time, rowid, cycle_id) in exits {
        let fill_id = FillId::try_new(id).map_err(|error| error.to_string())?;
        trades.push(project_exit(
            &transaction,
            &query.account_id,
            &fill_id,
            time,
            rowid,
            cycle_id,
        )?);
    }
    let next = if has_more {
        let trade = trades
            .last()
            .ok_or_else(|| "empty historical page".to_string())?;
        Some(TradeHistoryCursor {
            exit_unix_nanos: trade
                .exit
                .ok_or_else(|| "historical trade has no exit".to_string())?
                .last_unix_nanos,
            exit_fill_id: trade
                .exit_fill_id
                .clone()
                .ok_or_else(|| "historical trade has no identity".to_string())?,
        })
    } else {
        None
    };
    transaction.commit().map_err(database_error)?;
    Ok(TradeHistoryPage { trades, next })
}

fn project_exit(
    connection: &Connection,
    account_id: &TradingAccountId,
    exit_id: &FillId,
    time: i64,
    rowid: i64,
    cycle_id: i64,
) -> Result<crate::TradeRoundTrip, String> {
    // The cycle's signed net immediately after this exit is needed for partial exits and
    // reversals. SUM is exact integer SQLite arithmetic; mixed quantity scales are refused.
    let (net, minimum_scale, maximum_scale): (i64, u8, u8) = connection
        .query_row(
            "SELECT SUM(CASE f.side WHEN 'buy' THEN f.quantity_units
                            ELSE -f.quantity_units END),
                MIN(f.quantity_scale), MAX(f.quantity_scale)
         FROM fills f JOIN fill_accounting a ON a.fill_id = f.id
         WHERE f.account_id = ?1 AND f.instrument_id = (
             SELECT instrument_id FROM fills WHERE id = ?2)
           AND a.cycle_id = ?3
           AND (f.execution_unix_nanos < ?4
                OR (f.execution_unix_nanos = ?4 AND f.rowid <= ?5))",
            params![account_id.as_str(), exit_id.as_str(), cycle_id, time, rowid],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(database_error)?;
    if minimum_scale != maximum_scale {
        return Err("historical cycle contains mixed quantity scales".to_string());
    }
    let net_after = fixed(net, minimum_scale)?;
    let mut statement = connection
        .prepare(
            "SELECT f.id, f.order_id, f.account_id, f.instrument_id, f.side, f.price_units,
                f.price_scale, f.quantity_units, f.quantity_scale, f.execution_unix_nanos,
                f.provenance_json, a.cycle_id, a.realized_gross_units,
                a.realized_gross_scale, a.cycle_realized_units, a.cycle_realized_scale,
                a.cycle_closed, a.commission_units, a.commission_scale
         FROM fills f JOIN fill_accounting a ON a.fill_id = f.id
         WHERE f.account_id = ?1 AND f.instrument_id = (
             SELECT instrument_id FROM fills WHERE id = ?2)
           AND a.cycle_id = ?3
           AND (f.execution_unix_nanos < ?4
                OR (f.execution_unix_nanos = ?4 AND f.rowid <= ?5))
         ORDER BY f.execution_unix_nanos DESC, f.rowid DESC LIMIT ?6",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map(
            params![
                account_id.as_str(),
                exit_id.as_str(),
                cycle_id,
                time,
                rowid,
                i64::try_from(crate::publication::WORKING_SET_ROUND_TRIP_FILL_LIMIT)
                    .map_err(|error| error.to_string())?
            ],
            |row| {
                let fill = super::fills::decode_fill_row(row)?;
                let cycle_id = row.get(11)?;
                let realized_units = row.get(12)?;
                let realized_scale = row.get(13)?;
                let cycle_units = row.get(14)?;
                let cycle_scale = row.get(15)?;
                let cycle_closed = row.get(16)?;
                let commission_units = row.get(17)?;
                let commission_scale = row.get(18)?;
                Ok(fill.and_then(|fill| {
                    Ok((
                        fill,
                        FillAccounting {
                            cycle_id,
                            realized_gross: super::optional_fixed(realized_units, realized_scale)?,
                            cycle_realized: super::optional_fixed(cycle_units, cycle_scale)?,
                            cycle_closed,
                            commission: fixed(commission_units, commission_scale)?,
                        },
                    ))
                }))
            },
        )
        .map_err(database_error)?;
    let mut fills = Vec::<Fill>::new();
    let mut accounting = BTreeMap::new();
    for row in rows {
        let (fill, record) = row.map_err(database_error)??;
        accounting.insert(fill.id.clone(), record);
        fills.push(fill);
    }
    crate::round_trip::project_historical_cycle(fills.iter(), &accounting, net_after)?
        .into_iter()
        .find(|trade| trade.exit_fill_id.as_ref() == Some(exit_id))
        .ok_or_else(|| "historical exit could not be projected".to_string())
}
