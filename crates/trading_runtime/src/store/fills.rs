use super::{
    BTreeMap, BrokerFillRecord, Fill, FillId, FillPolicyPersistence, FixedPoint, InstrumentId,
    Order, OrderEvent, OrderId, Position, TradingAccountId, TradingStore, VecDeque, database_error,
    decode_provenance, encode_provenance, fixed, insert_event_row, insert_order_row, params,
    parse_side, sqlite_i64, update_next_sequence, upsert_discipline_state, upsert_position,
    upsert_risk_rule_state, upsert_risk_trade_cycle,
};

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
                    filled_units = ?4, filled_scale = ?5, provenance_json = ?6 WHERE id = ?7",
                params![
                    order.status.as_str(),
                    order.quantity.units(),
                    order.quantity.scale(),
                    order.filled_quantity.units(),
                    order.filled_quantity.scale(),
                    encode_provenance(&order.provenance),
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
                    provenance_json, realized_pnl_units, realized_pnl_scale)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
                    record.realized_pnl.map(FixedPoint::units),
                    record.realized_pnl.map(FixedPoint::scale),
                ],
            )
            .map_err(database_error)?;
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
        transaction.commit().map_err(database_error)
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
                "UPDATE orders SET status = 'filled', filled_units = quantity_units,
                    filled_scale = quantity_scale
                 WHERE id = ?1 AND status IN ('working', 'partially_filled')",
                [order.id.as_str()],
            )
            .map_err(database_error)?;
        insert_event_row(&transaction, event)?;
        transaction
            .execute(
                "INSERT INTO fills(id, order_id, account_id, instrument_id, side, price_units,
                    price_scale, quantity_units, quantity_scale, execution_unix_nanos,
                    provenance_json, completed_trade_pnl_units, completed_trade_pnl_scale,
                    realized_pnl_units, realized_pnl_scale)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                    policy.completed_trade_pnl.map(FixedPoint::units),
                    policy.completed_trade_pnl.map(FixedPoint::scale),
                    policy.fill_realized_pnl.map(FixedPoint::units),
                    policy.fill_realized_pnl.map(FixedPoint::scale),
                ],
            )
            .map_err(database_error)?;
        upsert_position(&transaction, position)?;
        if let Some(cycle) = policy.trade_pnl_cycle {
            transaction
                .execute(
                    "INSERT INTO trade_pnl_cycles(account_id, instrument_id, realized_units, realized_scale)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(account_id, instrument_id) DO UPDATE SET
                       realized_units=excluded.realized_units, realized_scale=excluded.realized_scale",
                    params![
                        fill.account_id.as_str(),
                        fill.instrument_id.as_str(),
                        cycle.units(),
                        cycle.scale(),
                    ],
                )
                .map_err(database_error)?;
        } else {
            transaction
                .execute(
                    "DELETE FROM trade_pnl_cycles WHERE account_id = ?1 AND instrument_id = ?2",
                    params![fill.account_id.as_str(), fill.instrument_id.as_str()],
                )
                .map_err(database_error)?;
        }
        if let Some(rule_state) = policy.rule_state {
            upsert_risk_rule_state(&transaction, rule_state)?;
        }
        upsert_risk_trade_cycle(&transaction, policy.trade_cycle)?;
        upsert_discipline_state(&transaction, policy.discipline_state)?;
        update_next_sequence(&transaction, next_sequence)?;
        transaction.commit().map_err(database_error)
    }
}

impl TradingStore {
    pub(super) fn load_fills(&self) -> Result<VecDeque<Fill>, String> {
        let mut output = VecDeque::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, order_id, account_id, instrument_id, side, price_units, price_scale,
                    quantity_units, quantity_scale, execution_unix_nanos, provenance_json
             FROM fills ORDER BY execution_unix_nanos",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, u8>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, u8>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, String>(10)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                id,
                order,
                account,
                instrument,
                side,
                price_units,
                price_scale,
                quantity_units,
                quantity_scale,
                time,
                provenance,
            ) = row.map_err(database_error)?;
            let fill = Fill {
                id: FillId::try_new(id).map_err(|error| error.to_string())?,
                order_id: OrderId::try_new(order).map_err(|error| error.to_string())?,
                account_id: TradingAccountId::try_new(account)
                    .map_err(|error| error.to_string())?,
                instrument_id: InstrumentId::try_new(instrument)
                    .map_err(|error| error.to_string())?,
                side: parse_side(&side)?,
                price: fixed(price_units, price_scale)?,
                quantity: fixed(quantity_units, quantity_scale)?,
                execution_unix_nanos: time,
                provenance: decode_provenance(&provenance)?,
            };
            fill.validate().map_err(|error| error.to_string())?;
            output.push_back(fill);
        }
        Ok(output)
    }

    pub(super) fn load_completed_trade_pnl(&self) -> Result<BTreeMap<FillId, FixedPoint>, String> {
        self.load_fill_amounts(
            "SELECT id, completed_trade_pnl_units, completed_trade_pnl_scale
             FROM fills WHERE completed_trade_pnl_units IS NOT NULL
             ORDER BY execution_unix_nanos",
        )
    }

    pub(super) fn load_fill_realized_pnl(&self) -> Result<BTreeMap<FillId, FixedPoint>, String> {
        self.load_fill_amounts(
            "SELECT id, realized_pnl_units, realized_pnl_scale
             FROM fills WHERE realized_pnl_units IS NOT NULL
             ORDER BY execution_unix_nanos",
        )
    }

    /// Loads one optional fixed-point amount per fill from a `(id, units, scale)` query.
    pub(super) fn load_fill_amounts(
        &self,
        query: &str,
    ) -> Result<BTreeMap<FillId, FixedPoint>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self.connection.prepare(query).map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, u8>(2)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (id, units, scale) = row.map_err(database_error)?;
            output.insert(
                FillId::try_new(id).map_err(|error| error.to_string())?,
                fixed(units, scale)?,
            );
        }
        Ok(output)
    }

    pub(super) fn load_trade_pnl_cycles(
        &self,
    ) -> Result<BTreeMap<(TradingAccountId, InstrumentId), FixedPoint>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, instrument_id, realized_units, realized_scale
                 FROM trade_pnl_cycles ORDER BY account_id, instrument_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, u8>(3)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (account, instrument, units, scale) = row.map_err(database_error)?;
            output.insert(
                (
                    TradingAccountId::try_new(account).map_err(|error| error.to_string())?,
                    InstrumentId::try_new(instrument).map_err(|error| error.to_string())?,
                ),
                fixed(units, scale)?,
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
