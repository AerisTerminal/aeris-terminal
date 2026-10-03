//! Identity, outcome, and validation shared by the order and P&L plants.

use core::fmt;

/// Borrowed provider routing identity sent on every account-scoped request.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RithmicAccountKey<'a> {
    pub fcm_id: &'a str,
    pub ib_id: &'a str,
    pub account_id: &'a str,
}

impl fmt::Debug for RithmicAccountKey<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicAccountKey")
            .field("fcm_id", &self.fcm_id)
            .field("ib_id", &self.ib_id)
            .field("account_id", &"[REDACTED]")
            .finish()
    }
}

/// Provider account identifier received from the order or P&L plant.
///
/// Debug output is redacted so decoded messages can be logged without leaking
/// account data.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RithmicAccountRef(String);

impl RithmicAccountRef {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RithmicAccountRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RithmicAccountRef([REDACTED])")
    }
}

/// Order- and PnL-plant request families whose provider replies are correlated
/// by kind.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RithmicRequestKind {
    LoginInfo,
    AccountList,
    OrderUpdates,
    TradeRoutes,
    NewOrder,
    ModifyOrder,
    CancelOrder,
    CancelAllOrders,
    ShowOrders,
    ReplayExecutions,
    FillHistory,
    PnlUpdates,
    PnlSnapshot,
}

/// Provider verdict carried by one `rq_handler_rp_code` or `rp_code` field.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RithmicRequestOutcome {
    Accepted,
    /// Response code `7`: the request was valid and matched no records.
    NoData,
    Rejected {
        code: u32,
        reason: Option<String>,
    },
}

impl RithmicRequestOutcome {
    #[must_use]
    pub const fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted)
    }
}

#[cfg(rithmic_kit)]
pub(crate) mod wire {
    use super::{RithmicAccountKey, RithmicAccountRef, RithmicRequestOutcome};
    use crate::{ProtocolError, ProviderTimestamp, RithmicDecimal};

    pub(crate) const MAX_FRAME_BYTES: usize = 1024 * 1024;
    const MAX_FIELD_BYTES: usize = 256;
    const MAX_USER_MESSAGES: usize = 2;
    const NO_DATA_RESPONSE_CODE: u32 = 7;

    /// Which of the two mutually exclusive response-code fields a reply used.
    pub(crate) enum ResponseFrame {
        /// One data row or per-request acknowledgement (`rq_handler_rp_code`).
        Handler(RithmicRequestOutcome),
        /// The terminal reply that ends one request (`rp_code`).
        Terminal(RithmicRequestOutcome),
    }

    pub(crate) fn bound_frame(frame: &[u8]) -> Result<(), ProtocolError> {
        if frame.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge {
                requested: frame.len(),
                maximum: MAX_FRAME_BYTES,
            });
        }
        Ok(())
    }

    pub(crate) fn response_frame(
        user_messages: &[String],
        handler_codes: &[String],
        terminal_codes: &[String],
    ) -> Result<ResponseFrame, ProtocolError> {
        validate_user_messages(user_messages)?;
        match (handler_codes.is_empty(), terminal_codes.is_empty()) {
            (false, true) => Ok(ResponseFrame::Handler(outcome(
                "rq_handler_rp_code",
                handler_codes,
            )?)),
            (true, false) => Ok(ResponseFrame::Terminal(outcome("rp_code", terminal_codes)?)),
            _ => Err(ProtocolError::ResponseCodeShape),
        }
    }

    pub(crate) fn terminal_outcome(
        user_messages: &[String],
        codes: &[String],
    ) -> Result<RithmicRequestOutcome, ProtocolError> {
        validate_user_messages(user_messages)?;
        outcome("rp_code", codes)
    }

    fn validate_user_messages(user_messages: &[String]) -> Result<(), ProtocolError> {
        if user_messages.len() > MAX_USER_MESSAGES {
            return Err(ProtocolError::RepeatedFieldLimitExceeded {
                field: "user_msg",
                maximum: MAX_USER_MESSAGES,
            });
        }
        user_messages
            .iter()
            .try_for_each(|message| validate_string("user_msg", message))
    }

    fn outcome(
        field: &'static str,
        codes: &[String],
    ) -> Result<RithmicRequestOutcome, ProtocolError> {
        let (code, reason) = match codes {
            [code] => (code, None),
            [code, reason] => {
                validate_string(field, reason)?;
                (code, Some(reason.clone()))
            }
            _ => return Err(ProtocolError::ResponseCodeShape),
        };
        let code = code
            .parse::<u32>()
            .map_err(|_| ProtocolError::ResponseCodeShape)?;
        match (code, reason) {
            (0, None) => Ok(RithmicRequestOutcome::Accepted),
            (0, Some(_)) => Err(ProtocolError::ResponseCodeShape),
            (NO_DATA_RESPONSE_CODE, _) => Ok(RithmicRequestOutcome::NoData),
            (code, reason) => Ok(RithmicRequestOutcome::Rejected { code, reason }),
        }
    }

    pub(crate) fn validate_string(field: &'static str, value: &str) -> Result<(), ProtocolError> {
        if value.is_empty() {
            return Err(ProtocolError::EmptyField(field));
        }
        if value.len() > MAX_FIELD_BYTES {
            return Err(ProtocolError::FieldTooLong {
                field,
                maximum: MAX_FIELD_BYTES,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(ProtocolError::ControlCharacter(field));
        }
        Ok(())
    }

    pub(crate) fn validate_account(account: RithmicAccountKey<'_>) -> Result<(), ProtocolError> {
        validate_string("fcm_id", account.fcm_id)?;
        validate_string("ib_id", account.ib_id)?;
        validate_string("account_id", account.account_id)
    }

    pub(crate) fn required_string(
        field: &'static str,
        value: Option<String>,
    ) -> Result<String, ProtocolError> {
        let value = value.ok_or(ProtocolError::MissingField(field))?;
        validate_string(field, &value)?;
        Ok(value)
    }

    pub(crate) fn optional_string(
        field: &'static str,
        value: Option<String>,
    ) -> Result<Option<String>, ProtocolError> {
        if let Some(value) = &value {
            validate_string(field, value)?;
        }
        Ok(value)
    }

    pub(crate) fn account_ref(
        field: &'static str,
        value: Option<String>,
    ) -> Result<RithmicAccountRef, ProtocolError> {
        required_string(field, value).map(RithmicAccountRef)
    }

    pub(crate) fn optional_count(
        field: &'static str,
        value: Option<i32>,
    ) -> Result<Option<u32>, ProtocolError> {
        value
            .map(|value| u32::try_from(value).map_err(|_| ProtocolError::InvalidNumber(field)))
            .transpose()
    }

    pub(crate) fn optional_wide_count(
        field: &'static str,
        value: Option<u64>,
    ) -> Result<Option<u32>, ProtocolError> {
        value
            .map(|value| u32::try_from(value).map_err(|_| ProtocolError::InvalidNumber(field)))
            .transpose()
    }

    pub(crate) fn optional_price(
        field: &'static str,
        value: Option<f64>,
    ) -> Result<Option<RithmicDecimal>, ProtocolError> {
        value
            .map(|value| RithmicDecimal::from_wire_f64(field, value))
            .transpose()
    }

    pub(crate) fn optional_decimal_text(
        field: &'static str,
        value: Option<&str>,
    ) -> Result<Option<RithmicDecimal>, ProtocolError> {
        value
            .map(|value| RithmicDecimal::from_wire_str(field, value))
            .transpose()
    }

    pub(crate) fn optional_timestamp(
        seconds: Option<i32>,
        microseconds: Option<i32>,
    ) -> Result<Option<ProviderTimestamp>, ProtocolError> {
        match (seconds, microseconds) {
            (None, None) => Ok(None),
            (Some(seconds), Some(microseconds))
                if seconds >= 0 && (0..1_000_000).contains(&microseconds) =>
            {
                Ok(Some(ProviderTimestamp {
                    seconds,
                    microseconds,
                }))
            }
            (Some(_), None) | (None, Some(_)) => {
                Err(ProtocolError::InconsistentFields("ssboe/usecs"))
            }
            (Some(_), Some(_)) => Err(ProtocolError::InvalidNumber("ssboe/usecs")),
        }
    }
}
