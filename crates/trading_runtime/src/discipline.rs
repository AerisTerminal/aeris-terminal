use aeris_trading::TradingAccountId;

/// Re-entry during the minute after a filled stop produces an explicit warning.
pub const FAST_STOP_REENTRY_NANOS: i64 = 60 * 1_000_000_000;

/// Durable per-account discipline state derived only from canonical fills. It feeds
/// advisory warnings only; it never blocks an order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisciplineState {
    pub account_id: TradingAccountId,
    pub last_stop_fill_unix_nanos: Option<i64>,
}

impl DisciplineState {
    pub(super) fn new(account_id: TradingAccountId) -> Self {
        Self {
            account_id,
            last_stop_fill_unix_nanos: None,
        }
    }
}
