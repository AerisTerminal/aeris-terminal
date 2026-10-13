use crate::ProviderHistoryError;
use std::{
    collections::BTreeSet,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
};

const MAXIMUM_IDENTITY_BYTES: usize = 192;

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
}

/// Half-open requested provider time range.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HistoryRange {
    pub start_unix_nanos: i64,
    pub end_unix_nanos: i64,
}

/// Provider pagination token.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Continuation {
    Cursor(String),
    EndBeforeUnixNanos(i64),
}

/// One exact page request to a provider adapter.
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

/// One bounded provider history item.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryItem {
    pub sequence: u64,
    pub event_time_unix_nanos: i64,
    pub payload: Vec<u8>,
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
