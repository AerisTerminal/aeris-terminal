use super::{
    BTreeMap, TradeCopierConfig, TradeCopierTarget, TradingAccountId, TradingStore, UserRecord,
    database_error, fixed, params, sqlite_i64,
};

impl TradingStore {
    pub(crate) fn put_trade_copier(&mut self, config: &TradeCopierConfig) -> Result<(), String> {
        config.validate()?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO trade_copiers(source_account_id, revision, enabled)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(source_account_id) DO UPDATE SET revision=excluded.revision,
                    enabled=excluded.enabled WHERE excluded.revision > trade_copiers.revision",
                params![
                    config.source_account_id.as_str(),
                    config.revision,
                    i64::from(config.enabled)
                ],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "DELETE FROM trade_copier_targets WHERE source_account_id = ?1",
                [config.source_account_id.as_str()],
            )
            .map_err(database_error)?;
        for target in &config.targets {
            transaction
                .execute(
                    "INSERT INTO trade_copier_targets(source_account_id, target_account_id,
                        multiplier_units, multiplier_scale, enabled) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        config.source_account_id.as_str(),
                        target.account_id.as_str(),
                        target.quantity_multiplier.units(),
                        target.quantity_multiplier.scale(),
                        i64::from(target.enabled),
                    ],
                )
                .map_err(database_error)?;
        }
        transaction.commit().map_err(database_error)
    }
}

impl TradingStore {
    pub(super) fn load_trade_copiers(
        &self,
    ) -> Result<BTreeMap<TradingAccountId, TradeCopierConfig>, String> {
        let mut result = BTreeMap::new();
        let mut configs = self
            .connection
            .prepare(
                "SELECT source_account_id, revision, enabled FROM trade_copiers
                 ORDER BY source_account_id",
            )
            .map_err(database_error)?;
        let rows = configs
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (source, revision, enabled) = row.map_err(database_error)?;
            let source_account_id =
                TradingAccountId::try_new(source).map_err(|error| error.to_string())?;
            let mut targets = self
                .connection
                .prepare(
                    "SELECT target_account_id, multiplier_units, multiplier_scale, enabled
                     FROM trade_copier_targets WHERE source_account_id = ?1
                     ORDER BY target_account_id",
                )
                .map_err(database_error)?;
            let targets = targets
                .query_map([source_account_id.as_str()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, u8>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .map_err(database_error)?
                .map(|row| {
                    let (account, units, scale, enabled) = row.map_err(database_error)?;
                    Ok(TradeCopierTarget {
                        account_id: TradingAccountId::try_new(account)
                            .map_err(|error| error.to_string())?,
                        quantity_multiplier: fixed(units, scale)?,
                        enabled: enabled != 0,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            let config = TradeCopierConfig {
                source_account_id: source_account_id.clone(),
                revision,
                enabled: enabled != 0,
                targets,
            };
            config.validate()?;
            result.insert(source_account_id, config);
        }
        Ok(result)
    }
}

impl TradingStore {
    pub(crate) fn put_user_record(&self, record: &UserRecord) -> Result<(), String> {
        let revision = sqlite_i64(record.revision)?;
        self.connection
            .execute(
                "INSERT INTO user_records(kind, id, revision, updated_unix_nanos, json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(kind, id) DO UPDATE SET revision=excluded.revision,
                    updated_unix_nanos=excluded.updated_unix_nanos, json=excluded.json
                 WHERE excluded.revision > user_records.revision",
                params![
                    record.kind.as_str(),
                    record.id,
                    revision,
                    record.updated_unix_nanos,
                    record.json,
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }
}
