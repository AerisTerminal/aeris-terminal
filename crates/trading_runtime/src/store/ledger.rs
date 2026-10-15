use super::{
    AccountEnvironment, BTreeMap, BrokerPosition, FixedPoint, InstrumentId, Position,
    TradingAccount, TradingAccountId, TradingInstrument, TradingStore, Transaction, database_error,
    encode_contract, params, parse_side, sqlite_i64,
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
    pub(crate) fn put_account(&self, account: &TradingAccount) -> Result<(), String> {
        account.validate().map_err(|error| error.to_string())?;
        self.connection
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
}

impl TradingStore {
    pub(crate) fn put_instrument(&self, instrument: &TradingInstrument) -> Result<(), String> {
        let contract = encode_contract(&instrument.contract)?;
        self.connection
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
        Ok(())
    }
}

impl TradingStore {
    /// Writes one account's broker positions: upserts and removals, or, with
    /// `replace_all`, exactly the given positions.
    pub(crate) fn write_broker_positions(
        &mut self,
        account_id: &TradingAccountId,
        upserts: &[BrokerPosition],
        removals: &[String],
        replace_all: bool,
    ) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        if replace_all {
            transaction
                .execute(
                    "DELETE FROM broker_positions WHERE account_id = ?1",
                    [account_id.as_str()],
                )
                .map_err(database_error)?;
        }
        for broker_position_id in removals {
            transaction
                .execute(
                    "DELETE FROM broker_positions WHERE account_id = ?1
                        AND broker_position_id = ?2",
                    params![account_id.as_str(), broker_position_id],
                )
                .map_err(database_error)?;
        }
        for position in upserts {
            position.validate().map_err(|error| error.to_string())?;
            transaction
                .execute(
                    "INSERT INTO broker_positions(account_id, broker_position_id, instrument_id,
                        side, quantity_units, quantity_scale, entry_units, entry_scale,
                        stop_units, stop_scale, take_units, take_scale, swap_units, swap_scale,
                        commission_units, commission_scale, gross_unrealized_units,
                        gross_unrealized_scale, net_unrealized_units, net_unrealized_scale,
                        opened_unix_nanos)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                        ?16, ?17, ?18, ?19, ?20, ?21)
                     ON CONFLICT(account_id, broker_position_id) DO UPDATE SET
                        instrument_id = excluded.instrument_id, side = excluded.side,
                        quantity_units = excluded.quantity_units,
                        quantity_scale = excluded.quantity_scale,
                        entry_units = excluded.entry_units, entry_scale = excluded.entry_scale,
                        stop_units = excluded.stop_units, stop_scale = excluded.stop_scale,
                        take_units = excluded.take_units, take_scale = excluded.take_scale,
                        swap_units = excluded.swap_units, swap_scale = excluded.swap_scale,
                        commission_units = excluded.commission_units,
                        commission_scale = excluded.commission_scale,
                        gross_unrealized_units = excluded.gross_unrealized_units,
                        gross_unrealized_scale = excluded.gross_unrealized_scale,
                        net_unrealized_units = excluded.net_unrealized_units,
                        net_unrealized_scale = excluded.net_unrealized_scale,
                        opened_unix_nanos = excluded.opened_unix_nanos",
                    params![
                        position.account_id.as_str(),
                        position.broker_position_id,
                        position.instrument_id.as_str(),
                        position.side.as_str(),
                        position.quantity.units(),
                        position.quantity.scale(),
                        position.entry_price.units(),
                        position.entry_price.scale(),
                        position.stop_loss.map(FixedPoint::units),
                        position.stop_loss.map(FixedPoint::scale),
                        position.take_profit.map(FixedPoint::units),
                        position.take_profit.map(FixedPoint::scale),
                        position.swap.units(),
                        position.swap.scale(),
                        position.commission.units(),
                        position.commission.scale(),
                        position.gross_unrealized.units(),
                        position.gross_unrealized.scale(),
                        position.net_unrealized.units(),
                        position.net_unrealized.scale(),
                        position.opened_unix_nanos,
                    ],
                )
                .map_err(database_error)?;
        }
        transaction.commit().map_err(database_error)
    }

    pub(crate) fn set_broker_balance(
        &self,
        account_id: &TradingAccountId,
        balance: FixedPoint,
        session_generation: u64,
    ) -> Result<(), String> {
        self.connection
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
        Ok(())
    }
}

impl TradingStore {
    pub(crate) fn update_position(&mut self, position: &Position) -> Result<(), String> {
        let transaction = self.connection.transaction().map_err(database_error)?;
        upsert_position(&transaction, position)?;
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
    pub(super) fn load_broker_positions(
        &self,
    ) -> Result<BTreeMap<(TradingAccountId, String), BrokerPosition>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT account_id, broker_position_id, instrument_id, side,
                quantity_units, quantity_scale, entry_units, entry_scale,
                stop_units, stop_scale, take_units, take_scale,
                swap_units, swap_scale, commission_units, commission_scale,
                gross_unrealized_units, gross_unrealized_scale,
                net_unrealized_units, net_unrealized_scale, opened_unix_nanos
             FROM broker_positions ORDER BY account_id, broker_position_id",
            )
            .map_err(database_error)?;
        let mut rows = statement.query([]).map_err(database_error)?;
        let mut positions = BTreeMap::new();
        while let Some(row) = rows.next().map_err(database_error)? {
            let string = |column| row.get::<_, String>(column).map_err(database_error);
            let integer = |column| row.get::<_, i64>(column).map_err(database_error);
            let scale = |column| row.get::<_, u8>(column).map_err(database_error);
            let optional_integer =
                |column| row.get::<_, Option<i64>>(column).map_err(database_error);
            let optional_scale = |column| row.get::<_, Option<u8>>(column).map_err(database_error);
            let account_id =
                TradingAccountId::try_new(string(0)?).map_err(|error| error.to_string())?;
            let broker_position_id = string(1)?;
            let position = BrokerPosition {
                account_id: account_id.clone(),
                broker_position_id: broker_position_id.clone(),
                instrument_id: InstrumentId::try_new(string(2)?)
                    .map_err(|error| error.to_string())?,
                side: parse_side(&string(3)?)?,
                quantity: fixed(integer(4)?, scale(5)?)?,
                entry_price: fixed(integer(6)?, scale(7)?)?,
                stop_loss: optional_fixed(optional_integer(8)?, optional_scale(9)?)?,
                take_profit: optional_fixed(optional_integer(10)?, optional_scale(11)?)?,
                swap: fixed(integer(12)?, scale(13)?)?,
                commission: fixed(integer(14)?, scale(15)?)?,
                gross_unrealized: fixed(integer(16)?, scale(17)?)?,
                net_unrealized: fixed(integer(18)?, scale(19)?)?,
                opened_unix_nanos: integer(20)?,
            };
            position.validate().map_err(|error| error.to_string())?;
            positions.insert((account_id, broker_position_id), position);
        }
        Ok(positions)
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
