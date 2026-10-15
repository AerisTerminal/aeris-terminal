use super::{
    BTreeMap, BracketStrategyTemplate, BracketTarget, BreakEvenRule, ClientOrderId, Connection,
    FixedPoint, ManagedBracket, ManagedBracketStatus, ProtectiveOrder, ProtectiveOrderRole, StepTx,
    TradingStore, TrailingStopRule, database_error, fixed, params,
};

impl TradingStore {
    pub(super) fn ensure_default_strategy_templates(&self) -> Result<(), String> {
        for template in [
            BracketStrategyTemplate {
                template_id: "managed-bracket".to_string(),
                revision: 1,
                name: "Managed stop + target".to_string(),
                stop_offset_ticks: 8,
                targets: vec![BracketTarget {
                    offset_ticks: 8,
                    quantity_percent: 100,
                }],
                trailing_stop: Some(TrailingStopRule {
                    activation_ticks: 12,
                    distance_ticks: 6,
                }),
                break_even: Some(BreakEvenRule {
                    activation_ticks: 8,
                    offset_ticks: 1,
                }),
                enabled: true,
            },
            BracketStrategyTemplate {
                template_id: "managed-scale-out".to_string(),
                revision: 1,
                name: "Managed 50/50 scale-out".to_string(),
                stop_offset_ticks: 8,
                targets: vec![
                    BracketTarget {
                        offset_ticks: 8,
                        quantity_percent: 50,
                    },
                    BracketTarget {
                        offset_ticks: 16,
                        quantity_percent: 50,
                    },
                ],
                trailing_stop: Some(TrailingStopRule {
                    activation_ticks: 12,
                    distance_ticks: 6,
                }),
                break_even: Some(BreakEvenRule {
                    activation_ticks: 8,
                    offset_ticks: 1,
                }),
                enabled: true,
            },
        ] {
            Self::write_strategy_template(&self.connection, &template)?;
        }
        Ok(())
    }
}

impl TradingStore {
    pub(crate) fn put_strategy_template(
        &self,
        template: &BracketStrategyTemplate,
    ) -> Result<(), String> {
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        Self::write_strategy_template(&transaction, template)?;
        transaction.commit().map_err(database_error)
    }

    fn write_strategy_template(
        connection: &Connection,
        template: &BracketStrategyTemplate,
    ) -> Result<(), String> {
        template.validate()?;
        let targets = serde_json::to_string(&template.targets)
            .map_err(|error| format!("strategy targets could not be encoded: {error}"))?;
        connection
            .execute(
                "INSERT INTO strategy_templates(template_id, revision, name, stop_offset_ticks,
                    targets_json, trailing_activation_ticks, trailing_distance_ticks,
                    break_even_activation_ticks, break_even_offset_ticks, enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(template_id) DO UPDATE SET revision=excluded.revision,
                    name=excluded.name, stop_offset_ticks=excluded.stop_offset_ticks,
                    targets_json=excluded.targets_json,
                    trailing_activation_ticks=excluded.trailing_activation_ticks,
                    trailing_distance_ticks=excluded.trailing_distance_ticks,
                    break_even_activation_ticks=excluded.break_even_activation_ticks,
                    break_even_offset_ticks=excluded.break_even_offset_ticks,
                    enabled=excluded.enabled
                 WHERE excluded.revision > strategy_templates.revision",
                params![
                    template.template_id,
                    template.revision,
                    template.name,
                    template.stop_offset_ticks,
                    targets,
                    template.trailing_stop.map(|rule| rule.activation_ticks),
                    template.trailing_stop.map(|rule| rule.distance_ticks),
                    template.break_even.map(|rule| rule.activation_ticks),
                    template.break_even.map(|rule| rule.offset_ticks),
                    i64::from(template.enabled),
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub(crate) fn put_managed_bracket(&self, bracket: &ManagedBracket) -> Result<(), String> {
        let transaction = StepTx::begin_shared(&self.connection).map_err(database_error)?;
        upsert_managed_bracket(&transaction, bracket)?;
        transaction.commit().map_err(database_error)
    }

    pub(crate) fn put_managed_bracket_projection(
        &self,
        bracket: &ManagedBracket,
    ) -> Result<(), String> {
        upsert_managed_bracket(&self.connection, bracket)
    }
}

impl TradingStore {
    pub(super) fn load_strategy_templates(
        &self,
    ) -> Result<BTreeMap<String, BracketStrategyTemplate>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT template_id, revision, name, stop_offset_ticks, targets_json,
                    trailing_activation_ticks, trailing_distance_ticks,
                    break_even_activation_ticks, break_even_offset_ticks, enabled
                 FROM strategy_templates ORDER BY template_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<u32>>(5)?,
                    row.get::<_, Option<u32>>(6)?,
                    row.get::<_, Option<u32>>(7)?,
                    row.get::<_, Option<i32>>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                template_id,
                revision,
                name,
                stop_offset_ticks,
                targets,
                trailing_activation_ticks,
                trailing_distance_ticks,
                break_even_activation_ticks,
                break_even_offset_ticks,
                enabled,
            ) = row.map_err(database_error)?;
            let targets = serde_json::from_str::<Vec<BracketTarget>>(&targets)
                .map_err(|error| format!("strategy targets could not be decoded: {error}"))?;
            let trailing_stop = match (trailing_activation_ticks, trailing_distance_ticks) {
                (Some(activation_ticks), Some(distance_ticks)) => Some(TrailingStopRule {
                    activation_ticks,
                    distance_ticks,
                }),
                (None, None) => None,
                _ => return Err("stored strategy trailing rule is incomplete".to_string()),
            };
            let break_even = match (break_even_activation_ticks, break_even_offset_ticks) {
                (Some(activation_ticks), Some(offset_ticks)) => Some(BreakEvenRule {
                    activation_ticks,
                    offset_ticks,
                }),
                (None, None) => None,
                _ => return Err("stored strategy break-even rule is incomplete".to_string()),
            };
            let template = BracketStrategyTemplate {
                template_id: template_id.clone(),
                revision,
                name,
                stop_offset_ticks,
                targets,
                trailing_stop,
                break_even,
                enabled: enabled != 0,
            };
            template.validate()?;
            result.insert(template_id, template);
        }
        Ok(result)
    }

    pub(super) fn load_managed_brackets(&self) -> Result<BTreeMap<String, ManagedBracket>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT bracket_id, template_json, entry_client_order_id,
                    stop_client_order_id, target_client_order_ids_json, status,
                    entry_price_units, entry_price_scale
                 FROM managed_brackets ORDER BY bracket_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<u8>>(7)?,
                ))
            })
            .map_err(database_error)?;
        for row in rows {
            let (
                bracket_id,
                template,
                entry_client_order_id,
                stop_client_order_id,
                target_client_order_ids,
                status,
                entry_price_units,
                entry_price_scale,
            ) = row.map_err(database_error)?;
            let target_client_order_ids =
                serde_json::from_str::<Vec<String>>(&target_client_order_ids)
                    .map_err(|error| {
                        format!("managed bracket targets could not be decoded: {error}")
                    })?
                    .into_iter()
                    .map(ClientOrderId::try_new)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| error.to_string())?;
            let template =
                serde_json::from_str::<BracketStrategyTemplate>(&template).map_err(|error| {
                    format!("managed bracket template could not be decoded: {error}")
                })?;
            let entry_price = entry_price_units
                .zip(entry_price_scale)
                .map(|(units, scale)| fixed(units, scale))
                .transpose()?;
            if entry_price_units.is_some() != entry_price_scale.is_some() {
                return Err("stored managed bracket entry price is incomplete".to_string());
            }
            let bracket = ManagedBracket {
                bracket_id: bracket_id.clone(),
                template,
                entry_client_order_id: ClientOrderId::try_new(entry_client_order_id)
                    .map_err(|error| error.to_string())?,
                stop_client_order_id: stop_client_order_id
                    .map(ClientOrderId::try_new)
                    .transpose()
                    .map_err(|error| error.to_string())?,
                target_client_order_ids,
                status: ManagedBracketStatus::parse(&status)?,
                entry_price,
            };
            bracket.validate()?;
            result.insert(bracket_id, bracket);
        }
        Ok(result)
    }

    pub(super) fn load_protective_orders(
        &self,
    ) -> Result<BTreeMap<ClientOrderId, ProtectiveOrder>, String> {
        let mut result = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare("SELECT client_order_id, role FROM protective_orders ORDER BY client_order_id")
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(database_error)?;
        for row in rows {
            let (client_order_id, role) = row.map_err(database_error)?;
            let client_order_id =
                ClientOrderId::try_new(client_order_id).map_err(|error| error.to_string())?;
            result.insert(
                client_order_id.clone(),
                ProtectiveOrder {
                    client_order_id,
                    role: ProtectiveOrderRole::parse(&role)?,
                },
            );
        }
        Ok(result)
    }
}

pub(super) fn upsert_managed_bracket(
    connection: &Connection,
    bracket: &ManagedBracket,
) -> Result<(), String> {
    bracket.validate()?;
    let target_client_order_ids = bracket
        .target_client_order_ids
        .iter()
        .map(ClientOrderId::as_str)
        .collect::<Vec<_>>();
    let target_client_order_ids = serde_json::to_string(&target_client_order_ids)
        .map_err(|error| format!("managed bracket targets could not be encoded: {error}"))?;
    let template = serde_json::to_string(&bracket.template)
        .map_err(|error| format!("managed bracket template could not be encoded: {error}"))?;
    connection
        .execute(
            "INSERT INTO managed_brackets(bracket_id, template_json,
                entry_client_order_id, stop_client_order_id, target_client_order_ids_json,
                status, entry_price_units, entry_price_scale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(bracket_id) DO UPDATE SET template_json=excluded.template_json,
                stop_client_order_id=excluded.stop_client_order_id,
                target_client_order_ids_json=excluded.target_client_order_ids_json,
                status=excluded.status, entry_price_units=excluded.entry_price_units,
                entry_price_scale=excluded.entry_price_scale",
            params![
                bracket.bracket_id,
                template,
                bracket.entry_client_order_id.as_str(),
                bracket
                    .stop_client_order_id
                    .as_ref()
                    .map(ClientOrderId::as_str),
                target_client_order_ids,
                bracket.status.as_str(),
                bracket.entry_price.map(FixedPoint::units),
                bracket.entry_price.map(FixedPoint::scale),
            ],
        )
        .map_err(database_error)?;
    Ok(())
}
