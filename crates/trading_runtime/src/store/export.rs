use super::*;

impl TradingStore {
    pub(crate) fn export(&self, directory: &Path) -> Result<(), String> {
        fs::create_dir_all(directory)
            .map_err(|error| format!("trading export directory could not be created: {error}"))?;
        let executions = self.export_rows(
            "SELECT id, order_id, account_id, instrument_id, side, price_units, price_scale,
                    quantity_units, quantity_scale, execution_unix_nanos, provenance_json
             FROM fills ORDER BY execution_unix_nanos",
            |row| {
                Ok(json!({
                    "id": row.get::<_, String>(0)?,
                    "order_id": row.get::<_, String>(1)?,
                    "account_id": row.get::<_, String>(2)?,
                    "instrument_id": row.get::<_, String>(3)?,
                    "side": row.get::<_, String>(4)?,
                    "price": { "units": row.get::<_, i64>(5)?, "scale": row.get::<_, u8>(6)? },
                    "quantity": { "units": row.get::<_, i64>(7)?, "scale": row.get::<_, u8>(8)? },
                    "execution_unix_nanos": row.get::<_, i64>(9)?,
                    "provenance": serde_json::from_str::<Value>(&row.get::<_, String>(10)?)
                        .unwrap_or(Value::Null),
                }))
            },
        )?;
        let records = self.export_rows(
            "SELECT kind, id, revision, updated_unix_nanos, json FROM user_records
             ORDER BY kind, updated_unix_nanos",
            |row| {
                Ok(json!({
                    "kind": row.get::<_, String>(0)?,
                    "id": row.get::<_, String>(1)?,
                    "revision": row.get::<_, i64>(2)?,
                    "updated_unix_nanos": row.get::<_, i64>(3)?,
                    "value": serde_json::from_str::<Value>(&row.get::<_, String>(4)?)
                        .unwrap_or(Value::Null),
                }))
            },
        )?;
        write_json(directory.join("executions.json").as_path(), &executions)?;
        write_json(directory.join("user_records.json").as_path(), &records)?;
        let mut csv = String::from(
            "id,order_id,account_id,instrument_id,side,price_units,price_scale,quantity_units,quantity_scale,execution_unix_nanos\n",
        );
        for row in &executions {
            let price = &row["price"];
            let quantity = &row["quantity"];
            writeln!(
                csv,
                "{},{},{},{},{},{},{},{},{},{}",
                csv_field(row["id"].as_str().unwrap_or_default()),
                csv_field(row["order_id"].as_str().unwrap_or_default()),
                csv_field(row["account_id"].as_str().unwrap_or_default()),
                csv_field(row["instrument_id"].as_str().unwrap_or_default()),
                csv_field(row["side"].as_str().unwrap_or_default()),
                price["units"],
                price["scale"],
                quantity["units"],
                quantity["scale"],
                row["execution_unix_nanos"],
            )
            .map_err(|_| "trading CSV export could not be encoded".to_string())?;
        }
        write_atomic(directory.join("executions.csv").as_path(), csv.as_bytes())
    }

    pub(super) fn export_rows(
        &self,
        sql: &str,
        map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
    ) -> Result<Vec<Value>, String> {
        let mut statement = self.connection.prepare(sql).map_err(database_error)?;
        statement
            .query_map([], map)
            .map_err(database_error)?
            .map(|row| row.map_err(database_error))
            .collect()
    }
}

pub(super) fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

pub(super) fn write_json(path: &Path, values: &[Value]) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(values)
        .map_err(|_| "trading export could not be encoded".to_string())?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    aeris_platform_sys::fs::write_file_durably(path, bytes)
        .map_err(|error| format!("trading export could not be committed: {error}"))
}
