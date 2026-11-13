//! The trading owner's order template commands.

use super::{TradingCore, TradingService, respond};
use crate::order_templates::{MAXIMUM_ORDER_TEMPLATES, OrderTemplate, OrderTemplateError};
use crate::service::{Command, OrderTemplateCommand};

impl TradingService {
    /// Saves a new order template, or a new revision of one.
    ///
    /// # Errors
    /// Returns the refused part of the template, a revision that does not advance, the
    /// template bound, or an owner failure.
    pub fn save_order_template(
        &self,
        template: OrderTemplate,
    ) -> Result<OrderTemplate, OrderTemplateError> {
        self.request(|reply| Command::OrderTemplate(OrderTemplateCommand::Save(template, reply)))
            .map_err(OrderTemplateError::Owner)?
    }

    /// Removes an order template. Brackets already placed with it keep their legs.
    ///
    /// # Errors
    /// Returns an error for an unknown template or an owner failure.
    pub fn remove_order_template(&self, template_id: String) -> Result<(), OrderTemplateError> {
        self.request(|reply| {
            Command::OrderTemplate(OrderTemplateCommand::Remove(template_id, reply))
        })
        .map_err(OrderTemplateError::Owner)?
    }
}

impl TradingCore {
    pub(super) fn handle_order_template_command(&mut self, command: OrderTemplateCommand) {
        match command {
            OrderTemplateCommand::Save(template, reply) => {
                respond(&reply, Ok(self.save_order_template(template)));
            }
            OrderTemplateCommand::Remove(template_id, reply) => {
                respond(&reply, Ok(self.remove_order_template(&template_id)));
            }
        }
    }

    fn save_order_template(
        &mut self,
        template: OrderTemplate,
    ) -> Result<OrderTemplate, OrderTemplateError> {
        template.validate()?;
        match self.state.order_templates.get(&template.template_id) {
            Some(current) if template.revision <= current.revision => {
                return Err(OrderTemplateError::RevisionMustAdvance {
                    current: current.revision,
                });
            }
            None if self.state.order_templates.len() >= MAXIMUM_ORDER_TEMPLATES => {
                return Err(OrderTemplateError::TooManyTemplates);
            }
            _ => {}
        }
        self.store.put_order_template(&template)?;
        self.state
            .order_templates
            .insert(template.template_id.clone(), template.clone());
        self.bump_revision().map_err(OrderTemplateError::Owner)?;
        Ok(template)
    }

    fn remove_order_template(&mut self, template_id: &str) -> Result<(), OrderTemplateError> {
        if !self.state.order_templates.contains_key(template_id) {
            return Err(OrderTemplateError::UnknownTemplate);
        }
        self.store.delete_order_template(template_id)?;
        self.state.order_templates.remove(template_id);
        self.bump_revision().map_err(OrderTemplateError::Owner)
    }
}
