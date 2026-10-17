use super::{
    BTreeMap, BracketStrategyTemplate, BracketTarget, BreakEvenRule, ClientOrderId, Connection,
    FixedPoint, InstrumentId, ManagedBracket, ManagedBracketStatus, ProtectiveOrder,
    ProtectiveOrderRole, StepTx, TradingAccountId, TradingStore, TrailingStopRule, database_error,
    fixed, params,
};
use crate::core::{GroupKind, GroupLeg, GroupStatus, LegRole, OrderGroup, StopManagement};

/// Write a newly owned group after its parent and children, in their shared order step, or
/// replace its row when the group owner advances its status or a leg's offset. Historical
/// groups use the idempotent migration transaction instead.
pub(super) fn upsert_order_group(
    connection: &Connection,
    group: &OrderGroup,
) -> Result<(), String> {
    group
        .validate()
        .map_err(|error| format!("invalid order group: {error:?}"))?;
    for order_id in group
        .entry
        .iter()
        .chain(group.legs.iter().map(|leg| &leg.order_id))
    {
        let matches_scope: i64 = connection
            .query_row(
                "SELECT count(*) FROM orders
                 WHERE id=?1 AND account_id=?2 AND instrument_id=?3",
                params![
                    order_id.as_str(),
                    group.account_id.as_str(),
                    group.instrument_id.as_str()
                ],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        if matches_scope != 1 {
            return Err("order group member is missing or belongs to another scope".into());
        }
    }
    let (kind, status) = (
        match group.kind {
            GroupKind::Bracket => "bracket",
            GroupKind::Oco => "oco",
            GroupKind::Protection => "protection",
        },
        match group.status {
            GroupStatus::Pending => "pending",
            GroupStatus::Active => "active",
            GroupStatus::Completed => "completed",
            GroupStatus::Cancelled => "cancelled",
        },
    );
    let management = serde_json::json!({
        "trailing": group.management.trailing,
        "break_even": group.management.break_even,
        "entry_price": group.management.entry_price.map(|price| serde_json::json!({
            "units": price.units(),
            "scale": price.scale(),
        })),
    });
    connection
        .execute(
            "INSERT INTO order_groups(group_id,account_id,instrument_id,kind,status,
                entry_order_id,management_json,template_id,template_revision,revision)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(group_id) DO UPDATE SET status=excluded.status,
                entry_order_id=excluded.entry_order_id,
                management_json=excluded.management_json,
                template_id=excluded.template_id,
                template_revision=excluded.template_revision,
                revision=excluded.revision",
            params![
                group.group_id,
                group.account_id.as_str(),
                group.instrument_id.as_str(),
                kind,
                status,
                group.entry.as_ref().map(aeris_trading::OrderId::as_str),
                management.to_string(),
                group.template.as_ref().map(|(id, _)| id.as_str()),
                group.template.as_ref().map(|(_, revision)| revision),
                group.revision,
            ],
        )
        .map_err(database_error)?;
    connection
        .execute(
            "DELETE FROM order_group_legs WHERE group_id=?1",
            [&group.group_id],
        )
        .map_err(database_error)?;
    for leg in &group.legs {
        let (role, index) = match leg.role {
            LegRole::StopLoss => ("stop_loss", None),
            LegRole::TakeProfit { index } => ("take_profit", Some(index)),
        };
        connection
            .execute(
                "INSERT INTO order_group_legs(group_id,order_id,role,target_index,
                    offset_ticks,allocation_percent) VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    group.group_id,
                    leg.order_id.as_str(),
                    role,
                    index,
                    leg.offset_ticks,
                    leg.allocation_percent,
                ],
            )
            .map_err(database_error)?;
    }
    Ok(())
}

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

    /// Persist a group transition outside an order step (startup alignment). Group-owned
    /// lifecycle writes ride the quote and order steps instead.
    pub(crate) fn put_order_group_projection(&self, group: &OrderGroup) -> Result<(), String> {
        upsert_order_group(&self.connection, group)
    }

    pub(super) fn load_order_groups(&self) -> Result<BTreeMap<String, OrderGroup>, String> {
        let mut groups = BTreeMap::new();
        let mut statement = self
            .connection
            .prepare(
                "SELECT group_id, account_id, instrument_id, kind, status, entry_order_id,
                    management_json, template_id, template_revision, revision
                 FROM order_groups ORDER BY group_id",
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
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<u32>>(8)?,
                    row.get::<_, u32>(9)?,
                ))
            })
            .map_err(database_error)?;
        let mut leg_statement = self
            .connection
            .prepare(
                "SELECT order_id, role, target_index, offset_ticks, allocation_percent
                 FROM order_group_legs WHERE group_id=?1
                 ORDER BY role, target_index, order_id",
            )
            .map_err(database_error)?;
        for row in rows {
            let (
                group_id,
                account_id,
                instrument_id,
                kind,
                status,
                entry_order_id,
                management,
                template_id,
                template_revision,
                revision,
            ) = row.map_err(database_error)?;
            let management: StopManagement = serde_json::from_str::<ManagementJson>(&management)
                .map_err(|error| {
                    format!("stored order group management could not be decoded: {error}")
                })?
                .decode()?;
            let kind = match kind.as_str() {
                "bracket" => GroupKind::Bracket,
                "oco" => GroupKind::Oco,
                "protection" => GroupKind::Protection,
                _ => return Err("stored order group kind is invalid".to_string()),
            };
            let status = match status.as_str() {
                "pending" => GroupStatus::Pending,
                "active" => GroupStatus::Active,
                "completed" => GroupStatus::Completed,
                "cancelled" => GroupStatus::Cancelled,
                _ => return Err("stored order group status is invalid".to_string()),
            };
            let legs = leg_statement
                .query_map([&group_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<u8>>(2)?,
                        row.get::<_, Option<u32>>(3)?,
                        row.get::<_, Option<u8>>(4)?,
                    ))
                })
                .map_err(database_error)?
                .map(|row| decode_group_leg(row.map_err(database_error)?))
                .collect::<Result<Vec<_>, String>>()?;
            let group = build_order_group(
                StoredGroupRow {
                    group_id,
                    account_id,
                    instrument_id,
                    kind,
                    status,
                    entry_order_id,
                    management,
                    template_id,
                    template_revision,
                    revision,
                },
                legs,
            )?;
            groups.insert(group.group_id.clone(), group);
        }
        Ok(groups)
    }
}

/// Decodes one stored group leg row, rejecting unknown role shapes.
fn decode_group_leg(
    row: (String, String, Option<u8>, Option<u32>, Option<u8>),
) -> Result<GroupLeg, String> {
    let (order_id, role, target_index, offset_ticks, allocation_percent) = row;
    let role = match (role.as_str(), target_index) {
        ("stop_loss", None) => LegRole::StopLoss,
        ("take_profit", Some(index)) => LegRole::TakeProfit { index },
        _ => return Err("stored order group leg role is invalid".to_string()),
    };
    Ok(GroupLeg {
        order_id: aeris_trading::OrderId::try_new(order_id).map_err(|error| error.to_string())?,
        role,
        offset_ticks,
        allocation_percent,
    })
}

/// One decoded group row, before its legs attach.
struct StoredGroupRow {
    group_id: String,
    account_id: String,
    instrument_id: String,
    kind: GroupKind,
    status: GroupStatus,
    entry_order_id: Option<String>,
    management: StopManagement,
    template_id: Option<String>,
    template_revision: Option<u32>,
    revision: u32,
}

/// Rebuilds one stored group from its decoded row and legs, validating it before it enters
/// owner memory.
fn build_order_group(row: StoredGroupRow, legs: Vec<GroupLeg>) -> Result<OrderGroup, String> {
    let template = match (row.template_id, row.template_revision) {
        (Some(id), Some(revision)) => Some((id, revision)),
        (None, None) => None,
        _ => return Err("stored order group template is incomplete".to_string()),
    };
    let group = OrderGroup {
        group_id: row.group_id,
        account_id: TradingAccountId::try_new(row.account_id).map_err(|error| error.to_string())?,
        instrument_id: InstrumentId::try_new(row.instrument_id)
            .map_err(|error| error.to_string())?,
        kind: row.kind,
        status: row.status,
        entry: row
            .entry_order_id
            .map(aeris_trading::OrderId::try_new)
            .transpose()
            .map_err(|error| error.to_string())?,
        legs,
        management: row.management,
        template,
        revision: row.revision,
    };
    group
        .validate()
        .map_err(|error| format!("stored order group is invalid: {error:?}"))?;
    Ok(group)
}

/// Storage form of a group's stop-management rules.
#[derive(serde::Deserialize)]
struct ManagementJson {
    trailing: Option<TrailingStopRule>,
    break_even: Option<BreakEvenRule>,
    #[serde(default)]
    entry_price: Option<EntryPriceJson>,
}

#[derive(serde::Deserialize)]
struct EntryPriceJson {
    units: i64,
    scale: u8,
}

impl ManagementJson {
    fn decode(self) -> Result<StopManagement, String> {
        Ok(StopManagement {
            trailing: self.trailing,
            break_even: self.break_even,
            entry_price: self
                .entry_price
                .map(|price| fixed(price.units, price.scale))
                .transpose()?,
        })
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

#[cfg(test)]
mod group_step_tests {
    use super::*;
    use crate::core::{
        GroupKind, GroupLeg, GroupStatus, LegRole, OrderGroup, StopManagement, TradingRetention,
    };
    use crate::store::OrderStep;
    use aeris_instruments::InstrumentId;
    use aeris_trading::{OrderId, TradingAccountId};

    #[test]
    fn group_step_rolls_back_on_last_leg_and_retries_without_orphan_members() {
        let connection = Connection::open_in_memory().unwrap();
        super::super::create_schema_at(&connection, 23);
        connection
            .execute_batch(
                "PRAGMA foreign_keys=ON;
             INSERT INTO accounts(id,display_name,environment,currency,currency_scale,venue_id)
             VALUES ('sim','Sanitized','simulated','USD',2,'aeris-sim');
             INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
             VALUES ('BTC-USDC',2,0,'{}');
             INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                 time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
                 status,submitted_unix_nanos,provenance_json)
             VALUES ('entry','entry-client','sim','BTC-USDC','buy','limit','gtc',
                     1,0,0,0,'working',1,'{}'),
                    ('stop','stop-client','sim','BTC-USDC','sell','stop','gtc',
                     1,0,0,0,'inactive',1,'{}'),
                    ('target','target-client','sim','BTC-USDC','sell','limit','gtc',
                     1,0,0,0,'inactive',1,'{}');
             CREATE TRIGGER fail_last_leg BEFORE INSERT ON order_group_legs
             WHEN NEW.order_id='target' BEGIN SELECT RAISE(ABORT,'last leg failure'); END;",
            )
            .unwrap();
        let mut store = TradingStore {
            connection,
            retention: TradingRetention::default(),
        };
        let mut step = OrderStep::new(1);
        step.groups.push(OrderGroup {
            group_id: "entry-client".into(),
            account_id: TradingAccountId::try_new("sim").unwrap(),
            instrument_id: InstrumentId::try_new("BTC-USDC").unwrap(),
            kind: GroupKind::Bracket,
            status: GroupStatus::Pending,
            entry: Some(OrderId::try_new("entry").unwrap()),
            legs: vec![
                GroupLeg {
                    order_id: OrderId::try_new("stop").unwrap(),
                    role: LegRole::StopLoss,
                    offset_ticks: Some(8),
                    allocation_percent: None,
                },
                GroupLeg {
                    order_id: OrderId::try_new("target").unwrap(),
                    role: LegRole::TakeProfit { index: 0 },
                    offset_ticks: Some(8),
                    allocation_percent: Some(100),
                },
            ],
            management: StopManagement::default(),
            template: Some(("managed-bracket".into(), 1)),
            revision: 1,
        });
        let before = store.revision().unwrap();
        assert!(store.commit_order_step(&step).is_err());
        for table in ["order_groups", "order_group_legs"] {
            assert_eq!(
                store
                    .connection
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0,
                "{table} must roll back"
            );
        }
        assert_eq!(store.revision().unwrap(), before);
        store
            .connection
            .execute_batch("DROP TRIGGER fail_last_leg")
            .unwrap();
        store.commit_order_step(&step).unwrap();
        assert_eq!(
            store
                .connection
                .query_row(
                    "SELECT count(*) FROM order_group_legs WHERE group_id='entry-client'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            2
        );
    }

    #[test]
    fn group_step_rejects_a_member_from_another_account() {
        let connection = Connection::open_in_memory().unwrap();
        super::super::create_schema_at(&connection, 23);
        connection
            .execute_batch(
                "PRAGMA foreign_keys=ON;
             INSERT INTO accounts(id,display_name,environment,currency,currency_scale,venue_id)
             VALUES ('sim','Sanitized','simulated','USD',2,'aeris-sim'),
                    ('other','Other','simulated','USD',2,'aeris-sim');
             INSERT INTO instruments(id,price_scale,quantity_scale,contract_json)
             VALUES ('BTC-USDC',2,0,'{}');
             INSERT INTO orders(id,client_order_id,account_id,instrument_id,side,order_type,
                 time_in_force,quantity_units,quantity_scale,filled_units,filled_scale,
                 status,submitted_unix_nanos,provenance_json)
             VALUES ('entry','entry-client','sim','BTC-USDC','buy','limit','gtc',
                     1,0,0,0,'working',1,'{}'),
                    ('stop','stop-client','sim','BTC-USDC','sell','stop','gtc',
                     1,0,0,0,'inactive',1,'{}'),
                    ('target','target-client','other','BTC-USDC','sell','limit','gtc',
                     1,0,0,0,'inactive',1,'{}');",
            )
            .unwrap();
        let mut store = TradingStore {
            connection,
            retention: TradingRetention::default(),
        };
        let mut step = OrderStep::new(1);
        step.groups.push(OrderGroup {
            group_id: "cross-account".into(),
            account_id: TradingAccountId::try_new("sim").unwrap(),
            instrument_id: InstrumentId::try_new("BTC-USDC").unwrap(),
            kind: GroupKind::Bracket,
            status: GroupStatus::Pending,
            entry: Some(OrderId::try_new("entry").unwrap()),
            legs: vec![
                GroupLeg {
                    order_id: OrderId::try_new("stop").unwrap(),
                    role: LegRole::StopLoss,
                    offset_ticks: Some(8),
                    allocation_percent: None,
                },
                GroupLeg {
                    order_id: OrderId::try_new("target").unwrap(),
                    role: LegRole::TakeProfit { index: 0 },
                    offset_ticks: Some(8),
                    allocation_percent: Some(100),
                },
            ],
            management: StopManagement::default(),
            template: Some(("managed-bracket".into(), 1)),
            revision: 1,
        });
        assert!(store.commit_order_step(&step).is_err());
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM order_groups", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
