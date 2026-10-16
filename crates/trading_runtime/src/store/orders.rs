use super::{
    BTreeMap, ClientOrderId, ContractDate, ContractMetadata, FixedPoint, InstrumentClass,
    InstrumentDecimal, InstrumentId, InstrumentMetadataProvenance, Order, OrderEvent, OrderEventId,
    OrderEventKind, OrderId, OrderSide, OrderStatus, OrderType, PriceGrid, SessionHours, StepTx,
    TimeInForce, TradingAccountId, TradingInstrument, TradingProvenance, TradingStore, Transaction,
    Value, VecDeque, database_error, fixed, json, json_i64, json_string, json_u64, optional_fixed,
    params, split_optional, sqlite_i64, sqlite_u64,
};
use aeris_trading::OrderEventReason;

const LOAD_ORDERS_SQL: &str = "SELECT id, client_order_id, account_id, instrument_id, side,
    order_type, time_in_force, quantity_units, quantity_scale, limit_units, limit_scale,
    stop_units, stop_scale, status, submitted_unix_nanos, provenance_json,
    filled_units, filled_scale, terminal_unix_nanos, revision,
    accepted_unix_nanos, updated_unix_nanos, eligible_after_unix_nanos,
    triggered_unix_nanos, expires_at_unix_nanos
    FROM orders ORDER BY submitted_unix_nanos";

impl TradingStore {
    /// All the facts produced by one simulated order command are committed together.
    pub(crate) fn commit_order_step(&mut self, changes: &super::OrderStep) -> Result<(), String> {
        let tx = StepTx::begin(self).map_err(database_error)?;
        for id in &changes.deleted_brackets {
            tx.execute("DELETE FROM managed_brackets WHERE bracket_id = ?1", [id])
                .map_err(database_error)?;
        }
        for (order, event) in &changes.inserted {
            insert_order_row(&tx, order)?;
            insert_event_row(&tx, event)?;
        }
        for (order, event) in &changes.updated {
            tx.execute(
                "UPDATE orders SET status = ?1, time_in_force = ?2,
                    limit_units = ?3, limit_scale = ?4, stop_units = ?5,
                    stop_scale = ?6, provenance_json = ?7, terminal_unix_nanos = ?8,
                    revision = ?9, updated_unix_nanos = ?10,
                    eligible_after_unix_nanos = ?11, triggered_unix_nanos = ?12,
                    expires_at_unix_nanos = ?13 WHERE id = ?14",
                params![
                    order.status.as_str(),
                    order.time_in_force.as_str(),
                    order.limit_price.map(FixedPoint::units),
                    order.limit_price.map(FixedPoint::scale),
                    order.stop_price.map(FixedPoint::units),
                    order.stop_price.map(FixedPoint::scale),
                    encode_provenance(&order.provenance),
                    order.terminal_unix_nanos,
                    order.revision,
                    order.updated_unix_nanos,
                    order.eligible_after_unix_nanos,
                    order.triggered_unix_nanos,
                    order.expires_at_unix_nanos,
                    order.id.as_str(),
                ],
            )
            .map_err(database_error)?;
            insert_event_row(&tx, event)?;
        }
        for bracket in &changes.brackets {
            super::groups::upsert_managed_bracket(&tx, bracket)?;
        }
        for protective in &changes.protective {
            tx.execute(
                "INSERT INTO protective_orders(client_order_id, role) VALUES (?1, ?2)",
                params![
                    protective.client_order_id.as_str(),
                    protective.role.as_str()
                ],
            )
            .map_err(database_error)?;
        }
        update_next_sequence(&tx, changes.next_sequence)?;
        // Preserve the existing revision sequence for bulk cancellations while
        // making their individual transitions durable in the same owner step.
        if changes.updated.len() > 1 {
            tx.execute(
                "UPDATE metadata SET value = min(value + ?1, 9223372036854775807)
                 WHERE key = 'revision'",
                [i64::try_from(changes.updated.len() - 1).map_err(|error| error.to_string())?],
            )
            .map_err(database_error)?;
        }
        tx.commit().map_err(database_error)
    }

    pub(crate) fn insert_order(
        &mut self,
        order: &Order,
        event: &OrderEvent,
        next_sequence: u64,
        bump_revision: bool,
    ) -> Result<(), String> {
        let transaction = StepTx::begin(self).map_err(database_error)?;
        insert_order_row(&transaction, order)?;
        insert_event_row(&transaction, event)?;
        update_next_sequence(&transaction, next_sequence)?;
        if bump_revision {
            transaction.commit().map_err(database_error)
        } else {
            transaction
                .commit_without_revision()
                .map_err(database_error)
        }
    }

    /// Persists a submitted broker order with its broker mapping row; the broker order id
    /// is bound when the venue first reports the order.
    pub(crate) fn insert_broker_order(
        &mut self,
        order: &Order,
        event: &OrderEvent,
        next_sequence: u64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        insert_order_row(&transaction, order)?;
        insert_event_row(&transaction, event)?;
        transaction
            .execute(
                "INSERT INTO broker_orders(order_id, broker_order_id, client_order_id,
                    session_generation, updated_unix_nanos)
                 VALUES (?1, NULL, ?2, ?3, ?4)",
                params![
                    order.id.as_str(),
                    order.client_order_id.as_str(),
                    sqlite_i64(order.provenance.session_generation)?,
                    order.provenance.observed_unix_nanos,
                ],
            )
            .map_err(database_error)?;
        update_next_sequence(&transaction, next_sequence)?;
        StepTx::commit_revision(transaction).map_err(database_error)
    }

    pub(crate) fn set_venue_generation(&self, generation: u64) -> Result<(), String> {
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        transaction
            .execute(
                "UPDATE metadata SET value = ?1 WHERE key = 'venue_generation'",
                [sqlite_i64(generation)?],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    /// Persists one broker order transition. `binding` records the broker order id the
    /// venue reported for it, in the same transaction.
    pub(crate) fn transition_broker_order(
        &mut self,
        order: &Order,
        event: &OrderEvent,
        next_sequence: u64,
        binding: Option<&str>,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "UPDATE orders SET status = ?1, time_in_force = ?2, limit_units = ?3,
             limit_scale = ?4, stop_units = ?5, stop_scale = ?6, provenance_json = ?7,
             quantity_units = ?8, quantity_scale = ?9, filled_units = ?10, filled_scale = ?11,
             revision = ?12, updated_unix_nanos = ?13, terminal_unix_nanos = ?14,
             eligible_after_unix_nanos = ?15, triggered_unix_nanos = ?16,
             expires_at_unix_nanos = ?17 WHERE id = ?18",
                params![
                    order.status.as_str(),
                    order.time_in_force.as_str(),
                    order.limit_price.map(FixedPoint::units),
                    order.limit_price.map(FixedPoint::scale),
                    order.stop_price.map(FixedPoint::units),
                    order.stop_price.map(FixedPoint::scale),
                    encode_provenance(&order.provenance),
                    order.quantity.units(),
                    order.quantity.scale(),
                    order.filled_quantity.units(),
                    order.filled_quantity.scale(),
                    order.revision,
                    order.updated_unix_nanos,
                    order.terminal_unix_nanos,
                    order.eligible_after_unix_nanos,
                    order.triggered_unix_nanos,
                    order.expires_at_unix_nanos,
                    order.id.as_str(),
                ],
            )
            .map_err(database_error)?;
        if let Some(broker_order_id) = binding {
            transaction
                .execute(
                    "UPDATE broker_orders SET broker_order_id = ?1, session_generation = ?2,
                        updated_unix_nanos = ?3 WHERE order_id = ?4",
                    params![
                        broker_order_id,
                        sqlite_i64(order.provenance.session_generation)?,
                        order.provenance.observed_unix_nanos,
                        order.id.as_str(),
                    ],
                )
                .map_err(database_error)?;
        }
        insert_event_row(&transaction, event)?;
        update_next_sequence(&transaction, next_sequence)?;
        StepTx::commit_revision(transaction).map_err(database_error)
    }
}

impl TradingStore {
    pub(crate) fn modify_order(
        &mut self,
        order: &Order,
        event: &OrderEvent,
        next_sequence: u64,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "UPDATE orders SET time_in_force = ?1, limit_units = ?2, limit_scale = ?3,
                    stop_units = ?4, stop_scale = ?5, provenance_json = ?6,
                    quantity_units = ?7, quantity_scale = ?8, revision = ?9,
                    updated_unix_nanos = ?10, eligible_after_unix_nanos = ?11,
                    triggered_unix_nanos = ?12, expires_at_unix_nanos = ?13
                 WHERE id = ?14 AND status IN ('working', 'partially_filled')",
                params![
                    order.time_in_force.as_str(),
                    order.limit_price.map(FixedPoint::units),
                    order.limit_price.map(FixedPoint::scale),
                    order.stop_price.map(FixedPoint::units),
                    order.stop_price.map(FixedPoint::scale),
                    encode_provenance(&order.provenance),
                    order.quantity.units(),
                    order.quantity.scale(),
                    order.revision,
                    order.updated_unix_nanos,
                    order.eligible_after_unix_nanos,
                    order.triggered_unix_nanos,
                    order.expires_at_unix_nanos,
                    order.id.as_str(),
                ],
            )
            .map_err(database_error)?;
        insert_event_row(&transaction, event)?;
        update_next_sequence(&transaction, next_sequence)?;
        StepTx::commit_revision(transaction).map_err(database_error)
    }
}

impl TradingStore {
    pub(super) fn load_market_routes(
        &self,
    ) -> Result<BTreeMap<InstrumentId, super::MarketRoute>, String> {
        let mut routes = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, market_route_json FROM instruments WHERE market_route_json IS NOT NULL",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(database_error)?;
        for row in rows {
            let (id, json) = row.map_err(database_error)?;
            let id = InstrumentId::try_new(id).map_err(|error| error.to_string())?;
            let route = super::decode_market_route(&json)?;
            if route.instrument_id != id {
                return Err("stored market route has a different instrument identity".into());
            }
            routes.insert(id, route);
        }
        Ok(routes)
    }

    pub(super) fn load_instruments(
        &self,
    ) -> Result<BTreeMap<InstrumentId, TradingInstrument>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare("SELECT id, price_scale, quantity_scale, contract_json FROM instruments")
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u8>(1)?,
                    row.get::<_, u8>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (id, price_scale, quantity_scale, contract) = row.map_err(database_error)?;
            let instrument_id = InstrumentId::try_new(id).map_err(|error| error.to_string())?;
            let contract = decode_contract(&contract, &instrument_id)?;
            output.insert(
                instrument_id.clone(),
                TradingInstrument {
                    instrument_id,
                    price_scale,
                    quantity_scale,
                    contract,
                },
            );
        }
        Ok(output)
    }

    pub(super) fn load_orders(&self) -> Result<BTreeMap<OrderId, Order>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(LOAD_ORDERS_SQL)
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, u8>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, Option<u8>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, Option<u8>>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, i64>(16)?,
                    row.get::<_, u8>(17)?,
                    row.get::<_, Option<i64>>(18)?,
                    row.get::<_, u32>(19)?,
                    row.get::<_, Option<i64>>(20)?,
                    row.get::<_, Option<i64>>(21)?,
                    row.get::<_, Option<i64>>(22)?,
                    row.get::<_, Option<i64>>(23)?,
                    row.get::<_, Option<i64>>(24)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                id,
                client,
                account,
                instrument,
                side,
                order_type,
                tif,
                quantity_units,
                quantity_scale,
                limit_units,
                limit_scale,
                stop_units,
                stop_scale,
                status,
                submitted,
                provenance,
                filled_units,
                filled_scale,
                terminal_unix_nanos,
                revision,
                accepted_unix_nanos,
                updated_unix_nanos,
                eligible_after_unix_nanos,
                triggered_unix_nanos,
                expires_at_unix_nanos,
            ) = row.map_err(database_error)?;
            let id = OrderId::try_new(id).map_err(|error| error.to_string())?;
            let order = Order {
                id: id.clone(),
                client_order_id: ClientOrderId::try_new(client)
                    .map_err(|error| error.to_string())?,
                account_id: TradingAccountId::try_new(account)
                    .map_err(|error| error.to_string())?,
                instrument_id: InstrumentId::try_new(instrument)
                    .map_err(|error| error.to_string())?,
                side: parse_side(&side)?,
                order_type: parse_order_type(&order_type)?,
                time_in_force: parse_time_in_force(&tif)?,
                quantity: fixed(quantity_units, quantity_scale)?,
                filled_quantity: fixed(filled_units, filled_scale)?,
                limit_price: optional_fixed(limit_units, limit_scale)?,
                stop_price: optional_fixed(stop_units, stop_scale)?,
                status: parse_status(&status)?,
                submitted_unix_nanos: submitted,
                revision,
                // Early V22 builds wrote this nullable column only at migration.
                accepted_unix_nanos: accepted_unix_nanos.unwrap_or(submitted),
                updated_unix_nanos: updated_unix_nanos
                    .or(accepted_unix_nanos)
                    .unwrap_or(submitted),
                eligible_after_unix_nanos: eligible_after_unix_nanos.unwrap_or(submitted),
                triggered_unix_nanos,
                expires_at_unix_nanos,
                terminal_unix_nanos,
                provenance: decode_provenance(&provenance)?,
            };
            order.validate().map_err(|error| error.to_string())?;
            output.insert(id, order);
        }
        Ok(output)
    }

    pub(super) fn load_events(&self) -> Result<VecDeque<OrderEvent>, String> {
        let mut output = VecDeque::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, order_id, sequence, kind, event_unix_nanos, detail, provenance_json,
                        reason_code
             FROM order_events ORDER BY sequence",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (id, order, sequence, kind, time, detail, provenance, reason) =
                row.map_err(database_error)?;
            let event = OrderEvent {
                id: OrderEventId::try_new(id).map_err(|error| error.to_string())?,
                order_id: OrderId::try_new(order).map_err(|error| error.to_string())?,
                sequence: sqlite_u64(sequence)?,
                kind: parse_event_kind(&kind)?,
                reason: reason.as_deref().map(parse_event_reason).transpose()?,
                event_unix_nanos: time,
                detail,
                provenance: decode_provenance(&provenance)?,
            };
            event.validate().map_err(|error| error.to_string())?;
            output.push_back(event);
        }
        Ok(output)
    }
}

impl TradingStore {
    pub(super) fn load_broker_order_ids(&self) -> Result<BTreeMap<String, OrderId>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT broker_order_id, order_id FROM broker_orders
                 WHERE broker_order_id IS NOT NULL",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(database_error)?;
        rows.map(|row| {
            let (broker_order_id, order_id) = row.map_err(database_error)?;
            Ok((
                broker_order_id,
                OrderId::try_new(order_id).map_err(|error| error.to_string())?,
            ))
        })
        .collect()
    }
}

pub(super) fn insert_order_row(transaction: &Transaction<'_>, order: &Order) -> Result<(), String> {
    let (limit_units, limit_scale) = split_optional(order.limit_price);
    let (stop_units, stop_scale) = split_optional(order.stop_price);
    transaction
        .execute(
            "INSERT INTO orders(id, client_order_id, account_id, instrument_id, side, order_type,
            time_in_force, quantity_units, quantity_scale, limit_units, limit_scale, stop_units,
            stop_scale, status, submitted_unix_nanos, provenance_json, filled_units, filled_scale,
            terminal_unix_nanos, revision, accepted_unix_nanos, updated_unix_nanos,
            eligible_after_unix_nanos, triggered_unix_nanos, expires_at_unix_nanos)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
            ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            params![
                order.id.as_str(),
                order.client_order_id.as_str(),
                order.account_id.as_str(),
                order.instrument_id.as_str(),
                order.side.as_str(),
                order.order_type.as_str(),
                order.time_in_force.as_str(),
                order.quantity.units(),
                order.quantity.scale(),
                limit_units,
                limit_scale,
                stop_units,
                stop_scale,
                order.status.as_str(),
                order.submitted_unix_nanos,
                encode_provenance(&order.provenance),
                order.filled_quantity.units(),
                order.filled_quantity.scale(),
                order.terminal_unix_nanos,
                order.revision,
                order.accepted_unix_nanos,
                order.updated_unix_nanos,
                order.eligible_after_unix_nanos,
                order.triggered_unix_nanos,
                order.expires_at_unix_nanos,
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

pub(super) fn insert_event_row(
    transaction: &Transaction<'_>,
    event: &OrderEvent,
) -> Result<(), String> {
    let sequence = sqlite_i64(event.sequence)?;
    transaction.execute(
        "INSERT INTO order_events(id, order_id, sequence, kind, event_unix_nanos, detail, provenance_json,
             reason_code)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![event.id.as_str(), event.order_id.as_str(), sequence, event.kind.as_str(),
            event.event_unix_nanos, event.detail, encode_provenance(&event.provenance),
            event.reason.map(OrderEventReason::as_str)],
    ).map_err(database_error)?;
    Ok(())
}

pub(super) fn update_next_sequence(transaction: &Transaction<'_>, next: u64) -> Result<(), String> {
    let next = sqlite_i64(next)?;
    transaction
        .execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'next_sequence'",
            [next],
        )
        .map_err(database_error)?;
    Ok(())
}

pub(super) fn encode_provenance(value: &TradingProvenance) -> String {
    json!({"venue_id": value.venue_id, "provider_id": value.provider_id,
        "session_generation": value.session_generation, "source_sequence": value.source_sequence,
        "observed_unix_nanos": value.observed_unix_nanos})
    .to_string()
}

pub(super) fn decode_provenance(raw: &str) -> Result<TradingProvenance, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|_| "stored provenance is invalid".to_string())?;
    let provenance = TradingProvenance {
        venue_id: json_string(&value, "venue_id")?,
        provider_id: json_string(&value, "provider_id")?,
        session_generation: json_u64(&value, "session_generation")?,
        source_sequence: json_u64(&value, "source_sequence")?,
        observed_unix_nanos: json_i64(&value, "observed_unix_nanos")?,
    };
    provenance.validate().map_err(|error| error.to_string())?;
    Ok(provenance)
}

pub(super) fn encode_contract(contract: &ContractMetadata) -> Result<String, String> {
    contract.validate().map_err(|error| error.to_string())?;
    let decimal = |value: Option<InstrumentDecimal>| {
        value.map(|value| {
            json!({
                "units": value.units(), "scale": value.scale()
            })
        })
    };
    let date = |value: Option<ContractDate>| {
        value.map(|value| {
            json!({
                "year": value.year, "month": value.month, "day": value.day
            })
        })
    };
    let price_grid = match contract.price_grid {
        PriceGrid::Fixed(tick) => json!({"kind": "fixed", "tick": decimal(Some(tick))}),
        PriceGrid::SignificantFigures {
            figures,
            max_decimals,
        } => {
            json!({"kind": "significant_figures", "figures": figures, "max_decimals": max_decimals})
        }
        PriceGrid::Unknown => json!({"kind": "unknown"}),
    };
    let instrument_class = match contract.instrument_class {
        InstrumentClass::Equity => "equity",
        InstrumentClass::Option => "option",
        InstrumentClass::Future => "future",
        InstrumentClass::ForeignExchange => "foreign_exchange",
        InstrumentClass::CryptoSpot => "crypto_spot",
        InstrumentClass::CryptoPerpetual => "crypto_perpetual",
        InstrumentClass::Other => "other",
    };
    Ok(json!({"tick_size": decimal(contract.tick_size), "point_value": decimal(contract.point_value),
        "price_grid": price_grid, "instrument_class": instrument_class,
        "order_quantity_increment": decimal(contract.order_quantity_increment),
        "currency": contract.currency, "expiry": date(contract.expiry),
        "first_notice": date(contract.first_notice), "last_trade": date(contract.last_trade),
        "session_hours": contract.session_hours.iter().map(|value| json!({"weekday": value.weekday,
            "open_seconds": value.open_seconds, "close_seconds": value.close_seconds,
            "timezone": value.timezone})).collect::<Vec<_>>(),
        "provenance": {"provider_id": contract.provenance.provider_id,
            "provider_symbol": contract.provenance.provider_symbol,
            "display_symbol": contract.provenance.display_symbol,
            "session_generation": contract.provenance.session_generation}}).to_string())
}

pub(super) fn decode_contract(
    raw: &str,
    instrument_id: &InstrumentId,
) -> Result<ContractMetadata, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|_| "stored contract metadata is invalid".to_string())?;
    let provenance = value
        .get("provenance")
        .ok_or_else(|| "stored contract provenance is missing".to_string())?;
    let sessions = value
        .get("session_hours")
        .and_then(Value::as_array)
        .ok_or_else(|| "stored session hours are invalid".to_string())?
        .iter()
        .map(|session| {
            Ok(SessionHours {
                weekday: json_u64(session, "weekday")?
                    .try_into()
                    .map_err(|_| "weekday is invalid".to_string())?,
                open_seconds: json_u64(session, "open_seconds")?
                    .try_into()
                    .map_err(|_| "open time is invalid".to_string())?,
                close_seconds: json_u64(session, "close_seconds")?
                    .try_into()
                    .map_err(|_| "close time is invalid".to_string())?,
                timezone: json_string(session, "timezone")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let contract = ContractMetadata {
        tick_size: decode_decimal(value.get("tick_size"))?,
        price_grid: match value
            .get("price_grid")
            .and_then(|grid| grid.get("kind"))
            .and_then(Value::as_str)
        {
            None | Some("unknown") => PriceGrid::Unknown,
            Some("fixed") => PriceGrid::Fixed(
                decode_decimal(value.get("price_grid").and_then(|grid| grid.get("tick")))?
                    .ok_or_else(|| "stored fixed price grid has no tick".to_string())?,
            ),
            Some("significant_figures") => {
                let grid = value
                    .get("price_grid")
                    .ok_or_else(|| "stored price grid is missing".to_string())?;
                PriceGrid::SignificantFigures {
                    figures: json_u64(grid, "figures")?
                        .try_into()
                        .map_err(|_| "invalid significant figures".to_string())?,
                    max_decimals: json_u64(grid, "max_decimals")?
                        .try_into()
                        .map_err(|_| "invalid maximum decimals".to_string())?,
                }
            }
            Some(_) => return Err("stored price grid is unknown".into()),
        },
        instrument_class: match value.get("instrument_class").and_then(Value::as_str) {
            None | Some("other") => InstrumentClass::Other,
            Some("equity") => InstrumentClass::Equity,
            Some("option") => InstrumentClass::Option,
            Some("future") => InstrumentClass::Future,
            Some("foreign_exchange") => InstrumentClass::ForeignExchange,
            Some("crypto_spot") => InstrumentClass::CryptoSpot,
            Some("crypto_perpetual") => InstrumentClass::CryptoPerpetual,
            Some(_) => return Err("stored instrument class is unknown".into()),
        },
        point_value: decode_decimal(value.get("point_value"))?,
        order_quantity_increment: decode_decimal(value.get("order_quantity_increment"))?,
        currency: json_string(&value, "currency")?,
        expiry: decode_date(value.get("expiry"))?,
        first_notice: decode_date(value.get("first_notice"))?,
        last_trade: decode_date(value.get("last_trade"))?,
        session_hours: sessions,
        provenance: InstrumentMetadataProvenance {
            provider_id: json_string(provenance, "provider_id")?,
            provider_symbol: json_string(provenance, "provider_symbol")?,
            display_symbol: provenance
                .get("display_symbol")
                .and_then(Value::as_str)
                .or_else(|| instrument_id.as_str().strip_prefix("tastytrade:Future:/"))
                .or_else(|| provenance.get("provider_symbol").and_then(Value::as_str))
                .unwrap_or_default()
                .to_string(),
            session_generation: json_u64(provenance, "session_generation")?,
        },
    };
    contract.validate().map_err(|error| error.to_string())?;
    Ok(contract)
}

pub(super) fn decode_decimal(value: Option<&Value>) -> Result<Option<InstrumentDecimal>, String> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    Ok(Some(
        InstrumentDecimal::try_new(
            json_i64(value, "units")?,
            json_u64(value, "scale")?
                .try_into()
                .map_err(|_| "decimal scale is invalid".to_string())?,
        )
        .map_err(|error| error.to_string())?,
    ))
}

pub(super) fn decode_date(value: Option<&Value>) -> Result<Option<ContractDate>, String> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let date = ContractDate {
        year: json_u64(value, "year")?
            .try_into()
            .map_err(|_| "year is invalid".to_string())?,
        month: json_u64(value, "month")?
            .try_into()
            .map_err(|_| "month is invalid".to_string())?,
        day: json_u64(value, "day")?
            .try_into()
            .map_err(|_| "day is invalid".to_string())?,
    };
    date.validate().map_err(|error| error.to_string())?;
    Ok(Some(date))
}

pub(super) fn parse_side(value: &str) -> Result<OrderSide, String> {
    match value {
        "buy" => Ok(OrderSide::Buy),
        "sell" => Ok(OrderSide::Sell),
        _ => Err("stored order side is invalid".to_string()),
    }
}
pub(super) fn parse_order_type(value: &str) -> Result<OrderType, String> {
    match value {
        "market" => Ok(OrderType::Market),
        "limit" => Ok(OrderType::Limit),
        "stop" => Ok(OrderType::Stop),
        "stop_limit" => Ok(OrderType::StopLimit),
        _ => Err("stored order type is invalid".to_string()),
    }
}
pub(super) fn parse_time_in_force(value: &str) -> Result<TimeInForce, String> {
    match value {
        "day" => Ok(TimeInForce::Day),
        "gtc" => Ok(TimeInForce::GoodTillCancelled),
        "ioc" => Ok(TimeInForce::ImmediateOrCancel),
        "fok" => Ok(TimeInForce::FillOrKill),
        _ => Err("stored time in force is invalid".to_string()),
    }
}
pub(super) fn parse_status(value: &str) -> Result<OrderStatus, String> {
    match value {
        "inactive" => Ok(OrderStatus::Inactive),
        "pending" => Ok(OrderStatus::Pending),
        "working" => Ok(OrderStatus::Working),
        "pending_modify" => Ok(OrderStatus::PendingModify),
        "partially_filled" => Ok(OrderStatus::PartiallyFilled),
        "pending_cancel" => Ok(OrderStatus::PendingCancel),
        "filled" => Ok(OrderStatus::Filled),
        "cancelled" => Ok(OrderStatus::Cancelled),
        "expired" => Ok(OrderStatus::Expired),
        "rejected" => Ok(OrderStatus::Rejected),
        _ => Err("stored order status is invalid".to_string()),
    }
}
pub(super) fn parse_event_kind(value: &str) -> Result<OrderEventKind, String> {
    match value {
        "accepted" => Ok(OrderEventKind::Accepted),
        "activated" => Ok(OrderEventKind::Activated),
        "modified" => Ok(OrderEventKind::Modified),
        "resized" => Ok(OrderEventKind::Resized),
        "triggered" => Ok(OrderEventKind::Triggered),
        "filled" => Ok(OrderEventKind::Filled),
        "cancelled" => Ok(OrderEventKind::Cancelled),
        "expired" => Ok(OrderEventKind::Expired),
        "rejected" => Ok(OrderEventKind::Rejected),
        _ => Err("stored order event kind is invalid".to_string()),
    }
}

fn parse_event_reason(value: &str) -> Result<OrderEventReason, String> {
    match value {
        "touch_fill" => Ok(OrderEventReason::TouchFill),
        "user_cancel" => Ok(OrderEventReason::UserCancel),
        "user_modify" => Ok(OrderEventReason::UserModify),
        "management" => Ok(OrderEventReason::Management),
        "coverage" => Ok(OrderEventReason::Coverage),
        "leg_moved" => Ok(OrderEventReason::LegMoved),
        "parent_repriced" => Ok(OrderEventReason::ParentRepriced),
        "coverage_shrink" => Ok(OrderEventReason::CoverageShrink),
        "coverage_grow" => Ok(OrderEventReason::CoverageGrow),
        "cascade" => Ok(OrderEventReason::Cascade),
        "kill_switch" => Ok(OrderEventReason::KillSwitch),
        "flatten" => Ok(OrderEventReason::Flatten),
        "not_marketable" => Ok(OrderEventReason::NotMarketable),
        "no_quote" => Ok(OrderEventReason::NoQuote),
        "risk_warning" => Ok(OrderEventReason::RiskWarning),
        "local_managed" => Ok(OrderEventReason::LocalManaged),
        _ => Err("stored order event reason is invalid".to_string()),
    }
}

#[cfg(test)]
mod lifecycle_wire_tests {
    use super::*;

    #[test]
    fn new_status_and_event_values_round_trip_without_aliases() {
        for status in [OrderStatus::Inactive, OrderStatus::Expired] {
            assert_eq!(parse_status(status.as_str()), Ok(status));
        }
        for kind in [
            OrderEventKind::Triggered,
            OrderEventKind::Expired,
            OrderEventKind::Activated,
            OrderEventKind::Resized,
        ] {
            assert_eq!(parse_event_kind(kind.as_str()), Ok(kind));
        }
        assert!(
            parse_status("triggered").is_err(),
            "trigger is an event, not a status"
        );
    }

    #[test]
    fn recognized_historical_reason_codes_have_exact_typed_values() {
        for reason in [
            OrderEventReason::TouchFill,
            OrderEventReason::UserCancel,
            OrderEventReason::UserModify,
            OrderEventReason::Management,
            OrderEventReason::Coverage,
            OrderEventReason::LegMoved,
            OrderEventReason::ParentRepriced,
            OrderEventReason::CoverageShrink,
            OrderEventReason::CoverageGrow,
            OrderEventReason::Cascade,
            OrderEventReason::KillSwitch,
            OrderEventReason::Flatten,
            OrderEventReason::NotMarketable,
            OrderEventReason::RiskWarning,
            OrderEventReason::LocalManaged,
        ] {
            assert_eq!(parse_event_reason(reason.as_str()), Ok(reason));
        }
        assert!(parse_event_reason("display text is not a reason").is_err());
    }
}
