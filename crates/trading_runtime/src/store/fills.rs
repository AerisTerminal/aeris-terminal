use super::{
    BTreeMap, BrokerFillRecord, Fill, FillAccounting, FillId, FillPolicyPersistence, FixedPoint,
    InstrumentId, Order, OrderEvent, OrderId, Position, StepTx, TradeCycle, TradingAccountId,
    TradingStore, VecDeque, database_error, decode_provenance, encode_provenance, fixed,
    insert_event_row, insert_order_row, params, parse_side, sqlite_i64, update_next_sequence,
    upsert_discipline_state, upsert_position, upsert_position_basis, upsert_risk_rule_state,
};

pub(super) fn insert_fill_accounting(
    transaction: &rusqlite::Transaction<'_>,
    fill: &Fill,
    accounting: FillAccounting,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO fill_accounting(fill_id,account_id,instrument_id,
            realized_gross_units,realized_gross_scale,commission_units,commission_scale,
            cycle_id,cycle_closed,cycle_realized_units,cycle_realized_scale)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                fill.id.as_str(),
                fill.account_id.as_str(),
                fill.instrument_id.as_str(),
                accounting.realized_gross.map(FixedPoint::units),
                accounting.realized_gross.map(FixedPoint::scale),
                accounting.commission.units(),
                accounting.commission.scale(),
                accounting.cycle_id,
                accounting.cycle_closed,
                accounting.cycle_realized.map(FixedPoint::units),
                accounting.cycle_realized.map(FixedPoint::scale),
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

pub(super) fn upsert_trade_cycle(
    transaction: &rusqlite::Transaction<'_>,
    cycle: &TradeCycle,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO trade_cycles(account_id,instrument_id,cycle_id,peak_quantity_units,
            peak_quantity_scale,realized_units,realized_scale,
            risk_realized_units,risk_realized_scale,pnl_known)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(account_id,instrument_id) DO UPDATE SET
            cycle_id=excluded.cycle_id,peak_quantity_units=excluded.peak_quantity_units,
            peak_quantity_scale=excluded.peak_quantity_scale,
            realized_units=excluded.realized_units,realized_scale=excluded.realized_scale,
            risk_realized_units=excluded.risk_realized_units,
            risk_realized_scale=excluded.risk_realized_scale,
            pnl_known=excluded.pnl_known",
            params![
                cycle.account_id.as_str(),
                cycle.instrument_id.as_str(),
                cycle.cycle_id,
                cycle.peak_quantity.units(),
                cycle.peak_quantity.scale(),
                cycle.realized_pnl.units(),
                cycle.realized_pnl.scale(),
                cycle.risk_realized_pnl.units(),
                cycle.risk_realized_pnl.scale(),
                cycle.pnl_known,
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

impl TradingStore {
    pub(crate) fn broker_deal_recorded(&self, broker_deal_id: &str) -> Result<bool, String> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM broker_deals WHERE broker_deal_id = ?1",
                [broker_deal_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)
            .map_err(database_error)
    }

    /// Records one broker deal exactly once, with the order transition it causes. A
    /// `mirror` is a broker-originated order first seen through this deal.
    pub(crate) fn insert_broker_fill(
        &mut self,
        record: &BrokerFillRecord<'_>,
        next_sequence: u64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        if let Some((mirror, broker_order_id)) = record.mirror {
            insert_order_row(&transaction, mirror)?;
            transaction
                .execute(
                    "INSERT INTO broker_orders(order_id, broker_order_id, client_order_id,
                        session_generation, updated_unix_nanos) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        mirror.id.as_str(),
                        broker_order_id,
                        mirror.client_order_id.as_str(),
                        sqlite_i64(mirror.provenance.session_generation)?,
                        mirror.provenance.observed_unix_nanos,
                    ],
                )
                .map_err(database_error)?;
        }
        let order = record.order;
        transaction
            .execute(
                "UPDATE orders SET status = ?1, quantity_units = ?2, quantity_scale = ?3,
                    filled_units = ?4, filled_scale = ?5, provenance_json = ?6,
                    revision = ?7, updated_unix_nanos = ?8, terminal_unix_nanos = ?9,
                    triggered_unix_nanos = ?10, expires_at_unix_nanos = ?11
                    WHERE id = ?12",
                params![
                    order.status.as_str(),
                    order.quantity.units(),
                    order.quantity.scale(),
                    order.filled_quantity.units(),
                    order.filled_quantity.scale(),
                    encode_provenance(&order.provenance),
                    order.revision,
                    order.updated_unix_nanos,
                    order.terminal_unix_nanos,
                    order.triggered_unix_nanos,
                    order.expires_at_unix_nanos,
                    order.id.as_str(),
                ],
            )
            .map_err(database_error)?;
        insert_event_row(&transaction, record.event)?;
        let fill = record.fill;
        transaction
            .execute(
                "INSERT INTO fills(id, order_id, account_id, instrument_id, side, price_units,
                    price_scale, quantity_units, quantity_scale, execution_unix_nanos,
                    provenance_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    fill.id.as_str(),
                    fill.order_id.as_str(),
                    fill.account_id.as_str(),
                    fill.instrument_id.as_str(),
                    fill.side.as_str(),
                    fill.price.units(),
                    fill.price.scale(),
                    fill.quantity.units(),
                    fill.quantity.scale(),
                    fill.execution_unix_nanos,
                    encode_provenance(&fill.provenance),
                ],
            )
            .map_err(database_error)?;
        insert_fill_accounting(
            &transaction,
            fill,
            FillAccounting {
                cycle_id: 0,
                realized_gross: record.realized_pnl,
                commission: FixedPoint::try_new(
                    0,
                    record.realized_pnl.map_or(2, FixedPoint::scale),
                )
                .map_err(|error| error.to_string())?,
                cycle_realized: None,
                cycle_closed: false,
            },
        )?;
        transaction
            .execute(
                "INSERT INTO broker_deals(broker_deal_id, fill_id, account_id,
                    broker_position_id, executed_unix_millis) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    record.broker_deal_id,
                    fill.id.as_str(),
                    fill.account_id.as_str(),
                    record.broker_position_id,
                    fill.execution_unix_nanos / 1_000_000,
                ],
            )
            .map_err(database_error)?;
        update_next_sequence(&transaction, next_sequence)?;
        StepTx::commit_revision(transaction).map_err(database_error)
    }
}

impl TradingStore {
    pub(crate) fn insert_fill(
        &mut self,
        order: &Order,
        event: &OrderEvent,
        fill: &Fill,
        position: &Position,
        policy: FillPolicyPersistence<'_>,
        next_sequence: u64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "UPDATE orders SET status = ?1, filled_units = ?2,
                    filled_scale = ?3, revision = ?4, updated_unix_nanos = ?5,
                    terminal_unix_nanos = ?6, triggered_unix_nanos = ?7,
                    expires_at_unix_nanos = ?8
                 WHERE id = ?9 AND status IN ('working', 'partially_filled')",
                params![
                    order.status.as_str(),
                    order.filled_quantity.units(),
                    order.filled_quantity.scale(),
                    order.revision,
                    order.updated_unix_nanos,
                    order.terminal_unix_nanos,
                    order.triggered_unix_nanos,
                    order.expires_at_unix_nanos,
                    order.id.as_str(),
                ],
            )
            .map_err(database_error)?;
        insert_event_row(&transaction, event)?;
        transaction
            .execute(
                "INSERT INTO fills(id, order_id, account_id, instrument_id, side, price_units,
                    price_scale, quantity_units, quantity_scale, execution_unix_nanos,
                    provenance_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    fill.id.as_str(),
                    fill.order_id.as_str(),
                    fill.account_id.as_str(),
                    fill.instrument_id.as_str(),
                    fill.side.as_str(),
                    fill.price.units(),
                    fill.price.scale(),
                    fill.quantity.units(),
                    fill.quantity.scale(),
                    fill.execution_unix_nanos,
                    encode_provenance(&fill.provenance),
                ],
            )
            .map_err(database_error)?;
        insert_fill_accounting(&transaction, fill, policy.accounting)?;
        super::ledger::insert_ledger_entries(&transaction, policy.ledger_entries)?;
        upsert_position(&transaction, position)?;
        upsert_position_basis(&transaction, position, policy.cost_basis)?;
        upsert_trade_cycle(&transaction, policy.trade_cycle)?;
        if let Some(rule_state) = policy.rule_state {
            upsert_risk_rule_state(&transaction, rule_state)?;
        }
        upsert_discipline_state(&transaction, policy.discipline_state)?;
        update_next_sequence(&transaction, next_sequence)?;
        transaction.commit().map_err(database_error)
    }
}

const FILL_COLUMNS: &str = "SELECT id, order_id, account_id, instrument_id, side, price_units,
    price_scale, quantity_units, quantity_scale, execution_unix_nanos, provenance_json";

fn decode_fill_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Fill, String>> {
    let id: String = row.get(0)?;
    let order: String = row.get(1)?;
    let account: String = row.get(2)?;
    let instrument: String = row.get(3)?;
    let side: String = row.get(4)?;
    let price_units: i64 = row.get(5)?;
    let price_scale: u8 = row.get(6)?;
    let quantity_units: i64 = row.get(7)?;
    let quantity_scale: u8 = row.get(8)?;
    let time: i64 = row.get(9)?;
    let provenance: String = row.get(10)?;
    Ok((move || {
        let fill = Fill {
            id: FillId::try_new(id).map_err(|error| error.to_string())?,
            order_id: OrderId::try_new(order).map_err(|error| error.to_string())?,
            account_id: TradingAccountId::try_new(account).map_err(|error| error.to_string())?,
            instrument_id: InstrumentId::try_new(instrument).map_err(|error| error.to_string())?,
            side: parse_side(&side)?,
            price: fixed(price_units, price_scale)?,
            quantity: fixed(quantity_units, quantity_scale)?,
            execution_unix_nanos: time,
            provenance: decode_provenance(&provenance)?,
        };
        fill.validate().map_err(|error| error.to_string())?;
        Ok(fill)
    })())
}

impl TradingStore {
    pub(super) fn load_fills(&self) -> Result<VecDeque<Fill>, String> {
        // Replay order is execution time with rowid as the commit-order tie-break; the
        // retention checkpoint fold uses the same order so checkpoint + suffix replays exactly.
        let mut statement = self
            .connection
            .prepare(&format!(
                "{FILL_COLUMNS} FROM fills ORDER BY execution_unix_nanos, rowid"
            ))
            .map_err(database_error)?;
        let rows = statement
            .query_map([], decode_fill_row)
            .map_err(database_error)?;
        let mut output = VecDeque::new();
        for row in rows {
            output.push_back(row.map_err(database_error)??);
        }
        Ok(output)
    }

    /// Loads full fill rows inside an existing transaction, in replay order.
    pub(super) fn load_fills_by_ids(
        transaction: &rusqlite::Transaction<'_>,
        ids: &[String],
    ) -> Result<Vec<Fill>, String> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (1..=ids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut statement = transaction
            .prepare(&format!(
                "{FILL_COLUMNS} FROM fills WHERE id IN ({placeholders})
                 ORDER BY execution_unix_nanos, rowid"
            ))
            .map_err(database_error)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(ids.iter()), decode_fill_row)
            .map_err(database_error)?;
        let mut output = Vec::with_capacity(ids.len());
        for row in rows {
            output.push(row.map_err(database_error)??);
        }
        Ok(output)
    }

    pub(super) fn load_fill_accounting(&self) -> Result<BTreeMap<FillId, FillAccounting>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT fill_id, cycle_id, realized_gross_units, realized_gross_scale,
                cycle_realized_units, cycle_realized_scale, cycle_closed,
                commission_units, commission_scale
             FROM fill_accounting",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<u8>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<u8>>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, u8>(8)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                id,
                cycle_id,
                realized_units,
                realized_scale,
                cycle_units,
                cycle_scale,
                cycle_closed,
                commission_units,
                commission_scale,
            ) = row.map_err(database_error)?;
            output.insert(
                FillId::try_new(id).map_err(|error| error.to_string())?,
                FillAccounting {
                    cycle_id,
                    realized_gross: super::optional_fixed(realized_units, realized_scale)?,
                    commission: fixed(commission_units, commission_scale)?,
                    cycle_realized: super::optional_fixed(cycle_units, cycle_scale)?,
                    cycle_closed,
                },
            );
        }
        Ok(output)
    }

    pub(super) fn load_trade_cycles(
        &self,
    ) -> Result<BTreeMap<(TradingAccountId, InstrumentId), TradeCycle>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, instrument_id, cycle_id, peak_quantity_units,
                    peak_quantity_scale, realized_units, realized_scale,
                    risk_realized_units, risk_realized_scale, pnl_known
                 FROM trade_cycles ORDER BY account_id, instrument_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, u8>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, u8>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, u8>(8)?,
                    row.get::<_, bool>(9)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                account,
                instrument,
                cycle_id,
                peak_units,
                peak_scale,
                units,
                scale,
                risk_units,
                risk_scale,
                pnl_known,
            ) = row.map_err(database_error)?;
            let account_id = TradingAccountId::try_new(account).map_err(|e| e.to_string())?;
            let instrument_id = InstrumentId::try_new(instrument).map_err(|e| e.to_string())?;
            output.insert(
                (account_id.clone(), instrument_id.clone()),
                TradeCycle {
                    account_id,
                    instrument_id,
                    cycle_id,
                    peak_quantity: fixed(peak_units, peak_scale)?,
                    realized_pnl: fixed(units, scale)?,
                    risk_realized_pnl: fixed(risk_units, risk_scale)?,
                    pnl_known,
                },
            );
        }
        Ok(output)
    }
}

impl TradingStore {
    pub(super) fn load_broker_fill_positions(&self) -> Result<BTreeMap<FillId, String>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT fill_id, broker_position_id FROM broker_deals
                 WHERE broker_position_id IS NOT NULL",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(database_error)?;
        rows.map(|row| {
            let (fill_id, broker_position_id) = row.map_err(database_error)?;
            Ok((
                FillId::try_new(fill_id).map_err(|error| error.to_string())?,
                broker_position_id,
            ))
        })
        .collect()
    }
}
