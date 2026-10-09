//! The server's unrealized profit and loss of each open position, in the account's
//! deposit currency, so no quote-to-deposit conversion happens on our side.

use super::{
    MAXIMUM_STATE_ITEMS, Money, TradingRequest,
    events::{check_account, money, payload},
    positive_id, wire_id,
};
use crate::{
    ProtoMessage,
    codec::{self, require_nested_fields},
    generated::{ProtoOaGetPositionUnrealizedPnLReq, ProtoOaGetPositionUnrealizedPnLRes},
    market::MarketDecodeError,
};

/// One open position's unrealized P&L (`ProtoOAPositionUnrealizedPnL`). `net` excludes the
/// potential closing commission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PositionUnrealizedPnl {
    pub position_id: u64,
    pub gross: Money,
    pub net: Money,
}

impl TradingRequest {
    /// `ProtoOAGetPositionUnrealizedPnLReq` (2187): unrealized P&L of every open position.
    /// It reads state only, so it is allowed for every observed account.
    ///
    /// # Errors
    /// Rejects a zero account id.
    pub fn position_unrealized_pnl(ctid: u64) -> Result<Self, MarketDecodeError> {
        Ok(Self::general(
            2187,
            2188,
            &ProtoOaGetPositionUnrealizedPnLReq {
                payload_type: None,
                ctid_trader_account_id: wire_id(ctid, "ctidTraderAccountId")?,
            },
        ))
    }
}

/// Decode a `ProtoOAGetPositionUnrealizedPnLRes` (2188) for one account.
///
/// # Errors
/// Rejects another account, missing required fields, invalid ids or money digits, and
/// oversized lists.
pub fn decode_position_unrealized_pnl(
    frame: &ProtoMessage,
    ctid: u64,
) -> Result<Vec<PositionUnrealizedPnl>, MarketDecodeError> {
    let response: ProtoOaGetPositionUnrealizedPnLRes = codec::decode_typed(
        frame,
        2188,
        &[(2, "ctidTraderAccountId"), (4, "moneyDigits")],
        |_| Ok(()),
    )?;
    require_nested_fields(
        payload(frame),
        3,
        &[
            (1, "positionId"),
            (2, "grossUnrealizedPnL"),
            (3, "netUnrealizedPnL"),
        ],
    )?;
    check_account(ctid, response.ctid_trader_account_id)?;
    if response.position_unrealized_pn_l.len() > MAXIMUM_STATE_ITEMS {
        return Err(MarketDecodeError::LimitExceeded("position unrealized P&L"));
    }
    let digits = Some(response.money_digits);
    response
        .position_unrealized_pn_l
        .into_iter()
        .map(|position| {
            Ok(PositionUnrealizedPnl {
                position_id: positive_id(position.position_id, "positionId")?,
                gross: money(position.gross_unrealized_pn_l, digits)?,
                net: money(position.net_unrealized_pn_l, digits)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        generated::ProtoOaPositionUnrealizedPnL,
        market::fixtures::{CTID, CTID_WIRE, bytes_frame, frame, strip, strip_nested},
    };
    use prost::Message;

    fn response() -> ProtoOaGetPositionUnrealizedPnLRes {
        ProtoOaGetPositionUnrealizedPnLRes {
            payload_type: None,
            ctid_trader_account_id: CTID_WIRE,
            position_unrealized_pn_l: vec![ProtoOaPositionUnrealizedPnL {
                position_id: 42,
                gross_unrealized_pn_l: -1_250,
                net_unrealized_pn_l: -1_310,
            }],
            money_digits: 2,
        }
    }

    #[test]
    fn unrealized_pnl_carries_money_digits_and_the_position() {
        let request = TradingRequest::position_unrealized_pnl(CTID).expect("request");
        assert_eq!((request.payload_type, request.response_type), (2187, 2188));
        assert_eq!(
            decode_position_unrealized_pnl(&frame(2188, &response()), CTID).expect("pnl"),
            [PositionUnrealizedPnl {
                position_id: 42,
                gross: Money {
                    units: -1_250,
                    digits: 2
                },
                net: Money {
                    units: -1_310,
                    digits: 2
                },
            }]
        );
    }

    #[test]
    fn unrealized_pnl_rejects_missing_fields_and_other_accounts() {
        let payload = response().encode_to_vec();
        for field in [2, 4] {
            assert!(
                decode_position_unrealized_pnl(&bytes_frame(2188, strip(&payload, field)), CTID)
                    .is_err(),
                "field {field} must be required"
            );
        }
        for inner in [1, 2, 3] {
            assert!(
                decode_position_unrealized_pnl(
                    &bytes_frame(2188, strip_nested(&payload, 3, inner)),
                    CTID
                )
                .is_err(),
                "position field {inner} must be required"
            );
        }
        assert!(matches!(
            decode_position_unrealized_pnl(&frame(2188, &response()), CTID + 1),
            Err(MarketDecodeError::AccountMismatch)
        ));
        let mut digits = response();
        digits.money_digits = 19;
        assert!(decode_position_unrealized_pnl(&frame(2188, &digits), CTID).is_err());
        let mut id = response();
        id.position_unrealized_pn_l[0].position_id = 0;
        assert!(decode_position_unrealized_pnl(&frame(2188, &id), CTID).is_err());
        assert!(TradingRequest::position_unrealized_pnl(0).is_err());
    }
}
