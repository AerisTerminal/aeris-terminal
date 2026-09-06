use crate::{MarketBar, MarketDataValidationError};
use std::collections::BTreeSet;

const NANOS_PER_SECOND: i64 = 1_000_000_000;
/// Maximum bytes accepted in one canonical market-data identity field.
pub const MAXIMUM_MARKET_DATA_FIELD_BYTES: usize = 256;

/// Provider and entitlement identity plus ordering and timestamp evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventMetadata {
    pub provider_id: String,
    pub instrument_id: String,
    pub entitlement_id: String,
    pub source_sequence: u64,
    pub session_generation: u64,
    pub timestamps: QualifiedTimestamp,
}

impl EventMetadata {
    /// Validates canonical identity, generation, ordering, and timestamps.
    ///
    /// # Errors
    ///
    /// Returns an error when required identity or ordering evidence is absent.
    pub fn validate(&self) -> Result<(), MarketDataValidationError> {
        for (field, value) in [
            ("provider_id", self.provider_id.as_str()),
            ("instrument_id", self.instrument_id.as_str()),
            ("entitlement_id", self.entitlement_id.as_str()),
        ] {
            validate_identity_field(field, value)?;
        }
        if self.source_sequence == 0 {
            return Err(MarketDataValidationError::ZeroSourceSequence);
        }
        if self.session_generation == 0 {
            return Err(MarketDataValidationError::InvalidRevision);
        }
        self.timestamps.validate()
    }
}

/// Provider and local timestamps retained without conflating their meanings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QualifiedTimestamp {
    pub exchange_unix_nanos: Option<i64>,
    pub provider_unix_nanos: Option<i64>,
    pub received_unix_nanos: i64,
}

impl QualifiedTimestamp {
    /// Validates nanosecond timestamp fields.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-positive local receipt timestamp or a negative
    /// optional provider timestamp.
    pub fn validate(self) -> Result<(), MarketDataValidationError> {
        if self.received_unix_nanos <= 0 {
            return Err(MarketDataValidationError::InvalidTimestamp(
                "received_unix_nanos",
            ));
        }
        if self.exchange_unix_nanos.is_some_and(|value| value < 0) {
            return Err(MarketDataValidationError::InvalidTimestamp(
                "exchange_unix_nanos",
            ));
        }
        if self.provider_unix_nanos.is_some_and(|value| value < 0) {
            return Err(MarketDataValidationError::InvalidTimestamp(
                "provider_unix_nanos",
            ));
        }
        Ok(())
    }
}

/// Side of the aggressing order for a canonical trade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AggressorSide {
    Buy,
    Sell,
    Unknown,
}

/// One provider-neutral fixed-point trade.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketTrade {
    pub metadata: EventMetadata,
    pub trade_id: String,
    pub price: i64,
    pub quantity: i64,
    pub aggressor: AggressorSide,
}

impl MarketTrade {
    /// Validates trade identity and positive fixed-point values.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid metadata, identity, price, or quantity.
    pub fn validate(&self) -> Result<(), MarketDataValidationError> {
        self.metadata.validate()?;
        validate_identity_field("trade_id", &self.trade_id)?;
        if self.price <= 0 {
            return Err(MarketDataValidationError::InvalidPrice);
        }
        if self.quantity <= 0 {
            return Err(MarketDataValidationError::InvalidQuantity);
        }
        Ok(())
    }
}

/// One canonical top-of-book update. Either side may be absent after an
/// explicit provider clear; callers must replace their retained BBO with the
/// complete state carried here rather than retaining a cleared side.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopOfBookQuote {
    pub metadata: EventMetadata,
    pub bid: Option<DepthLevel>,
    pub ask: Option<DepthLevel>,
}

impl TopOfBookQuote {
    /// Validates metadata, present levels, and a non-crossed spread.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid metadata or quote values.
    pub fn validate(&self) -> Result<(), MarketDataValidationError> {
        self.metadata.validate()?;
        if let Some(bid) = self.bid {
            bid.validate(false)?;
        }
        if let Some(ask) = self.ask {
            ask.validate(false)?;
        }
        if self
            .bid
            .zip(self.ask)
            .is_some_and(|(bid, ask)| bid.price >= ask.price)
        {
            return Err(MarketDataValidationError::InvalidQuote);
        }
        Ok(())
    }
}

/// One side of an order book.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BookSide {
    Bid,
    Ask,
}

/// One fixed-point price level. Zero quantity is reserved for delta removal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DepthLevel {
    pub price: i64,
    pub quantity: i64,
    pub order_count: Option<u32>,
}

impl DepthLevel {
    pub(crate) fn validate(self, allow_removal: bool) -> Result<(), MarketDataValidationError> {
        if self.price <= 0 {
            return Err(MarketDataValidationError::InvalidPrice);
        }
        if self.quantity < 0 || (!allow_removal && self.quantity == 0) {
            return Err(MarketDataValidationError::InvalidQuantity);
        }
        Ok(())
    }
}

/// A complete order-book image at one source sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepthSnapshot {
    pub metadata: EventMetadata,
    pub bids: Vec<DepthLevel>,
    pub asks: Vec<DepthLevel>,
}

impl DepthSnapshot {
    /// Validates identity, bounded levels, sorting, uniqueness, and spread.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot cannot safely establish book state.
    pub fn validate(&self, maximum_levels: usize) -> Result<(), MarketDataValidationError> {
        self.metadata.validate()?;
        if self.bids.len() > maximum_levels || self.asks.len() > maximum_levels {
            return Err(MarketDataValidationError::DepthLimitExceeded {
                maximum: maximum_levels,
            });
        }
        validate_levels(&self.bids, BookSide::Bid, false)?;
        validate_levels(&self.asks, BookSide::Ask, false)?;
        if self
            .bids
            .first()
            .zip(self.asks.first())
            .is_some_and(|(bid, ask)| bid.price >= ask.price)
        {
            return Err(MarketDataValidationError::InvalidDepth);
        }
        Ok(())
    }
}

/// One ordered replacement or removal at a depth price.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepthDelta {
    pub metadata: EventMetadata,
    pub side: BookSide,
    pub level: DepthLevel,
}

impl DepthDelta {
    /// Validates delta identity and replacement/removal values.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid metadata, price, or quantity.
    pub fn validate(&self) -> Result<(), MarketDataValidationError> {
        self.metadata.validate()?;
        self.level.validate(true)
    }
}

fn validate_levels(
    levels: &[DepthLevel],
    side: BookSide,
    allow_removal: bool,
) -> Result<(), MarketDataValidationError> {
    let mut prices = BTreeSet::new();
    let mut previous = None;
    for level in levels {
        level.validate(allow_removal)?;
        if !prices.insert(level.price) {
            return Err(MarketDataValidationError::DuplicateDepthPrice);
        }
        if previous.is_some_and(|value| match side {
            BookSide::Bid => value <= level.price,
            BookSide::Ask => value >= level.price,
        }) {
            return Err(MarketDataValidationError::InvalidDepth);
        }
        previous = Some(level.price);
    }
    Ok(())
}

/// Canonical series periods allowed by the provider-neutral runtime.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum BarPeriod {
    Tick { trades: u32 },
    Time { seconds: u32 },
    Session { days: u32 },
    Week { weeks: u32 },
    Month { months: u32 },
}

impl BarPeriod {
    /// Validates the period even when constructed directly as an enum variant.
    ///
    /// # Errors
    ///
    /// Returns an error for zero-count ticks or unsupported clock periods.
    pub fn validate(self) -> Result<(), MarketDataValidationError> {
        match self {
            Self::Tick { trades } if trades > 0 => Ok(()),
            Self::Time {
                seconds:
                    60 | 180 | 300 | 900 | 1_800 | 3_600 | 7_200 | 14_400 | 28_800 | 43_200 | 86_400,
            }
            | Self::Session { days: 1 | 3 }
            | Self::Week { weeks: 1 }
            | Self::Month { months: 1 } => Ok(()),
            Self::Tick { .. }
            | Self::Time { .. }
            | Self::Session { .. }
            | Self::Week { .. }
            | Self::Month { .. } => Err(MarketDataValidationError::InvalidPeriod),
        }
    }

    /// Creates a validated standard tick period.
    ///
    /// # Errors
    ///
    /// Returns an error when the trade count is zero.
    pub fn tick(trades: u32) -> Result<Self, MarketDataValidationError> {
        if trades == 0 {
            return Err(MarketDataValidationError::InvalidPeriod);
        }
        Ok(Self::Tick { trades })
    }

    /// Creates a validated supported time period.
    ///
    /// # Errors
    ///
    /// Returns an error unless seconds represent a supported fixed interval.
    pub fn time(seconds: u32) -> Result<Self, MarketDataValidationError> {
        if !matches!(
            seconds,
            60 | 180 | 300 | 900 | 1_800 | 3_600 | 7_200 | 14_400 | 28_800 | 43_200 | 86_400
        ) {
            return Err(MarketDataValidationError::InvalidPeriod);
        }
        Ok(Self::Time { seconds })
    }

    /// Creates a supported exchange-session day period.
    ///
    /// # Errors
    /// Returns an error unless the day count is supported.
    pub fn session(days: u32) -> Result<Self, MarketDataValidationError> {
        let period = Self::Session { days };
        period.validate().map(|()| period)
    }

    /// Creates a supported calendar-week period.
    ///
    /// # Errors
    /// Returns an error unless the week count is supported.
    pub fn week(weeks: u32) -> Result<Self, MarketDataValidationError> {
        let period = Self::Week { weeks };
        period.validate().map(|()| period)
    }

    /// Creates a supported calendar-month period.
    ///
    /// # Errors
    /// Returns an error unless the month count is supported.
    pub fn month(months: u32) -> Result<Self, MarketDataValidationError> {
        let period = Self::Month { months };
        period.validate().map(|()| period)
    }

    /// Returns the time period in nanoseconds, when clock-based.
    #[must_use]
    pub fn duration_nanos(self) -> Option<i64> {
        match self {
            Self::Time { seconds } => Some(i64::from(seconds) * NANOS_PER_SECOND),
            Self::Tick { .. } | Self::Session { .. } | Self::Week { .. } | Self::Month { .. } => {
                None
            }
        }
    }
}

/// Stable identity for one provider-neutral bar series.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BarSeriesKey {
    pub provider_id: String,
    pub instrument_id: String,
    pub entitlement_id: String,
    pub period: BarPeriod,
    pub definition_version: u32,
}

impl BarSeriesKey {
    /// Validates identity and definition version.
    ///
    /// # Errors
    ///
    /// Returns an error for empty identity or a zero definition version.
    pub fn validate(&self) -> Result<(), MarketDataValidationError> {
        for (field, value) in [
            ("provider_id", self.provider_id.as_str()),
            ("instrument_id", self.instrument_id.as_str()),
            ("entitlement_id", self.entitlement_id.as_str()),
        ] {
            validate_identity_field(field, value)?;
        }
        if self.definition_version == 0 {
            return Err(MarketDataValidationError::InvalidRevision);
        }
        self.period.validate()?;
        Ok(())
    }
}

/// Forming or completed bar publication with routing and provenance evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BarUpdate {
    Forming {
        series: BarSeriesKey,
        metadata: EventMetadata,
        bar: MarketBar,
    },
    Completed {
        series: BarSeriesKey,
        metadata: EventMetadata,
        bar: MarketBar,
    },
}

impl BarUpdate {
    /// Returns the canonical series identity.
    #[must_use]
    pub const fn series(&self) -> &BarSeriesKey {
        match self {
            Self::Forming { series, .. } | Self::Completed { series, .. } => series,
        }
    }

    /// Returns ordering, generation, and timestamp evidence.
    #[must_use]
    pub const fn metadata(&self) -> &EventMetadata {
        match self {
            Self::Forming { metadata, .. } | Self::Completed { metadata, .. } => metadata,
        }
    }

    /// Returns the contained bar.
    #[must_use]
    pub const fn bar(&self) -> MarketBar {
        match self {
            Self::Forming { bar, .. } | Self::Completed { bar, .. } => *bar,
        }
    }

    /// Validates series identity, provenance, and the contained fixed-point bar.
    ///
    /// # Errors
    ///
    /// Returns the canonical update validation failure.
    pub fn validate(&self) -> Result<(), MarketDataValidationError> {
        self.series().validate()?;
        self.metadata().validate()?;
        if self.series().provider_id != self.metadata().provider_id
            || self.series().instrument_id != self.metadata().instrument_id
            || self.series().entitlement_id != self.metadata().entitlement_id
        {
            return Err(MarketDataValidationError::BarSeriesMetadataMismatch);
        }
        if self.bar().source_sequence != self.metadata().source_sequence {
            return Err(MarketDataValidationError::BarSourceSequenceMismatch);
        }
        self.bar().validate()
    }
}

fn validate_identity_field(
    field: &'static str,
    value: &str,
) -> Result<(), MarketDataValidationError> {
    if value.trim().is_empty() {
        return Err(MarketDataValidationError::EmptyField(field));
    }
    if value.len() > MAXIMUM_MARKET_DATA_FIELD_BYTES {
        return Err(MarketDataValidationError::FieldTooLong {
            field,
            maximum: MAXIMUM_MARKET_DATA_FIELD_BYTES,
        });
    }
    Ok(())
}

/// Provider-neutral semantic market event. Wire frames never cross this boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketEvent {
    Trade(MarketTrade),
    Quote(TopOfBookQuote),
    DepthSnapshot(DepthSnapshot),
    DepthDelta(DepthDelta),
}

impl MarketEvent {
    /// Returns common ordering and provenance metadata.
    #[must_use]
    pub const fn metadata(&self) -> &EventMetadata {
        match self {
            Self::Trade(value) => &value.metadata,
            Self::Quote(value) => &value.metadata,
            Self::DepthSnapshot(value) => &value.metadata,
            Self::DepthDelta(value) => &value.metadata,
        }
    }

    /// Validates the event with an explicit snapshot depth bound.
    ///
    /// # Errors
    ///
    /// Returns the event-specific canonical validation failure.
    pub fn validate(&self, maximum_depth_levels: usize) -> Result<(), MarketDataValidationError> {
        match self {
            Self::Trade(value) => value.validate(),
            Self::Quote(value) => value.validate(),
            Self::DepthSnapshot(value) => value.validate(maximum_depth_levels),
            Self::DepthDelta(value) => value.validate(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(sequence: u64) -> EventMetadata {
        EventMetadata {
            provider_id: "fixture".to_string(),
            instrument_id: "instrument:fixture:es".to_string(),
            entitlement_id: "test".to_string(),
            source_sequence: sequence,
            session_generation: 7,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(10),
                provider_unix_nanos: Some(11),
                received_unix_nanos: 12,
            },
        }
    }

    #[test]
    fn canonical_trade_and_quote_enforce_fixed_point_invariants() {
        let trade = MarketTrade {
            metadata: metadata(1),
            trade_id: "trade-1".to_string(),
            price: 10_000,
            quantity: 25,
            aggressor: AggressorSide::Buy,
        };
        assert_eq!(trade.validate(), Ok(()));
        let mut quote = TopOfBookQuote {
            metadata: metadata(2),
            bid: Some(DepthLevel {
                price: 9_999,
                quantity: 10,
                order_count: Some(2),
            }),
            ask: Some(DepthLevel {
                price: 10_001,
                quantity: 12,
                order_count: Some(3),
            }),
        };
        assert_eq!(quote.validate(), Ok(()));
        quote.ask.as_mut().expect("ask exists").price = 9_999;
        assert_eq!(
            quote.validate(),
            Err(MarketDataValidationError::InvalidQuote)
        );
        quote.ask = None;
        assert_eq!(quote.validate(), Ok(()));
        quote.bid = None;
        assert_eq!(quote.validate(), Ok(()));
    }

    #[test]
    fn canonical_event_identity_fields_have_explicit_byte_bounds() {
        let oversized = "x".repeat(MAXIMUM_MARKET_DATA_FIELD_BYTES + 1);
        let mut trade = MarketTrade {
            metadata: metadata(1),
            trade_id: oversized.clone(),
            price: 100,
            quantity: 1,
            aggressor: AggressorSide::Buy,
        };
        assert_eq!(
            trade.validate(),
            Err(MarketDataValidationError::FieldTooLong {
                field: "trade_id",
                maximum: MAXIMUM_MARKET_DATA_FIELD_BYTES,
            })
        );
        trade.trade_id = "trade-1".to_string();
        trade.metadata.provider_id = oversized;
        assert_eq!(
            trade.validate(),
            Err(MarketDataValidationError::FieldTooLong {
                field: "provider_id",
                maximum: MAXIMUM_MARKET_DATA_FIELD_BYTES,
            })
        );
    }

    #[test]
    fn depth_snapshot_requires_sorted_unique_uncrossed_levels() {
        let mut snapshot = DepthSnapshot {
            metadata: metadata(3),
            bids: vec![
                DepthLevel {
                    price: 100,
                    quantity: 3,
                    order_count: Some(1),
                },
                DepthLevel {
                    price: 99,
                    quantity: 4,
                    order_count: None,
                },
            ],
            asks: vec![
                DepthLevel {
                    price: 101,
                    quantity: 2,
                    order_count: Some(1),
                },
                DepthLevel {
                    price: 102,
                    quantity: 5,
                    order_count: None,
                },
            ],
        };
        assert_eq!(snapshot.validate(2), Ok(()));
        snapshot.bids[1].price = 100;
        assert_eq!(
            snapshot.validate(2),
            Err(MarketDataValidationError::DuplicateDepthPrice)
        );
    }

    #[test]
    fn canonical_periods_cover_the_complete_rithmic_chart_catalog() {
        for seconds in [
            60, 180, 300, 900, 1_800, 3_600, 7_200, 14_400, 28_800, 43_200,
        ] {
            assert_eq!(BarPeriod::time(seconds), Ok(BarPeriod::Time { seconds }));
        }
        assert_eq!(BarPeriod::tick(100), Ok(BarPeriod::Tick { trades: 100 }));
        assert_eq!(BarPeriod::session(1), Ok(BarPeriod::Session { days: 1 }));
        assert_eq!(BarPeriod::session(3), Ok(BarPeriod::Session { days: 3 }));
        assert_eq!(BarPeriod::week(1), Ok(BarPeriod::Week { weeks: 1 }));
        assert_eq!(BarPeriod::month(1), Ok(BarPeriod::Month { months: 1 }));
        assert_eq!(
            BarPeriod::time(30),
            Err(MarketDataValidationError::InvalidPeriod)
        );
        assert_eq!(
            BarPeriod::tick(0),
            Err(MarketDataValidationError::InvalidPeriod)
        );
        assert_eq!(
            BarPeriod::session(2),
            Err(MarketDataValidationError::InvalidPeriod)
        );
        assert_eq!(BarPeriod::month(1).expect("month").duration_nanos(), None);
        let invalid_key = BarSeriesKey {
            provider_id: "fixture".to_string(),
            instrument_id: "instrument:fixture:es".to_string(),
            entitlement_id: "test".to_string(),
            period: BarPeriod::Time { seconds: 30 },
            definition_version: 1,
        };
        assert_eq!(
            invalid_key.validate(),
            Err(MarketDataValidationError::InvalidPeriod)
        );
    }

    #[test]
    fn bar_updates_carry_and_validate_series_provenance() {
        let series = BarSeriesKey {
            provider_id: "fixture".to_string(),
            instrument_id: "instrument:fixture:es".to_string(),
            entitlement_id: "test".to_string(),
            period: BarPeriod::Time { seconds: 60 },
            definition_version: 1,
        };
        let bar = MarketBar {
            source_sequence: 9,
            exchange_timestamp_seconds: 1_700_000_000,
            exchange_timestamp_unix_nanos: 1_700_000_000_000_000_000,
            open: 100,
            high: 102,
            low: 99,
            close: 101,
            volume: 50,
        };
        let update = BarUpdate::Completed {
            series: series.clone(),
            metadata: metadata(9),
            bar,
        };
        assert_eq!(update.validate(), Ok(()));
        assert_eq!(update.series(), &series);
        assert_eq!(update.metadata().session_generation, 7);
        assert_eq!(update.bar(), bar);

        let mut mismatched_identity = update.clone();
        let BarUpdate::Completed { metadata, .. } = &mut mismatched_identity else {
            unreachable!("constructed completed update")
        };
        metadata.instrument_id = "instrument:fixture:nq".to_string();
        assert_eq!(
            mismatched_identity.validate(),
            Err(MarketDataValidationError::BarSeriesMetadataMismatch)
        );

        let mut mismatched_sequence = update;
        let BarUpdate::Completed { metadata, .. } = &mut mismatched_sequence else {
            unreachable!("constructed completed update")
        };
        metadata.source_sequence = 10;
        assert_eq!(
            mismatched_sequence.validate(),
            Err(MarketDataValidationError::BarSourceSequenceMismatch)
        );
    }
}
