use super::{
    AccountEnvironment, BTreeMap, Connection, CostBasis, EquitySample, FeeModel, FeeSchedule,
    FillId, FixedPoint, InstrumentId, LedgerCheckpoint, LedgerEntry, LedgerEntryKind, Position,
    PositionCheckpoint, StepTx, TradingAccount, TradingAccountId, TradingInstrument, TradingStore,
    Transaction, Value, database_error, encode_contract, json_string, json_u64, params, sqlite_i64,
};

impl TradingStore {
    pub(super) fn load_accounts(
        &self,
    ) -> Result<BTreeMap<TradingAccountId, TradingAccount>, String> {
        let mut accounts = BTreeMap::new();
        {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT id, display_name, environment, currency, currency_scale,
                        starting_equity_units, starting_equity_scale, venue_id, broker_ref
                     FROM accounts ORDER BY id",
                )
                .map_err(database_error)?;
            let rows = statement
                .query_map([], |row| {
                    let environment: String = row.get(2)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        environment,
                        row.get::<_, String>(3)?,
                        row.get::<_, u8>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, Option<u8>>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                    ))
                })
                .map_err(database_error)?;
            for row in rows {
                let (
                    id,
                    display_name,
                    environment,
                    currency,
                    currency_scale,
                    starting_equity_units,
                    starting_equity_scale,
                    venue_id,
                    broker_ref,
                ) = row.map_err(database_error)?;
                let id = TradingAccountId::try_new(id).map_err(|error| error.to_string())?;
                let account = TradingAccount {
                    id: id.clone(),
                    display_name,
                    environment: parse_environment(&environment)?,
                    venue_id,
                    broker_ref,
                    currency,
                    currency_scale,
                    starting_equity: starting_equity_units
                        .zip(starting_equity_scale)
                        .map(|(units, scale)| FixedPoint::try_new(units, scale))
                        .transpose()
                        .map_err(|error| error.to_string())?,
                };
                account.validate().map_err(|error| error.to_string())?;
                accounts.insert(id, account);
            }
        }
        Ok(accounts)
    }
}

impl TradingStore {
    #[cfg(test)]
    pub(crate) fn put_account(&self, account: &TradingAccount) -> Result<(), String> {
        account.validate().map_err(|error| error.to_string())?;
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        upsert_account_row(&transaction, account)?;
        transaction.commit().map_err(database_error)
    }

    /// Registers one account and its funding deposit in a single commit. Returns whether the
    /// deposit landed: a gated pre-group store has no ledger tables yet and its backfill
    /// writes the deposit at conversion instead.
    pub(crate) fn put_account_with_deposit(
        &self,
        account: &TradingAccount,
        deposit: Option<&LedgerEntry>,
    ) -> Result<bool, String> {
        account.validate().map_err(|error| error.to_string())?;
        let deposit = match deposit {
            Some(entry) if self.ledger_tables_present()? => Some(entry),
            _ => None,
        };
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        upsert_account_row(&transaction, account)?;
        if let Some(entry) = deposit {
            insert_ledger_entries(&transaction, std::slice::from_ref(entry))?;
        }
        transaction.commit().map_err(database_error)?;
        Ok(deposit.is_some())
    }
}

/// The accounts-table upsert shared by account registration paths.
fn upsert_account_row(
    transaction: &Transaction<'_>,
    account: &TradingAccount,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO accounts(id, display_name, environment, currency, currency_scale,
                starting_equity_units, starting_equity_scale, venue_id, broker_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name,
                environment=excluded.environment, currency=excluded.currency,
                currency_scale=excluded.currency_scale,
                starting_equity_units=excluded.starting_equity_units,
                starting_equity_scale=excluded.starting_equity_scale,
                venue_id=excluded.venue_id, broker_ref=excluded.broker_ref",
            params![
                account.id.as_str(),
                account.display_name,
                account.environment.as_str(),
                account.currency,
                account.currency_scale,
                account.starting_equity.map(FixedPoint::units),
                account.starting_equity.map(FixedPoint::scale),
                account.venue_id,
                account.broker_ref,
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

impl TradingStore {
    pub(crate) fn put_instrument(&self, instrument: &TradingInstrument) -> Result<(), String> {
        let contract = encode_contract(&instrument.contract)?;
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO instruments(id, price_scale, quantity_scale, contract_json)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET price_scale=excluded.price_scale,
                    quantity_scale=excluded.quantity_scale, contract_json=excluded.contract_json",
                params![
                    instrument.instrument_id.as_str(),
                    instrument.price_scale,
                    instrument.quantity_scale,
                    contract,
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    pub(crate) fn put_verified_instrument(
        &mut self,
        instrument: &TradingInstrument,
        route: &super::MarketRoute,
    ) -> Result<(), String> {
        let contract = encode_contract(&instrument.contract)?;
        let route_json = super::encode_market_route(route)?;
        let tx = self.connection.transaction().map_err(database_error)?;
        tx.execute(
            "INSERT INTO instruments(id, price_scale, quantity_scale, contract_json, market_route_json)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET price_scale=excluded.price_scale,
                quantity_scale=excluded.quantity_scale, contract_json=excluded.contract_json,
                market_route_json=excluded.market_route_json",
            params![
                instrument.instrument_id.as_str(),
                instrument.price_scale,
                instrument.quantity_scale,
                contract,
                route_json,
            ],
        )
        .map_err(database_error)?;
        StepTx::commit_revision(tx).map_err(database_error)
    }
}

impl TradingStore {
    pub(crate) fn set_broker_balance(
        &self,
        account_id: &TradingAccountId,
        balance: FixedPoint,
        session_generation: u64,
    ) -> Result<(), String> {
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO broker_account_state(account_id, balance_units, balance_scale,
                    session_generation) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(account_id) DO UPDATE SET balance_units = excluded.balance_units,
                    balance_scale = excluded.balance_scale,
                    session_generation = excluded.session_generation",
                params![
                    account_id.as_str(),
                    balance.units(),
                    balance.scale(),
                    sqlite_i64(session_generation)?,
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }
}

impl TradingStore {
    pub(crate) fn update_position(
        &mut self,
        position: &Position,
        basis: Option<CostBasis>,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        upsert_position(&transaction, position)?;
        upsert_position_basis(&transaction, position, basis)?;
        transaction.commit().map_err(database_error)
    }
}

impl TradingStore {
    pub(super) fn load_broker_balances(
        &self,
    ) -> Result<BTreeMap<TradingAccountId, FixedPoint>, String> {
        let mut statement = self
            .connection
            .prepare("SELECT account_id, balance_units, balance_scale FROM broker_account_state")
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, u8>(2)?,
                ))
            })
            .map_err(database_error)?;
        rows.map(|row| {
            let (account, units, scale) = row.map_err(database_error)?;
            Ok((
                TradingAccountId::try_new(account).map_err(|error| error.to_string())?,
                fixed(units, scale)?,
            ))
        })
        .collect()
    }
}

impl TradingStore {
    pub(super) fn load_cost_basis(
        &self,
    ) -> Result<BTreeMap<(TradingAccountId, InstrumentId), CostBasis>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, instrument_id, cost_basis_hi, cost_basis_lo,
                    cost_price_scale, cost_quantity_scale FROM positions
                 WHERE cost_basis_hi IS NOT NULL",
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
                    row.get::<_, u8>(5)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (account, instrument, hi, lo, price_scale, quantity_scale) =
                row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account).map_err(|error| error.to_string())?;
            let instrument_id =
                InstrumentId::try_new(instrument).map_err(|error| error.to_string())?;
            output.insert(
                (account_id, instrument_id),
                CostBasis {
                    units: join_i128(hi, lo),
                    scale: price_scale
                        .checked_add(quantity_scale)
                        .ok_or_else(|| "stored cost basis scale overflowed".to_string())?,
                },
            );
        }
        Ok(output)
    }
    pub(super) fn load_positions(
        &self,
    ) -> Result<BTreeMap<(TradingAccountId, InstrumentId), Position>, String> {
        let mut output = BTreeMap::new();
        let mut statement = self.connection.prepare(
            "SELECT account_id, instrument_id, net_units, net_scale, average_units, average_scale,
                    realized_units, realized_scale, unrealized_units, unrealized_scale,
                    last_fill_unix_nanos FROM positions",
        ).map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, u8>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<u8>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, u8>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, u8>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                account,
                instrument,
                net_units,
                net_scale,
                average_units,
                average_scale,
                realized_units,
                realized_scale,
                unrealized_units,
                unrealized_scale,
                time,
            ) = row.map_err(database_error)?;
            let account_id =
                TradingAccountId::try_new(account).map_err(|error| error.to_string())?;
            let instrument_id =
                InstrumentId::try_new(instrument).map_err(|error| error.to_string())?;
            output.insert(
                (account_id.clone(), instrument_id.clone()),
                Position {
                    account_id,
                    instrument_id,
                    net_quantity: fixed(net_units, net_scale)?,
                    average_entry_price: optional_fixed(average_units, average_scale)?,
                    realized_pnl: fixed(realized_units, realized_scale)?,
                    unrealized_pnl: fixed(unrealized_units, unrealized_scale)?,
                    last_fill_unix_nanos: time,
                },
            );
        }
        Ok(output)
    }
}

impl TradingStore {
    /// Whether the V26 checkpoint and equity tables exist. A gated pre-group store stays on
    /// the legacy retention path until its conversion completes the chain.
    pub(super) fn checkpoint_tables_present(&self) -> Result<bool, String> {
        self.connection
            .query_row(
                "SELECT count(*) = 3 FROM sqlite_master
                 WHERE type = 'table' AND name IN
                     ('position_checkpoints', 'ledger_checkpoints', 'equity_samples')",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)
    }
}

/// Loads every position checkpoint; accepts a transaction so the retention fold reads a
/// consistent view inside its own commit.
pub(super) fn load_position_checkpoints(
    connection: &Connection,
) -> Result<BTreeMap<(TradingAccountId, InstrumentId), PositionCheckpoint>, String> {
    let mut output = BTreeMap::new();
    let mut statement = connection
        .prepare(
            "SELECT account_id, instrument_id, net_quantity_units, net_quantity_scale,
                    average_entry_units, average_entry_scale, realized_units, realized_scale,
                    cost_basis_hi, cost_basis_lo, cost_basis_scale,
                    last_fill_unix_nanos, fill_count FROM position_checkpoints",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, u8>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<u8>>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, u8>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<u8>>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, i64>(12)?,
            ))
        })
        .map_err(database_error)?;
    for row in rows {
        let (
            account,
            instrument,
            net_units,
            net_scale,
            average_units,
            average_scale,
            realized_units,
            realized_scale,
            basis_hi,
            basis_lo,
            basis_scale,
            last_fill,
            fill_count,
        ) = row.map_err(database_error)?;
        let account_id = TradingAccountId::try_new(account).map_err(|error| error.to_string())?;
        let instrument_id = InstrumentId::try_new(instrument).map_err(|error| error.to_string())?;
        let cost_basis = match (basis_hi, basis_lo, basis_scale) {
            (Some(hi), Some(lo), Some(scale)) => Some(CostBasis {
                units: join_i128(hi, lo),
                scale,
            }),
            (None, None, None) => None,
            _ => return Err("stored position checkpoint cost basis is partial".to_string()),
        };
        output.insert(
            (account_id.clone(), instrument_id.clone()),
            PositionCheckpoint {
                account_id,
                instrument_id,
                net_quantity: fixed(net_units, net_scale)?,
                average_entry_price: optional_fixed(average_units, average_scale)?,
                realized_pnl: fixed(realized_units, realized_scale)?,
                cost_basis,
                last_fill_unix_nanos: last_fill,
                fill_count: u64::try_from(fill_count)
                    .map_err(|_| "stored checkpoint fill count is negative".to_string())?,
            },
        );
    }
    Ok(output)
}

/// Loads every ledger checkpoint; accepts a transaction so the retention fold reads a
/// consistent view inside its own commit.
pub(super) fn load_ledger_checkpoints(
    connection: &Connection,
) -> Result<BTreeMap<TradingAccountId, LedgerCheckpoint>, String> {
    let mut output = BTreeMap::new();
    let mut statement = connection
        .prepare(
            "SELECT account_id, balance_units, balance_scale, entry_count, at_unix_nanos
                 FROM ledger_checkpoints",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, u8>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(database_error)?;
    for row in rows {
        let (account, units, scale, count, at) = row.map_err(database_error)?;
        let account_id = TradingAccountId::try_new(account).map_err(|error| error.to_string())?;
        output.insert(
            account_id.clone(),
            LedgerCheckpoint {
                account_id,
                balance: fixed(units, scale)?,
                entry_count: u64::try_from(count)
                    .map_err(|_| "stored ledger checkpoint count is negative".to_string())?,
                at_unix_nanos: at,
            },
        );
    }
    Ok(output)
}

/// Upserts one position checkpoint inside the retention transaction.
pub(super) fn upsert_position_checkpoint(
    transaction: &Transaction<'_>,
    checkpoint: &PositionCheckpoint,
) -> Result<(), String> {
    let (basis_hi, basis_lo, basis_scale) = match checkpoint.cost_basis {
        Some(basis) => {
            let (hi, lo) = split_i128(basis.units)?;
            (Some(hi), Some(lo), Some(basis.scale))
        }
        None => (None, None, None),
    };
    let (average_units, average_scale) = checkpoint
        .average_entry_price
        .map(|price| (price.units(), price.scale()))
        .unzip();
    transaction
        .execute(
            "INSERT INTO position_checkpoints (
                account_id, instrument_id, net_quantity_units, net_quantity_scale,
                average_entry_units, average_entry_scale, realized_units, realized_scale,
                cost_basis_hi, cost_basis_lo, cost_basis_scale,
                last_fill_unix_nanos, fill_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(account_id, instrument_id) DO UPDATE SET
                net_quantity_units=excluded.net_quantity_units,
                net_quantity_scale=excluded.net_quantity_scale,
                average_entry_units=excluded.average_entry_units,
                average_entry_scale=excluded.average_entry_scale,
                realized_units=excluded.realized_units,
                realized_scale=excluded.realized_scale,
                cost_basis_hi=excluded.cost_basis_hi,
                cost_basis_lo=excluded.cost_basis_lo,
                cost_basis_scale=excluded.cost_basis_scale,
                last_fill_unix_nanos=excluded.last_fill_unix_nanos,
                fill_count=excluded.fill_count",
            params![
                checkpoint.account_id.as_str(),
                checkpoint.instrument_id.as_str(),
                checkpoint.net_quantity.units(),
                checkpoint.net_quantity.scale(),
                average_units,
                average_scale,
                checkpoint.realized_pnl.units(),
                checkpoint.realized_pnl.scale(),
                basis_hi,
                basis_lo,
                basis_scale,
                checkpoint.last_fill_unix_nanos,
                sqlite_i64(checkpoint.fill_count)?,
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

/// Upserts one ledger checkpoint inside the retention transaction.
pub(super) fn upsert_ledger_checkpoint(
    transaction: &Transaction<'_>,
    checkpoint: &LedgerCheckpoint,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO ledger_checkpoints (
                account_id, balance_units, balance_scale, entry_count, at_unix_nanos)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(account_id) DO UPDATE SET
                balance_units=excluded.balance_units,
                balance_scale=excluded.balance_scale,
                entry_count=excluded.entry_count,
                at_unix_nanos=excluded.at_unix_nanos",
            params![
                checkpoint.account_id.as_str(),
                checkpoint.balance.units(),
                checkpoint.balance.scale(),
                sqlite_i64(checkpoint.entry_count)?,
                checkpoint.at_unix_nanos,
            ],
        )
        .map_err(database_error)?;
    Ok(())
}
/// Splits an exact `i128` into the signed hi/lo pair the positions table stores.
pub(super) fn split_i128(value: i128) -> Result<(i64, i64), String> {
    let hi = i64::try_from(value >> 64)
        .map_err(|_| "cost basis exceeds the stored range".to_string())?;
    let lo = u64::try_from(value & i128::from(u64::MAX))
        .map_err(|_| "cost basis exceeds the stored range".to_string())?
        .cast_signed();
    Ok((hi, lo))
}

fn join_i128(hi: i64, lo: i64) -> i128 {
    (i128::from(hi) << 64) | i128::from(lo.cast_unsigned())
}

/// Inserts cash-ledger entries inside an existing fill commit.
pub(super) fn insert_ledger_entries(
    transaction: &Transaction<'_>,
    entries: &[LedgerEntry],
) -> Result<(), String> {
    let mut statement = transaction
        .prepare(
            "INSERT INTO ledger_entries(id,account_id,kind,amount_units,amount_scale,fill_id,
            at_unix_nanos)
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        )
        .map_err(database_error)?;
    for entry in entries {
        entry.validate().map_err(|error| error.to_string())?;
        statement
            .execute(params![
                entry.entry_id.as_str(),
                entry.account_id.as_str(),
                entry.kind.as_str(),
                entry.amount.units(),
                sqlite_i64(u64::from(entry.amount.scale()))?,
                entry.fill_id.as_ref().map(FillId::as_str),
                entry.at_unix_nanos,
            ])
            .map_err(database_error)?;
    }
    Ok(())
}

/// Serializes one fee model into its deterministic stored JSON.
fn encode_fee_model(model: &FeeModel) -> String {
    match model {
        FeeModel::None => "{\"kind\":\"none\"}".to_string(),
        FeeModel::PerContract { per_side } => {
            format!(
                "{{\"kind\":\"per_contract\",\"per_side\":{}}}",
                encode_fixed(*per_side)
            )
        }
        FeeModel::PerUnit {
            per_unit,
            minimum,
            maximum_bps,
        } => format!(
            "{{\"kind\":\"per_unit\",\"per_unit\":{},\"minimum\":{},\"maximum_bps\":{}}}",
            encode_fixed(*per_unit),
            minimum.map_or_else(|| "null".to_string(), encode_fixed),
            maximum_bps.map_or_else(|| "null".to_string(), |value| value.to_string()),
        ),
        FeeModel::NotionalBps {
            maker_bps,
            taker_bps,
        } => format!(
            "{{\"kind\":\"notional_bps\",\"maker_bps\":{maker_bps},\"taker_bps\":{taker_bps}}}"
        ),
    }
}

/// Serializes one fixed-point value as `{"units":N,"scale":N}`.
fn encode_fixed(point: FixedPoint) -> String {
    format!(
        "{{\"units\":{},\"scale\":{}}}",
        point.units(),
        point.scale()
    )
}

/// Serializes one fee schedule with sorted class and instrument keys.
pub(super) fn encode_fee_schedule(schedule: &FeeSchedule) -> String {
    let by_class = schedule
        .by_class
        .iter()
        .map(|(class, model)| {
            format!(
                "\"{}\":{}",
                super::orders::instrument_class_str(*class),
                encode_fee_model(model)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let by_instrument = schedule
        .by_instrument
        .iter()
        .map(|(instrument, model)| {
            format!("\"{}\":{}", instrument.as_str(), encode_fee_model(model))
        })
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"by_class\":{{{by_class}}},\"by_instrument\":{{{by_instrument}}}}}")
}

/// Deserializes one stored fee schedule, rejecting unknown kinds, classes and shapes.
pub(super) fn decode_fee_schedule(raw: &str) -> Result<FeeSchedule, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|_| "stored fee schedule is invalid".to_string())?;
    let by_class = value
        .get("by_class")
        .and_then(Value::as_object)
        .ok_or_else(|| "stored fee schedule classes are missing".to_string())?;
    let by_instrument = value
        .get("by_instrument")
        .and_then(Value::as_object)
        .ok_or_else(|| "stored fee schedule instruments are missing".to_string())?;
    let mut schedule = FeeSchedule::default();
    for (class, model) in by_class {
        schedule.by_class.insert(
            super::orders::instrument_class_parse(Some(class.as_str()))?,
            decode_fee_model(model)?,
        );
    }
    for (instrument, model) in by_instrument {
        schedule.by_instrument.insert(
            InstrumentId::try_new(instrument).map_err(|error| error.to_string())?,
            decode_fee_model(model)?,
        );
    }
    schedule.validate().map_err(|error| error.to_string())?;
    Ok(schedule)
}

/// Deserializes one stored fee model.
fn decode_fee_model(value: &Value) -> Result<FeeModel, String> {
    match json_string(value, "kind")?.as_str() {
        "none" => Ok(FeeModel::None),
        "per_contract" => Ok(FeeModel::PerContract {
            per_side: decode_fixed(value.get("per_side"))?,
        }),
        "per_unit" => Ok(FeeModel::PerUnit {
            per_unit: decode_fixed(value.get("per_unit"))?,
            minimum: decode_optional_fixed(value.get("minimum"))?,
            maximum_bps: decode_optional_bps(value.get("maximum_bps"))?,
        }),
        "notional_bps" => Ok(FeeModel::NotionalBps {
            maker_bps: decode_bps(value, "maker_bps")?,
            taker_bps: decode_bps(value, "taker_bps")?,
        }),
        _ => Err("stored fee model kind is unknown".to_string()),
    }
}

/// Deserializes one `{"units":N,"scale":N}` fixed-point value.
fn decode_fixed(value: Option<&Value>) -> Result<FixedPoint, String> {
    let value = value.ok_or_else(|| "stored fee value is missing".to_string())?;
    let units = value
        .get("units")
        .and_then(Value::as_i64)
        .ok_or_else(|| "stored fee units are invalid".to_string())?;
    let scale = json_u64(value, "scale")?
        .try_into()
        .map_err(|_| "stored fee scale is invalid".to_string())?;
    fixed(units, scale)
}

/// Deserializes an optional fixed-point fee bound.
fn decode_optional_fixed(value: Option<&Value>) -> Result<Option<FixedPoint>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => decode_fixed(Some(value)).map(Some),
    }
}

/// Deserializes an optional basis-point bound.
fn decode_optional_bps(value: Option<&Value>) -> Result<Option<u32>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(|bps| u32::try_from(bps).map_err(|_| "stored fee bound is invalid".to_string()))
            .transpose(),
    }
}

/// Deserializes one required basis-point field.
fn decode_bps(value: &Value, key: &str) -> Result<u32, String> {
    u32::try_from(json_u64(value, key)?)
        .map_err(|_| "stored fee basis points are invalid".to_string())
}

impl TradingStore {
    /// All cash-ledger entries in commit order. `rowid` is insertion order, so a restart
    /// reproduces the replayed chain exactly even when an entry's timestamp is earlier than
    /// a later-committed entry's.
    pub(super) fn load_ledger_entries(&self) -> Result<Vec<LedgerEntry>, String> {
        self.connection
            .prepare(
                "SELECT id, account_id, kind, amount_units, amount_scale, fill_id, at_unix_nanos
                 FROM ledger_entries ORDER BY rowid",
            )
            .map_err(database_error)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, u8>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })
            .map_err(database_error)?
            .map(|row| {
                let (id, account, kind, units, scale, fill, at) = row.map_err(database_error)?;
                Ok(LedgerEntry {
                    entry_id: id,
                    account_id: TradingAccountId::try_new(account)
                        .map_err(|error| error.to_string())?,
                    kind: LedgerEntryKind::parse(&kind).map_err(|error| error.to_string())?,
                    amount: fixed(units, scale)?,
                    fill_id: fill
                        .map(FillId::try_new)
                        .transpose()
                        .map_err(|error| error.to_string())?,
                    at_unix_nanos: at,
                })
            })
            .collect()
    }

    /// Atomically persists one account's fee schedule with its revision.
    pub(crate) fn put_fee_schedule(
        &mut self,
        account_id: &TradingAccountId,
        schedule: &FeeSchedule,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO fee_schedules(account_id, model_json) VALUES (?1, ?2)
                 ON CONFLICT(account_id) DO UPDATE SET model_json = excluded.model_json",
                params![account_id.as_str(), encode_fee_schedule(schedule)],
            )
            .map_err(database_error)?;
        StepTx::commit_revision(transaction).map_err(database_error)
    }

    /// Every stored fee schedule, keyed by account.
    pub(super) fn load_fee_schedules(
        &self,
    ) -> Result<BTreeMap<TradingAccountId, FeeSchedule>, String> {
        self.connection
            .prepare("SELECT account_id, model_json FROM fee_schedules ORDER BY account_id")
            .map_err(database_error)?
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(database_error)?
            .map(|row| {
                let (account, raw) = row.map_err(database_error)?;
                Ok((
                    TradingAccountId::try_new(account).map_err(|error| error.to_string())?,
                    decode_fee_schedule(&raw)?,
                ))
            })
            .collect()
    }
}

/// Writes only the cost-basis columns; quote-mark upserts never touch them, so a mark-only
/// write cannot erase the basis.
pub(super) fn upsert_position_basis(
    transaction: &Transaction<'_>,
    position: &Position,
    basis: Option<CostBasis>,
) -> Result<(), String> {
    let (hi, lo, price_scale) = match basis {
        Some(basis) => {
            let (hi, lo) = split_i128(basis.units)?;
            let price_scale = basis
                .scale
                .checked_sub(position.net_quantity.scale())
                .ok_or_else(|| "cost basis scale is below the quantity scale".to_string())?;
            (Some(hi), Some(lo), Some(price_scale))
        }
        None => (None, None, None),
    };
    let quantity_scale = basis.map(|_| position.net_quantity.scale());
    transaction
        .execute(
            "UPDATE positions SET cost_basis_hi = ?3, cost_basis_lo = ?4,
                cost_price_scale = ?5, cost_quantity_scale = ?6
             WHERE account_id = ?1 AND instrument_id = ?2",
            params![
                position.account_id.as_str(),
                position.instrument_id.as_str(),
                hi,
                lo,
                price_scale,
                quantity_scale
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

pub(super) fn upsert_position(
    transaction: &Transaction<'_>,
    position: &Position,
) -> Result<(), String> {
    let (average_units, average_scale) = split_optional(position.average_entry_price);
    transaction
        .execute(
            "INSERT INTO positions(account_id, instrument_id, net_units, net_scale, average_units,
            average_scale, realized_units, realized_scale, unrealized_units, unrealized_scale,
            last_fill_unix_nanos) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(account_id, instrument_id) DO UPDATE SET net_units=excluded.net_units,
            net_scale=excluded.net_scale, average_units=excluded.average_units,
            average_scale=excluded.average_scale, realized_units=excluded.realized_units,
            realized_scale=excluded.realized_scale, unrealized_units=excluded.unrealized_units,
            unrealized_scale=excluded.unrealized_scale,
            last_fill_unix_nanos=excluded.last_fill_unix_nanos",
            params![
                position.account_id.as_str(),
                position.instrument_id.as_str(),
                position.net_quantity.units(),
                position.net_quantity.scale(),
                average_units,
                average_scale,
                position.realized_pnl.units(),
                position.realized_pnl.scale(),
                position.unrealized_pnl.units(),
                position.unrealized_pnl.scale(),
                position.last_fill_unix_nanos
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

pub(super) fn fixed(units: i64, scale: u8) -> Result<FixedPoint, String> {
    FixedPoint::try_new(units, scale).map_err(|error| error.to_string())
}

pub(super) fn optional_fixed(
    units: Option<i64>,
    scale: Option<u8>,
) -> Result<Option<FixedPoint>, String> {
    match (units, scale) {
        (None, None) => Ok(None),
        (Some(units), Some(scale)) => fixed(units, scale).map(Some),
        _ => Err("stored optional fixed-point value is incomplete".to_string()),
    }
}

pub(super) fn split_optional(value: Option<FixedPoint>) -> (Option<i64>, Option<u8>) {
    (value.map(FixedPoint::units), value.map(FixedPoint::scale))
}

pub(super) fn parse_environment(value: &str) -> Result<AccountEnvironment, String> {
    match value {
        "simulated" => Ok(AccountEnvironment::Simulated),
        "demo" => Ok(AccountEnvironment::Demo),
        "live" => Ok(AccountEnvironment::Live),
        _ => Err("stored account environment is invalid".to_string()),
    }
}

impl TradingStore {
    /// Writes one durable minute equity sample; a gated store without the V26 tables skips
    /// sampling until its conversion finishes.
    pub(crate) fn put_equity_sample(&self, sample: &EquitySample) -> Result<(), String> {
        if !self.checkpoint_tables_present()? {
            return Ok(());
        }
        self.connection
            .execute(
                "INSERT OR REPLACE INTO equity_samples(
                    account_id, at_unix_nanos, equity_units, equity_scale, resolution)
                 VALUES (?1, ?2, ?3, ?4, 'minute')",
                params![
                    sample.account_id.as_str(),
                    sample.at_unix_nanos,
                    sample.equity.units(),
                    sample.equity.scale(),
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }
}

/// Inserts fill-time equity samples inside the caller's transaction.
pub(super) fn insert_equity_samples(
    transaction: &Transaction<'_>,
    samples: &[EquitySample],
) -> Result<(), String> {
    let mut statement = transaction
        .prepare(
            "INSERT OR REPLACE INTO equity_samples(
                account_id, at_unix_nanos, equity_units, equity_scale, resolution)
             VALUES (?1, ?2, ?3, ?4, 'minute')",
        )
        .map_err(database_error)?;
    for sample in samples {
        statement
            .execute(params![
                sample.account_id.as_str(),
                sample.at_unix_nanos,
                sample.equity.units(),
                sample.equity.scale(),
            ])
            .map_err(database_error)?;
    }
    Ok(())
}
/// Loads the newest minute samples per account into the bounded in-memory rings.
pub(super) fn load_equity_rings(
    connection: &Connection,
) -> Result<BTreeMap<TradingAccountId, std::collections::VecDeque<EquitySample>>, String> {
    let mut statement = connection
        .prepare(
            "SELECT account_id, at_unix_nanos, equity_units, equity_scale FROM (
                SELECT account_id, at_unix_nanos, equity_units, equity_scale,
                    ROW_NUMBER() OVER (
                        PARTITION BY account_id ORDER BY at_unix_nanos DESC
                    ) AS recency
                 FROM equity_samples WHERE resolution = 'minute')
             WHERE recency <= ?1 ORDER BY account_id, at_unix_nanos",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map(
            [i64::try_from(crate::core::ledger::EQUITY_RING_CAPACITY)
                .map_err(|_| "equity ring capacity exceeds i64".to_string())?],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, u8>(3)?,
                ))
            },
        )
        .map_err(database_error)?;
    let mut rings: BTreeMap<TradingAccountId, std::collections::VecDeque<EquitySample>> =
        BTreeMap::new();
    for row in rows {
        let (account, at_unix_nanos, equity_units, equity_scale) = row.map_err(database_error)?;
        let account_id = TradingAccountId::try_new(account).map_err(|error| error.to_string())?;
        let equity = fixed(equity_units, equity_scale)?;
        rings
            .entry(account_id.clone())
            .or_default()
            .push_back(EquitySample {
                account_id,
                at_unix_nanos,
                equity,
            });
    }
    Ok(rings)
}
