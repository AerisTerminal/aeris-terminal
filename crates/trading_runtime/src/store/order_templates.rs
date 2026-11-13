//! Durable user order templates, apart from the owner's internal bracket templates.

use super::{BTreeMap, StepTx, TradingStore, TrailingStopRule, database_error, params};
use crate::order_templates::{OrderTemplate, OrderTemplateError, OrderTemplateTarget};
use serde::{Deserialize, Serialize};

fn store_error(error: rusqlite::Error) -> OrderTemplateError {
    OrderTemplateError::Owner(database_error(error))
}

fn corrupt(detail: impl std::fmt::Display) -> OrderTemplateError {
    OrderTemplateError::Owner(format!("stored order template is invalid: {detail}"))
}

/// The stored shape of one target; the rest of a template is in columns.
#[derive(Deserialize, Serialize)]
struct StoredTarget {
    offset_ticks: u32,
    quantity: u32,
}

impl TradingStore {
    /// A store still waiting for its group conversion has not reached the template table.
    fn order_template_table_present(&self) -> Result<bool, OrderTemplateError> {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema
                 WHERE type = 'table' AND name = 'order_templates')",
                [],
                |row| row.get(0),
            )
            .map_err(store_error)
    }

    pub(crate) fn put_order_template(
        &self,
        template: &OrderTemplate,
    ) -> Result<(), OrderTemplateError> {
        template.validate()?;
        let targets = serde_json::to_string(
            &template
                .targets
                .iter()
                .map(|target| StoredTarget {
                    offset_ticks: target.offset_ticks,
                    quantity: target.quantity,
                })
                .collect::<Vec<_>>(),
        )
        .map_err(|error| OrderTemplateError::Owner(error.to_string()))?;
        let transaction = StepTx::begin_shared(&self.connection).map_err(store_error)?;
        transaction
            .execute(
                "INSERT INTO order_templates(template_id, revision, name, quantity,
                    stop_offset_ticks, targets_json, trailing_activation_ticks,
                    trailing_distance_ticks)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(template_id) DO UPDATE SET revision=excluded.revision,
                    name=excluded.name, quantity=excluded.quantity,
                    stop_offset_ticks=excluded.stop_offset_ticks,
                    targets_json=excluded.targets_json,
                    trailing_activation_ticks=excluded.trailing_activation_ticks,
                    trailing_distance_ticks=excluded.trailing_distance_ticks
                 WHERE excluded.revision > order_templates.revision",
                params![
                    template.template_id,
                    template.revision,
                    template.name,
                    template.quantity,
                    template.stop_offset_ticks,
                    targets,
                    template.trailing_stop.map(|rule| rule.activation_ticks),
                    template.trailing_stop.map(|rule| rule.distance_ticks),
                ],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)
    }

    pub(crate) fn delete_order_template(
        &self,
        template_id: &str,
    ) -> Result<(), OrderTemplateError> {
        let transaction = StepTx::begin_shared(&self.connection).map_err(store_error)?;
        transaction
            .execute(
                "DELETE FROM order_templates WHERE template_id = ?1",
                [template_id],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)
    }

    pub(super) fn load_order_templates(
        &self,
    ) -> Result<BTreeMap<String, OrderTemplate>, OrderTemplateError> {
        let mut templates = BTreeMap::new();
        if !self.order_template_table_present()? {
            return Ok(templates);
        }
        let mut statement = self
            .connection
            .prepare(
                "SELECT template_id, revision, name, quantity, stop_offset_ticks, targets_json,
                    trailing_activation_ticks, trailing_distance_ticks
                 FROM order_templates ORDER BY template_id",
            )
            .map_err(store_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u32>(3)?,
                    row.get::<_, u32>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<u32>>(6)?,
                    row.get::<_, Option<u32>>(7)?,
                ))
            })
            .map_err(store_error)?;
        for row in rows {
            let (
                template_id,
                revision,
                name,
                quantity,
                stop_offset_ticks,
                targets,
                trailing_activation_ticks,
                trailing_distance_ticks,
            ) = row.map_err(store_error)?;
            let targets = serde_json::from_str::<Vec<StoredTarget>>(&targets)
                .map_err(corrupt)?
                .into_iter()
                .map(|target| OrderTemplateTarget {
                    offset_ticks: target.offset_ticks,
                    quantity: target.quantity,
                })
                .collect();
            let trailing_stop = match (trailing_activation_ticks, trailing_distance_ticks) {
                (Some(activation_ticks), Some(distance_ticks)) => Some(TrailingStopRule {
                    activation_ticks,
                    distance_ticks,
                }),
                (None, None) => None,
                _ => return Err(corrupt("its trailing stop is incomplete")),
            };
            let template = OrderTemplate {
                template_id,
                revision,
                name,
                quantity,
                stop_offset_ticks,
                targets,
                trailing_stop,
            };
            template
                .validate()
                .map_err(|invalid| corrupt(format_args!("{invalid:?}")))?;
            templates.insert(template.template_id.clone(), template);
        }
        Ok(templates)
    }
}
