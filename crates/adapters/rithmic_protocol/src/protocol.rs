use core::fmt;
use std::error::Error;
use zeroize::Zeroize;

/// An installed R|Protocol kit was not available when this crate was built.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RithmicKitUnavailable;

/// Runtime-selected protocol backend.
#[derive(Debug)]
pub enum RithmicProtocolBackend {
    Kit(RithmicProtocolCodec),
    Unavailable(RithmicKitUnavailable),
}

impl RithmicProtocolBackend {
    /// Selects the kit-backed codec when build-time protocol generation succeeded.
    #[must_use]
    pub const fn detected() -> Self {
        #[cfg(rithmic_kit)]
        {
            Self::Kit(RithmicProtocolCodec)
        }
        #[cfg(not(rithmic_kit))]
        {
            Self::Unavailable(RithmicKitUnavailable)
        }
    }

    /// Reports whether the installed kit was available during this build.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Kit(_))
    }

    /// Encodes one allowlisted read-only request when the kit is available.
    ///
    /// # Errors
    ///
    /// Returns a bounded protocol error or [`ProtocolError::KitUnavailable`].
    pub fn encode(&self, request: OutboundRequest<'_>) -> Result<SensitiveFrame, ProtocolError> {
        match self {
            Self::Kit(codec) => codec.encode(request),
            Self::Unavailable(_) => Err(ProtocolError::KitUnavailable),
        }
    }

    /// Decodes one bounded control response when the kit is available.
    ///
    /// # Errors
    ///
    /// Returns a bounded protocol error or [`ProtocolError::KitUnavailable`].
    pub fn decode_control(&self, frame: &[u8]) -> Result<DecodedControlMessage, ProtocolError> {
        match self {
            Self::Kit(codec) => codec.decode_control(frame),
            Self::Unavailable(_) => Err(ProtocolError::KitUnavailable),
        }
    }

    /// Decodes one bounded read-only market-data update when the kit is available.
    /// Returns `Ok(None)` for a schema-valid marker frame that carries no
    /// market content; the session layer skips those without an event.
    ///
    /// # Errors
    ///
    /// Returns a bounded protocol error or [`ProtocolError::KitUnavailable`].
    pub fn decode_market(
        &self,
        frame: &[u8],
    ) -> Result<Option<crate::DecodedMarketMessage>, ProtocolError> {
        match self {
            Self::Kit(codec) => codec.decode_market(frame),
            Self::Unavailable(_) => Err(ProtocolError::KitUnavailable),
        }
    }

    /// Decodes one bounded symbol-search or instrument-reference response.
    ///
    /// # Errors
    ///
    /// Returns a bounded protocol error or [`ProtocolError::KitUnavailable`].
    pub fn decode_catalog(
        &self,
        frame: &[u8],
    ) -> Result<crate::DecodedCatalogMessage, ProtocolError> {
        match self {
            Self::Kit(codec) => codec.decode_catalog(frame),
            Self::Unavailable(_) => Err(ProtocolError::KitUnavailable),
        }
    }

    /// Decodes one bounded live or replay bar response.
    ///
    /// # Errors
    ///
    /// Returns a bounded protocol error or [`ProtocolError::KitUnavailable`].
    pub fn decode_history(
        &self,
        frame: &[u8],
    ) -> Result<crate::DecodedHistoryMessage, ProtocolError> {
        match self {
            Self::Kit(codec) => codec.decode_history(frame),
            Self::Unavailable(_) => Err(ProtocolError::KitUnavailable),
        }
    }
}

/// Credentials and identity required for one read-only plant login.
#[derive(Clone, Copy)]
pub struct LoginRequest<'a> {
    pub user: &'a str,
    pub password: &'a str,
    pub app_name: &'a str,
    pub app_version: &'a str,
    pub system_name: &'a str,
    pub plant: ReadOnlyPlant,
}

impl fmt::Debug for LoginRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LoginRequest")
            .field("user", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .field("app_name", &self.app_name)
            .field("app_version", &self.app_version)
            .field("system_name", &self.system_name)
            .field("plant", &self.plant)
            .finish()
    }
}

/// Rithmic infrastructure plants used by the read-only terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadOnlyPlant {
    Ticker,
    History,
}

/// Complete market-data subscription request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MarketDataSubscription<'a> {
    pub symbol: &'a str,
    pub exchange: &'a str,
    pub action: SubscriptionAction,
    pub trades: bool,
    pub quotes: bool,
    pub order_book: bool,
}

/// Read-only depth-by-order subscription request.
///
/// Rithmic's DBO stream is distinct from the aggregate order-book bit carried
/// by [`MarketDataSubscription`]. The ticker plant acknowledges this request
/// before emitting order-level image/update messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DepthByOrderSubscription<'a> {
    pub symbol: &'a str,
    pub exchange: &'a str,
    pub action: SubscriptionAction,
}

/// Read-only covering depth-by-order snapshot request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DepthByOrderSnapshotRequest<'a> {
    pub symbol: &'a str,
    pub exchange: &'a str,
}

/// Whether a complete market-data selection is installed or removed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubscriptionAction {
    Subscribe,
    Unsubscribe,
}

/// Match behavior for a bounded provider symbol search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchPattern {
    Equals,
    Contains,
}

/// Optional provider instrument class used to narrow symbol search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstrumentType {
    Future,
    FutureOption,
    FutureStrategy,
    Equity,
    EquityOption,
    EquityStrategy,
    Index,
    IndexOption,
    Spread,
    Synthetic,
}

/// Bounded symbol search request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SymbolSearchRequest<'a> {
    pub search_text: &'a str,
    pub exchange: Option<&'a str>,
    pub product_code: Option<&'a str>,
    pub instrument_type: Option<InstrumentType>,
    pub pattern: SearchPattern,
}

/// Exact instrument metadata lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstrumentReferenceRequest<'a> {
    pub symbol: &'a str,
    pub exchange: &'a str,
}

/// Provider-supported time-bar family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeBarType {
    Second,
    Minute,
    Daily,
    Weekly,
}

/// Live time-bar subscription.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeBarSubscription<'a> {
    pub symbol: &'a str,
    pub exchange: &'a str,
    pub action: SubscriptionAction,
    pub bar_type: TimeBarType,
    pub period: i32,
}

/// Forward time-bar history request over an inclusive Unix-second range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeBarReplayRequest<'a> {
    pub symbol: &'a str,
    pub exchange: &'a str,
    pub bar_type: TimeBarType,
    pub period: i32,
    pub start_seconds: i32,
    pub finish_seconds: i32,
    pub maximum_bars: u16,
}

/// Live regular tick-bar subscription.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TickBarSubscription<'a> {
    pub symbol: &'a str,
    pub exchange: &'a str,
    pub action: SubscriptionAction,
    pub trades_per_bar: u16,
}

/// Forward regular tick-bar history request over an inclusive Unix-second range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TickBarReplayRequest<'a> {
    pub symbol: &'a str,
    pub exchange: &'a str,
    pub trades_per_bar: u16,
    pub start_seconds: i32,
    pub finish_seconds: i32,
    pub maximum_bars: u16,
}

/// Outbound requests accepted by the read-only protocol boundary.
#[derive(Clone, Copy)]
pub enum OutboundRequest<'a> {
    DiscoverSystems,
    Login(LoginRequest<'a>),
    Logout,
    Heartbeat,
    MarketData(MarketDataSubscription<'a>),
    DepthByOrder(DepthByOrderSubscription<'a>),
    DepthByOrderSnapshot(DepthByOrderSnapshotRequest<'a>),
    SearchSymbols(SymbolSearchRequest<'a>),
    InstrumentReference(InstrumentReferenceRequest<'a>),
    TimeBarUpdate(TimeBarSubscription<'a>),
    TimeBarReplay(TimeBarReplayRequest<'a>),
    TickBarUpdate(TickBarSubscription<'a>),
    TickBarReplay(TickBarReplayRequest<'a>),
}

impl fmt::Debug for OutboundRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DiscoverSystems => formatter.write_str("DiscoverSystems"),
            Self::Login(request) => request.fmt(formatter),
            Self::Logout => formatter.write_str("Logout"),
            Self::Heartbeat => formatter.write_str("Heartbeat"),
            Self::MarketData(_) => formatter.write_str("MarketData"),
            Self::DepthByOrder(_) => formatter.write_str("DepthByOrder"),
            Self::DepthByOrderSnapshot(_) => formatter.write_str("DepthByOrderSnapshot"),
            Self::SearchSymbols(_) => formatter.write_str("SearchSymbols"),
            Self::InstrumentReference(_) => formatter.write_str("InstrumentReference"),
            Self::TimeBarUpdate(_) => formatter.write_str("TimeBarUpdate"),
            Self::TimeBarReplay(_) => formatter.write_str("TimeBarReplay"),
            Self::TickBarUpdate(_) => formatter.write_str("TickBarUpdate"),
            Self::TickBarReplay(_) => formatter.write_str("TickBarReplay"),
        }
    }
}

/// Encoded binary WebSocket message that clears its bytes on drop.
pub struct SensitiveFrame(Vec<u8>);

impl SensitiveFrame {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl AsRef<[u8]> for SensitiveFrame {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl fmt::Debug for SensitiveFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SensitiveFrame")
            .field("length", &self.0.len())
            .finish()
    }
}

impl Drop for SensitiveFrame {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Sanitized control-plane message decoded from one provider WebSocket message.
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedControlMessage {
    Reject,
    ForcedLogout,
    Systems {
        accepted: bool,
        names: Vec<String>,
    },
    Login {
        accepted: bool,
        heartbeat_seconds: Option<f64>,
    },
    Logout {
        accepted: bool,
    },
    Heartbeat {
        accepted: bool,
        seconds: Option<i32>,
        microseconds: Option<i32>,
    },
    MarketDataSubscription {
        accepted: bool,
    },
    DepthByOrderSubscription {
        accepted: bool,
    },
}

/// Stateless R|Protocol message codec generated from the installed kit.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RithmicProtocolCodec;

impl RithmicProtocolCodec {
    /// Encodes one allowlisted read-only request as one binary WebSocket message.
    ///
    /// # Errors
    ///
    /// Returns a bounded validation, encoding, or kit-availability error.
    pub fn encode(self, request: OutboundRequest<'_>) -> Result<SensitiveFrame, ProtocolError> {
        encode_request(request)
    }

    /// Decodes one bounded control-plane WebSocket message.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized, malformed, unsupported, or unbounded input.
    pub fn decode_control(self, frame: &[u8]) -> Result<DecodedControlMessage, ProtocolError> {
        decode_control(frame)
    }

    /// Decodes one bounded read-only market-data WebSocket message.
    /// Returns `Ok(None)` for a schema-valid marker frame that carries no
    /// market content.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized, malformed, unsupported, or unbounded input.
    pub fn decode_market(
        self,
        frame: &[u8],
    ) -> Result<Option<crate::DecodedMarketMessage>, ProtocolError> {
        crate::market::decode(frame)
    }

    /// Decodes one bounded symbol-search or instrument-reference message.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized, malformed, unsupported, or unbounded input.
    pub fn decode_catalog(
        self,
        frame: &[u8],
    ) -> Result<crate::DecodedCatalogMessage, ProtocolError> {
        crate::catalog::decode(frame)
    }

    /// Decodes one bounded live or replay bar message.
    ///
    /// # Errors
    ///
    /// Returns an error for oversized, malformed, unsupported, or unbounded input.
    pub fn decode_history(
        self,
        frame: &[u8],
    ) -> Result<crate::DecodedHistoryMessage, ProtocolError> {
        crate::history::decode(frame)
    }
}

/// Bounded protocol or kit-availability failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    KitUnavailable,
    EmptyField(&'static str),
    FieldTooLong { field: &'static str, maximum: usize },
    ControlCharacter(&'static str),
    InvalidHeartbeat,
    UnsupportedSystem,
    TemplateVersionMismatch,
    EmptyMarketDataSelection,
    InvalidRange,
    InvalidPeriod,
    InvalidMaximumBars,
    FrameTooLarge { requested: usize, maximum: usize },
    Decode,
    MissingField(&'static str),
    InvalidNumber(&'static str),
    InvalidPresenceBits,
    InconsistentFields(&'static str),
    ParallelFieldLength(&'static str),
    UnknownEnum(&'static str),
    ResponseCodeShape,
    RejectedDataFrame,
    ForbiddenOutboundTemplate(i32),
    UnsupportedTemplate(i32),
    RepeatedFieldLimitExceeded { field: &'static str, maximum: usize },
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Rithmic protocol boundary failed: {self:?}")
    }
}

impl Error for ProtocolError {}

#[cfg(rithmic_kit)]
fn encode_request(request: OutboundRequest<'_>) -> Result<SensitiveFrame, ProtocolError> {
    kit::encode_request(request)
}

#[cfg(not(rithmic_kit))]
fn encode_request(_request: OutboundRequest<'_>) -> Result<SensitiveFrame, ProtocolError> {
    Err(ProtocolError::KitUnavailable)
}

#[cfg(rithmic_kit)]
fn decode_control(frame: &[u8]) -> Result<DecodedControlMessage, ProtocolError> {
    kit::decode_control(frame)
}

#[cfg(not(rithmic_kit))]
fn decode_control(_frame: &[u8]) -> Result<DecodedControlMessage, ProtocolError> {
    Err(ProtocolError::KitUnavailable)
}

#[cfg(rithmic_kit)]
mod kit {
    use super::{
        DecodedControlMessage, DepthByOrderSnapshotRequest, InstrumentReferenceRequest,
        InstrumentType, LoginRequest, MarketDataSubscription, OutboundRequest, ProtocolError,
        ReadOnlyPlant, SearchPattern, SensitiveFrame, SubscriptionAction, SymbolSearchRequest,
        TickBarReplayRequest, TickBarSubscription, TimeBarReplayRequest, TimeBarSubscription,
        TimeBarType,
    };
    use crate::generated::rti;
    use prost::Message;
    use zeroize::Zeroize;

    const MAX_FRAME_BYTES: usize = 1024 * 1024;
    const MAX_IDENTITY_BYTES: usize = 256;
    const MAX_TEMPLATE_VERSION_BYTES: usize = 32;
    const MAX_RESPONSE_CODES: usize = 8;
    const MAX_SYSTEMS: usize = 64;
    const MAX_HEARTBEAT_INTERVAL_SECONDS: f64 = 300.0;
    const TEST_SYSTEM: &str = "Rithmic Test";
    const TEMPLATE_VERSION: &str = env!("RITHMIC_TEMPLATE_VERSION");
    const SYSTEM_INFO_REQUEST: i32 = 16;
    const SYSTEM_INFO_RESPONSE: i32 = 17;
    const LOGIN_REQUEST: i32 = 10;
    const LOGIN_RESPONSE: i32 = 11;
    const LOGOUT_REQUEST: i32 = 12;
    const LOGOUT_RESPONSE: i32 = 13;
    const HEARTBEAT_REQUEST: i32 = 18;
    const HEARTBEAT_RESPONSE: i32 = 19;
    const MARKET_DATA_REQUEST: i32 = 100;
    const MARKET_DATA_RESPONSE: i32 = 101;
    const DEPTH_BY_ORDER_SNAPSHOT_REQUEST: i32 = 115;
    const DEPTH_BY_ORDER_UPDATES_REQUEST: i32 = 117;
    const DEPTH_BY_ORDER_UPDATES_RESPONSE: i32 = 118;
    const REFERENCE_DATA_REQUEST: i32 = 14;
    const SEARCH_SYMBOLS_REQUEST: i32 = 109;
    const TIME_BAR_UPDATE_REQUEST: i32 = 200;
    const TIME_BAR_REPLAY_REQUEST: i32 = 202;
    const TICK_BAR_UPDATE_REQUEST: i32 = 204;
    const TICK_BAR_REPLAY_REQUEST: i32 = 206;
    const REJECT: i32 = 75;
    const FORCED_LOGOUT: i32 = 77;
    pub(super) const READ_ONLY_OUTBOUND_TEMPLATES: &[i32] = &[
        LOGIN_REQUEST,
        LOGOUT_REQUEST,
        REFERENCE_DATA_REQUEST,
        SYSTEM_INFO_REQUEST,
        HEARTBEAT_REQUEST,
        MARKET_DATA_REQUEST,
        DEPTH_BY_ORDER_SNAPSHOT_REQUEST,
        DEPTH_BY_ORDER_UPDATES_REQUEST,
        SEARCH_SYMBOLS_REQUEST,
        TIME_BAR_UPDATE_REQUEST,
        TIME_BAR_REPLAY_REQUEST,
        TICK_BAR_UPDATE_REQUEST,
        TICK_BAR_REPLAY_REQUEST,
    ];

    pub(super) fn encode_request(
        request: OutboundRequest<'_>,
    ) -> Result<SensitiveFrame, ProtocolError> {
        let bytes = match request {
            OutboundRequest::DiscoverSystems => rti::RequestRithmicSystemInfo {
                template_id: SYSTEM_INFO_REQUEST,
                user_msg: Vec::new(),
            }
            .encode_to_vec(),
            OutboundRequest::Login(request) => encode_login(request)?,
            OutboundRequest::Logout => rti::RequestLogout {
                template_id: LOGOUT_REQUEST,
                user_msg: Vec::new(),
            }
            .encode_to_vec(),
            OutboundRequest::Heartbeat => rti::RequestHeartbeat {
                template_id: HEARTBEAT_REQUEST,
                user_msg: Vec::new(),
                ssboe: None,
                usecs: None,
            }
            .encode_to_vec(),
            OutboundRequest::MarketData(request) => encode_market_data(request)?,
            OutboundRequest::DepthByOrder(request) => encode_depth_by_order(request)?,
            OutboundRequest::DepthByOrderSnapshot(request) => {
                encode_depth_by_order_snapshot(request)?
            }
            OutboundRequest::SearchSymbols(request) => encode_symbol_search(request)?,
            OutboundRequest::InstrumentReference(request) => encode_instrument_reference(request)?,
            OutboundRequest::TimeBarUpdate(request) => encode_time_bar_update(request)?,
            OutboundRequest::TimeBarReplay(request) => encode_time_bar_replay(request)?,
            OutboundRequest::TickBarUpdate(request) => encode_tick_bar_update(request)?,
            OutboundRequest::TickBarReplay(request) => encode_tick_bar_replay(request)?,
        };
        bound_outbound_frame(bytes)
    }

    fn encode_login(request: LoginRequest<'_>) -> Result<Vec<u8>, ProtocolError> {
        validate_field("user", request.user)?;
        validate_field("password", request.password)?;
        validate_field("app_name", request.app_name)?;
        validate_field("app_version", request.app_version)?;
        validate_field("system_name", request.system_name)?;
        if request.system_name != TEST_SYSTEM {
            return Err(ProtocolError::UnsupportedSystem);
        }
        let (infra_type, aggregated_quotes) = match request.plant {
            ReadOnlyPlant::Ticker => (rti::request_login::SysInfraType::TickerPlant, Some(false)),
            ReadOnlyPlant::History => (rti::request_login::SysInfraType::HistoryPlant, None),
        };
        let mut message = rti::RequestLogin {
            template_id: LOGIN_REQUEST,
            template_version: Some(TEMPLATE_VERSION.to_string()),
            user_msg: Vec::new(),
            user: Some(request.user.to_string()),
            password: Some(request.password.to_string()),
            app_name: Some(request.app_name.to_string()),
            app_version: Some(request.app_version.to_string()),
            system_name: Some(request.system_name.to_string()),
            infra_type: Some(infra_type.into()),
            mac_addr: Vec::new(),
            os_version: None,
            os_platform: None,
            aggregated_quotes,
        };
        let encoded = message.encode_to_vec();
        message.user.as_mut().into_iter().for_each(Zeroize::zeroize);
        message
            .password
            .as_mut()
            .into_iter()
            .for_each(Zeroize::zeroize);
        Ok(encoded)
    }

    fn encode_market_data(request: MarketDataSubscription<'_>) -> Result<Vec<u8>, ProtocolError> {
        validate_field("symbol", request.symbol)?;
        validate_field("exchange", request.exchange)?;
        let update_bits = u32::from(request.trades)
            | (u32::from(request.quotes) << 1)
            | (u32::from(request.order_book) << 2);
        if update_bits == 0 {
            return Err(ProtocolError::EmptyMarketDataSelection);
        }
        let operation = match request.action {
            SubscriptionAction::Subscribe => rti::request_market_data_update::Request::Subscribe,
            SubscriptionAction::Unsubscribe => {
                rti::request_market_data_update::Request::Unsubscribe
            }
        };
        Ok(rti::RequestMarketDataUpdate {
            template_id: MARKET_DATA_REQUEST,
            user_msg: Vec::new(),
            symbol: Some(request.symbol.to_string()),
            exchange: Some(request.exchange.to_string()),
            request: Some(operation.into()),
            update_bits: Some(update_bits),
        }
        .encode_to_vec())
    }

    fn encode_depth_by_order(
        request: super::DepthByOrderSubscription<'_>,
    ) -> Result<Vec<u8>, ProtocolError> {
        validate_field("symbol", request.symbol)?;
        validate_field("exchange", request.exchange)?;
        let operation = match request.action {
            SubscriptionAction::Subscribe => {
                rti::request_depth_by_order_updates::Request::Subscribe
            }
            SubscriptionAction::Unsubscribe => {
                rti::request_depth_by_order_updates::Request::Unsubscribe
            }
        };
        Ok(rti::RequestDepthByOrderUpdates {
            template_id: DEPTH_BY_ORDER_UPDATES_REQUEST,
            user_msg: Vec::new(),
            request: Some(operation.into()),
            symbol: Some(request.symbol.to_string()),
            exchange: Some(request.exchange.to_string()),
            depth_price: None,
        }
        .encode_to_vec())
    }

    fn encode_depth_by_order_snapshot(
        request: DepthByOrderSnapshotRequest<'_>,
    ) -> Result<Vec<u8>, ProtocolError> {
        validate_field("symbol", request.symbol)?;
        validate_field("exchange", request.exchange)?;
        Ok(rti::RequestDepthByOrderSnapshot {
            template_id: DEPTH_BY_ORDER_SNAPSHOT_REQUEST,
            user_msg: Vec::new(),
            symbol: Some(request.symbol.to_string()),
            exchange: Some(request.exchange.to_string()),
            depth_price: None,
        }
        .encode_to_vec())
    }

    fn encode_symbol_search(request: SymbolSearchRequest<'_>) -> Result<Vec<u8>, ProtocolError> {
        validate_field("search_text", request.search_text)?;
        validate_optional_field("exchange", request.exchange)?;
        validate_optional_field("product_code", request.product_code)?;
        let pattern = match request.pattern {
            SearchPattern::Equals => rti::request_search_symbols::Pattern::Equals,
            SearchPattern::Contains => rti::request_search_symbols::Pattern::Contains,
        };
        let instrument_type = request.instrument_type.map(|instrument_type| {
            match instrument_type {
                InstrumentType::Future => rti::request_search_symbols::InstrumentType::Future,
                InstrumentType::FutureOption => {
                    rti::request_search_symbols::InstrumentType::FutureOption
                }
                InstrumentType::FutureStrategy => {
                    rti::request_search_symbols::InstrumentType::FutureStrategy
                }
                InstrumentType::Equity => rti::request_search_symbols::InstrumentType::Equity,
                InstrumentType::EquityOption => {
                    rti::request_search_symbols::InstrumentType::EquityOption
                }
                InstrumentType::EquityStrategy => {
                    rti::request_search_symbols::InstrumentType::EquityStrategy
                }
                InstrumentType::Index => rti::request_search_symbols::InstrumentType::Index,
                InstrumentType::IndexOption => {
                    rti::request_search_symbols::InstrumentType::IndexOption
                }
                InstrumentType::Spread => rti::request_search_symbols::InstrumentType::Spread,
                InstrumentType::Synthetic => rti::request_search_symbols::InstrumentType::Synthetic,
            }
            .into()
        });
        Ok(rti::RequestSearchSymbols {
            template_id: SEARCH_SYMBOLS_REQUEST,
            user_msg: Vec::new(),
            search_text: Some(request.search_text.to_string()),
            exchange: request.exchange.map(str::to_string),
            product_code: request.product_code.map(str::to_string),
            instrument_type,
            pattern: Some(pattern.into()),
        }
        .encode_to_vec())
    }

    fn encode_instrument_reference(
        request: InstrumentReferenceRequest<'_>,
    ) -> Result<Vec<u8>, ProtocolError> {
        validate_identity(request.symbol, request.exchange)?;
        Ok(rti::RequestReferenceData {
            template_id: REFERENCE_DATA_REQUEST,
            user_msg: Vec::new(),
            symbol: Some(request.symbol.to_string()),
            exchange: Some(request.exchange.to_string()),
        }
        .encode_to_vec())
    }

    fn encode_time_bar_update(request: TimeBarSubscription<'_>) -> Result<Vec<u8>, ProtocolError> {
        validate_identity(request.symbol, request.exchange)?;
        let (bar_type, period) = time_bar_fields(request.bar_type, request.period)?;
        Ok(rti::RequestTimeBarUpdate {
            template_id: TIME_BAR_UPDATE_REQUEST,
            user_msg: Vec::new(),
            symbol: Some(request.symbol.to_string()),
            exchange: Some(request.exchange.to_string()),
            request: Some(subscription_action(request.action).into()),
            bar_type: Some(bar_type),
            bar_type_period: Some(period),
        }
        .encode_to_vec())
    }

    fn encode_time_bar_replay(request: TimeBarReplayRequest<'_>) -> Result<Vec<u8>, ProtocolError> {
        validate_identity(request.symbol, request.exchange)?;
        validate_replay_range(
            request.start_seconds,
            request.finish_seconds,
            request.maximum_bars,
        )?;
        let (bar_type, period) = time_bar_fields(request.bar_type, request.period)?;
        Ok(rti::RequestTimeBarReplay {
            template_id: TIME_BAR_REPLAY_REQUEST,
            user_msg: Vec::new(),
            symbol: Some(request.symbol.to_string()),
            exchange: Some(request.exchange.to_string()),
            bar_type: Some(bar_type),
            bar_type_period: Some(period),
            start_index: Some(request.start_seconds),
            finish_index: Some(request.finish_seconds),
            user_max_count: Some(i32::from(request.maximum_bars)),
            direction: Some(rti::request_time_bar_replay::Direction::First.into()),
            time_order: Some(rti::request_time_bar_replay::TimeOrder::Forwards.into()),
            resume_bars: Some(false),
        }
        .encode_to_vec())
    }

    fn encode_tick_bar_update(request: TickBarSubscription<'_>) -> Result<Vec<u8>, ProtocolError> {
        validate_identity(request.symbol, request.exchange)?;
        let trades = validate_trades_per_bar(request.trades_per_bar)?;
        Ok(rti::RequestTickBarUpdate {
            template_id: TICK_BAR_UPDATE_REQUEST,
            user_msg: Vec::new(),
            symbol: Some(request.symbol.to_string()),
            exchange: Some(request.exchange.to_string()),
            request: Some(tick_subscription_action(request.action).into()),
            bar_type: Some(rti::request_tick_bar_update::BarType::TickBar.into()),
            bar_sub_type: Some(rti::request_tick_bar_update::BarSubType::Regular.into()),
            bar_type_specifier: Some(trades),
            custom_session_open_ssm: None,
            custom_session_close_ssm: None,
        }
        .encode_to_vec())
    }

    fn encode_tick_bar_replay(request: TickBarReplayRequest<'_>) -> Result<Vec<u8>, ProtocolError> {
        validate_identity(request.symbol, request.exchange)?;
        validate_replay_range(
            request.start_seconds,
            request.finish_seconds,
            request.maximum_bars,
        )?;
        let trades = validate_trades_per_bar(request.trades_per_bar)?;
        Ok(rti::RequestTickBarReplay {
            template_id: TICK_BAR_REPLAY_REQUEST,
            user_msg: Vec::new(),
            symbol: Some(request.symbol.to_string()),
            exchange: Some(request.exchange.to_string()),
            bar_type: Some(rti::request_tick_bar_replay::BarType::TickBar.into()),
            bar_sub_type: Some(rti::request_tick_bar_replay::BarSubType::Regular.into()),
            bar_type_specifier: Some(trades),
            start_index: Some(request.start_seconds),
            finish_index: Some(request.finish_seconds),
            user_max_count: Some(i32::from(request.maximum_bars)),
            custom_session_open_ssm: None,
            custom_session_close_ssm: None,
            direction: Some(rti::request_tick_bar_replay::Direction::First.into()),
            time_order: Some(rti::request_tick_bar_replay::TimeOrder::Forwards.into()),
            resume_bars: Some(false),
        }
        .encode_to_vec())
    }

    fn validate_identity(symbol: &str, exchange: &str) -> Result<(), ProtocolError> {
        validate_field("symbol", symbol)?;
        validate_field("exchange", exchange)
    }

    fn validate_optional_field(
        field: &'static str,
        value: Option<&str>,
    ) -> Result<(), ProtocolError> {
        value.map_or(Ok(()), |value| validate_field(field, value))
    }

    fn subscription_action(action: SubscriptionAction) -> rti::request_time_bar_update::Request {
        match action {
            SubscriptionAction::Subscribe => rti::request_time_bar_update::Request::Subscribe,
            SubscriptionAction::Unsubscribe => rti::request_time_bar_update::Request::Unsubscribe,
        }
    }

    fn tick_subscription_action(
        action: SubscriptionAction,
    ) -> rti::request_tick_bar_update::Request {
        match action {
            SubscriptionAction::Subscribe => rti::request_tick_bar_update::Request::Subscribe,
            SubscriptionAction::Unsubscribe => rti::request_tick_bar_update::Request::Unsubscribe,
        }
    }

    fn time_bar_fields(bar_type: TimeBarType, period: i32) -> Result<(i32, i32), ProtocolError> {
        let maximum = match bar_type {
            TimeBarType::Second => 86_400,
            TimeBarType::Minute => 1_440,
            TimeBarType::Daily | TimeBarType::Weekly => 365,
        };
        if !(1..=maximum).contains(&period) {
            return Err(ProtocolError::InvalidPeriod);
        }
        let bar_type = match bar_type {
            TimeBarType::Second => rti::request_time_bar_update::BarType::SecondBar,
            TimeBarType::Minute => rti::request_time_bar_update::BarType::MinuteBar,
            TimeBarType::Daily => rti::request_time_bar_update::BarType::DailyBar,
            TimeBarType::Weekly => rti::request_time_bar_update::BarType::WeeklyBar,
        };
        Ok((bar_type.into(), period))
    }

    fn validate_replay_range(
        start_seconds: i32,
        finish_seconds: i32,
        maximum_bars: u16,
    ) -> Result<(), ProtocolError> {
        if start_seconds < 0 || start_seconds > finish_seconds {
            return Err(ProtocolError::InvalidRange);
        }
        if maximum_bars == 0 || maximum_bars > 10_000 {
            return Err(ProtocolError::InvalidMaximumBars);
        }
        Ok(())
    }

    fn validate_trades_per_bar(trades_per_bar: u16) -> Result<String, ProtocolError> {
        if trades_per_bar == 0 || trades_per_bar > 10_000 {
            return Err(ProtocolError::InvalidPeriod);
        }
        Ok(trades_per_bar.to_string())
    }

    fn bound_outbound_frame(bytes: Vec<u8>) -> Result<SensitiveFrame, ProtocolError> {
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge {
                requested: bytes.len(),
                maximum: MAX_FRAME_BYTES,
            });
        }
        let template = rti::MessageType::decode(bytes.as_slice())
            .map_err(|_| ProtocolError::Decode)?
            .template_id;
        if !READ_ONLY_OUTBOUND_TEMPLATES.contains(&template) {
            return Err(ProtocolError::ForbiddenOutboundTemplate(template));
        }
        Ok(SensitiveFrame(bytes))
    }

    pub(super) fn decode_control(frame: &[u8]) -> Result<DecodedControlMessage, ProtocolError> {
        if frame.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::FrameTooLarge {
                requested: frame.len(),
                maximum: MAX_FRAME_BYTES,
            });
        }
        let message_type = rti::MessageType::decode(frame).map_err(|_| ProtocolError::Decode)?;
        match message_type.template_id {
            REJECT => {
                let response = rti::Reject::decode(frame).map_err(|_| ProtocolError::Decode)?;
                validate_response_fields(&response.user_msg, &response.rp_code)?;
                Ok(DecodedControlMessage::Reject)
            }
            FORCED_LOGOUT => {
                rti::ForcedLogout::decode(frame).map_err(|_| ProtocolError::Decode)?;
                Ok(DecodedControlMessage::ForcedLogout)
            }
            SYSTEM_INFO_RESPONSE => decode_systems(frame),
            LOGIN_RESPONSE => decode_login(frame),
            LOGOUT_RESPONSE => {
                let response =
                    rti::ResponseLogout::decode(frame).map_err(|_| ProtocolError::Decode)?;
                validate_response_fields(&response.user_msg, &response.rp_code)?;
                Ok(DecodedControlMessage::Logout {
                    accepted: accepted(&response.rp_code),
                })
            }
            HEARTBEAT_RESPONSE => {
                let response =
                    rti::ResponseHeartbeat::decode(frame).map_err(|_| ProtocolError::Decode)?;
                validate_response_fields(&response.user_msg, &response.rp_code)?;
                validate_heartbeat(response.ssboe, response.usecs)?;
                Ok(DecodedControlMessage::Heartbeat {
                    accepted: accepted(&response.rp_code),
                    seconds: response.ssboe,
                    microseconds: response.usecs,
                })
            }
            MARKET_DATA_RESPONSE => {
                let response = rti::ResponseMarketDataUpdate::decode(frame)
                    .map_err(|_| ProtocolError::Decode)?;
                validate_response_fields(&response.user_msg, &response.rp_code)?;
                Ok(DecodedControlMessage::MarketDataSubscription {
                    accepted: accepted(&response.rp_code),
                })
            }
            DEPTH_BY_ORDER_UPDATES_RESPONSE => {
                let response = rti::ResponseDepthByOrderUpdates::decode(frame)
                    .map_err(|_| ProtocolError::Decode)?;
                validate_response_fields(&response.user_msg, &response.rp_code)?;
                Ok(DecodedControlMessage::DepthByOrderSubscription {
                    accepted: accepted(&response.rp_code),
                })
            }
            template => Err(ProtocolError::UnsupportedTemplate(template)),
        }
    }

    fn decode_systems(frame: &[u8]) -> Result<DecodedControlMessage, ProtocolError> {
        let response =
            rti::ResponseRithmicSystemInfo::decode(frame).map_err(|_| ProtocolError::Decode)?;
        validate_response_fields(&response.user_msg, &response.rp_code)?;
        bound_repeated(
            "system_info.system_name",
            response.system_name.len(),
            MAX_SYSTEMS,
        )?;
        bound_repeated(
            "system_info.has_aggregated_quotes",
            response.has_aggregated_quotes.len(),
            MAX_SYSTEMS,
        )?;
        for name in &response.system_name {
            validate_field("system_name", name)?;
        }
        Ok(DecodedControlMessage::Systems {
            accepted: accepted(&response.rp_code),
            names: response.system_name,
        })
    }

    fn decode_login(frame: &[u8]) -> Result<DecodedControlMessage, ProtocolError> {
        let response = rti::ResponseLogin::decode(frame).map_err(|_| ProtocolError::Decode)?;
        validate_response_fields(&response.user_msg, &response.rp_code)?;
        if accepted(&response.rp_code)
            && !response
                .template_version
                .as_deref()
                .is_some_and(valid_template_version)
        {
            return Err(ProtocolError::TemplateVersionMismatch);
        }
        let heartbeat_seconds = response.heartbeat_interval;
        if accepted(&response.rp_code)
            && heartbeat_seconds.is_none_or(|value| {
                !value.is_finite() || value <= 0.0 || value > MAX_HEARTBEAT_INTERVAL_SECONDS
            })
        {
            return Err(ProtocolError::InvalidHeartbeat);
        }
        Ok(DecodedControlMessage::Login {
            accepted: accepted(&response.rp_code),
            heartbeat_seconds,
        })
    }

    fn valid_template_version(version: &str) -> bool {
        !version.is_empty()
            && version.len() <= MAX_TEMPLATE_VERSION_BYTES
            && version
                .chars()
                .all(|character| character.is_ascii_digit() || character == '.')
            && version.chars().any(|character| character.is_ascii_digit())
    }

    fn validate_response_fields(
        user_messages: &[String],
        codes: &[String],
    ) -> Result<(), ProtocolError> {
        bound_repeated("user_msg", user_messages.len(), 2)?;
        for message in user_messages {
            validate_field("user_msg", message)?;
        }
        if codes == ["0"] {
            return Ok(());
        }
        let Some((code, details)) = codes.split_first() else {
            return Err(ProtocolError::ResponseCodeShape);
        };
        if codes.len() > MAX_RESPONSE_CODES || !code.parse::<u32>().is_ok_and(|value| value > 0) {
            return Err(ProtocolError::ResponseCodeShape);
        }
        validate_field("rp_code", code)?;
        for detail in details {
            validate_field("rp_code_detail", detail)?;
        }
        Ok(())
    }

    fn accepted(codes: &[String]) -> bool {
        codes.len() == 1 && codes[0] == "0"
    }

    fn validate_heartbeat(
        seconds: Option<i32>,
        microseconds: Option<i32>,
    ) -> Result<(), ProtocolError> {
        if seconds.is_some_and(|value| value < 0)
            || microseconds.is_some_and(|value| !(0..1_000_000).contains(&value))
            || seconds.is_some() != microseconds.is_some()
        {
            return Err(ProtocolError::InvalidHeartbeat);
        }
        Ok(())
    }

    fn validate_field(field: &'static str, value: &str) -> Result<(), ProtocolError> {
        if value.is_empty() {
            return Err(ProtocolError::EmptyField(field));
        }
        if value.len() > MAX_IDENTITY_BYTES {
            return Err(ProtocolError::FieldTooLong {
                field,
                maximum: MAX_IDENTITY_BYTES,
            });
        }
        if value.chars().any(char::is_control) {
            return Err(ProtocolError::ControlCharacter(field));
        }
        Ok(())
    }

    fn bound_repeated(
        field: &'static str,
        length: usize,
        maximum: usize,
    ) -> Result<(), ProtocolError> {
        if length > maximum {
            return Err(ProtocolError::RepeatedFieldLimitExceeded { field, maximum });
        }
        Ok(())
    }
}

#[cfg(all(test, rithmic_kit))]
mod tests {
    use super::*;
    use crate::{RITHMIC_APPLICATION_NAME, generated::rti};
    use prost::Message;

    fn template_id(frame: &SensitiveFrame) -> i32 {
        rti::MessageType::decode(frame.as_bytes())
            .expect("allowlisted frame has a template ID")
            .template_id
    }

    #[test]
    fn outbound_surface_emits_only_read_only_templates() {
        let codec = RithmicProtocolCodec;
        let requests = [
            OutboundRequest::DiscoverSystems,
            OutboundRequest::Login(LoginRequest {
                user: "fixture-user",
                password: "fixture-password",
                app_name: RITHMIC_APPLICATION_NAME,
                app_version: "0.1.0",
                system_name: "Rithmic Test",
                plant: ReadOnlyPlant::Ticker,
            }),
            OutboundRequest::Logout,
            OutboundRequest::Heartbeat,
            OutboundRequest::MarketData(MarketDataSubscription {
                symbol: "ESM7",
                exchange: "CME",
                action: SubscriptionAction::Subscribe,
                trades: true,
                quotes: true,
                order_book: true,
            }),
            OutboundRequest::DepthByOrder(DepthByOrderSubscription {
                symbol: "ESM7",
                exchange: "CME",
                action: SubscriptionAction::Subscribe,
            }),
            OutboundRequest::DepthByOrderSnapshot(DepthByOrderSnapshotRequest {
                symbol: "ESM7",
                exchange: "CME",
            }),
            OutboundRequest::SearchSymbols(SymbolSearchRequest {
                search_text: "ES",
                exchange: Some("CME"),
                product_code: None,
                instrument_type: Some(InstrumentType::Future),
                pattern: SearchPattern::Contains,
            }),
            OutboundRequest::InstrumentReference(InstrumentReferenceRequest {
                symbol: "ESM7",
                exchange: "CME",
            }),
            OutboundRequest::TimeBarUpdate(TimeBarSubscription {
                symbol: "ESM7",
                exchange: "CME",
                action: SubscriptionAction::Subscribe,
                bar_type: TimeBarType::Minute,
                period: 1,
            }),
            OutboundRequest::TimeBarReplay(TimeBarReplayRequest {
                symbol: "ESM7",
                exchange: "CME",
                bar_type: TimeBarType::Minute,
                period: 1,
                start_seconds: 1_800_000_000,
                finish_seconds: 1_800_003_600,
                maximum_bars: 100,
            }),
            OutboundRequest::TickBarUpdate(TickBarSubscription {
                symbol: "ESM7",
                exchange: "CME",
                action: SubscriptionAction::Subscribe,
                trades_per_bar: 100,
            }),
            OutboundRequest::TickBarReplay(TickBarReplayRequest {
                symbol: "ESM7",
                exchange: "CME",
                trades_per_bar: 100,
                start_seconds: 1_800_000_000,
                finish_seconds: 1_800_003_600,
                maximum_bars: 100,
            }),
        ];
        let emitted = requests
            .into_iter()
            .map(|request| {
                let frame = codec.encode(request).expect("request encodes");
                template_id(&frame)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            emitted,
            [16, 10, 12, 18, 100, 117, 115, 109, 14, 200, 202, 204, 206]
        );
        assert_eq!(
            emitted.len(),
            kit::READ_ONLY_OUTBOUND_TEMPLATES.len(),
            "the test must enumerate every public outbound request variant"
        );
        assert!(emitted.iter().all(|template| *template < 300));
        assert!(!kit::READ_ONLY_OUTBOUND_TEMPLATES.contains(&312));
        assert!(!kit::READ_ONLY_OUTBOUND_TEMPLATES.contains(&3512));
    }

    #[test]
    fn control_responses_decode_to_bounded_sanitized_messages() {
        let codec = RithmicProtocolCodec;
        let systems = rti::ResponseRithmicSystemInfo {
            template_id: 17,
            user_msg: Vec::new(),
            rp_code: vec!["0".to_string()],
            system_name: vec!["Rithmic Test".to_string()],
            has_aggregated_quotes: vec![false],
        }
        .encode_to_vec();
        assert_eq!(
            codec.decode_control(&systems).expect("systems decode"),
            DecodedControlMessage::Systems {
                accepted: true,
                names: vec!["Rithmic Test".to_string()],
            }
        );

        let reject = rti::Reject {
            template_id: 75,
            user_msg: vec!["provider detail".to_string()],
            rp_code: vec!["1".to_string(), "rejected".to_string()],
        }
        .encode_to_vec();
        assert_eq!(
            codec.decode_control(&reject).expect("reject decodes"),
            DecodedControlMessage::Reject
        );

        let forced_logout = rti::ForcedLogout { template_id: 77 }.encode_to_vec();
        assert_eq!(
            codec
                .decode_control(&forced_logout)
                .expect("forced logout decodes"),
            DecodedControlMessage::ForcedLogout
        );

        let malformed_codes = rti::ResponseRithmicSystemInfo {
            template_id: 17,
            user_msg: Vec::new(),
            rp_code: Vec::new(),
            system_name: Vec::new(),
            has_aggregated_quotes: Vec::new(),
        }
        .encode_to_vec();
        assert!(matches!(
            codec.decode_control(&malformed_codes),
            Err(ProtocolError::ResponseCodeShape)
        ));

        let rejected_subscription = rti::ResponseMarketDataUpdate {
            template_id: 101,
            user_msg: Vec::new(),
            rp_code: vec!["1".to_string()],
        }
        .encode_to_vec();
        assert_eq!(
            codec
                .decode_control(&rejected_subscription)
                .expect("single-code rejection decodes"),
            DecodedControlMessage::MarketDataSubscription { accepted: false }
        );

        let accepted_depth = rti::ResponseDepthByOrderUpdates {
            template_id: 118,
            user_msg: Vec::new(),
            rp_code: vec!["0".to_string()],
        }
        .encode_to_vec();
        assert_eq!(
            codec
                .decode_control(&accepted_depth)
                .expect("DBO subscription acknowledgement decodes"),
            DecodedControlMessage::DepthByOrderSubscription { accepted: true }
        );

        let login = rti::ResponseLogin {
            template_id: 11,
            template_version: Some(env!("RITHMIC_TEMPLATE_VERSION").to_string()),
            user_msg: Vec::new(),
            rp_code: vec!["0".to_string()],
            fcm_id: None,
            ib_id: None,
            country_code: None,
            state_code: None,
            unique_user_id: None,
            heartbeat_interval: Some(10.0),
        }
        .encode_to_vec();
        assert_eq!(
            codec.decode_control(&login).expect("login decodes"),
            DecodedControlMessage::Login {
                accepted: true,
                heartbeat_seconds: Some(10.0),
            }
        );
    }

    #[test]
    fn bounds_and_redaction_fail_closed() {
        let codec = RithmicProtocolCodec;
        let login = LoginRequest {
            user: "fixture-user",
            password: "fixture-password",
            app_name: RITHMIC_APPLICATION_NAME,
            app_version: "0.1.0",
            system_name: "Rithmic Test",
            plant: ReadOnlyPlant::History,
        };
        let debug = format!("{:?}", OutboundRequest::Login(login));
        assert!(!debug.contains("fixture-user"));
        assert!(!debug.contains("fixture-password"));

        assert!(matches!(
            codec.encode(OutboundRequest::MarketData(MarketDataSubscription {
                symbol: "ESM7",
                exchange: "CME",
                action: SubscriptionAction::Subscribe,
                trades: false,
                quotes: false,
                order_book: false,
            })),
            Err(ProtocolError::EmptyMarketDataSelection)
        ));

        let oversized = vec![0_u8; 1024 * 1024 + 1];
        assert!(matches!(
            codec.decode_control(&oversized),
            Err(ProtocolError::FrameTooLarge { .. })
        ));

        for unsupported in [
            "Rithmic Paper Trading",
            "Rithmic 01",
            "rithmic test",
            "Rithmic Test ",
        ] {
            let request = LoginRequest {
                system_name: unsupported,
                ..login
            };
            assert!(matches!(
                codec.encode(OutboundRequest::Login(request)),
                Err(ProtocolError::UnsupportedSystem)
            ));
        }

        assert!(matches!(
            codec.encode(OutboundRequest::TimeBarReplay(TimeBarReplayRequest {
                symbol: "ESM7",
                exchange: "CME",
                bar_type: TimeBarType::Minute,
                period: 1,
                start_seconds: 20,
                finish_seconds: 10,
                maximum_bars: 100,
            })),
            Err(ProtocolError::InvalidRange)
        ));
    }

    #[test]
    fn login_wire_uses_installed_version_and_plant_specific_fields() {
        let codec = RithmicProtocolCodec;
        for (plant, expected_infra, expected_aggregated_quotes) in [
            (
                ReadOnlyPlant::Ticker,
                rti::request_login::SysInfraType::TickerPlant,
                Some(false),
            ),
            (
                ReadOnlyPlant::History,
                rti::request_login::SysInfraType::HistoryPlant,
                None,
            ),
        ] {
            let frame = codec
                .encode(OutboundRequest::Login(LoginRequest {
                    user: "fixture-user",
                    password: "fixture-password",
                    app_name: RITHMIC_APPLICATION_NAME,
                    app_version: "0.1.0",
                    system_name: "Rithmic Test",
                    plant,
                }))
                .expect("login encodes");
            let wire = rti::RequestLogin::decode(frame.as_bytes()).expect("login wire decodes");
            assert_eq!(
                wire.template_version.as_deref(),
                Some(env!("RITHMIC_TEMPLATE_VERSION"))
            );
            assert_eq!(wire.system_name.as_deref(), Some("Rithmic Test"));
            assert_eq!(wire.infra_type, Some(expected_infra.into()));
            assert_eq!(wire.aggregated_quotes, expected_aggregated_quotes);
        }
    }

    #[test]
    fn successful_login_requires_current_version_and_bounded_heartbeat() {
        let codec = RithmicProtocolCodec;
        let response = |template_version: Option<&str>, heartbeat_interval| {
            rti::ResponseLogin {
                template_id: 11,
                template_version: template_version.map(str::to_string),
                user_msg: Vec::new(),
                rp_code: vec!["0".to_string()],
                fcm_id: None,
                ib_id: None,
                country_code: None,
                state_code: None,
                unique_user_id: None,
                heartbeat_interval,
            }
            .encode_to_vec()
        };
        assert!(matches!(
            codec.decode_control(&response(Some("broken"), Some(10.0))),
            Err(ProtocolError::TemplateVersionMismatch)
        ));
        assert!(matches!(
            codec.decode_control(&response(None, Some(10.0))),
            Err(ProtocolError::TemplateVersionMismatch)
        ));
        assert!(matches!(
            codec.decode_control(&response(
                Some(env!("RITHMIC_SCHEMA_TEMPLATE_VERSION")),
                Some(10.0)
            )),
            Ok(DecodedControlMessage::Login { accepted: true, .. })
        ));
        assert!(matches!(
            codec.decode_control(&response(
                Some(env!("RITHMIC_TEMPLATE_VERSION")),
                Some(301.0)
            )),
            Err(ProtocolError::InvalidHeartbeat)
        ));
    }
}

#[cfg(all(test, not(rithmic_kit)))]
mod kitless_tests {
    use super::*;

    #[test]
    fn detected_backend_fails_closed_without_the_kit() {
        let backend = RithmicProtocolBackend::detected();
        assert!(!backend.is_available());
        assert!(matches!(
            backend.encode(OutboundRequest::DiscoverSystems),
            Err(ProtocolError::KitUnavailable)
        ));
        assert!(matches!(
            backend.decode_control(&[]),
            Err(ProtocolError::KitUnavailable)
        ));
    }
}
