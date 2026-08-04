use crate::ProviderHistoryError;
use std::{
    collections::BTreeSet,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
};

const MAXIMUM_IDENTITY_BYTES: usize = 192;
const MAXIMUM_CURSOR_BYTES: usize = 512;
const MAXIMUM_ITEM_BYTES: usize = 1_048_576;

/// Provider history dataset class.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DataClass {
    Bars,
    Ticks,
    Depth,
}

/// Pagination mechanism implemented by one provider dataset adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PaginationStyle {
    None,
    OpaqueCursor,
    EndTime,
}

/// Deterministic request-rate window used before provider dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RateLimit {
    pub requests: NonZeroU32,
    pub window_nanos: NonZeroU64,
    pub maximum_inflight: NonZeroUsize,
}

/// Capability for one bars/ticks/depth dataset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DatasetCapability {
    Unsupported {
        reason: String,
    },
    Supported {
        resolutions: BTreeSet<String>,
        maximum_lookback_nanos: NonZeroU64,
        maximum_request_span_nanos: NonZeroU64,
        maximum_page_items: NonZeroUsize,
        pagination: PaginationStyle,
        rate_limit: RateLimit,
    },
}

impl DatasetCapability {
    /// Constructs a validated supported capability.
    ///
    /// # Errors
    ///
    /// Returns an error when resolutions are empty or contain invalid values.
    pub fn supported(
        resolutions: impl IntoIterator<Item = String>,
        maximum_lookback_nanos: NonZeroU64,
        maximum_request_span_nanos: NonZeroU64,
        maximum_page_items: NonZeroUsize,
        pagination: PaginationStyle,
        rate_limit: RateLimit,
    ) -> Result<Self, ProviderHistoryError> {
        let resolutions = resolutions.into_iter().collect::<BTreeSet<_>>();
        if resolutions.is_empty() {
            return Err(ProviderHistoryError::InvalidConfiguration(
                "supported dataset has no resolutions",
            ));
        }
        for resolution in &resolutions {
            validate_identifier("resolution", resolution)?;
        }
        Ok(Self::Supported {
            resolutions,
            maximum_lookback_nanos,
            maximum_request_span_nanos,
            maximum_page_items,
            pagination,
            rate_limit,
        })
    }

    #[must_use]
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self::Unsupported {
            reason: reason.into(),
        }
    }
}

/// Complete implemented history matrix for one provider adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryCapabilities {
    provider_id: String,
    bars: DatasetCapability,
    ticks: DatasetCapability,
    depth: DatasetCapability,
}

impl HistoryCapabilities {
    /// Creates one provider matrix.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid provider identity or unsupported reason.
    pub fn try_new(
        provider_id: String,
        bars: DatasetCapability,
        ticks: DatasetCapability,
        depth: DatasetCapability,
    ) -> Result<Self, ProviderHistoryError> {
        validate_identifier("provider_id", &provider_id)?;
        for capability in [&bars, &ticks, &depth] {
            if let DatasetCapability::Unsupported { reason } = capability {
                validate_identifier("unsupported_reason", reason)?;
            }
        }
        Ok(Self {
            provider_id,
            bars,
            ticks,
            depth,
        })
    }

    #[must_use]
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    #[must_use]
    pub const fn dataset(&self, data_class: DataClass) -> &DatasetCapability {
        match data_class {
            DataClass::Bars => &self.bars,
            DataClass::Ticks => &self.ticks,
            DataClass::Depth => &self.depth,
        }
    }

    pub(crate) fn validate_request(
        &self,
        request: &HistoryPageRequest,
        now_unix_nanos: i64,
    ) -> Result<(), ProviderHistoryError> {
        request.validate_identity()?;
        if request.provider_id != self.provider_id {
            return Err(ProviderHistoryError::InvalidIdentity("provider_id"));
        }
        let DatasetCapability::Supported {
            resolutions,
            maximum_lookback_nanos,
            maximum_request_span_nanos,
            maximum_page_items,
            pagination,
            ..
        } = self.dataset(request.data_class)
        else {
            return Err(ProviderHistoryError::UnsupportedDataClass);
        };
        if !resolutions.contains(&request.resolution) {
            return Err(ProviderHistoryError::UnsupportedResolution);
        }
        let span = request.range.span_nanos()?;
        if span > maximum_request_span_nanos.get() {
            return Err(ProviderHistoryError::RequestSpanExceeded);
        }
        let lookback = now_unix_nanos
            .checked_sub(request.range.start_unix_nanos)
            .and_then(|value| u64::try_from(value).ok());
        if request.range.end_unix_nanos > now_unix_nanos
            || lookback.is_none_or(|value| value > maximum_lookback_nanos.get())
        {
            return Err(ProviderHistoryError::LookbackExceeded);
        }
        if request.maximum_items.get() > maximum_page_items.get() {
            return Err(ProviderHistoryError::PageLimitExceeded);
        }
        validate_continuation(request.continuation.as_ref(), *pagination, request.range)
    }
}

/// Half-open requested provider time range.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HistoryRange {
    pub start_unix_nanos: i64,
    pub end_unix_nanos: i64,
}

impl HistoryRange {
    pub(crate) fn span_nanos(self) -> Result<u64, ProviderHistoryError> {
        let span = self
            .end_unix_nanos
            .checked_sub(self.start_unix_nanos)
            .ok_or(ProviderHistoryError::InvalidIdentity("time_range"))?;
        let span =
            u64::try_from(span).map_err(|_| ProviderHistoryError::InvalidIdentity("time_range"))?;
        if span == 0 {
            return Err(ProviderHistoryError::InvalidIdentity("time_range"));
        }
        Ok(span)
    }
}

/// Provider pagination token.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Continuation {
    Cursor(String),
    EndBeforeUnixNanos(i64),
}

/// One exact page request; equality is the scheduler deduplication identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HistoryPageRequest {
    pub provider_id: String,
    pub account_id: String,
    pub entitlement_revision: String,
    pub instrument_id: String,
    pub data_class: DataClass,
    pub resolution: String,
    pub range: HistoryRange,
    pub maximum_items: NonZeroUsize,
    pub continuation: Option<Continuation>,
}

impl HistoryPageRequest {
    fn validate_identity(&self) -> Result<(), ProviderHistoryError> {
        validate_identifier("provider_id", &self.provider_id)?;
        validate_identifier("account_id", &self.account_id)?;
        validate_identifier("entitlement_revision", &self.entitlement_revision)?;
        validate_identifier("instrument_id", &self.instrument_id)?;
        validate_identifier("resolution", &self.resolution)?;
        self.range.span_nanos().map(|_| ())
    }
}

/// One bounded provider history item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryItem {
    pub sequence: u64,
    pub event_time_unix_nanos: i64,
    pub payload: Vec<u8>,
}

impl HistoryItem {
    pub(crate) fn validate(&self) -> Result<(), ProviderHistoryError> {
        if self.sequence == 0 {
            return Err(ProviderHistoryError::InvalidPage("zero item sequence"));
        }
        if self.payload.len() > MAXIMUM_ITEM_BYTES {
            return Err(ProviderHistoryError::InvalidPage(
                "item payload exceeds bound",
            ));
        }
        Ok(())
    }
}

/// One provider response page and its optional next-page token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryPage {
    pub request: HistoryPageRequest,
    pub items: Vec<HistoryItem>,
    pub next: Option<Continuation>,
}

/// Port implemented by provider-specific history adapters.
pub trait ProviderHistoryAdapter {
    fn capabilities(&self) -> &HistoryCapabilities;

    /// Fetches one exact bounded page.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific transport, protocol, or entitlement failure.
    fn fetch_page(&mut self, request: &HistoryPageRequest) -> Result<HistoryPage, String>;
}

fn validate_continuation(
    continuation: Option<&Continuation>,
    pagination: PaginationStyle,
    range: HistoryRange,
) -> Result<(), ProviderHistoryError> {
    match (continuation, pagination) {
        (None, _)
        | (Some(Continuation::Cursor(_)), PaginationStyle::OpaqueCursor)
        | (Some(Continuation::EndBeforeUnixNanos(_)), PaginationStyle::EndTime) => {}
        _ => return Err(ProviderHistoryError::InvalidContinuation),
    }
    if let Some(Continuation::Cursor(cursor)) = continuation
        && (cursor.is_empty()
            || cursor.len() > MAXIMUM_CURSOR_BYTES
            || cursor.chars().any(char::is_control))
    {
        return Err(ProviderHistoryError::InvalidContinuation);
    }
    if let Some(Continuation::EndBeforeUnixNanos(end)) = continuation
        && (*end <= range.start_unix_nanos || *end >= range.end_unix_nanos)
    {
        return Err(ProviderHistoryError::InvalidContinuation);
    }
    Ok(())
}

pub(crate) fn page_capability(
    capabilities: &HistoryCapabilities,
    request: &HistoryPageRequest,
) -> Result<(PaginationStyle, usize), ProviderHistoryError> {
    let DatasetCapability::Supported {
        pagination,
        maximum_page_items,
        ..
    } = capabilities.dataset(request.data_class)
    else {
        return Err(ProviderHistoryError::UnsupportedDataClass);
    };
    Ok((*pagination, maximum_page_items.get()))
}

pub(crate) fn rate_limit(
    capabilities: &HistoryCapabilities,
    data_class: DataClass,
) -> Result<RateLimit, ProviderHistoryError> {
    let DatasetCapability::Supported { rate_limit, .. } = capabilities.dataset(data_class) else {
        return Err(ProviderHistoryError::UnsupportedDataClass);
    };
    Ok(*rate_limit)
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), ProviderHistoryError> {
    if value.is_empty()
        || value.len() > MAXIMUM_IDENTITY_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(ProviderHistoryError::InvalidIdentity(field));
    }
    Ok(())
}
