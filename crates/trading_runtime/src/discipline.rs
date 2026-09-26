use aeris_trading::{FixedPoint, TradingAccountId};

/// Two completed losing round trips inside five minutes trigger a cooldown.
pub const RAPID_LOSS_THRESHOLD: u8 = 2;
pub const RAPID_LOSS_WINDOW_NANOS: i64 = 5 * 60 * 1_000_000_000;
/// A rapid-loss cooldown lasts fifteen minutes and cannot be manually bypassed.
pub const RAPID_LOSS_COOLDOWN_NANOS: i64 = 15 * 60 * 1_000_000_000;
/// Re-entry during the minute after a filled stop produces an explicit warning.
pub const FAST_STOP_REENTRY_NANOS: i64 = 60 * 1_000_000_000;

/// Durable deterministic tilt state derived only from canonical fills.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisciplineState {
    pub account_id: TradingAccountId,
    pub rapid_loss_count: u8,
    pub loss_window_started_unix_nanos: Option<i64>,
    pub last_loss_unix_nanos: Option<i64>,
    /// Until a non-losing round trip completes, new entries cannot exceed this quantity.
    pub post_loss_quantity_cap: Option<FixedPoint>,
    pub last_stop_fill_unix_nanos: Option<i64>,
    pub cooldown_until_unix_nanos: Option<i64>,
}

impl DisciplineState {
    pub(super) fn new(account_id: TradingAccountId) -> Self {
        Self {
            account_id,
            rapid_loss_count: 0,
            loss_window_started_unix_nanos: None,
            last_loss_unix_nanos: None,
            post_loss_quantity_cap: None,
            last_stop_fill_unix_nanos: None,
            cooldown_until_unix_nanos: None,
        }
    }
}
