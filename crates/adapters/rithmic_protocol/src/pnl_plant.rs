//! PnL-plant requests and position/account P&L updates.

use crate::{
    ProviderTimestamp, RithmicAccountKey, RithmicAccountRef, RithmicDecimal, RithmicRequestKind,
    RithmicRequestOutcome, SubscriptionAction,
};
use core::fmt;

/// Installs or removes pushed position and account P&L updates for one account.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PnlPositionUpdatesRequest<'a> {
    pub account: RithmicAccountKey<'a>,
    pub action: SubscriptionAction,
}

/// Outbound request accepted only on an authenticated PnL-plant session.
#[derive(Clone, Copy, Eq, PartialEq)]
pub enum PnlPlantRequest<'a> {
    PositionUpdates(PnlPositionUpdatesRequest<'a>),
    PositionSnapshot(RithmicAccountKey<'a>),
}

impl PnlPlantRequest<'_> {
    #[must_use]
    pub const fn kind(&self) -> RithmicRequestKind {
        match self {
            Self::PositionUpdates(_) => RithmicRequestKind::PnlUpdates,
            Self::PositionSnapshot(_) => RithmicRequestKind::PnlSnapshot,
        }
    }
}

impl fmt::Debug for PnlPlantRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PnlPlantRequest::{:?}", self.kind())
    }
}

/// Quantities shared by instrument and account P&L updates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RithmicPositionQuantities {
    pub net_quantity: Option<i32>,
    pub open_position_quantity: Option<u32>,
    pub closed_position_quantity: Option<u32>,
    pub buy_quantity: Option<u32>,
    pub sell_quantity: Option<u32>,
    pub fill_buy_quantity: Option<u32>,
    pub fill_sell_quantity: Option<u32>,
    pub order_buy_quantity: Option<u32>,
    pub order_sell_quantity: Option<u32>,
}

/// Decoded `instrument_pnl_position_update` (template 450). Monetary values are
/// in the account currency at the scale the provider sent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicInstrumentPnl {
    pub is_snapshot: bool,
    pub account_id: RithmicAccountRef,
    pub symbol: String,
    pub exchange: String,
    pub quantities: RithmicPositionQuantities,
    pub average_open_fill_price: Option<RithmicDecimal>,
    pub open_position_pnl: Option<RithmicDecimal>,
    pub closed_position_pnl: Option<RithmicDecimal>,
    pub day_open_pnl: Option<RithmicDecimal>,
    pub day_closed_pnl: Option<RithmicDecimal>,
    pub day_pnl: Option<RithmicDecimal>,
    pub timestamp: Option<ProviderTimestamp>,
}

/// Decoded `account_pnl_position_update` (template 451). Monetary values are in
/// the account currency at the scale the provider sent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RithmicAccountPnl {
    pub is_snapshot: bool,
    pub account_id: RithmicAccountRef,
    pub quantities: RithmicPositionQuantities,
    pub account_balance: Option<RithmicDecimal>,
    pub cash_on_hand: Option<RithmicDecimal>,
    pub margin_balance: Option<RithmicDecimal>,
    pub minimum_account_balance: Option<RithmicDecimal>,
    pub available_buying_power: Option<RithmicDecimal>,
    pub used_buying_power: Option<RithmicDecimal>,
    pub reserved_buying_power: Option<RithmicDecimal>,
    pub open_position_pnl: Option<RithmicDecimal>,
    pub closed_position_pnl: Option<RithmicDecimal>,
    pub day_open_pnl: Option<RithmicDecimal>,
    pub day_closed_pnl: Option<RithmicDecimal>,
    pub day_pnl: Option<RithmicDecimal>,
    pub timestamp: Option<ProviderTimestamp>,
}

/// Sanitized PnL-plant message decoded from one provider WebSocket message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodedPnlMessage {
    Instrument(RithmicInstrumentPnl),
    Account(RithmicAccountPnl),
    RequestComplete {
        request: RithmicRequestKind,
        outcome: RithmicRequestOutcome,
    },
}

#[cfg(rithmic_kit)]
pub(crate) use kit::{OUTBOUND_TEMPLATES, decode, encode};

#[cfg(not(rithmic_kit))]
pub(crate) const fn decode(_frame: &[u8]) -> Result<DecodedPnlMessage, crate::ProtocolError> {
    Err(crate::ProtocolError::KitUnavailable)
}

#[cfg(rithmic_kit)]
mod kit {
    use super::{
        DecodedPnlMessage, PnlPlantRequest, RithmicAccountPnl, RithmicInstrumentPnl,
        RithmicPositionQuantities,
    };
    use crate::{
        ProtocolError, RithmicRequestKind, SubscriptionAction,
        generated::rti,
        plant_common::wire::{
            account_ref, bound_frame, optional_count, optional_decimal_text, optional_price,
            optional_timestamp, required_string, terminal_outcome, validate_account,
        },
    };
    use prost::Message;

    const PNL_UPDATES_REQUEST: i32 = 400;
    const PNL_UPDATES_RESPONSE: i32 = 401;
    const PNL_SNAPSHOT_REQUEST: i32 = 402;
    const PNL_SNAPSHOT_RESPONSE: i32 = 403;
    const INSTRUMENT_PNL_UPDATE: i32 = 450;
    const ACCOUNT_PNL_UPDATE: i32 = 451;

    pub(crate) const OUTBOUND_TEMPLATES: &[i32] = &[PNL_UPDATES_REQUEST, PNL_SNAPSHOT_REQUEST];

    pub(crate) fn encode(request: PnlPlantRequest<'_>) -> Result<Vec<u8>, ProtocolError> {
        match request {
            PnlPlantRequest::PositionUpdates(request) => {
                validate_account(request.account)?;
                let action = match request.action {
                    SubscriptionAction::Subscribe => {
                        rti::request_pn_l_position_updates::Request::Subscribe
                    }
                    SubscriptionAction::Unsubscribe => {
                        rti::request_pn_l_position_updates::Request::Unsubscribe
                    }
                };
                Ok(rti::RequestPnLPositionUpdates {
                    template_id: PNL_UPDATES_REQUEST,
                    user_msg: Vec::new(),
                    request: Some(action.into()),
                    fcm_id: Some(request.account.fcm_id.to_string()),
                    ib_id: Some(request.account.ib_id.to_string()),
                    account_id: Some(request.account.account_id.to_string()),
                    rms_updates_only: None,
                }
                .encode_to_vec())
            }
            PnlPlantRequest::PositionSnapshot(account) => {
                validate_account(account)?;
                Ok(rti::RequestPnLPositionSnapshot {
                    template_id: PNL_SNAPSHOT_REQUEST,
                    user_msg: Vec::new(),
                    fcm_id: Some(account.fcm_id.to_string()),
                    ib_id: Some(account.ib_id.to_string()),
                    account_id: Some(account.account_id.to_string()),
                }
                .encode_to_vec())
            }
        }
    }

    pub(crate) fn decode(frame: &[u8]) -> Result<DecodedPnlMessage, ProtocolError> {
        bound_frame(frame)?;
        let template = rti::MessageType::decode(frame)
            .map_err(|_| ProtocolError::Decode)?
            .template_id;
        match template {
            PNL_UPDATES_RESPONSE => {
                let message = rti::ResponsePnLPositionUpdates::decode(frame)
                    .map_err(|_| ProtocolError::Decode)?;
                Ok(DecodedPnlMessage::RequestComplete {
                    request: RithmicRequestKind::PnlUpdates,
                    outcome: terminal_outcome(&message.user_msg, &message.rp_code)?,
                })
            }
            PNL_SNAPSHOT_RESPONSE => {
                let message = rti::ResponsePnLPositionSnapshot::decode(frame)
                    .map_err(|_| ProtocolError::Decode)?;
                Ok(DecodedPnlMessage::RequestComplete {
                    request: RithmicRequestKind::PnlSnapshot,
                    outcome: terminal_outcome(&message.user_msg, &message.rp_code)?,
                })
            }
            INSTRUMENT_PNL_UPDATE => decode_instrument(frame),
            ACCOUNT_PNL_UPDATE => decode_account(frame),
            template => Err(ProtocolError::UnsupportedTemplate(template)),
        }
    }

    #[derive(Clone, Copy)]
    struct QuantityWire {
        net_quantity: Option<i32>,
        open_position_quantity: Option<i32>,
        closed_position_quantity: Option<i32>,
        buy_qty: Option<i32>,
        sell_qty: Option<i32>,
        fill_buy_qty: Option<i32>,
        fill_sell_qty: Option<i32>,
        order_buy_qty: Option<i32>,
        order_sell_qty: Option<i32>,
    }

    fn quantities(wire: QuantityWire) -> Result<RithmicPositionQuantities, ProtocolError> {
        Ok(RithmicPositionQuantities {
            net_quantity: wire.net_quantity,
            open_position_quantity: optional_count(
                "pnl.open_position_quantity",
                wire.open_position_quantity,
            )?,
            closed_position_quantity: optional_count(
                "pnl.closed_position_quantity",
                wire.closed_position_quantity,
            )?,
            buy_quantity: optional_count("pnl.buy_qty", wire.buy_qty)?,
            sell_quantity: optional_count("pnl.sell_qty", wire.sell_qty)?,
            fill_buy_quantity: optional_count("pnl.fill_buy_qty", wire.fill_buy_qty)?,
            fill_sell_quantity: optional_count("pnl.fill_sell_qty", wire.fill_sell_qty)?,
            order_buy_quantity: optional_count("pnl.order_buy_qty", wire.order_buy_qty)?,
            order_sell_quantity: optional_count("pnl.order_sell_qty", wire.order_sell_qty)?,
        })
    }

    fn decode_instrument(frame: &[u8]) -> Result<DecodedPnlMessage, ProtocolError> {
        let message =
            rti::InstrumentPnLPositionUpdate::decode(frame).map_err(|_| ProtocolError::Decode)?;
        Ok(DecodedPnlMessage::Instrument(RithmicInstrumentPnl {
            // `is_snapshot` is only set on snapshot replays; absence marks a live update.
            is_snapshot: message.is_snapshot == Some(true),
            account_id: account_ref("instrument_pnl.account_id", message.account_id)?,
            symbol: required_string("instrument_pnl.symbol", message.symbol)?,
            exchange: required_string("instrument_pnl.exchange", message.exchange)?,
            quantities: quantities(QuantityWire {
                net_quantity: message.net_quantity,
                open_position_quantity: message.open_position_quantity,
                closed_position_quantity: message.closed_position_quantity,
                buy_qty: message.buy_qty,
                sell_qty: message.sell_qty,
                fill_buy_qty: message.fill_buy_qty,
                fill_sell_qty: message.fill_sell_qty,
                order_buy_qty: message.order_buy_qty,
                order_sell_qty: message.order_sell_qty,
            })?,
            average_open_fill_price: optional_price(
                "instrument_pnl.avg_open_fill_price",
                message.avg_open_fill_price,
            )?,
            open_position_pnl: optional_decimal_text(
                "instrument_pnl.open_position_pnl",
                message.open_position_pnl.as_deref(),
            )?,
            closed_position_pnl: optional_decimal_text(
                "instrument_pnl.closed_position_pnl",
                message.closed_position_pnl.as_deref(),
            )?,
            day_open_pnl: optional_price("instrument_pnl.day_open_pnl", message.day_open_pnl)?,
            day_closed_pnl: optional_price(
                "instrument_pnl.day_closed_pnl",
                message.day_closed_pnl,
            )?,
            day_pnl: optional_price("instrument_pnl.day_pnl", message.day_pnl)?,
            timestamp: optional_timestamp(message.ssboe, message.usecs)?,
        }))
    }

    fn decode_account(frame: &[u8]) -> Result<DecodedPnlMessage, ProtocolError> {
        let message =
            rti::AccountPnLPositionUpdate::decode(frame).map_err(|_| ProtocolError::Decode)?;
        let text = |field, value: &Option<String>| optional_decimal_text(field, value.as_deref());
        Ok(DecodedPnlMessage::Account(RithmicAccountPnl {
            // `is_snapshot` is only set on snapshot replays; absence marks a live update.
            is_snapshot: message.is_snapshot == Some(true),
            account_id: account_ref("account_pnl.account_id", message.account_id)?,
            quantities: quantities(QuantityWire {
                net_quantity: message.net_quantity,
                open_position_quantity: message.open_position_quantity,
                closed_position_quantity: message.closed_position_quantity,
                buy_qty: message.buy_qty,
                sell_qty: message.sell_qty,
                fill_buy_qty: message.fill_buy_qty,
                fill_sell_qty: message.fill_sell_qty,
                order_buy_qty: message.order_buy_qty,
                order_sell_qty: message.order_sell_qty,
            })?,
            account_balance: text("account_pnl.account_balance", &message.account_balance)?,
            cash_on_hand: text("account_pnl.cash_on_hand", &message.cash_on_hand)?,
            margin_balance: text("account_pnl.margin_balance", &message.margin_balance)?,
            minimum_account_balance: text(
                "account_pnl.min_account_balance",
                &message.min_account_balance,
            )?,
            available_buying_power: text(
                "account_pnl.available_buying_power",
                &message.available_buying_power,
            )?,
            used_buying_power: text("account_pnl.used_buying_power", &message.used_buying_power)?,
            reserved_buying_power: text(
                "account_pnl.reserved_buying_power",
                &message.reserved_buying_power,
            )?,
            open_position_pnl: text("account_pnl.open_position_pnl", &message.open_position_pnl)?,
            closed_position_pnl: text(
                "account_pnl.closed_position_pnl",
                &message.closed_position_pnl,
            )?,
            day_open_pnl: text("account_pnl.day_open_pnl", &message.day_open_pnl)?,
            day_closed_pnl: text("account_pnl.day_closed_pnl", &message.day_closed_pnl)?,
            day_pnl: text("account_pnl.day_pnl", &message.day_pnl)?,
            timestamp: optional_timestamp(message.ssboe, message.usecs)?,
        }))
    }
}

#[cfg(all(test, rithmic_kit))]
mod tests {
    use super::{DecodedPnlMessage, PnlPlantRequest, PnlPositionUpdatesRequest, decode, encode};
    use crate::{
        ProtocolError, RithmicAccountKey, RithmicRequestKind, RithmicRequestOutcome,
        SubscriptionAction, generated::rti,
    };
    use prost::Message as _;

    const ACCOUNT: RithmicAccountKey<'static> = RithmicAccountKey {
        fcm_id: "fixture-fcm",
        ib_id: "fixture-ib",
        account_id: "fixture-account",
    };

    #[test]
    fn position_update_requests_carry_account_and_action() {
        let frame = encode(PnlPlantRequest::PositionUpdates(
            PnlPositionUpdatesRequest {
                account: ACCOUNT,
                action: SubscriptionAction::Unsubscribe,
            },
        ))
        .expect("encode PnL unsubscribe");
        let request =
            rti::RequestPnLPositionUpdates::decode(frame.as_slice()).expect("decode request");
        assert_eq!(request.template_id, 400);
        assert_eq!(
            request.request,
            Some(rti::request_pn_l_position_updates::Request::Unsubscribe.into())
        );
        assert_eq!(request.account_id.as_deref(), Some("fixture-account"));

        let frame = encode(PnlPlantRequest::PositionSnapshot(ACCOUNT)).expect("encode snapshot");
        let request =
            rti::RequestPnLPositionSnapshot::decode(frame.as_slice()).expect("decode snapshot");
        assert_eq!(request.template_id, 402);
        assert_eq!(request.fcm_id.as_deref(), Some("fixture-fcm"));

        let mut unrouted = ACCOUNT;
        unrouted.ib_id = "";
        assert_eq!(
            encode(PnlPlantRequest::PositionSnapshot(unrouted)),
            Err(ProtocolError::EmptyField("ib_id"))
        );
    }

    #[test]
    fn subscription_and_snapshot_replies_complete_their_request() {
        let updates = rti::ResponsePnLPositionUpdates {
            template_id: 401,
            rp_code: vec!["0".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(
            decode(&updates),
            Ok(DecodedPnlMessage::RequestComplete {
                request: RithmicRequestKind::PnlUpdates,
                outcome: RithmicRequestOutcome::Accepted,
            })
        );
        let snapshot = rti::ResponsePnLPositionSnapshot {
            template_id: 403,
            rp_code: vec!["5".to_string(), "fixture rejection".to_string()],
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(
            decode(&snapshot),
            Ok(DecodedPnlMessage::RequestComplete {
                request: RithmicRequestKind::PnlSnapshot,
                outcome: RithmicRequestOutcome::Rejected {
                    code: 5,
                    reason: Some("fixture rejection".to_string()),
                },
            })
        );
    }

    #[test]
    fn instrument_pnl_keeps_provider_scale_for_text_and_double_fields() {
        let update = rti::InstrumentPnLPositionUpdate {
            template_id: 450,
            is_snapshot: Some(true),
            account_id: Some("fixture-account".to_string()),
            symbol: Some("ESM7".to_string()),
            exchange: Some("CME".to_string()),
            net_quantity: Some(-2),
            open_position_quantity: Some(2),
            avg_open_fill_price: Some(5_100.25),
            open_position_pnl: Some("-125.50".to_string()),
            day_pnl: Some(37.5),
            ssboe: Some(1_800_000_000),
            usecs: Some(10),
            ..Default::default()
        };
        let Ok(DecodedPnlMessage::Instrument(pnl)) = decode(&update.encode_to_vec()) else {
            panic!("instrument PnL decodes");
        };
        assert!(pnl.is_snapshot);
        assert_eq!(pnl.quantities.net_quantity, Some(-2));
        assert_eq!(pnl.quantities.open_position_quantity, Some(2));
        let open = pnl.open_position_pnl.expect("open PnL is present");
        assert_eq!((open.units(), open.scale()), (-12_550, 2));
        let day = pnl.day_pnl.expect("day PnL is present");
        assert_eq!((day.units(), day.scale()), (375, 1));
        let average = pnl
            .average_open_fill_price
            .expect("average price is present");
        assert_eq!((average.units(), average.scale()), (510_025, 2));
        assert!(!format!("{pnl:?}").contains("fixture-account"));

        let mut unidentified = update.clone();
        unidentified.symbol = None;
        assert_eq!(
            decode(&unidentified.encode_to_vec()),
            Err(ProtocolError::MissingField("instrument_pnl.symbol"))
        );
        let mut negative_open = update;
        negative_open.open_position_quantity = Some(-1);
        assert_eq!(
            decode(&negative_open.encode_to_vec()),
            Err(ProtocolError::InvalidNumber("pnl.open_position_quantity"))
        );
    }

    #[test]
    fn account_pnl_text_fields_are_parsed_without_rounding() {
        let update = rti::AccountPnLPositionUpdate {
            template_id: 451,
            account_id: Some("fixture-account".to_string()),
            account_balance: Some("100000.125".to_string()),
            available_buying_power: Some("95000".to_string()),
            day_pnl: Some("-12.5".to_string()),
            ..Default::default()
        };
        let Ok(DecodedPnlMessage::Account(pnl)) = decode(&update.encode_to_vec()) else {
            panic!("account PnL decodes");
        };
        assert!(!pnl.is_snapshot);
        let balance = pnl.account_balance.expect("balance is present");
        assert_eq!((balance.units(), balance.scale()), (100_000_125, 3));
        let buying_power = pnl.available_buying_power.expect("buying power is present");
        assert_eq!((buying_power.units(), buying_power.scale()), (95_000, 0));
        let day = pnl.day_pnl.expect("day PnL is present");
        assert_eq!((day.units(), day.scale()), (-125, 1));
        assert_eq!(pnl.cash_on_hand, None);

        let mut malformed = update;
        malformed.cash_on_hand = Some("12,50".to_string());
        assert!(matches!(
            decode(&malformed.encode_to_vec()),
            Err(ProtocolError::InvalidNumber("account_pnl.cash_on_hand"))
        ));
    }
}
