use super::{MAXIMUM_IDENTITY_BYTES, MAXIMUM_INSTRUMENTS, valid_identity};
use crate::{
    CollectedProducts, CollectedSymbols, DepthByOrderSnapshotRequest, DepthByOrderSubscription,
    InstrumentDescriptor, InstrumentType, MarketDataSubscription, MarketIdentity, SearchPattern,
    SessionGeneration, SubscriptionAction, SymbolSearchCollectionRequest, SymbolSearchRequest,
};
use std::{collections::BTreeSet, error::Error, fmt, num::NonZeroUsize};

/// Explicit provider-to-canonical registry entry and data selection.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // Four independent provider stream subscriptions.
pub struct RithmicProviderInstrument {
    pub descriptor: InstrumentDescriptor,
    pub entitlement_id: String,
    pub trades: bool,
    pub quotes: bool,
    /// Aggregate price-level depth.
    pub order_book: bool,
    /// Every resting order (market by order), subscribed only on demand.
    pub order_levels: bool,
}

/// Bounded, owned symbol-search intent for one session-unique search generation.
#[derive(Clone, Eq, PartialEq)]
pub struct RithmicSymbolSearch {
    pub(super) search_generation: NonZeroUsize,
    search_text: String,
    exchange: Option<String>,
    product_code: Option<String>,
    instrument_type: Option<InstrumentType>,
    pattern: SearchPattern,
    pub(super) maximum_results: NonZeroUsize,
}

impl fmt::Debug for RithmicSymbolSearch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicSymbolSearch")
            .field("search_generation", &self.search_generation)
            .field("maximum_results", &self.maximum_results)
            .field("query", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl RithmicSymbolSearch {
    /// Returns the generation that fences this search's callbacks.
    #[must_use]
    pub const fn generation(&self) -> NonZeroUsize {
        self.search_generation
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.search_text
    }

    #[must_use]
    pub fn exchange(&self) -> Option<&str> {
        self.exchange.as_deref()
    }

    #[must_use]
    pub fn product_code(&self) -> Option<&str> {
        self.product_code.as_deref()
    }

    #[must_use]
    pub const fn instrument_type(&self) -> Option<InstrumentType> {
        self.instrument_type
    }

    #[must_use]
    pub const fn pattern(&self) -> SearchPattern {
        self.pattern
    }

    #[must_use]
    pub const fn maximum_results(&self) -> NonZeroUsize {
        self.maximum_results
    }

    /// Creates one bounded search without retaining provider text in diagnostics.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, oversized, or control-bearing field, or
    /// for a result bound above the direct-session instrument limit.
    pub fn try_new(
        search_generation: NonZeroUsize,
        search_text: impl Into<String>,
        exchange: Option<String>,
        product_code: Option<String>,
        instrument_type: Option<InstrumentType>,
        pattern: SearchPattern,
        maximum_results: NonZeroUsize,
    ) -> Result<Self, RithmicProviderCommandError> {
        let search_text = search_text.into();
        if maximum_results.get() > MAXIMUM_INSTRUMENTS
            || !valid_command_field(&search_text)
            || exchange
                .as_ref()
                .is_some_and(|value| !valid_command_field(value))
            || product_code
                .as_ref()
                .is_some_and(|value| !valid_command_field(value))
        {
            return Err(RithmicProviderCommandError::InvalidRequest);
        }
        Ok(Self {
            search_generation,
            search_text,
            exchange,
            product_code,
            instrument_type,
            pattern,
            maximum_results,
        })
    }

    pub(super) fn protocol_request(&self) -> SymbolSearchRequest<'_> {
        SymbolSearchRequest {
            search_text: &self.search_text,
            exchange: self.exchange.as_deref(),
            product_code: self.product_code.as_deref(),
            instrument_type: self.instrument_type,
            pattern: self.pattern,
        }
    }

    pub(super) fn collection_request(&self) -> SymbolSearchCollectionRequest {
        SymbolSearchCollectionRequest {
            exchange: self.exchange.clone(),
            product_code: self.product_code.clone(),
            maximum_results: self.maximum_results,
        }
    }
}

/// Bounded product-code listing for one session-unique search generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicProductListing {
    pub(super) listing_generation: NonZeroUsize,
    pub(super) maximum_results: NonZeroUsize,
}

impl RithmicProductListing {
    /// Creates one bounded listing of every exchange's products.
    ///
    /// # Errors
    ///
    /// Returns an error for a result bound above the direct-session instrument limit.
    pub const fn try_new(
        listing_generation: NonZeroUsize,
        maximum_results: NonZeroUsize,
    ) -> Result<Self, RithmicProviderCommandError> {
        if maximum_results.get() > MAXIMUM_INSTRUMENTS {
            return Err(RithmicProviderCommandError::InvalidRequest);
        }
        Ok(Self {
            listing_generation,
            maximum_results,
        })
    }

    /// Returns the generation that fences this listing's callbacks.
    #[must_use]
    pub const fn generation(self) -> NonZeroUsize {
        self.listing_generation
    }

    #[must_use]
    pub const fn maximum_results(self) -> NonZeroUsize {
        self.maximum_results
    }
}

/// Exact selection from one retained completed search.
#[derive(Clone, Eq, PartialEq)]
pub struct RithmicInstrumentSelection {
    pub(super) selection_generation: NonZeroUsize,
    pub(super) search_generation: NonZeroUsize,
    pub(super) symbol: String,
    pub(super) exchange: String,
    pub(super) entitlement_id: String,
    pub(super) subscription: RithmicReadOnlySubscription,
    pub(super) mode: RithmicSelectionMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RithmicSelectionMode {
    Subscribe,
    ReferenceOnly,
}

/// Read-only market-data families enabled for one selected instrument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RithmicReadOnlySubscription {
    pub(super) trades: bool,
    pub(super) quotes: bool,
    pub(super) order_book: bool,
}

impl RithmicReadOnlySubscription {
    /// Creates a non-empty read-only market-data selection.
    ///
    /// # Errors
    ///
    /// Returns an error when every market-data family is disabled.
    pub fn try_new(
        trades: bool,
        quotes: bool,
        order_book: bool,
    ) -> Result<Self, RithmicProviderCommandError> {
        if !(trades || quotes || order_book) {
            return Err(RithmicProviderCommandError::InvalidRequest);
        }
        Ok(Self {
            trades,
            quotes,
            order_book,
        })
    }

    const fn is_empty(self) -> bool {
        !(self.trades || self.quotes || self.order_book)
    }

    #[must_use]
    pub const fn trades(self) -> bool {
        self.trades
    }

    #[must_use]
    pub const fn quotes(self) -> bool {
        self.quotes
    }

    #[must_use]
    pub const fn order_book(self) -> bool {
        self.order_book
    }
}

impl fmt::Debug for RithmicInstrumentSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RithmicInstrumentSelection")
            .field("selection_generation", &self.selection_generation)
            .field("search_generation", &self.search_generation)
            .field("instrument", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl RithmicInstrumentSelection {
    /// Returns the generation that fences this selection's callbacks.
    #[must_use]
    pub const fn generation(&self) -> NonZeroUsize {
        self.selection_generation
    }

    #[must_use]
    pub const fn search_generation(&self) -> NonZeroUsize {
        self.search_generation
    }

    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    #[must_use]
    pub fn exchange(&self) -> &str {
        &self.exchange
    }

    #[must_use]
    pub fn entitlement_id(&self) -> &str {
        &self.entitlement_id
    }

    #[must_use]
    pub const fn subscription(&self) -> RithmicReadOnlySubscription {
        self.subscription
    }

    /// Creates one bounded read-only selection.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identities or an empty market-data intent.
    pub fn try_new(
        selection_generation: NonZeroUsize,
        search_generation: NonZeroUsize,
        symbol: impl Into<String>,
        exchange: impl Into<String>,
        entitlement_id: impl Into<String>,
        subscription: RithmicReadOnlySubscription,
    ) -> Result<Self, RithmicProviderCommandError> {
        Self::try_new_inner(
            selection_generation,
            search_generation,
            symbol,
            exchange,
            entitlement_id,
            subscription,
            RithmicSelectionMode::Subscribe,
        )
    }

    /// Creates one bounded catalog selection that resolves instrument metadata
    /// without changing the active market-data subscription set.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identities or an empty market-data intent.
    pub fn try_new_reference_only(
        selection_generation: NonZeroUsize,
        search_generation: NonZeroUsize,
        symbol: impl Into<String>,
        exchange: impl Into<String>,
        entitlement_id: impl Into<String>,
        subscription: RithmicReadOnlySubscription,
    ) -> Result<Self, RithmicProviderCommandError> {
        Self::try_new_inner(
            selection_generation,
            search_generation,
            symbol,
            exchange,
            entitlement_id,
            subscription,
            RithmicSelectionMode::ReferenceOnly,
        )
    }

    fn try_new_inner(
        selection_generation: NonZeroUsize,
        search_generation: NonZeroUsize,
        symbol: impl Into<String>,
        exchange: impl Into<String>,
        entitlement_id: impl Into<String>,
        subscription: RithmicReadOnlySubscription,
        mode: RithmicSelectionMode,
    ) -> Result<Self, RithmicProviderCommandError> {
        let symbol = symbol.into();
        let exchange = exchange.into();
        let entitlement_id = entitlement_id.into();
        if !valid_command_field(&symbol)
            || !valid_command_field(&exchange)
            || !valid_command_field(&entitlement_id)
            || subscription.is_empty()
        {
            return Err(RithmicProviderCommandError::InvalidRequest);
        }
        Ok(Self {
            selection_generation,
            search_generation,
            symbol,
            exchange,
            entitlement_id,
            subscription,
            mode,
        })
    }
}

/// Coarse asynchronous rejection for a bounded catalog command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicCatalogRejection {
    SearchRejected,
    SupersededSearch,
    InstrumentUnavailable,
    SubscriptionRejected,
    SearchDispatchUnavailable,
    SelectionDispatchUnavailable,
}

/// Adapter-local catalog output, fenced by session and UI generations.
#[derive(Clone, Eq, PartialEq)]
pub enum RithmicCatalogEvent {
    SearchCompleted {
        session_generation: SessionGeneration,
        search_generation: NonZeroUsize,
        symbols: CollectedSymbols,
    },
    ProductsListed {
        session_generation: SessionGeneration,
        listing_generation: NonZeroUsize,
        products: CollectedProducts,
    },
    SelectionInstalled {
        session_generation: SessionGeneration,
        selection_generation: NonZeroUsize,
        instrument: InstrumentDescriptor,
        entitlement_id: String,
    },
    CommandRejected {
        session_generation: Option<SessionGeneration>,
        command_generation: NonZeroUsize,
        reason: RithmicCatalogRejection,
    },
}

impl fmt::Debug for RithmicCatalogEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SearchCompleted {
                session_generation,
                search_generation,
                symbols,
            } => formatter
                .debug_struct("SearchCompleted")
                .field("session_generation", session_generation)
                .field("search_generation", search_generation)
                .field("result_count", &symbols.results.len())
                .finish(),
            Self::ProductsListed {
                session_generation,
                listing_generation,
                products,
            } => formatter
                .debug_struct("ProductsListed")
                .field("session_generation", session_generation)
                .field("listing_generation", listing_generation)
                .field("product_count", &products.products.len())
                .finish(),
            Self::SelectionInstalled {
                session_generation,
                selection_generation,
                ..
            } => formatter
                .debug_struct("SelectionInstalled")
                .field("session_generation", session_generation)
                .field("selection_generation", selection_generation)
                .field("instrument", &"[REDACTED]")
                .finish(),
            Self::CommandRejected {
                session_generation,
                command_generation,
                reason,
            } => formatter
                .debug_struct("CommandRejected")
                .field("session_generation", session_generation)
                .field("command_generation", command_generation)
                .field("reason", reason)
                .finish(),
        }
    }
}

impl RithmicCatalogEvent {
    #[must_use]
    pub fn search_completed(
        session_generation: std::num::NonZeroU64,
        search_generation: NonZeroUsize,
        symbols: CollectedSymbols,
    ) -> Self {
        Self::SearchCompleted {
            session_generation: SessionGeneration::new(session_generation),
            search_generation,
            symbols,
        }
    }

    #[must_use]
    pub fn products_listed(
        session_generation: std::num::NonZeroU64,
        listing_generation: NonZeroUsize,
        products: CollectedProducts,
    ) -> Self {
        Self::ProductsListed {
            session_generation: SessionGeneration::new(session_generation),
            listing_generation,
            products,
        }
    }

    /// Creates a selection publication without exposing runtime ownership types to the UI.
    ///
    /// # Errors
    /// Returns an error when the provider-neutral instrument descriptor is invalid.
    #[allow(clippy::too_many_arguments)]
    pub fn selection_installed(
        session_generation: std::num::NonZeroU64,
        selection_generation: NonZeroUsize,
        instrument_id: String,
        provider_symbol: String,
        display_symbol: String,
        venue_id: String,
        price_scale: u8,
        quantity_scale: u8,
        entitlement_id: String,
    ) -> Result<Self, RithmicProviderCommandError> {
        let instrument = InstrumentDescriptor {
            instrument_id,
            provider_symbol,
            display_symbol,
            venue_id,
            price_scale,
            quantity_scale,
            price_increment: None,
            contract: None,
        };
        instrument
            .validate()
            .map_err(|_| RithmicProviderCommandError::InvalidRequest)?;
        Ok(Self::SelectionInstalled {
            session_generation: SessionGeneration::new(session_generation),
            selection_generation,
            instrument,
            entitlement_id,
        })
    }

    #[must_use]
    pub fn command_rejected(
        session_generation: Option<std::num::NonZeroU64>,
        command_generation: NonZeroUsize,
        reason: RithmicCatalogRejection,
    ) -> Self {
        Self::CommandRejected {
            session_generation: session_generation.map(SessionGeneration::new),
            command_generation,
            reason,
        }
    }
}

/// Redacted command-validation and delivery failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RithmicProviderCommandError {
    InvalidRequest,
    SessionUnavailable,
    StaleGeneration,
    QueueFull,
}

impl fmt::Display for RithmicProviderCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Rithmic catalog command failed: {self:?}")
    }
}

impl Error for RithmicProviderCommandError {}

fn valid_command_field(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAXIMUM_IDENTITY_BYTES
        && !value.chars().any(char::is_control)
}

impl RithmicProviderInstrument {
    pub(super) fn as_request(&self) -> MarketDataSubscription<'_> {
        MarketDataSubscription {
            symbol: &self.descriptor.provider_symbol,
            exchange: &self.descriptor.venue_id,
            action: SubscriptionAction::Subscribe,
            trades: self.trades,
            quotes: self.quotes,
            order_book: self.order_book,
        }
    }

    pub(super) fn identity_matches(&self, identity: &MarketIdentity) -> bool {
        self.descriptor.provider_symbol == identity.symbol
            && self.descriptor.venue_id == identity.exchange
    }

    pub(super) fn depth_request(&self, action: SubscriptionAction) -> DepthByOrderSubscription<'_> {
        DepthByOrderSubscription {
            symbol: &self.descriptor.provider_symbol,
            exchange: &self.descriptor.venue_id,
            action,
        }
    }

    pub(super) fn depth_snapshot_request(&self) -> DepthByOrderSnapshotRequest<'_> {
        DepthByOrderSnapshotRequest {
            symbol: &self.descriptor.provider_symbol,
            exchange: &self.descriptor.venue_id,
        }
    }
}

pub(super) fn validate_provider_instruments(
    instruments: &[RithmicProviderInstrument],
) -> Result<(), RithmicProviderCommandError> {
    if instruments.len() > MAXIMUM_INSTRUMENTS {
        return Err(RithmicProviderCommandError::InvalidRequest);
    }
    let mut provider_identities = BTreeSet::new();
    let mut canonical_identities = BTreeSet::new();
    for instrument in instruments {
        if instrument.descriptor.validate().is_err()
            || !valid_identity(&instrument.entitlement_id)
            || !(instrument.trades || instrument.quotes || instrument.order_book)
            || !provider_identities.insert((
                instrument.descriptor.venue_id.as_str(),
                instrument.descriptor.provider_symbol.as_str(),
            ))
            || !canonical_identities.insert(instrument.descriptor.instrument_id.as_str())
        {
            return Err(RithmicProviderCommandError::InvalidRequest);
        }
    }
    Ok(())
}
