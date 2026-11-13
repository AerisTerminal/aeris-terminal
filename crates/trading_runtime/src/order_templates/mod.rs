//! User-managed order templates (ATM strategies): a named default quantity, one stop and up
//! to [`MAXIMUM_BRACKET_TARGETS`] targets in ticks, and an optional trailing stop. The trading
//! owner keeps at most [`MAXIMUM_ORDER_TEMPLATES`] of them apart from its internal bracket
//! templates, publishes them in the working set and places one through the same bracket path
//! as every other bracket. Removing a template never touches the order groups placed with it:
//! a group keeps its own legs and allocations.

use crate::strategy::{
    BracketStrategyTemplate, BracketTarget, MAXIMUM_BRACKET_TARGETS, TrailingStopRule,
};
use std::fmt;
use std::num::NonZeroU64;

/// At most this many order templates exist; saving another is refused, never evicting one.
pub const MAXIMUM_ORDER_TEMPLATES: usize = 64;
pub const MAXIMUM_ORDER_TEMPLATE_NAME_CHARS: usize = 64;
/// The largest default quantity, in whole order increments (contracts for a future).
pub const MAXIMUM_ORDER_TEMPLATE_QUANTITY: u32 = 1_000;
/// The largest stop, target or trailing distance, in ticks.
pub const MAXIMUM_ORDER_TEMPLATE_TICKS: u32 = 10_000;
/// Every order template identity starts with this prefix, which internal bracket templates
/// never use.
pub const ORDER_TEMPLATE_ID_PREFIX: &str = "user-";
const MAXIMUM_ORDER_TEMPLATE_NAME_BYTES: usize = 128;

/// The identity of the order template numbered `key`.
#[must_use]
pub fn order_template_id(key: NonZeroU64) -> String {
    format!("{ORDER_TEMPLATE_ID_PREFIX}{key:x}")
}

/// The number of an order template identity in its canonical form, or `None` for any other
/// text.
#[must_use]
pub fn order_template_key(template_id: &str) -> Option<NonZeroU64> {
    let digits = template_id.strip_prefix(ORDER_TEMPLATE_ID_PREFIX)?;
    let canonical = !digits.is_empty()
        && digits.len() <= 16
        && !digits.starts_with('0')
        && digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    canonical
        .then(|| u64::from_str_radix(digits, 16).ok())
        .flatten()
        .and_then(NonZeroU64::new)
}

/// One profit target: its distance from the entry and how many of the template's order
/// increments it takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrderTemplateTarget {
    pub offset_ticks: u32,
    pub quantity: u32,
}

/// One user-managed order template.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderTemplate {
    pub template_id: String,
    pub revision: u32,
    pub name: String,
    /// The default order quantity in whole order increments; the targets split exactly this
    /// many.
    pub quantity: u32,
    pub stop_offset_ticks: u32,
    /// Nearest first.
    pub targets: Vec<OrderTemplateTarget>,
    pub trailing_stop: Option<TrailingStopRule>,
}

/// The part of an order template validation refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrderTemplateInvalid {
    Identity,
    Revision,
    Name,
    Quantity,
    StopOffset,
    /// No target, or more than [`MAXIMUM_BRACKET_TARGETS`].
    TargetCount,
    /// A target distance of zero ticks or beyond [`MAXIMUM_ORDER_TEMPLATE_TICKS`].
    TargetOffset,
    /// Targets that are not each further than the one before.
    TargetOrder,
    /// A target that takes no quantity.
    TargetQuantity,
    /// Target quantities that do not add up to the template's quantity.
    TargetTotal,
    /// A split the owner cannot reproduce exactly in whole percent allocations.
    TargetSplit,
    TrailingStop,
}

/// A refused order template command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderTemplateError {
    Invalid(OrderTemplateInvalid),
    TooManyTemplates,
    UnknownTemplate,
    RevisionMustAdvance {
        current: u32,
    },
    /// The trading owner, its queue or its store failed.
    Owner(String),
}

impl fmt::Display for OrderTemplateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(invalid) => write!(formatter, "order template is invalid: {invalid:?}"),
            Self::TooManyTemplates => write!(
                formatter,
                "at most {MAXIMUM_ORDER_TEMPLATES} order templates are allowed"
            ),
            Self::UnknownTemplate => formatter.write_str("order template is not saved"),
            Self::RevisionMustAdvance { current } => {
                write!(
                    formatter,
                    "order template revision must advance past {current}"
                )
            }
            Self::Owner(detail) => formatter.write_str(detail),
        }
    }
}

impl std::error::Error for OrderTemplateError {}

impl From<OrderTemplateInvalid> for OrderTemplateError {
    fn from(invalid: OrderTemplateInvalid) -> Self {
        Self::Invalid(invalid)
    }
}

impl OrderTemplate {
    /// Checks every field the owner would refuse, in the order the editor shows them.
    ///
    /// # Errors
    /// Returns the first refused part.
    pub fn validate(&self) -> Result<(), OrderTemplateInvalid> {
        if order_template_key(&self.template_id).is_none() {
            return Err(OrderTemplateInvalid::Identity);
        }
        if self.revision == 0 {
            return Err(OrderTemplateInvalid::Revision);
        }
        let name = self.name.trim();
        if name.is_empty()
            || name.len() != self.name.len()
            || self.name.chars().count() > MAXIMUM_ORDER_TEMPLATE_NAME_CHARS
            || self.name.len() > MAXIMUM_ORDER_TEMPLATE_NAME_BYTES
            || self.name.chars().any(char::is_control)
        {
            return Err(OrderTemplateInvalid::Name);
        }
        if !(1..=MAXIMUM_ORDER_TEMPLATE_QUANTITY).contains(&self.quantity) {
            return Err(OrderTemplateInvalid::Quantity);
        }
        if !valid_ticks(self.stop_offset_ticks) {
            return Err(OrderTemplateInvalid::StopOffset);
        }
        if self.targets.is_empty() || self.targets.len() > MAXIMUM_BRACKET_TARGETS {
            return Err(OrderTemplateInvalid::TargetCount);
        }
        let mut previous_offset = 0;
        let mut total = 0_u32;
        for target in &self.targets {
            if !valid_ticks(target.offset_ticks) {
                return Err(OrderTemplateInvalid::TargetOffset);
            }
            if target.offset_ticks <= previous_offset {
                return Err(OrderTemplateInvalid::TargetOrder);
            }
            if target.quantity == 0 {
                return Err(OrderTemplateInvalid::TargetQuantity);
            }
            previous_offset = target.offset_ticks;
            total = total.saturating_add(target.quantity);
        }
        if total != self.quantity {
            return Err(OrderTemplateInvalid::TargetTotal);
        }
        if let Some(trailing) = self.trailing_stop
            && !(valid_ticks(trailing.activation_ticks) && valid_ticks(trailing.distance_ticks))
        {
            return Err(OrderTemplateInvalid::TrailingStop);
        }
        self.target_percents().map(|_| ())
    }

    /// The bracket the owner places for this template. Each target's share is a whole
    /// percent of the order quantity, chosen so an order of the template's own quantity
    /// splits into exactly the template's target quantities.
    ///
    /// # Errors
    /// Returns the first refused part of the template.
    pub fn bracket_template(&self) -> Result<BracketStrategyTemplate, OrderTemplateInvalid> {
        self.validate()?;
        let targets = self
            .targets
            .iter()
            .zip(self.target_percents()?)
            .map(|(target, quantity_percent)| BracketTarget {
                offset_ticks: target.offset_ticks,
                quantity_percent,
            })
            .collect();
        Ok(BracketStrategyTemplate {
            template_id: self.template_id.clone(),
            revision: self.revision,
            name: self.name.clone(),
            stop_offset_ticks: self.stop_offset_ticks,
            targets,
            trailing_stop: self.trailing_stop,
            break_even: None,
            enabled: true,
        })
    }

    /// The owner floors each target's percent share of the order's increments and gives the
    /// furthest target the rest, so every nearer target takes the smallest percent whose
    /// floor is its quantity and the furthest takes what remains.
    fn target_percents(&self) -> Result<Vec<u8>, OrderTemplateInvalid> {
        let quantity = u64::from(self.quantity);
        let (furthest, nearer) = self
            .targets
            .split_last()
            .ok_or(OrderTemplateInvalid::TargetCount)?;
        let mut percents = Vec::with_capacity(self.targets.len());
        let mut allocated = 0_u64;
        let mut allocated_quantity = 0_u64;
        for target in nearer {
            let wanted = u64::from(target.quantity);
            let percent = (wanted * 100).div_ceil(quantity);
            if quantity * percent / 100 != wanted {
                return Err(OrderTemplateInvalid::TargetSplit);
            }
            allocated += percent;
            allocated_quantity += wanted;
            percents.push(u8::try_from(percent).map_err(|_| OrderTemplateInvalid::TargetSplit)?);
        }
        let last = 100_u64
            .checked_sub(allocated)
            .filter(|percent| *percent > 0)
            .ok_or(OrderTemplateInvalid::TargetSplit)?;
        if quantity.checked_sub(allocated_quantity) != Some(u64::from(furthest.quantity)) {
            return Err(OrderTemplateInvalid::TargetSplit);
        }
        percents.push(u8::try_from(last).map_err(|_| OrderTemplateInvalid::TargetSplit)?);
        Ok(percents)
    }
}

const fn valid_ticks(ticks: u32) -> bool {
    ticks >= 1 && ticks <= MAXIMUM_ORDER_TEMPLATE_TICKS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FixedPoint;
    use crate::core::allocated_target_quantities;

    fn key(value: u64) -> NonZeroU64 {
        NonZeroU64::new(value).expect("non-zero key")
    }

    fn template(quantity: u32, targets: &[(u32, u32)]) -> OrderTemplate {
        OrderTemplate {
            template_id: order_template_id(key(0x2a)),
            revision: 1,
            name: "Scalp".to_string(),
            quantity,
            stop_offset_ticks: 8,
            targets: targets
                .iter()
                .map(|&(offset_ticks, quantity)| OrderTemplateTarget {
                    offset_ticks,
                    quantity,
                })
                .collect(),
            trailing_stop: None,
        }
    }

    #[test]
    fn identities_round_trip_only_in_canonical_form() {
        assert_eq!(order_template_id(key(0x2a)), "user-2a");
        assert_eq!(order_template_key("user-2a"), Some(key(0x2a)));
        assert_eq!(
            order_template_key(&order_template_id(key(u64::MAX))),
            Some(key(u64::MAX))
        );
        for rejected in [
            "user-",
            "user-0",
            "user-02a",
            "user-2A",
            "user-12345678901234567",
            "managed-bracket",
            " user-2a",
        ] {
            assert_eq!(order_template_key(rejected), None, "{rejected:?}");
        }
    }

    #[test]
    fn the_owner_split_of_the_template_quantity_is_exact() {
        for (quantity, targets) in [
            (1, vec![(8, 1)]),
            (3, vec![(4, 1), (8, 1), (12, 1)]),
            (4, vec![(4, 1), (8, 1), (12, 1), (16, 1)]),
            (7, vec![(4, 1), (8, 1), (12, 1), (16, 4)]),
            (99, vec![(4, 1), (8, 1), (12, 1), (16, 96)]),
            (100, vec![(4, 33), (8, 33), (12, 33), (16, 1)]),
            (1_000, vec![(4, 500), (8, 500)]),
        ] {
            let template = template(quantity, &targets);
            let bracket = template.bracket_template().expect("representable split");
            assert!(bracket.validate().is_ok());
            let split = allocated_target_quantities(
                FixedPoint::try_new(i64::from(quantity), 0).expect("quantity"),
                &bracket.targets,
                1,
            )
            .expect("allocation");
            let expected = targets
                .iter()
                .enumerate()
                .map(|(index, &(_, quantity))| {
                    (
                        index,
                        FixedPoint::try_new(i64::from(quantity), 0).expect("quantity"),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(split, expected, "{targets:?}");
        }
    }

    #[test]
    fn invalid_templates_name_the_refused_part() {
        type Case = (fn(&mut OrderTemplate), OrderTemplateInvalid);
        let valid = template(3, &[(4, 1), (8, 2)]);
        assert_eq!(valid.validate(), Ok(()));
        let cases: [Case; 12] = [
            (
                |t| t.template_id = "scalp".into(),
                OrderTemplateInvalid::Identity,
            ),
            (|t| t.revision = 0, OrderTemplateInvalid::Revision),
            (|t| t.name = "  ".into(), OrderTemplateInvalid::Name),
            (|t| t.name = " Scalp".into(), OrderTemplateInvalid::Name),
            (|t| t.name = "x".repeat(65), OrderTemplateInvalid::Name),
            (|t| t.quantity = 0, OrderTemplateInvalid::Quantity),
            (
                |t| t.stop_offset_ticks = 0,
                OrderTemplateInvalid::StopOffset,
            ),
            (|t| t.targets.clear(), OrderTemplateInvalid::TargetCount),
            (
                |t| t.targets[1].offset_ticks = 4,
                OrderTemplateInvalid::TargetOrder,
            ),
            (
                |t| t.targets[0].quantity = 0,
                OrderTemplateInvalid::TargetQuantity,
            ),
            (|t| t.quantity = 4, OrderTemplateInvalid::TargetTotal),
            (
                |t| {
                    t.trailing_stop = Some(TrailingStopRule {
                        activation_ticks: 4,
                        distance_ticks: 0,
                    });
                },
                OrderTemplateInvalid::TrailingStop,
            ),
        ];
        for (change, expected) in cases {
            let mut refused = valid.clone();
            change(&mut refused);
            assert_eq!(refused.validate(), Err(expected));
        }
        let mut far = valid.clone();
        far.targets[1].offset_ticks = MAXIMUM_ORDER_TEMPLATE_TICKS + 1;
        assert_eq!(far.validate(), Err(OrderTemplateInvalid::TargetOffset));
        // 32, 32 and 33 of 98: the nearer shares need 33 + 33 + 34 percent, leaving none.
        assert_eq!(
            template(98, &[(4, 32), (8, 32), (12, 33), (16, 1)]).validate(),
            Err(OrderTemplateInvalid::TargetSplit)
        );
    }
}
