//! Provider-neutral, bounded facts exchanged between market and trading owners.

use crate::FixedPoint;
use aeris_instruments::{
    ContractDate, InstrumentClass, InstrumentDecimal, InstrumentId, MAXIMUM_SESSION_SEGMENTS,
    PriceGrid, SessionHours, SessionPhase,
};
use std::{collections::BTreeSet, error::Error, fmt};

pub const MAXIMUM_QUOTE_INTEREST: usize = 256;
const MAXIMUM_ROUTE_FIELD_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarketRoute {
    pub provider_id: String,
    pub instrument_id: InstrumentId,
    pub provider_symbol: String,
    pub display_symbol: String,
    pub venue_id: String,
    pub entitlement_id: Option<String>,
    pub price_scale: u8,
    pub quantity_scale: u8,
}

impl MarketRoute {
    /// Rejects incomplete provider routes and unsupported decimal scales.
    ///
    /// # Errors
    /// Returns a typed error if any routing identity or scale is invalid.
    pub fn validate(&self) -> Result<(), MarketFeedError> {
        for (name, value) in [
            ("provider_id", self.provider_id.as_str()),
            ("provider_symbol", self.provider_symbol.as_str()),
            ("display_symbol", self.display_symbol.as_str()),
            ("venue_id", self.venue_id.as_str()),
        ] {
            if value.trim().is_empty() || value.len() > MAXIMUM_ROUTE_FIELD_BYTES {
                return Err(MarketFeedError::InvalidRoute(name));
            }
        }
        if self
            .entitlement_id
            .as_ref()
            .is_some_and(|value| value.trim().is_empty() || value.len() > MAXIMUM_ROUTE_FIELD_BYTES)
        {
            return Err(MarketFeedError::InvalidRoute("entitlement_id"));
        }
        if self.price_scale > 18 || self.quantity_scale > 18 {
            return Err(MarketFeedError::InvalidScale);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TradingQuote {
    pub instrument_id: InstrumentId,
    pub bid: FixedPoint,
    pub ask: FixedPoint,
    pub bid_size: Option<FixedPoint>,
    pub ask_size: Option<FixedPoint>,
    pub last: Option<FixedPoint>,
    pub provider_id: String,
    pub provider_generation: u64,
    pub source_sequence: Option<u64>,
    pub exchange_unix_nanos: Option<i64>,
    pub received_unix_nanos: i64,
}

impl TradingQuote {
    /// Validates touch prices and exact provider-route precision.
    ///
    /// # Errors
    /// Rejects crossed/locked quotes, negative quantities, wrong routes or scales.
    pub fn validate(&self, route: &MarketRoute) -> Result<(), MarketFeedError> {
        route.validate()?;
        if self.instrument_id != route.instrument_id || self.provider_id != route.provider_id {
            return Err(MarketFeedError::RouteMismatch);
        }
        if self.provider_generation == 0
            || self.received_unix_nanos < 0
            || self.exchange_unix_nanos.is_some_and(|time| time < 0)
        {
            return Err(MarketFeedError::InvalidQuoteTime);
        }
        if self.bid.scale() != route.price_scale
            || self.ask.scale() != route.price_scale
            || self
                .last
                .is_some_and(|value| value.scale() != route.price_scale)
            || self
                .bid_size
                .into_iter()
                .chain(self.ask_size)
                .any(|value| value.scale() != route.quantity_scale)
        {
            return Err(MarketFeedError::InvalidScale);
        }
        if self.bid.units() <= 0 || self.ask.units() <= self.bid.units() {
            return Err(MarketFeedError::InvalidSpread);
        }
        if self
            .bid_size
            .into_iter()
            .chain(self.ask_size)
            .any(|value| value.units() < 0)
        {
            return Err(MarketFeedError::InvalidSize);
        }
        Ok(())
    }
}

/// Provider-neutral reason for withholding execution against a market feed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedReason {
    RouteUnknown,
    Disconnected,
    Recovering,
    QuoteExpired,
    QuoteMissing,
    Unsupported,
    EntitlementUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedState {
    Live,
    Stale { reason: FeedReason },
    Unavailable { reason: FeedReason },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstrumentSession {
    pub instrument_id: InstrumentId,
    pub provider_generation: u64,
    pub phase: SessionPhase,
    pub source: aeris_instruments::CalendarSourceKind,
    pub session_end_unix_nanos: Option<i64>,
    pub next_open_unix_nanos: Option<i64>,
}

impl InstrumentSession {
    /// Rejects negative or reversed UTC session boundaries.
    ///
    /// # Errors
    /// Returns an error when a provider supplies malformed UTC timestamps.
    pub fn validate(&self) -> Result<(), MarketFeedError> {
        if self.provider_generation == 0
            || self.session_end_unix_nanos.is_some_and(|time| time < 0)
            || self.next_open_unix_nanos.is_some_and(|time| time < 0)
        {
            return Err(MarketFeedError::InvalidSessionTime);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstrumentTerms {
    pub route: MarketRoute,
    pub instrument_class: InstrumentClass,
    pub price_grid: PriceGrid,
    pub point_value: Option<InstrumentDecimal>,
    pub quantity_increment: Option<InstrumentDecimal>,
    pub currency: String,
    pub expiry: Option<ContractDate>,
    pub first_notice: Option<ContractDate>,
    pub last_trade: Option<ContractDate>,
    pub session_hours: Vec<SessionHours>,
    pub session_generation: u64,
}

impl InstrumentTerms {
    /// Ensures contract rules can be associated with the advertised route.
    ///
    /// # Errors
    /// Returns an error for invalid route, grid, or currency.
    pub fn validate(&self) -> Result<(), MarketFeedError> {
        self.route.validate()?;
        self.price_grid
            .validate()
            .map_err(|_| MarketFeedError::InvalidPriceGrid)?;
        if self.currency.trim().is_empty() || self.currency.len() > 16 {
            return Err(MarketFeedError::InvalidRoute("currency"));
        }
        for date in [self.expiry, self.first_notice, self.last_trade]
            .into_iter()
            .flatten()
        {
            date.validate().map_err(|_| MarketFeedError::InvalidTerms)?;
        }
        if self.session_generation == 0 || self.session_hours.len() > MAXIMUM_SESSION_SEGMENTS {
            return Err(MarketFeedError::InvalidTerms);
        }
        for hours in &self.session_hours {
            hours
                .validate()
                .map_err(|_| MarketFeedError::InvalidTerms)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuoteInterest {
    pub revision: u64,
    pub routes: Vec<MarketRoute>,
}

impl QuoteInterest {
    /// Validates a bounded interest revision without losing entitlement identity.
    ///
    /// # Errors
    /// Returns an error for a zero revision, excessive routes or duplicate keys.
    pub fn validate(&self) -> Result<(), MarketFeedError> {
        if self.revision == 0 {
            return Err(MarketFeedError::InvalidRevision);
        }
        if self.routes.len() > MAXIMUM_QUOTE_INTEREST {
            return Err(MarketFeedError::InterestLimitExceeded);
        }
        let mut keys = BTreeSet::new();
        for route in &self.routes {
            route.validate()?;
            if !keys.insert((
                route.provider_id.as_str(),
                &route.instrument_id,
                route.entitlement_id.as_deref(),
            )) {
                return Err(MarketFeedError::DuplicateRoute);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarketFeedEvent {
    Quote(TradingQuote),
    Feed {
        instrument_id: InstrumentId,
        state: FeedState,
        provider_generation: u64,
    },
    Session(InstrumentSession),
    Terms(InstrumentTerms),
}

/// Trading publishes its current interest without owning provider connections.
pub trait QuoteInterestSink: Send + Sync {
    /// Replaces the previous bounded interest set.
    ///
    /// # Errors
    /// Returns an error when the revision or interest set cannot be installed.
    fn set_interest(&self, interest: QuoteInterest) -> Result<(), MarketFeedError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarketFeedError {
    InvalidRoute(&'static str),
    InvalidScale,
    InvalidSpread,
    InvalidSize,
    InvalidQuoteTime,
    InvalidSessionTime,
    RouteMismatch,
    InvalidPriceGrid,
    InvalidTerms,
    InvalidRevision,
    DuplicateRoute,
    InterestLimitExceeded,
    Unavailable,
}

impl fmt::Display for MarketFeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl Error for MarketFeedError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn route() -> MarketRoute {
        MarketRoute {
            provider_id: "provider".into(),
            instrument_id: InstrumentId::try_new("spot/BTC").unwrap(),
            provider_symbol: "BTC".into(),
            display_symbol: "BTC".into(),
            venue_id: "venue".into(),
            entitlement_id: None,
            price_scale: 2,
            quantity_scale: 3,
        }
    }

    #[test]
    fn quote_rejects_crossed_touch_and_mismatched_precision() {
        let route = route();
        let mut quote = TradingQuote {
            instrument_id: route.instrument_id.clone(),
            bid: FixedPoint::try_new(1234, 2).unwrap(),
            ask: FixedPoint::try_new(1235, 2).unwrap(),
            bid_size: Some(FixedPoint::try_new(10, 3).unwrap()),
            ask_size: None,
            last: None,
            provider_id: route.provider_id.clone(),
            provider_generation: 2,
            source_sequence: None,
            exchange_unix_nanos: None,
            received_unix_nanos: 1,
        };
        assert_eq!(quote.validate(&route), Ok(()));
        quote.bid = quote.ask;
        assert_eq!(quote.validate(&route), Err(MarketFeedError::InvalidSpread));
        quote.bid = FixedPoint::try_new(1234, 3).unwrap();
        assert_eq!(quote.validate(&route), Err(MarketFeedError::InvalidScale));
    }

    #[test]
    fn interest_is_bounded_and_keeps_entitlements_distinct() {
        let route = route();
        let mut interest = QuoteInterest {
            revision: 1,
            routes: vec![route.clone(), route],
        };
        assert_eq!(interest.validate(), Err(MarketFeedError::DuplicateRoute));
        interest.routes[1].entitlement_id = Some("premium".into());
        assert_eq!(interest.validate(), Ok(()));
        interest.revision = 0;
        assert_eq!(interest.validate(), Err(MarketFeedError::InvalidRevision));
    }
}
