//! Bounded native study dependency runtime.
//!
//! This module deliberately stops at the market-runtime ownership boundary. Studies declare
//! dependencies on already-resolved canonical market series or earlier study outputs; provider
//! sessions, history, live handoff, and canonical market state remain owned by `MarketEngine` and
//! `market_service`. The runtime produces deterministic recalculation plans and shared upstream
//! stream requirements without creating provider work itself.

use axiusflow_market_data::{
    AggressorSide, BarSeriesKey, DepthLevel, MarketBar, OrderBook, OrderBookState, TopOfBookQuote,
};
use axiusflow_market_engine::{
    ConsumerId, EngineError, MarketDataLeaseId, MarketEngine, MarketStream, SeriesSnapshot,
    StreamRequirements,
};
use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
    num::{NonZeroU64, NonZeroUsize},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

/// Maximum UTF-8 bytes accepted in one stable study identifier.
pub const MAXIMUM_STUDY_IDENTIFIER_BYTES: usize = 128;
/// Maximum typed settings declared by one study.
pub const MAXIMUM_STUDY_SETTINGS: usize = 64;
/// Maximum static dependencies accepted for one production study instance.
pub const MAXIMUM_STUDY_DEPENDENCIES_PER_INSTANCE: usize = 16;
/// Maximum scalar outputs accepted for one production study instance.
pub const MAXIMUM_STUDY_OUTPUTS_PER_INSTANCE: usize = 16;
/// Maximum UTF-8 bytes accepted in one stable setting identifier.
pub const MAXIMUM_STUDY_SETTING_IDENTIFIER_BYTES: usize = 64;
/// Maximum UTF-8 bytes retained in one textual setting value.
pub const MAXIMUM_STUDY_SETTING_TEXT_BYTES: usize = 512;
/// Maximum decimal scale accepted by the typed settings surface.
pub const MAXIMUM_STUDY_SETTING_DECIMAL_SCALE: u8 = 18;
const MAXIMUM_STUDY_EXECUTION_ERROR_BYTES: usize = 256;

/// Stable runtime identity for one study instance.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StudyInstanceId(NonZeroU64);

impl StudyInstanceId {
    /// Creates a runtime identity from a non-zero numeric form.
    #[must_use]
    pub fn try_from_u64(value: u64) -> Option<Self> {
        NonZeroU64::new(value).map(Self)
    }

    /// Returns the non-zero numeric identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// Returns one output identity for this study instance.
    #[must_use]
    pub const fn output(self, output_index: usize) -> StudyOutputId {
        StudyOutputId {
            study_id: self,
            output_index,
        }
    }
}

/// Stable output identity within one study instance.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StudyOutputId {
    pub study_id: StudyInstanceId,
    pub output_index: usize,
}

/// One resolved canonical market input and its exact upstream stream requirements.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyMarketInput {
    pub series: BarSeriesKey,
    pub streams: StreamRequirements,
}

/// Canonical OHLCV field exposed by a native study market-series view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudyBarField {
    Open,
    High,
    Low,
    Close,
    Volume,
}

/// Zero-copy native study view over one canonical `MarketEngine` snapshot.
///
/// Cloning this value clones only the snapshot `Arc`; bar storage remains owned
/// by `MarketEngine`. Exact exchange timestamps, fixed-point values, scales, and
/// provider/publication generations therefore stay identical to canonical state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyMarketSeries {
    snapshot: Arc<SeriesSnapshot>,
}

/// One retained provider-neutral aggressor trade exposed to native studies.
///
/// This is the same bounded live sample retained by `market_runtime` for the
/// canonical instrument state; the Study Runtime does not keep a second trade
/// buffer. Prices and quantities remain fixed-point and use the scales reported
/// by [`StudyTradeWindow`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudyTradeSample {
    pub observed_unix_nanos: i64,
    pub price: i64,
    pub quantity: i64,
    pub aggressor: AggressorSide,
}

/// Borrowed bounded window over the runtime's retained recent trades.
#[derive(Clone, Copy)]
pub struct StudyTradeWindow<'a> {
    trades: &'a VecDeque<StudyTradeSample>,
    session_generation: u64,
    source_watermark: u64,
    price_scale: u8,
    quantity_scale: u8,
}

impl<'a> StudyTradeWindow<'a> {
    pub(crate) const fn new(
        trades: &'a VecDeque<StudyTradeSample>,
        session_generation: u64,
        source_watermark: u64,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Self {
        Self {
            trades,
            session_generation,
            source_watermark,
            price_scale,
            quantity_scale,
        }
    }

    /// Returns the number of retained live trade samples.
    #[must_use]
    pub fn len(self) -> usize {
        self.trades.len()
    }

    /// Returns whether the runtime currently retains no live trade samples.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.trades.is_empty()
    }

    /// Returns the provider session generation owning the retained trade window.
    #[must_use]
    pub const fn session_generation(self) -> u64 {
        self.session_generation
    }

    /// Returns the latest accepted trade source sequence.
    #[must_use]
    pub const fn source_watermark(self) -> u64 {
        self.source_watermark
    }

    /// Returns the fixed-point decimal scale for trade prices.
    #[must_use]
    pub const fn price_scale(self) -> u8 {
        self.price_scale
    }

    /// Returns the fixed-point decimal scale for trade quantities.
    #[must_use]
    pub const fn quantity_scale(self) -> u8 {
        self.quantity_scale
    }

    /// Returns one retained trade sample without copying the retained window.
    #[must_use]
    pub fn get(self, index: usize) -> Option<StudyTradeSample> {
        self.trades.get(index).copied()
    }

    /// Iterates retained trade samples from oldest to newest without allocating.
    #[must_use]
    pub fn iter(self) -> impl ExactSizeIterator<Item = StudyTradeSample> + 'a {
        self.trades.iter().copied()
    }
}

/// Borrowed current top-of-book quote for one canonical instrument.
#[derive(Clone, Copy)]
pub struct StudyQuoteView<'a> {
    quote: &'a TopOfBookQuote,
    price_scale: u8,
    quantity_scale: u8,
}

impl<'a> StudyQuoteView<'a> {
    pub(crate) const fn new(
        quote: &'a TopOfBookQuote,
        price_scale: u8,
        quantity_scale: u8,
    ) -> Self {
        Self {
            quote,
            price_scale,
            quantity_scale,
        }
    }

    /// Returns the current bid, if the provider has not explicitly cleared it.
    #[must_use]
    pub const fn bid(self) -> Option<DepthLevel> {
        self.quote.bid
    }

    /// Returns the current ask, if the provider has not explicitly cleared it.
    #[must_use]
    pub const fn ask(self) -> Option<DepthLevel> {
        self.quote.ask
    }

    /// Returns the latest accepted quote source sequence.
    #[must_use]
    pub const fn source_sequence(self) -> u64 {
        self.quote.metadata.source_sequence
    }

    /// Returns the provider session generation owning this quote.
    #[must_use]
    pub const fn session_generation(self) -> u64 {
        self.quote.metadata.session_generation
    }

    /// Returns the best available event-time timestamp for this quote.
    #[must_use]
    pub fn observed_unix_nanos(self) -> i64 {
        self.quote
            .metadata
            .timestamps
            .exchange_unix_nanos
            .or(self.quote.metadata.timestamps.provider_unix_nanos)
            .unwrap_or(self.quote.metadata.timestamps.received_unix_nanos)
    }

    /// Returns the fixed-point decimal scale for quote prices.
    #[must_use]
    pub const fn price_scale(self) -> u8 {
        self.price_scale
    }

    /// Returns the fixed-point decimal scale for quote quantities.
    #[must_use]
    pub const fn quantity_scale(self) -> u8 {
        self.quantity_scale
    }
}

/// Borrowed canonical depth image for one instrument.
///
/// Level iteration reads the existing `OrderBook` maps directly. No depth
/// vector is cloned merely to execute a study.
#[derive(Clone, Copy)]
pub struct StudyDepthView<'a> {
    book: &'a OrderBook,
    price_scale: u8,
    quantity_scale: u8,
}

impl<'a> StudyDepthView<'a> {
    pub(crate) const fn new(book: &'a OrderBook, price_scale: u8, quantity_scale: u8) -> Self {
        Self {
            book,
            price_scale,
            quantity_scale,
        }
    }

    /// Returns current canonical book readiness/recovery state.
    #[must_use]
    pub const fn state(self) -> OrderBookState {
        self.book.state()
    }

    /// Returns the current canonical book revision.
    #[must_use]
    pub const fn revision(self) -> u64 {
        self.book.revision()
    }

    /// Returns the latest accepted depth source sequence.
    #[must_use]
    pub const fn source_watermark(self) -> u64 {
        self.book.source_watermark()
    }

    /// Returns the provider session generation owning this depth image.
    #[must_use]
    pub fn session_generation(self) -> Option<u64> {
        self.book.session_generation()
    }

    /// Returns canonical bid levels from best to worst without allocating.
    #[must_use]
    pub fn bids(self) -> impl ExactSizeIterator<Item = DepthLevel> + 'a {
        self.book.bid_levels()
    }

    /// Returns canonical ask levels from best to worst without allocating.
    #[must_use]
    pub fn asks(self) -> impl ExactSizeIterator<Item = DepthLevel> + 'a {
        self.book.ask_levels()
    }

    /// Returns the fixed-point decimal scale for depth prices.
    #[must_use]
    pub const fn price_scale(self) -> u8 {
        self.price_scale
    }

    /// Returns the fixed-point decimal scale for depth quantities.
    #[must_use]
    pub const fn quantity_scale(self) -> u8 {
        self.quantity_scale
    }
}

#[derive(Clone, Copy)]
pub(crate) struct StudyNonBarChange<'a> {
    pub provider_id: &'a str,
    pub instrument_id: &'a str,
    pub entitlement_id: &'a str,
    pub stream: MarketStream,
    pub observed_unix_nanos: i64,
}

/// Borrowed non-bar market state aligned with one declared market dependency.
#[derive(Clone, Copy, Default)]
pub struct StudyLiveMarketData<'a> {
    quote: Option<StudyQuoteView<'a>>,
    trades: Option<StudyTradeWindow<'a>>,
    depth: Option<StudyDepthView<'a>>,
}

impl<'a> StudyLiveMarketData<'a> {
    pub(crate) const fn new(
        quote: Option<StudyQuoteView<'a>>,
        trades: Option<StudyTradeWindow<'a>>,
        depth: Option<StudyDepthView<'a>>,
    ) -> Self {
        Self {
            quote,
            trades,
            depth,
        }
    }

    /// Returns the current canonical top-of-book quote, when requested and available.
    #[must_use]
    pub const fn quote(self) -> Option<StudyQuoteView<'a>> {
        self.quote
    }

    /// Returns the runtime's bounded recent-trade window, when requested.
    #[must_use]
    pub const fn trades(self) -> Option<StudyTradeWindow<'a>> {
        self.trades
    }

    /// Returns the current canonical depth image, when requested.
    #[must_use]
    pub const fn depth(self) -> Option<StudyDepthView<'a>> {
        self.depth
    }
}

impl StudyMarketSeries {
    fn new(snapshot: Arc<SeriesSnapshot>) -> Self {
        Self { snapshot }
    }

    /// Returns the canonical series identity.
    #[must_use]
    pub fn series(&self) -> &BarSeriesKey {
        &self.snapshot.series
    }

    /// Returns the immutable canonical bars without copying them.
    #[must_use]
    pub fn bars(&self) -> &[MarketBar] {
        &self.snapshot.bars
    }

    /// Returns whether the last canonical bar is still forming.
    #[must_use]
    pub fn forming(&self) -> bool {
        self.snapshot.forming
    }

    /// Returns the canonical publication generation observed by this view.
    #[must_use]
    pub fn publication_generation(&self) -> u64 {
        self.snapshot.publication_generation
    }

    /// Returns one fixed-point OHLCV field view over the canonical bars.
    #[must_use]
    pub fn field(&self, field: StudyBarField) -> StudyBarFieldSeries<'_> {
        let scale = match field {
            StudyBarField::Open
            | StudyBarField::High
            | StudyBarField::Low
            | StudyBarField::Close => self.snapshot.price_scale,
            StudyBarField::Volume => self.snapshot.quantity_scale,
        };
        StudyBarFieldSeries {
            bars: &self.snapshot.bars,
            field,
            scale,
        }
    }
}

/// Non-owning fixed-point field projection over canonical bars.
#[derive(Clone, Copy, Debug)]
pub struct StudyBarFieldSeries<'a> {
    bars: &'a [MarketBar],
    field: StudyBarField,
    scale: u8,
}

impl StudyBarFieldSeries<'_> {
    /// Returns the number of canonical rows.
    #[must_use]
    pub const fn len(self) -> usize {
        self.bars.len()
    }

    /// Returns whether no canonical rows are available.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bars.is_empty()
    }

    /// Returns the decimal scale for this fixed-point field.
    #[must_use]
    pub const fn scale(self) -> u8 {
        self.scale
    }

    /// Returns one exact fixed-point value without allocating or converting.
    #[must_use]
    pub fn value(self, index: usize) -> Option<i64> {
        self.bars.get(index).map(|bar| match self.field {
            StudyBarField::Open => bar.open,
            StudyBarField::High => bar.high,
            StudyBarField::Low => bar.low,
            StudyBarField::Close => bar.close,
            StudyBarField::Volume => bar.volume,
        })
    }

    /// Returns one exact exchange timestamp for row alignment across inputs.
    #[must_use]
    pub fn exchange_timestamp_unix_nanos(self, index: usize) -> Option<i64> {
        self.bars
            .get(index)
            .map(|bar| bar.exchange_timestamp_unix_nanos)
    }
}

/// Exact base-10 decimal used by study settings without introducing binary
/// floating-point ambiguity into durable configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudyDecimal {
    pub mantissa: i64,
    pub scale: u8,
}

/// One typed study setting value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StudySettingValue {
    Boolean(bool),
    Integer(i64),
    Decimal(StudyDecimal),
    Text(String),
    Choice(String),
}

impl StudySettingValue {
    fn same_type(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Boolean(_), Self::Boolean(_))
                | (Self::Integer(_), Self::Integer(_))
                | (Self::Decimal(_), Self::Decimal(_))
                | (Self::Text(_), Self::Text(_))
                | (Self::Choice(_), Self::Choice(_))
        )
    }

    fn validate(&self) -> Result<(), StudyRuntimeError> {
        match self {
            Self::Decimal(decimal) if decimal.scale > MAXIMUM_STUDY_SETTING_DECIMAL_SCALE => {
                Err(StudyRuntimeError::InvalidSettingValue)
            }
            Self::Text(value) | Self::Choice(value)
                if value.len() > MAXIMUM_STUDY_SETTING_TEXT_BYTES =>
            {
                Err(StudyRuntimeError::InvalidSettingValue)
            }
            _ => Ok(()),
        }
    }
}

/// One durable typed setting declaration. Product UI can render a control from
/// the default value's type without knowing the native study implementation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudySettingSpec {
    pub identifier: String,
    pub default: StudySettingValue,
}

/// Validated setting values for one study instance.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StudySettings {
    values: BTreeMap<String, StudySettingValue>,
}

impl StudySettings {
    /// Builds settings from declared defaults.
    ///
    /// # Errors
    /// Returns an error for an invalid or duplicate setting declaration.
    pub fn defaults(specs: &[StudySettingSpec]) -> Result<Self, StudyRuntimeError> {
        validate_setting_specs(specs)?;
        Ok(Self {
            values: specs
                .iter()
                .map(|spec| (spec.identifier.clone(), spec.default.clone()))
                .collect(),
        })
    }

    /// Builds settings by applying typed overrides to declared defaults.
    ///
    /// # Errors
    /// Returns an error for unknown keys, type mismatches, or invalid values.
    pub fn with_overrides(
        specs: &[StudySettingSpec],
        overrides: BTreeMap<String, StudySettingValue>,
    ) -> Result<Self, StudyRuntimeError> {
        let mut settings = Self::defaults(specs)?;
        for (identifier, value) in overrides {
            let spec = specs
                .iter()
                .find(|spec| spec.identifier == identifier)
                .ok_or(StudyRuntimeError::UnknownSetting)?;
            value.validate()?;
            if !value.same_type(&spec.default) {
                return Err(StudyRuntimeError::SettingTypeMismatch);
            }
            settings.values.insert(identifier, value);
        }
        Ok(settings)
    }

    /// Returns one typed setting value.
    #[must_use]
    pub fn get(&self, identifier: &str) -> Option<&StudySettingValue> {
        self.values.get(identifier)
    }

    /// Returns the validated setting count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns whether this study declares no settings.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

fn validate_setting_specs(specs: &[StudySettingSpec]) -> Result<(), StudyRuntimeError> {
    if specs.len() > MAXIMUM_STUDY_SETTINGS {
        return Err(StudyRuntimeError::TooManySettings {
            maximum: MAXIMUM_STUDY_SETTINGS,
        });
    }
    let mut identifiers = BTreeSet::new();
    for spec in specs {
        let identifier = spec.identifier.trim();
        if identifier.is_empty()
            || identifier.len() > MAXIMUM_STUDY_SETTING_IDENTIFIER_BYTES
            || !identifiers.insert(identifier)
        {
            return Err(StudyRuntimeError::InvalidSettingIdentifier);
        }
        spec.default.validate()?;
    }
    Ok(())
}

fn validate_settings(
    specs: &[StudySettingSpec],
    settings: &StudySettings,
) -> Result<(), StudyRuntimeError> {
    validate_setting_specs(specs)?;
    if settings.len() != specs.len() {
        return Err(StudyRuntimeError::UnknownSetting);
    }
    for spec in specs {
        let value = settings
            .get(&spec.identifier)
            .ok_or(StudyRuntimeError::UnknownSetting)?;
        value.validate()?;
        if !value.same_type(&spec.default) {
            return Err(StudyRuntimeError::SettingTypeMismatch);
        }
    }
    Ok(())
}

/// One static dependency declared before a study begins calculating.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StudyDependency {
    Market(StudyMarketInput),
    Output(StudyOutputId),
}

/// How an input mutation can affect this study's output rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudyInvalidationPolicy {
    /// Only output rows aligned with the changed input range can change.
    SameRange,
    /// A change can affect every later output row, as with recursive smoothing.
    FromFirstChanged,
    /// A changed input row can affect this many trailing output rows in total.
    TrailingWindow { bars: NonZeroUsize },
}

/// Scalar plot family requested by one study output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudyPlotKind {
    Line,
    Histogram,
    Area,
}

/// Semantic pane placement. Outputs using the same dedicated group share one
/// chart pane; actual Nucleus pane identities remain presentation-owned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudyPaneTarget {
    Price,
    Dedicated { group: u8 },
}

/// Semantic scale placement inside the selected pane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudyScaleTarget {
    Primary,
    Left,
    Overlay,
}

/// Declarative presentation metadata for one scalar study output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyOutputSpec {
    pub identifier: String,
    pub title: String,
    pub plot: StudyPlotKind,
    pub pane: StudyPaneTarget,
    pub scale: StudyScaleTarget,
}

/// Runtime-independent definition for one native study instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyDefinition {
    pub identifier: String,
    pub dependencies: Vec<StudyDependency>,
    pub settings: Vec<StudySettingSpec>,
    pub outputs: Vec<StudyOutputSpec>,
    pub invalidation: StudyInvalidationPolicy,
}

/// Explicit finite limits for one study runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudyRuntimeConfig {
    pub maximum_studies: NonZeroUsize,
    pub maximum_dependencies_per_study: NonZeroUsize,
    pub maximum_outputs_per_study: NonZeroUsize,
    pub maximum_points_per_output: NonZeroUsize,
    pub maximum_total_output_points: NonZeroUsize,
    pub maximum_state_bytes_per_study: NonZeroUsize,
    pub maximum_total_state_bytes: NonZeroUsize,
}

/// Immutable scalar output series produced by one native study output.
///
/// Timestamps are inherited from the study's first dependency and shared by all
/// outputs from the same calculation. `None` represents a deliberate gap such
/// as an indicator warm-up period.
#[derive(Clone, Debug, PartialEq)]
pub struct StudyOutputSeries {
    timestamps: Arc<[i64]>,
    values: Arc<[Option<f64>]>,
    generation: u64,
}

impl StudyOutputSeries {
    /// Returns the number of output rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns whether no output rows are available.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns the output generation committed by the runtime.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the exact row timestamp.
    #[must_use]
    pub fn timestamp_unix_nanos(&self, index: usize) -> Option<i64> {
        self.timestamps.get(index).copied()
    }

    /// Returns one finite scalar value or a deliberate gap.
    #[must_use]
    pub fn value(&self, index: usize) -> Option<Option<f64>> {
        self.values.get(index).copied()
    }

    /// Returns all timestamps for binary-search alignment by secondary inputs.
    #[must_use]
    pub fn timestamps(&self) -> &[i64] {
        &self.timestamps
    }

    /// Returns all output values.
    #[must_use]
    pub fn values(&self) -> &[Option<f64>] {
        &self.values
    }
}

#[derive(Clone)]
enum ResolvedStudyInput {
    Market(StudyMarketSeries),
    Output(StudyOutputSeries),
}

/// One read-only input exposed to native Rust study code.
#[derive(Clone, Copy)]
pub enum StudyInputSeries<'a> {
    Market(&'a StudyMarketSeries),
    Output(&'a StudyOutputSeries),
}

/// One bounded output buffer for the current native calculation.
pub struct StudyOutputBuffer {
    values: Vec<Option<f64>>,
}

impl StudyOutputBuffer {
    fn new(len: usize) -> Self {
        Self {
            values: vec![None; len],
        }
    }

    fn from_previous(timestamps: &[i64], previous: Option<&StudyOutputSeries>) -> Self {
        let mut output = Self::new(timestamps.len());
        let Some(previous) = previous else {
            return output;
        };
        let mut previous_index = 0;
        let mut current_index = 0;
        while previous_index < previous.timestamps.len() && current_index < timestamps.len() {
            match previous.timestamps[previous_index].cmp(&timestamps[current_index]) {
                std::cmp::Ordering::Less => previous_index += 1,
                std::cmp::Ordering::Greater => current_index += 1,
                std::cmp::Ordering::Equal => {
                    output.values[current_index] = previous.values[previous_index];
                    previous_index += 1;
                    current_index += 1;
                }
            }
        }
        output
    }

    fn clear(&mut self, range: StudyDirtyRange) {
        let end = range
            .end_exclusive
            .unwrap_or(self.values.len())
            .min(self.values.len());
        if range.start < end {
            self.values[range.start..end].fill(None);
        }
    }

    /// Returns the output row count inherited from the first dependency.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns whether this output has no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Writes one finite scalar value or an explicit gap.
    ///
    /// # Errors
    /// Returns an error for an out-of-range row or non-finite value.
    pub fn set(&mut self, index: usize, value: Option<f64>) -> Result<(), String> {
        if value.is_some_and(|value| !value.is_finite()) {
            return Err("study output must be finite".to_string());
        }
        let slot = self
            .values
            .get_mut(index)
            .ok_or_else(|| "study output row is outside the calculation range".to_string())?;
        *slot = value;
        Ok(())
    }
}

/// In-process execution context for one trusted native Rust study calculation.
///
/// Inputs are immutable snapshots. Outputs are bounded buffers owned by the
/// runtime. Studies receive timestamps rather than pixel coordinates and cannot
/// reach provider sessions, `MarketEngine`, Nucleus, GPUI, or GPU state.
pub struct StudyExecutionContext<'a> {
    settings: &'a StudySettings,
    inputs: &'a [ResolvedStudyInput],
    live_inputs: &'a [Option<StudyLiveMarketData<'a>>],
    timestamps: &'a [i64],
    dirty: StudyDirtyRange,
    outputs: &'a mut [StudyOutputBuffer],
    state: Option<&'a mut NativeStudyState>,
}

/// Immutable half of one native study calculation.
#[derive(Clone, Copy)]
pub struct StudyExecutionInputs<'a> {
    settings: &'a StudySettings,
    inputs: &'a [ResolvedStudyInput],
    live_inputs: &'a [Option<StudyLiveMarketData<'a>>],
    timestamps: &'a [i64],
    dirty: StudyDirtyRange,
}

impl<'a> StudyExecutionInputs<'a> {
    /// Returns validated typed settings.
    #[must_use]
    pub const fn settings(self) -> &'a StudySettings {
        self.settings
    }

    /// Returns the primary output timeline inherited from dependency zero.
    #[must_use]
    pub const fn timestamps(self) -> &'a [i64] {
        self.timestamps
    }

    /// Returns the output rows that must be recomputed for this invocation.
    #[must_use]
    pub const fn dirty_range(self) -> StudyDirtyRange {
        self.dirty
    }

    /// Returns one declared input in definition order.
    #[must_use]
    pub fn input(self, index: usize) -> Option<StudyInputSeries<'a>> {
        self.inputs.get(index).map(|input| match input {
            ResolvedStudyInput::Market(series) => StudyInputSeries::Market(series),
            ResolvedStudyInput::Output(series) => StudyInputSeries::Output(series),
        })
    }

    /// Returns borrowed quote/trade/depth state for one market dependency.
    ///
    /// Output dependencies and market dependencies that requested only bars
    /// return `None`. The returned views borrow the coordinator's existing
    /// canonical live state and never allocate a second depth/trade snapshot.
    #[must_use]
    pub fn live_market(self, index: usize) -> Option<StudyLiveMarketData<'a>> {
        self.live_inputs.get(index).copied().flatten()
    }
}

impl StudyExecutionContext<'_> {
    /// Returns validated typed settings.
    #[must_use]
    pub const fn settings(&self) -> &StudySettings {
        self.settings
    }

    /// Returns the primary output timeline inherited from dependency zero.
    #[must_use]
    pub const fn timestamps(&self) -> &[i64] {
        self.timestamps
    }

    /// Returns the output rows that must be recomputed for this invocation.
    #[must_use]
    pub const fn dirty_range(&self) -> StudyDirtyRange {
        self.dirty
    }

    /// Returns one declared input in definition order.
    #[must_use]
    pub fn input(&self, index: usize) -> Option<StudyInputSeries<'_>> {
        self.inputs.get(index).map(|input| match input {
            ResolvedStudyInput::Market(series) => StudyInputSeries::Market(series),
            ResolvedStudyInput::Output(series) => StudyInputSeries::Output(series),
        })
    }

    /// Returns borrowed quote/trade/depth state for one market dependency.
    #[must_use]
    pub fn live_market(&self, index: usize) -> Option<StudyLiveMarketData<'_>> {
        self.live_inputs.get(index).copied().flatten()
    }

    /// Returns one mutable numeric output buffer.
    #[must_use]
    pub fn output(&mut self, index: usize) -> Option<&mut StudyOutputBuffer> {
        self.outputs.get_mut(index)
    }

    /// Returns the runtime-owned mutable state value for this study when its
    /// native registration declared state of type `T`.
    ///
    /// The runtime calculates against a cloned candidate and commits that state
    /// only together with successful outputs, so rejected or panicking native
    /// executions cannot partially advance recursive checkpoints.
    #[must_use]
    pub fn state_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.state.as_deref_mut()?.value_mut::<T>()
    }

    /// Splits immutable inputs/settings from mutable output buffers so native
    /// study code can read several inputs while writing several outputs without
    /// cloning canonical market data.
    #[must_use]
    pub fn split(&mut self) -> (StudyExecutionInputs<'_>, &mut [StudyOutputBuffer]) {
        (
            StudyExecutionInputs {
                settings: self.settings,
                inputs: self.inputs,
                live_inputs: self.live_inputs,
                timestamps: self.timestamps,
                dirty: self.dirty,
            },
            self.outputs,
        )
    }

    /// Splits immutable inputs/settings from one typed runtime state value and
    /// all mutable output buffers.
    ///
    /// This is the stateful counterpart to [`Self::split`]. `None` means the
    /// registration has no state or registered a different concrete state type.
    #[must_use]
    pub fn split_with_state<T: 'static>(
        &mut self,
    ) -> Option<(StudyExecutionInputs<'_>, &mut T, &mut [StudyOutputBuffer])> {
        let state = self.state.as_deref_mut()?.value_mut::<T>()?;
        Some((
            StudyExecutionInputs {
                settings: self.settings,
                inputs: self.inputs,
                live_inputs: self.live_inputs,
                timestamps: self.timestamps,
                dirty: self.dirty,
            },
            state,
            self.outputs,
        ))
    }
}

#[derive(Clone)]
struct NativeStudyStateValue<T> {
    value: T,
    runtime_bytes: fn(&T) -> usize,
}

type ErasedNativeStudyStateValue = dyn Any + Send;
type CloneNativeStudyStateValue =
    fn(&ErasedNativeStudyStateValue) -> Box<ErasedNativeStudyStateValue>;
type NativeStudyStateBytes = fn(&ErasedNativeStudyStateValue) -> usize;

fn clone_native_study_state_value<T: Clone + Send + 'static>(
    value: &ErasedNativeStudyStateValue,
) -> Box<ErasedNativeStudyStateValue> {
    Box::new(
        value
            .downcast_ref::<NativeStudyStateValue<T>>()
            .expect("native study state clone type")
            .clone(),
    )
}

fn native_study_state_value_bytes<T: Clone + Send + 'static>(
    value: &ErasedNativeStudyStateValue,
) -> usize {
    let state = value
        .downcast_ref::<NativeStudyStateValue<T>>()
        .expect("native study state accounting type");
    (state.runtime_bytes)(&state.value)
}

/// One opaque, cloneable native-study state value owned by [`StudyRuntime`].
///
/// The concrete type remains private to trusted native study code. The supplied
/// byte counter must include owned heap capacity in addition to the value's
/// fixed size so the runtime can enforce configured state-memory ceilings.
pub struct NativeStudyState {
    value: Box<ErasedNativeStudyStateValue>,
    clone_value: CloneNativeStudyStateValue,
    runtime_bytes: NativeStudyStateBytes,
}

impl Clone for NativeStudyState {
    fn clone(&self) -> Self {
        Self {
            value: (self.clone_value)(self.value.as_ref()),
            clone_value: self.clone_value,
            runtime_bytes: self.runtime_bytes,
        }
    }
}

impl NativeStudyState {
    /// Wraps one cloneable runtime state value and its exact memory-accounting
    /// callback.
    #[must_use]
    pub fn new<T: Clone + Send + 'static>(value: T, runtime_bytes: fn(&T) -> usize) -> Self {
        Self {
            value: Box::new(NativeStudyStateValue {
                value,
                runtime_bytes,
            }),
            clone_value: clone_native_study_state_value::<T>,
            runtime_bytes: native_study_state_value_bytes::<T>,
        }
    }

    fn value_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.value
            .downcast_mut::<NativeStudyStateValue<T>>()
            .map(|state| &mut state.value)
    }

    fn runtime_bytes(&self) -> usize {
        (self.runtime_bytes)(self.value.as_ref())
    }
}

/// Trusted native Rust calculation entry point.
pub type NativeStudyCalculate = for<'a> fn(&mut StudyExecutionContext<'a>) -> Result<(), String>;
/// Trusted native Rust factory for one fresh per-instance runtime state value.
pub type NativeStudyStateFactory = fn(&StudySettings) -> Result<NativeStudyState, String>;

/// Executable code handle kept separate from the durable study definition.
#[derive(Clone, Copy)]
pub struct NativeStudyProgram {
    pub calculate: NativeStudyCalculate,
    /// Optional factory for recursive/checkpoint state. Stateless studies leave
    /// this `None`. The runtime invokes it for registration, reinitialization,
    /// and every covering/full execution so stale checkpoints cannot cross a
    /// changed definition or a full rebuild boundary.
    pub state_factory: Option<NativeStudyStateFactory>,
}

impl NativeStudyProgram {
    /// Builds a trusted native program that carries no mutable per-instance state.
    #[must_use]
    pub const fn stateless(calculate: NativeStudyCalculate) -> Self {
        Self {
            calculate,
            state_factory: None,
        }
    }

    /// Builds a trusted native program with runtime-owned mutable state created
    /// from the registration's typed settings.
    #[must_use]
    pub const fn stateful(
        calculate: NativeStudyCalculate,
        state_factory: NativeStudyStateFactory,
    ) -> Self {
        Self {
            calculate,
            state_factory: Some(state_factory),
        }
    }
}

/// One native runtime registration. The definition/settings are durable data;
/// the function pointer is resolved from trusted code in the running binary.
#[derive(Clone)]
pub struct NativeStudyRegistration {
    pub definition: StudyDefinition,
    pub settings: StudySettings,
    pub program: NativeStudyProgram,
}

/// Half-open dirty output range. `None` means from `start` through the current tail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudyDirtyRange {
    pub start: usize,
    pub end_exclusive: Option<usize>,
}

impl StudyDirtyRange {
    /// Creates one finite non-empty half-open dirty range.
    ///
    /// # Errors
    /// Returns [`StudyRuntimeError::EmptyDirtyRange`] unless `start < end_exclusive`.
    pub fn bounded(start: usize, end_exclusive: usize) -> Result<Self, StudyRuntimeError> {
        if start >= end_exclusive {
            return Err(StudyRuntimeError::EmptyDirtyRange);
        }
        Ok(Self {
            start,
            end_exclusive: Some(end_exclusive),
        })
    }

    /// Marks every row from `start` through the current tail dirty.
    #[must_use]
    pub const fn to_tail(start: usize) -> Self {
        Self {
            start,
            end_exclusive: None,
        }
    }

    fn merge(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end_exclusive: match (self.end_exclusive, other.end_exclusive) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (None, _) | (_, None) => None,
            },
        }
    }

    fn invalidated_by(self, policy: StudyInvalidationPolicy) -> Result<Self, StudyRuntimeError> {
        match policy {
            StudyInvalidationPolicy::SameRange => Ok(self),
            StudyInvalidationPolicy::FromFirstChanged => Ok(Self::to_tail(self.start)),
            StudyInvalidationPolicy::TrailingWindow { bars } => {
                let trailing = bars.get().saturating_sub(1);
                let end_exclusive = self
                    .end_exclusive
                    .map(|end| {
                        end.checked_add(trailing)
                            .ok_or(StudyRuntimeError::RangeOverflow)
                    })
                    .transpose()?;
                Ok(Self {
                    start: self.start,
                    end_exclusive,
                })
            }
        }
    }
}

/// One study calculation scheduled in dependency order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudyCalculation {
    pub study_id: StudyInstanceId,
    pub range: StudyDirtyRange,
}

/// Exact change made while reconciling study market dependencies into
/// `MarketEngine`'s authoritative shared subscription set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StudyMarketLeaseChangeKind {
    Acquired,
    Updated,
    Released,
}

/// One deduplicated market-data lease transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyMarketLeaseChange {
    pub lease_id: MarketDataLeaseId,
    pub series: BarSeriesKey,
    pub streams: StreamRequirements,
    pub kind: StudyMarketLeaseChangeKind,
}

/// Validation or bounded-runtime failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StudyRuntimeError {
    InvalidIdentifier,
    IdentifierTooLong {
        maximum: usize,
    },
    MissingDependency,
    TooManyDependencies {
        maximum: usize,
    },
    TooManyOutputs {
        maximum: usize,
    },
    MissingOutput,
    InvalidOutputIdentifier,
    InvalidOutputTitle,
    DuplicateOutputIdentifier,
    OutputMetadataTooLong {
        maximum: usize,
    },
    TooManySettings {
        maximum: usize,
    },
    StudyLimitExceeded {
        maximum: usize,
    },
    InvalidSettingIdentifier,
    InvalidSettingValue,
    UnknownSetting,
    SettingTypeMismatch,
    InvalidMarketSeries,
    EmptyMarketStreams,
    UnknownStudy(StudyInstanceId),
    UnknownOutput(StudyOutputId),
    EmptyDirtyRange,
    RangeOverflow,
    IdentifierExhausted,
    OutputGenerationExhausted,
    DependencyIndexOutOfBounds {
        study_id: StudyInstanceId,
        dependency_index: usize,
    },
    DependencyIsNotMarket {
        study_id: StudyInstanceId,
        dependency_index: usize,
    },
    CrossConsumerDependency {
        study_id: StudyInstanceId,
        dependency: StudyOutputId,
    },
    DependencyOrderViolation {
        study_id: StudyInstanceId,
        dependency: StudyOutputId,
    },
    OutputInterfaceChanged(StudyInstanceId),
    StudyNotExecutable(StudyInstanceId),
    OutputPointLimitExceeded {
        maximum: usize,
        requested: usize,
    },
    TotalOutputPointLimitExceeded {
        maximum: usize,
        requested: usize,
    },
    StateMemoryLimitExceeded {
        maximum: usize,
        requested: usize,
    },
    TotalStateMemoryLimitExceeded {
        maximum: usize,
        requested: usize,
    },
    StateInitializationRejected {
        detail: String,
    },
    ExecutionRejected {
        study_id: StudyInstanceId,
        detail: String,
    },
    MarketDemand(EngineError),
}

impl fmt::Display for StudyRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentifier => formatter.write_str("study identifier must not be empty"),
            Self::IdentifierTooLong { maximum } => {
                write!(formatter, "study identifier exceeds {maximum} bytes")
            }
            Self::MissingDependency => {
                formatter.write_str("study must declare at least one static dependency")
            }
            Self::TooManyDependencies { maximum } => {
                write!(formatter, "study dependency count exceeds {maximum}")
            }
            Self::TooManyOutputs { maximum } => {
                write!(formatter, "study output count exceeds {maximum}")
            }
            Self::MissingOutput
            | Self::InvalidOutputIdentifier
            | Self::InvalidOutputTitle
            | Self::DuplicateOutputIdentifier
            | Self::OutputMetadataTooLong { .. } => fmt_output_metadata_error(self, formatter),
            Self::TooManySettings { maximum } => {
                write!(formatter, "study setting count exceeds {maximum}")
            }
            Self::StudyLimitExceeded { maximum } => {
                write!(formatter, "study runtime exceeds {maximum} instances")
            }
            Self::InvalidSettingIdentifier => {
                formatter.write_str("study setting identifier is invalid")
            }
            Self::InvalidSettingValue => formatter.write_str("study setting value is invalid"),
            Self::UnknownSetting => formatter.write_str("study setting is not declared"),
            Self::SettingTypeMismatch => formatter.write_str("study setting type does not match"),
            Self::InvalidMarketSeries => formatter.write_str("study market series is invalid"),
            Self::EmptyMarketStreams => {
                formatter.write_str("study market dependency must request at least one stream")
            }
            Self::UnknownStudy(study_id) => {
                write!(formatter, "unknown study instance {}", study_id.get())
            }
            Self::UnknownOutput(output) => write!(
                formatter,
                "unknown study output {}:{}",
                output.study_id.get(),
                output.output_index
            ),
            Self::EmptyDirtyRange => formatter.write_str("study dirty range must not be empty"),
            Self::RangeOverflow => formatter.write_str("study dirty range overflowed"),
            Self::IdentifierExhausted => formatter.write_str("study instance identity exhausted"),
            Self::OutputGenerationExhausted => {
                formatter.write_str("study output generation exhausted")
            }
            Self::DependencyIndexOutOfBounds {
                study_id,
                dependency_index,
            } => write!(
                formatter,
                "study {} has no dependency at index {dependency_index}",
                study_id.get()
            ),
            Self::DependencyIsNotMarket {
                study_id,
                dependency_index,
            } => write!(
                formatter,
                "study {} dependency {dependency_index} is not a market series",
                study_id.get()
            ),
            Self::CrossConsumerDependency { .. }
            | Self::DependencyOrderViolation { .. }
            | Self::OutputInterfaceChanged(_) => fmt_dependency_contract_error(self, formatter),
            Self::StudyNotExecutable(study_id) => {
                write!(formatter, "study {} has no native program", study_id.get())
            }
            Self::OutputPointLimitExceeded { maximum, requested } => write!(
                formatter,
                "study output point limit {maximum} exceeded by {requested} rows"
            ),
            Self::TotalOutputPointLimitExceeded { maximum, requested } => write!(
                formatter,
                "study runtime output point limit {maximum} exceeded by {requested} values"
            ),
            Self::StateMemoryLimitExceeded { maximum, requested } => write!(
                formatter,
                "study state limit {maximum} bytes exceeded by {requested} bytes"
            ),
            Self::TotalStateMemoryLimitExceeded { maximum, requested } => write!(
                formatter,
                "study runtime state limit {maximum} bytes exceeded by {requested} bytes"
            ),
            Self::StateInitializationRejected { detail } => {
                write!(formatter, "study state initialization failed: {detail}")
            }
            Self::ExecutionRejected { study_id, detail } => {
                write!(
                    formatter,
                    "study {} execution failed: {detail}",
                    study_id.get()
                )
            }
            Self::MarketDemand(error) => write!(formatter, "study market demand failed: {error}"),
        }
    }
}

fn fmt_dependency_contract_error(
    error: &StudyRuntimeError,
    formatter: &mut fmt::Formatter<'_>,
) -> fmt::Result {
    match error {
        StudyRuntimeError::CrossConsumerDependency {
            study_id,
            dependency,
        } => write!(
            formatter,
            "study {} cannot depend on output {}:{} owned by another consumer",
            study_id.get(),
            dependency.study_id.get(),
            dependency.output_index
        ),
        StudyRuntimeError::DependencyOrderViolation {
            study_id,
            dependency,
        } => write!(
            formatter,
            "study {} cannot depend on output {}:{} that is not earlier in dependency order",
            study_id.get(),
            dependency.study_id.get(),
            dependency.output_index
        ),
        StudyRuntimeError::OutputInterfaceChanged(study_id) => write!(
            formatter,
            "study {} reinitialization changed its output identity contract",
            study_id.get()
        ),
        _ => unreachable!("only dependency-contract errors reach this formatter"),
    }
}

fn fmt_output_metadata_error(
    error: &StudyRuntimeError,
    formatter: &mut fmt::Formatter<'_>,
) -> fmt::Result {
    match error {
        StudyRuntimeError::MissingOutput => {
            formatter.write_str("study must declare at least one output")
        }
        StudyRuntimeError::InvalidOutputIdentifier => {
            formatter.write_str("study output identifier is invalid")
        }
        StudyRuntimeError::InvalidOutputTitle => {
            formatter.write_str("study output title is invalid")
        }
        StudyRuntimeError::DuplicateOutputIdentifier => {
            formatter.write_str("study output identifiers must be unique")
        }
        StudyRuntimeError::OutputMetadataTooLong { maximum } => {
            write!(formatter, "study output metadata exceeds {maximum} bytes")
        }
        _ => unreachable!("only output metadata errors reach this formatter"),
    }
}

impl Error for StudyRuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MarketDemand(error) => Some(error),
            _ => None,
        }
    }
}

impl From<EngineError> for StudyRuntimeError {
    fn from(error: EngineError) -> Self {
        Self::MarketDemand(error)
    }
}

#[derive(Clone)]
struct StudyNode {
    owner: Option<ConsumerId>,
    definition: StudyDefinition,
    settings: StudySettings,
    program: Option<NativeStudyProgram>,
    state: Option<NativeStudyState>,
    state_bytes: usize,
}

pub(crate) struct StudySubtreeCheckpoint {
    nodes: BTreeMap<StudyInstanceId, StudyNode>,
    outputs: BTreeMap<StudyOutputId, StudyOutputSeries>,
    output_points: usize,
    state_bytes: usize,
}

struct NativeStudyCalculation<'a> {
    study_id: StudyInstanceId,
    settings: &'a StudySettings,
    program: NativeStudyProgram,
    output_count: usize,
    inputs: &'a [ResolvedStudyInput],
    live_inputs: &'a [Option<StudyLiveMarketData<'a>>],
    timestamps: &'a [i64],
    dirty: StudyDirtyRange,
}

struct PreparedStudyStates {
    states: BTreeMap<StudyInstanceId, (Option<NativeStudyState>, usize)>,
    total_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StudyMarketLease {
    lease_id: MarketDataLeaseId,
    streams: StreamRequirements,
}

/// Bounded dependency owner for native studies.
///
/// Registration order is dependency order: a study may reference only outputs that already exist.
/// That rule makes cycles impossible without maintaining a second graph authority.
pub struct StudyRuntime {
    config: StudyRuntimeConfig,
    studies: BTreeMap<StudyInstanceId, StudyNode>,
    market_dependents: BTreeMap<BarSeriesKey, BTreeSet<StudyInstanceId>>,
    output_dependents: BTreeMap<StudyOutputId, BTreeSet<StudyInstanceId>>,
    market_leases: BTreeMap<BarSeriesKey, StudyMarketLease>,
    outputs: BTreeMap<StudyOutputId, StudyOutputSeries>,
    output_points: usize,
    state_bytes: usize,
    next_id: u64,
    next_output_generation: u64,
}

impl StudyRuntime {
    /// Creates an empty bounded runtime.
    #[must_use]
    pub fn new(config: StudyRuntimeConfig) -> Self {
        Self {
            config,
            studies: BTreeMap::new(),
            market_dependents: BTreeMap::new(),
            output_dependents: BTreeMap::new(),
            market_leases: BTreeMap::new(),
            outputs: BTreeMap::new(),
            output_points: 0,
            state_bytes: 0,
            next_id: 1,
            next_output_generation: 1,
        }
    }

    /// Returns the number of registered study instances.
    #[must_use]
    pub fn len(&self) -> usize {
        self.studies.len()
    }

    /// Returns whether no study instances are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.studies.is_empty()
    }

    /// Returns one registered runtime-independent definition.
    #[must_use]
    pub fn definition(&self, study_id: StudyInstanceId) -> Option<&StudyDefinition> {
        self.studies.get(&study_id).map(|node| &node.definition)
    }

    /// Registers one study for an existing market consumer after validating all
    /// of its static dependencies.
    ///
    /// # Errors
    /// Returns an error for invalid identifiers, bounds, canonical market series, or references to
    /// outputs that are not already registered for the same consumer.
    pub(crate) fn register_native_for_consumer(
        &mut self,
        owner: ConsumerId,
        registration: NativeStudyRegistration,
    ) -> Result<StudyInstanceId, StudyRuntimeError> {
        self.register_node(
            Some(owner),
            registration.definition,
            registration.settings,
            Some(registration.program),
        )
    }

    /// Reinitializes one production study in place after a declared input or
    /// typed setting changes. The instance/output identities stay stable so
    /// downstream dependencies and presentation references do not need a second
    /// remapping layer.
    ///
    /// The output identifier sequence is the stable interface contract. Titles,
    /// plot kinds, pane placement, scales, settings, and market dependencies may
    /// change; removing/reordering an output requires a new study instance.
    pub(crate) fn reinitialize_native_for_consumer(
        &mut self,
        owner: ConsumerId,
        study_id: StudyInstanceId,
        registration: NativeStudyRegistration,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError> {
        let current = self
            .studies
            .get(&study_id)
            .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
        if current.owner != Some(owner) {
            return Err(StudyRuntimeError::UnknownStudy(study_id));
        }
        for dependency in &registration.definition.dependencies {
            if let StudyDependency::Output(output) = dependency
                && output.study_id >= study_id
            {
                return Err(StudyRuntimeError::DependencyOrderViolation {
                    study_id,
                    dependency: *output,
                });
            }
        }
        self.validate_definition(Some(owner), &registration.definition)?;
        validate_settings(&registration.definition.settings, &registration.settings)?;
        let same_output_interface = current.definition.outputs.len()
            == registration.definition.outputs.len()
            && current
                .definition
                .outputs
                .iter()
                .zip(&registration.definition.outputs)
                .all(|(left, right)| left.identifier == right.identifier);
        if !same_output_interface {
            return Err(StudyRuntimeError::OutputInterfaceChanged(study_id));
        }

        let affected = self.dependent_subtree(study_id)?;
        let PreparedStudyStates {
            states: mut fresh_states,
            total_bytes: replacement_state_bytes,
        } = self.fresh_states_for_reinitialization(
            study_id,
            &affected,
            registration.program,
            &registration.settings,
        )?;
        let (root_state, root_state_bytes) = fresh_states
            .remove(&study_id)
            .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
        self.studies.insert(
            study_id,
            StudyNode {
                owner: Some(owner),
                definition: registration.definition,
                settings: registration.settings,
                program: Some(registration.program),
                state: root_state,
                state_bytes: root_state_bytes,
            },
        );
        for (affected_id, (state, state_bytes)) in fresh_states {
            if let Some(node) = self.studies.get_mut(&affected_id) {
                node.state = state;
                node.state_bytes = state_bytes;
            }
        }
        self.state_bytes = replacement_state_bytes;
        self.rebuild_dependency_indexes();
        for affected_id in &affected {
            self.remove_outputs_for_study(*affected_id);
        }
        Ok(affected.into_iter().collect())
    }

    #[cfg(test)]
    fn register(
        &mut self,
        definition: StudyDefinition,
    ) -> Result<StudyInstanceId, StudyRuntimeError> {
        let settings = StudySettings::defaults(&definition.settings)?;
        self.register_node(None, definition, settings, None)
    }

    #[cfg(test)]
    fn register_for_consumer(
        &mut self,
        owner: ConsumerId,
        definition: StudyDefinition,
    ) -> Result<StudyInstanceId, StudyRuntimeError> {
        let settings = StudySettings::defaults(&definition.settings)?;
        self.register_node(Some(owner), definition, settings, None)
    }

    fn register_node(
        &mut self,
        owner: Option<ConsumerId>,
        definition: StudyDefinition,
        settings: StudySettings,
        program: Option<NativeStudyProgram>,
    ) -> Result<StudyInstanceId, StudyRuntimeError> {
        self.validate_definition(owner, &definition)?;
        validate_settings(&definition.settings, &settings)?;
        let state = Self::fresh_state(program, &settings)?;
        let state_bytes = Self::reported_state_bytes(state.as_ref())?;
        let next_state_bytes = self.validate_state_memory_bytes(state_bytes, 0)?;
        if self.studies.len() == self.config.maximum_studies.get() {
            return Err(StudyRuntimeError::StudyLimitExceeded {
                maximum: self.config.maximum_studies.get(),
            });
        }

        let study_id = NonZeroU64::new(self.next_id)
            .map(StudyInstanceId)
            .ok_or(StudyRuntimeError::IdentifierExhausted)?;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(StudyRuntimeError::IdentifierExhausted)?;

        for dependency in &definition.dependencies {
            match dependency {
                StudyDependency::Market(input) => {
                    self.market_dependents
                        .entry(input.series.clone())
                        .or_default()
                        .insert(study_id);
                }
                StudyDependency::Output(output) => {
                    self.output_dependents
                        .entry(*output)
                        .or_default()
                        .insert(study_id);
                }
            }
        }
        self.studies.insert(
            study_id,
            StudyNode {
                owner,
                definition,
                settings,
                program,
                state,
                state_bytes,
            },
        );
        self.state_bytes = next_state_bytes;
        Ok(study_id)
    }

    /// Returns the market consumer that owns one production study instance.
    #[must_use]
    pub(crate) fn owner(&self, study_id: StudyInstanceId) -> Option<ConsumerId> {
        self.studies.get(&study_id).and_then(|node| node.owner)
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn native_registration(
        &self,
        study_id: StudyInstanceId,
    ) -> Option<NativeStudyRegistration> {
        let node = self.studies.get(&study_id)?;
        Some(NativeStudyRegistration {
            definition: node.definition.clone(),
            settings: node.settings.clone(),
            program: node.program?,
        })
    }

    /// Returns production study identities owned by one chart consumer in
    /// dependency/registration order.
    #[must_use]
    pub(crate) fn owned_studies(&self, owner: ConsumerId) -> Vec<StudyInstanceId> {
        self.studies
            .iter()
            .filter_map(|(study_id, node)| (node.owner == Some(owner)).then_some(*study_id))
            .collect()
    }

    /// Removes every study owned by one market consumer.
    pub(crate) fn remove_consumer(&mut self, owner: ConsumerId) -> Vec<StudyInstanceId> {
        let removed = self
            .studies
            .iter()
            .filter_map(|(study_id, node)| (node.owner == Some(owner)).then_some(*study_id))
            .collect::<Vec<_>>();
        if removed.is_empty() {
            return removed;
        }
        for study_id in &removed {
            if let Some(node) = self.studies.remove(study_id) {
                self.state_bytes = self.state_bytes.saturating_sub(node.state_bytes);
            }
            self.remove_outputs_for_study(*study_id);
        }
        self.rebuild_dependency_indexes();
        removed
    }

    /// Removes one study and every downstream study that depends on one of its outputs.
    ///
    /// # Errors
    /// Returns [`StudyRuntimeError::UnknownStudy`] when `study_id` is not live.
    pub fn remove_subtree(
        &mut self,
        study_id: StudyInstanceId,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError> {
        if !self.studies.contains_key(&study_id) {
            return Err(StudyRuntimeError::UnknownStudy(study_id));
        }

        let mut removed = BTreeSet::from([study_id]);
        loop {
            let previous_len = removed.len();
            for (output, dependents) in &self.output_dependents {
                if removed.contains(&output.study_id) {
                    removed.extend(dependents.iter().copied());
                }
            }
            if removed.len() == previous_len {
                break;
            }
        }

        for id in &removed {
            if let Some(node) = self.studies.remove(id) {
                self.state_bytes = self.state_bytes.saturating_sub(node.state_bytes);
            }
            self.remove_outputs_for_study(*id);
        }
        self.rebuild_dependency_indexes();
        Ok(removed.into_iter().collect())
    }

    fn dependent_subtree(
        &self,
        study_id: StudyInstanceId,
    ) -> Result<BTreeSet<StudyInstanceId>, StudyRuntimeError> {
        if !self.studies.contains_key(&study_id) {
            return Err(StudyRuntimeError::UnknownStudy(study_id));
        }
        let mut affected = BTreeSet::from([study_id]);
        loop {
            let previous_len = affected.len();
            for (output, dependents) in &self.output_dependents {
                if affected.contains(&output.study_id) {
                    affected.extend(dependents.iter().copied());
                }
            }
            if affected.len() == previous_len {
                return Ok(affected);
            }
        }
    }

    pub(crate) fn checkpoint_subtree(
        &self,
        study_id: StudyInstanceId,
    ) -> Result<StudySubtreeCheckpoint, StudyRuntimeError> {
        let affected = self.dependent_subtree(study_id)?;
        let mut nodes = BTreeMap::new();
        for affected_id in &affected {
            let node = self
                .studies
                .get(affected_id)
                .ok_or(StudyRuntimeError::UnknownStudy(*affected_id))?;
            let cloned = catch_unwind(AssertUnwindSafe(|| node.clone())).map_err(|_| {
                StudyRuntimeError::ExecutionRejected {
                    study_id: *affected_id,
                    detail: "native study state clone panicked".to_string(),
                }
            })?;
            nodes.insert(*affected_id, cloned);
        }
        let outputs = self
            .outputs
            .iter()
            .filter(|(output, _)| affected.contains(&output.study_id))
            .map(|(output, series)| (*output, series.clone()))
            .collect();
        Ok(StudySubtreeCheckpoint {
            nodes,
            outputs,
            output_points: self.output_points,
            state_bytes: self.state_bytes,
        })
    }

    pub(crate) fn restore_subtree_checkpoint(&mut self, checkpoint: StudySubtreeCheckpoint) {
        let affected = checkpoint.nodes.keys().copied().collect::<BTreeSet<_>>();
        for (study_id, node) in checkpoint.nodes {
            self.studies.insert(study_id, node);
        }
        self.outputs
            .retain(|output, _| !affected.contains(&output.study_id));
        self.outputs.extend(checkpoint.outputs);
        self.output_points = checkpoint.output_points;
        self.state_bytes = checkpoint.state_bytes;
        self.rebuild_dependency_indexes();
    }

    /// Returns the deduplicated upstream market demand required by all live studies.
    ///
    /// Multiple studies requesting the same canonical series share one entry whose stream set is
    /// the union of their requirements. `MarketEngine` remains responsible for validating provider
    /// capabilities and turning this demand into actual subscriptions.
    #[must_use]
    pub fn market_requirements(&self) -> BTreeMap<BarSeriesKey, StreamRequirements> {
        let mut requirements: BTreeMap<BarSeriesKey, StreamRequirements> = BTreeMap::new();
        for node in self.studies.values() {
            for dependency in &node.definition.dependencies {
                let StudyDependency::Market(input) = dependency else {
                    continue;
                };
                requirements
                    .entry(input.series.clone())
                    .and_modify(|streams| *streams = streams.union(input.streams))
                    .or_insert(input.streams);
            }
        }
        requirements
    }

    /// Returns a zero-copy canonical market-series view for one study dependency.
    ///
    /// `Ok(None)` means the dependency is valid but canonical history has not
    /// arrived yet. The caller can therefore distinguish normal data loading from
    /// an invalid dependency index/type without inventing placeholder rows.
    ///
    /// # Errors
    /// Returns an error for an unknown study, an invalid dependency index, or a
    /// dependency that refers to another study output rather than market data.
    pub fn market_input(
        &self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        dependency_index: usize,
    ) -> Result<Option<StudyMarketSeries>, StudyRuntimeError> {
        let node = self
            .studies
            .get(&study_id)
            .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
        let dependency = node.definition.dependencies.get(dependency_index).ok_or(
            StudyRuntimeError::DependencyIndexOutOfBounds {
                study_id,
                dependency_index,
            },
        )?;
        let StudyDependency::Market(input) = dependency else {
            return Err(StudyRuntimeError::DependencyIsNotMarket {
                study_id,
                dependency_index,
            });
        };
        Ok(engine
            .series_snapshot(&input.series)
            .map(StudyMarketSeries::new))
    }

    /// Returns one committed native numeric output series.
    #[must_use]
    pub fn output_series(&self, output: StudyOutputId) -> Option<&StudyOutputSeries> {
        self.outputs.get(&output)
    }

    /// Executes one native study when every declared dependency is currently
    /// available. The first dependency owns the output timeline; secondary
    /// inputs retain their own timestamps and must be aligned explicitly by the
    /// study implementation.
    ///
    /// `Ok(false)` means at least one input is still loading. No placeholder
    /// output is committed in that case.
    ///
    /// # Errors
    /// Returns an error for unknown/non-executable studies, output-memory bounds,
    /// output-generation exhaustion, or a calculation rejection.
    #[cfg(test)]
    pub(crate) fn execute_ready(
        &mut self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
    ) -> Result<bool, StudyRuntimeError> {
        let mut no_live_market = |_input: &StudyMarketInput| None;
        self.execute_ready_with_live(engine, study_id, &mut no_live_market)
    }

    pub(crate) fn execute_ready_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        live_market: &mut F,
    ) -> Result<bool, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        self.execute_ready_range_with_live(engine, study_id, None, live_market)
    }

    /// Re-executes one study and every downstream dependent whose inputs become
    /// ready, preserving dependency/registration order. This is the covering
    /// path used immediately after in-place study reinitialization.
    #[cfg(test)]
    pub(crate) fn execute_ready_subtree(
        &mut self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError> {
        let mut no_live_market = |_input: &StudyMarketInput| None;
        self.execute_ready_subtree_with_live(engine, study_id, &mut no_live_market)
    }

    pub(crate) fn execute_ready_subtree_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        live_market: &mut F,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        let affected = self.dependent_subtree(study_id)?;
        let mut executed = Vec::new();
        for candidate in affected {
            match self.execute_ready_with_live(engine, candidate, live_market) {
                Ok(true) => executed.push(candidate),
                Ok(false) | Err(StudyRuntimeError::StudyNotExecutable(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(executed)
    }

    #[cfg(test)]
    fn execute_ready_range(
        &mut self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        requested_dirty: Option<StudyDirtyRange>,
    ) -> Result<bool, StudyRuntimeError> {
        let mut no_live_market = |_input: &StudyMarketInput| None;
        self.execute_ready_range_with_live(engine, study_id, requested_dirty, &mut no_live_market)
    }

    fn execute_ready_range_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        requested_dirty: Option<StudyDirtyRange>,
        live_market: &mut F,
    ) -> Result<bool, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        let (definition, settings, program, previous_state_bytes) = {
            let node = self
                .studies
                .get(&study_id)
                .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
            (
                node.definition.clone(),
                node.settings.clone(),
                node.program
                    .ok_or(StudyRuntimeError::StudyNotExecutable(study_id))?,
                node.state_bytes,
            )
        };
        let Some(inputs) = self.resolve_inputs(engine, &definition) else {
            return Ok(false);
        };
        let Some(live_inputs) = Self::resolve_live_inputs(&definition, live_market) else {
            return Ok(false);
        };
        let timestamps =
            primary_timestamps(inputs.first().ok_or(StudyRuntimeError::MissingDependency)?);
        if timestamps.is_empty() {
            return Ok(false);
        }
        if timestamps.len() > self.config.maximum_points_per_output.get() {
            return Err(StudyRuntimeError::OutputPointLimitExceeded {
                maximum: self.config.maximum_points_per_output.get(),
                requested: timestamps.len(),
            });
        }
        let output_count = definition.outputs.len();
        let total_points =
            self.output_points_after_replacement(study_id, timestamps.len(), output_count)?;
        let next_generation = self
            .next_output_generation
            .checked_add(1)
            .ok_or(StudyRuntimeError::OutputGenerationExhausted)?;
        let generation = self.next_output_generation;
        let dirty = if let Some(requested) = requested_dirty {
            let Some(clamped) = clamp_dirty_range(requested, timestamps.len()) else {
                return Ok(false);
            };
            clamped
        } else {
            StudyDirtyRange {
                start: 0,
                end_exclusive: Some(timestamps.len()),
            }
        };
        let covering_execution = dirty.start == 0 && dirty.end_exclusive == Some(timestamps.len());
        let mut candidate_state = if covering_execution {
            Self::fresh_state(Some(program), &settings)?
        } else {
            catch_unwind(AssertUnwindSafe(|| {
                self.studies
                    .get(&study_id)
                    .and_then(|node| node.state.clone())
            }))
            .map_err(|_| StudyRuntimeError::ExecutionRejected {
                study_id,
                detail: "native study state clone panicked".to_string(),
            })?
        };
        let outputs = self.calculate_outputs(
            &NativeStudyCalculation {
                study_id,
                settings: &settings,
                program,
                output_count,
                inputs: &inputs,
                live_inputs: &live_inputs,
                timestamps: &timestamps,
                dirty,
            },
            &mut candidate_state,
        )?;
        let candidate_state_bytes = Self::reported_state_bytes(candidate_state.as_ref())?;
        let next_state_bytes =
            self.validate_state_memory_bytes(candidate_state_bytes, previous_state_bytes)?;
        if let Some(live) = self.studies.get_mut(&study_id) {
            live.state = candidate_state;
            live.state_bytes = candidate_state_bytes;
        }
        self.state_bytes = next_state_bytes;
        self.commit_outputs(study_id, &timestamps, outputs, generation, total_points);
        self.next_output_generation = next_generation;
        Ok(true)
    }

    fn output_points_after_replacement(
        &self,
        study_id: StudyInstanceId,
        rows: usize,
        output_count: usize,
    ) -> Result<usize, StudyRuntimeError> {
        let requested_points = rows
            .checked_mul(output_count)
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        let current_points = self
            .outputs
            .iter()
            .filter(|(output, _)| output.study_id == study_id)
            .map(|(_, series)| series.len())
            .try_fold(0usize, usize::checked_add)
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        let total_points = self
            .output_points
            .checked_sub(current_points)
            .and_then(|points| points.checked_add(requested_points))
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        if total_points > self.config.maximum_total_output_points.get() {
            return Err(StudyRuntimeError::TotalOutputPointLimitExceeded {
                maximum: self.config.maximum_total_output_points.get(),
                requested: total_points,
            });
        }
        Ok(total_points)
    }

    fn validate_state_memory_bytes(
        &self,
        requested: usize,
        replaced_bytes: usize,
    ) -> Result<usize, StudyRuntimeError> {
        if requested > self.config.maximum_state_bytes_per_study.get() {
            return Err(StudyRuntimeError::StateMemoryLimitExceeded {
                maximum: self.config.maximum_state_bytes_per_study.get(),
                requested,
            });
        }
        let total = self
            .state_bytes
            .checked_sub(replaced_bytes)
            .and_then(|bytes| bytes.checked_add(requested))
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        if total > self.config.maximum_total_state_bytes.get() {
            return Err(StudyRuntimeError::TotalStateMemoryLimitExceeded {
                maximum: self.config.maximum_total_state_bytes.get(),
                requested: total,
            });
        }
        Ok(total)
    }

    fn reported_state_bytes(state: Option<&NativeStudyState>) -> Result<usize, StudyRuntimeError> {
        let Some(state) = state else {
            return Ok(0);
        };
        catch_unwind(AssertUnwindSafe(|| state.runtime_bytes())).map_err(|_| {
            StudyRuntimeError::StateInitializationRejected {
                detail: "native study state accounting panicked".to_string(),
            }
        })
    }

    fn fresh_state(
        program: Option<NativeStudyProgram>,
        settings: &StudySettings,
    ) -> Result<Option<NativeStudyState>, StudyRuntimeError> {
        let Some(factory) = program.and_then(|program| program.state_factory) else {
            return Ok(None);
        };
        match catch_unwind(AssertUnwindSafe(|| factory(settings))) {
            Ok(Ok(state)) => Ok(Some(state)),
            Ok(Err(detail)) => Err(StudyRuntimeError::StateInitializationRejected {
                detail: bounded_execution_detail(detail),
            }),
            Err(_) => Err(StudyRuntimeError::StateInitializationRejected {
                detail: "native study state factory panicked".to_string(),
            }),
        }
    }

    fn fresh_states_for_reinitialization(
        &self,
        study_id: StudyInstanceId,
        affected: &BTreeSet<StudyInstanceId>,
        root_program: NativeStudyProgram,
        root_settings: &StudySettings,
    ) -> Result<PreparedStudyStates, StudyRuntimeError> {
        let replaced_bytes = affected
            .iter()
            .filter_map(|affected_id| self.studies.get(affected_id))
            .map(|node| node.state_bytes)
            .try_fold(0usize, usize::checked_add)
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        let mut total = self
            .state_bytes
            .checked_sub(replaced_bytes)
            .ok_or(StudyRuntimeError::RangeOverflow)?;
        let mut states = BTreeMap::new();
        for affected_id in affected {
            let state = if *affected_id == study_id {
                Self::fresh_state(Some(root_program), root_settings)?
            } else {
                let node = self
                    .studies
                    .get(affected_id)
                    .ok_or(StudyRuntimeError::UnknownStudy(*affected_id))?;
                Self::fresh_state(node.program, &node.settings)?
            };
            let requested = Self::reported_state_bytes(state.as_ref())?;
            if requested > self.config.maximum_state_bytes_per_study.get() {
                return Err(StudyRuntimeError::StateMemoryLimitExceeded {
                    maximum: self.config.maximum_state_bytes_per_study.get(),
                    requested,
                });
            }
            total = total
                .checked_add(requested)
                .ok_or(StudyRuntimeError::RangeOverflow)?;
            states.insert(*affected_id, (state, requested));
        }
        if total > self.config.maximum_total_state_bytes.get() {
            return Err(StudyRuntimeError::TotalStateMemoryLimitExceeded {
                maximum: self.config.maximum_total_state_bytes.get(),
                requested: total,
            });
        }
        Ok(PreparedStudyStates {
            states,
            total_bytes: total,
        })
    }

    fn calculate_outputs(
        &self,
        calculation: &NativeStudyCalculation<'_>,
        state: &mut Option<NativeStudyState>,
    ) -> Result<Vec<StudyOutputBuffer>, StudyRuntimeError> {
        let mut outputs = (0..calculation.output_count)
            .map(|output_index| {
                let output = StudyOutputId {
                    study_id: calculation.study_id,
                    output_index,
                };
                let mut buffer = StudyOutputBuffer::from_previous(
                    calculation.timestamps,
                    self.outputs.get(&output),
                );
                buffer.clear(calculation.dirty);
                buffer
            })
            .collect::<Vec<_>>();
        let mut context = StudyExecutionContext {
            settings: calculation.settings,
            inputs: calculation.inputs,
            live_inputs: calculation.live_inputs,
            timestamps: calculation.timestamps,
            dirty: calculation.dirty,
            outputs: &mut outputs,
            state: state.as_mut(),
        };
        match catch_unwind(AssertUnwindSafe(|| {
            (calculation.program.calculate)(&mut context)
        })) {
            Ok(Ok(())) => {}
            Ok(Err(detail)) => {
                return Err(StudyRuntimeError::ExecutionRejected {
                    study_id: calculation.study_id,
                    detail: bounded_execution_detail(detail),
                });
            }
            Err(_) => {
                return Err(StudyRuntimeError::ExecutionRejected {
                    study_id: calculation.study_id,
                    detail: "native study panicked".to_string(),
                });
            }
        }
        Ok(outputs)
    }

    fn commit_outputs(
        &mut self,
        study_id: StudyInstanceId,
        timestamps: &Arc<[i64]>,
        outputs: Vec<StudyOutputBuffer>,
        generation: u64,
        total_points: usize,
    ) {
        self.remove_outputs_for_study(study_id);
        for (output_index, output) in outputs.into_iter().enumerate() {
            self.outputs.insert(
                StudyOutputId {
                    study_id,
                    output_index,
                },
                StudyOutputSeries {
                    timestamps: Arc::clone(timestamps),
                    values: output.values.into(),
                    generation,
                },
            );
        }
        self.output_points = total_points;
    }

    /// Incrementally executes every ready study affected by one accepted
    /// canonical live-bar mutation. The changed event is mapped to each study's
    /// own primary timeline by exact exchange timestamp before invalidation is
    /// applied, so secondary MTF inputs never reuse another series' row index.
    #[cfg(test)]
    pub(crate) fn execute_live_market_change(
        &mut self,
        engine: &MarketEngine,
        series: &BarSeriesKey,
        exchange_timestamp_unix_nanos: i64,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError> {
        let mut no_live_market = |_input: &StudyMarketInput| None;
        self.execute_live_market_change_with_live(
            engine,
            series,
            exchange_timestamp_unix_nanos,
            &mut no_live_market,
        )
    }

    pub(crate) fn execute_live_market_change_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        series: &BarSeriesKey,
        exchange_timestamp_unix_nanos: i64,
        live_market: &mut F,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        let direct = self
            .market_dependents
            .get(series)
            .cloned()
            .unwrap_or_default();
        let mut pending = BTreeMap::<StudyInstanceId, StudyDirtyRange>::new();
        for study_id in direct {
            let Some(range) = self.market_change_range(
                engine,
                study_id,
                series,
                MarketStream::Bars,
                exchange_timestamp_unix_nanos,
            )?
            else {
                continue;
            };
            pending
                .entry(study_id)
                .and_modify(|current| *current = current.merge(range))
                .or_insert(range);
        }

        self.execute_pending_ranges_with_live(engine, pending, live_market)
    }

    pub(crate) fn execute_live_non_bar_change_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        change: StudyNonBarChange<'_>,
        live_market: &mut F,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        debug_assert!(!matches!(change.stream, MarketStream::Bars));
        let matching_series = self
            .market_dependents
            .keys()
            .filter(|series| {
                series.provider_id == change.provider_id
                    && series.instrument_id == change.instrument_id
                    && series.entitlement_id == change.entitlement_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut pending = BTreeMap::<StudyInstanceId, StudyDirtyRange>::new();
        for series in matching_series {
            let direct = self
                .market_dependents
                .get(&series)
                .cloned()
                .unwrap_or_default();
            for study_id in direct {
                let Some(range) = self.market_change_range(
                    engine,
                    study_id,
                    &series,
                    change.stream,
                    change.observed_unix_nanos,
                )?
                else {
                    continue;
                };
                pending
                    .entry(study_id)
                    .and_modify(|current| *current = current.merge(range))
                    .or_insert(range);
            }
        }
        self.execute_pending_ranges_with_live(engine, pending, live_market)
    }

    fn execute_pending_ranges_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        mut pending: BTreeMap<StudyInstanceId, StudyDirtyRange>,
        live_market: &mut F,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        let ordered = self.studies.keys().copied().collect::<Vec<_>>();
        let mut executed = Vec::new();
        for study_id in ordered {
            let Some(range) = pending.remove(&study_id) else {
                continue;
            };
            match self.execute_ready_range_with_live(engine, study_id, Some(range), live_market) {
                Ok(true) => {
                    executed.push(study_id);
                    self.propagate_output_change(engine, study_id, range, &mut pending)?;
                }
                Ok(false) | Err(StudyRuntimeError::StudyNotExecutable(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(executed)
    }

    fn market_change_range(
        &self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        changed_series: &BarSeriesKey,
        changed_stream: MarketStream,
        observed_unix_nanos: i64,
    ) -> Result<Option<StudyDirtyRange>, StudyRuntimeError> {
        let node = self
            .studies
            .get(&study_id)
            .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
        let mut dirty = None;
        for (dependency_index, dependency) in node.definition.dependencies.iter().enumerate() {
            let StudyDependency::Market(input) = dependency else {
                continue;
            };
            if input.series != *changed_series || !input.streams.contains(changed_stream) {
                continue;
            }
            let Some(length) = self.primary_timeline_len(engine, &node.definition) else {
                continue;
            };
            let start = if matches!(changed_stream, MarketStream::Bars) {
                let Some(start) =
                    self.primary_lower_bound(engine, &node.definition, observed_unix_nanos)
                else {
                    continue;
                };
                start
            } else {
                let Some(start) =
                    self.primary_row_at_or_before(engine, &node.definition, observed_unix_nanos)
                else {
                    continue;
                };
                start
            };
            if start >= length {
                continue;
            }
            let candidate = if matches!(changed_stream, MarketStream::Bars) && dependency_index == 0
            {
                StudyDirtyRange::bounded(start, start + 1)?
                    .invalidated_by(node.definition.invalidation)?
            } else {
                // A secondary input may affect every later primary row through
                // as-of/event-time alignment. Without assuming two series share
                // row indexes, tail invalidation is the smallest safe default.
                StudyDirtyRange::to_tail(start)
            };
            dirty = Some(dirty.map_or(candidate, |current: StudyDirtyRange| {
                current.merge(candidate)
            }));
        }
        Ok(dirty)
    }

    fn primary_row_at_or_before(
        &self,
        engine: &MarketEngine,
        definition: &StudyDefinition,
        observed_unix_nanos: i64,
    ) -> Option<usize> {
        let timestamps: &[i64] = match definition.dependencies.first()? {
            StudyDependency::Market(input) => {
                let snapshot = engine.series_snapshot(&input.series)?;
                if snapshot.bars.is_empty() {
                    return None;
                }
                let upper = snapshot.bars.partition_point(|bar| {
                    bar.exchange_timestamp_unix_nanos <= observed_unix_nanos
                });
                return Some(upper.saturating_sub(1));
            }
            StudyDependency::Output(output) => self.outputs.get(output)?.timestamps(),
        };
        if timestamps.is_empty() {
            return None;
        }
        let upper = timestamps.partition_point(|timestamp| *timestamp <= observed_unix_nanos);
        Some(upper.saturating_sub(1))
    }

    fn propagate_output_change(
        &self,
        engine: &MarketEngine,
        source_study: StudyInstanceId,
        source_range: StudyDirtyRange,
        pending: &mut BTreeMap<StudyInstanceId, StudyDirtyRange>,
    ) -> Result<(), StudyRuntimeError> {
        let source_node = self
            .studies
            .get(&source_study)
            .ok_or(StudyRuntimeError::UnknownStudy(source_study))?;
        for output_index in 0..source_node.definition.outputs.len() {
            let output_id = StudyOutputId {
                study_id: source_study,
                output_index,
            };
            let Some(source_output) = self.outputs.get(&output_id) else {
                continue;
            };
            let Some(source_range) = clamp_dirty_range(source_range, source_output.len()) else {
                continue;
            };
            let Some(change_timestamp) = source_output.timestamp_unix_nanos(source_range.start)
            else {
                continue;
            };
            let dependents = self
                .output_dependents
                .get(&output_id)
                .cloned()
                .unwrap_or_default();
            for dependent in dependents {
                let node = self
                    .studies
                    .get(&dependent)
                    .ok_or(StudyRuntimeError::UnknownStudy(dependent))?;
                let mut dependent_dirty = None;
                for (dependency_index, dependency) in
                    node.definition.dependencies.iter().enumerate()
                {
                    if *dependency != StudyDependency::Output(output_id) {
                        continue;
                    }
                    let candidate = if dependency_index == 0 {
                        source_range.invalidated_by(node.definition.invalidation)?
                    } else {
                        let Some(length) = self.primary_timeline_len(engine, &node.definition)
                        else {
                            continue;
                        };
                        let Some(start) =
                            self.primary_lower_bound(engine, &node.definition, change_timestamp)
                        else {
                            continue;
                        };
                        if start >= length {
                            continue;
                        }
                        StudyDirtyRange::to_tail(start)
                    };
                    dependent_dirty = Some(
                        dependent_dirty.map_or(candidate, |current: StudyDirtyRange| {
                            current.merge(candidate)
                        }),
                    );
                }
                if let Some(range) = dependent_dirty {
                    pending
                        .entry(dependent)
                        .and_modify(|current| *current = current.merge(range))
                        .or_insert(range);
                }
            }
        }
        Ok(())
    }

    fn primary_timeline_len(
        &self,
        engine: &MarketEngine,
        definition: &StudyDefinition,
    ) -> Option<usize> {
        match definition.dependencies.first()? {
            StudyDependency::Market(input) => {
                Some(engine.series_snapshot(&input.series)?.bars.len())
            }
            StudyDependency::Output(output) => Some(self.outputs.get(output)?.len()),
        }
    }

    fn primary_lower_bound(
        &self,
        engine: &MarketEngine,
        definition: &StudyDefinition,
        exchange_timestamp_unix_nanos: i64,
    ) -> Option<usize> {
        match definition.dependencies.first()? {
            StudyDependency::Market(input) => {
                let snapshot = engine.series_snapshot(&input.series)?;
                Some(snapshot.bars.partition_point(|bar| {
                    bar.exchange_timestamp_unix_nanos < exchange_timestamp_unix_nanos
                }))
            }
            StudyDependency::Output(output) => Some(
                self.outputs
                    .get(output)?
                    .timestamps
                    .partition_point(|timestamp| *timestamp < exchange_timestamp_unix_nanos),
            ),
        }
    }

    /// Re-executes every ready study transitively affected by one canonical
    /// market series, in registration/dependency order.
    ///
    /// This is currently the covering-recalculation path used for initial
    /// history hydration. Live incremental execution is intentionally kept
    /// separate until event-time dirty ranges are mapped across MTF inputs.
    #[cfg(test)]
    pub(crate) fn execute_ready_for_market(
        &mut self,
        engine: &MarketEngine,
        series: &BarSeriesKey,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError> {
        let mut no_live_market = |_input: &StudyMarketInput| None;
        self.execute_ready_for_market_with_live(engine, series, &mut no_live_market)
    }

    pub(crate) fn execute_ready_for_market_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        series: &BarSeriesKey,
        live_market: &mut F,
    ) -> Result<Vec<StudyInstanceId>, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        let mut affected = self
            .market_dependents
            .get(series)
            .cloned()
            .unwrap_or_default();
        for (study_id, node) in &self.studies {
            if !affected.contains(study_id) {
                continue;
            }
            for output_index in 0..node.definition.outputs.len() {
                if let Some(dependents) = self.output_dependents.get(&StudyOutputId {
                    study_id: *study_id,
                    output_index,
                }) {
                    affected.extend(dependents.iter().copied());
                }
            }
        }

        let mut executed = Vec::new();
        for study_id in affected {
            match self.execute_ready_with_live(engine, study_id, live_market) {
                Ok(true) => executed.push(study_id),
                Ok(false) | Err(StudyRuntimeError::StudyNotExecutable(_)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(executed)
    }

    fn resolve_inputs(
        &self,
        engine: &MarketEngine,
        definition: &StudyDefinition,
    ) -> Option<Vec<ResolvedStudyInput>> {
        let mut inputs = Vec::with_capacity(definition.dependencies.len());
        for dependency in &definition.dependencies {
            match dependency {
                StudyDependency::Market(input) => {
                    let snapshot = engine.series_snapshot(&input.series)?;
                    inputs.push(ResolvedStudyInput::Market(StudyMarketSeries::new(snapshot)));
                }
                StudyDependency::Output(output) => {
                    let series = self.outputs.get(output).cloned()?;
                    inputs.push(ResolvedStudyInput::Output(series));
                }
            }
        }
        Some(inputs)
    }

    fn resolve_live_inputs<'a, F>(
        definition: &StudyDefinition,
        live_market: &mut F,
    ) -> Option<Vec<Option<StudyLiveMarketData<'a>>>>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        let mut live_inputs = Vec::with_capacity(definition.dependencies.len());
        for dependency in &definition.dependencies {
            let StudyDependency::Market(input) = dependency else {
                live_inputs.push(None);
                continue;
            };
            let needs_live = input.streams.contains(MarketStream::Trades)
                || input.streams.contains(MarketStream::Quotes)
                || input.streams.contains(MarketStream::Depth);
            if needs_live {
                live_inputs.push(Some(live_market(input)?));
            } else {
                live_inputs.push(None);
            }
        }
        Some(live_inputs)
    }

    /// Reconciles all study market dependencies into `MarketEngine` data leases.
    ///
    /// One lease is held per distinct canonical series regardless of how many
    /// studies depend on it. Stream requirements are unioned before reaching the
    /// engine, so studies never allocate fake chart consumers or create a second
    /// provider-demand registry.
    ///
    /// Provider capability checks and capacity are preflighted before any lease
    /// mutation. A cached series, when present, remains readable directly from
    /// `MarketEngine::series_snapshot`; this method does not duplicate its bars.
    ///
    /// # Errors
    /// Returns an error when a requested stream set is unsupported, the engine's
    /// bounded lease capacity would be exceeded, or lease identity is exhausted.
    pub fn reconcile_market_leases(
        &mut self,
        engine: &mut MarketEngine,
    ) -> Result<Vec<StudyMarketLeaseChange>, StudyRuntimeError> {
        let desired = self.market_requirements();
        for (series, streams) in &desired {
            series
                .validate()
                .map_err(|_| StudyRuntimeError::InvalidMarketSeries)?;
            engine.verify_provider_stream_requirements(&series.provider_id, *streams)?;
        }

        let mut planned = BTreeMap::new();
        let mut changes = Vec::new();
        let mut reusable = Vec::new();
        let mut acquire = Vec::new();
        let mut updates = Vec::new();

        for (series, streams) in &desired {
            let Some(existing) = self.market_leases.get(series).copied() else {
                acquire.push((series.clone(), *streams));
                continue;
            };
            if engine.data_lease(existing.lease_id).is_none() {
                acquire.push((series.clone(), *streams));
                continue;
            }
            if existing.streams != *streams {
                updates.push((series.clone(), existing.lease_id, *streams));
            }
            planned.insert(
                series.clone(),
                StudyMarketLease {
                    lease_id: existing.lease_id,
                    streams: *streams,
                },
            );
        }

        for (series, lease) in &self.market_leases {
            if desired.contains_key(series) {
                continue;
            }
            if engine.data_lease(lease.lease_id).is_some() {
                reusable.push((series.clone(), *lease));
            }
        }

        let additional = acquire.len().saturating_sub(reusable.len());
        engine.preflight_data_lease_acquisitions(additional)?;

        for (series, lease_id, streams) in updates {
            engine.update_data_lease(lease_id, &series, streams)?;
            changes.push(StudyMarketLeaseChange {
                lease_id,
                series,
                streams,
                kind: StudyMarketLeaseChangeKind::Updated,
            });
        }

        for (series, streams) in acquire {
            if let Some((old_series, old_lease)) = reusable.pop() {
                engine.update_data_lease(old_lease.lease_id, &series, streams)?;
                changes.push(StudyMarketLeaseChange {
                    lease_id: old_lease.lease_id,
                    series: old_series,
                    streams: old_lease.streams,
                    kind: StudyMarketLeaseChangeKind::Released,
                });
                changes.push(StudyMarketLeaseChange {
                    lease_id: old_lease.lease_id,
                    series: series.clone(),
                    streams,
                    kind: StudyMarketLeaseChangeKind::Acquired,
                });
                planned.insert(
                    series,
                    StudyMarketLease {
                        lease_id: old_lease.lease_id,
                        streams,
                    },
                );
                continue;
            }
            let (lease_id, _) = engine.acquire_data_lease(&series, streams)?;
            changes.push(StudyMarketLeaseChange {
                lease_id,
                series: series.clone(),
                streams,
                kind: StudyMarketLeaseChangeKind::Acquired,
            });
            planned.insert(series, StudyMarketLease { lease_id, streams });
        }

        for (series, lease) in reusable {
            if engine.release_data_lease(lease.lease_id) {
                changes.push(StudyMarketLeaseChange {
                    lease_id: lease.lease_id,
                    series,
                    streams: lease.streams,
                    kind: StudyMarketLeaseChangeKind::Released,
                });
            }
        }

        self.market_leases = planned;
        Ok(changes)
    }

    /// Builds the deterministic recalculation plan for one canonical market-series mutation.
    ///
    /// # Errors
    /// Returns an error only when a trailing-window expansion overflows `usize`.
    pub fn plan_market_change(
        &self,
        series: &BarSeriesKey,
        range: StudyDirtyRange,
    ) -> Result<Vec<StudyCalculation>, StudyRuntimeError> {
        let mut dirty = BTreeMap::new();
        if let Some(dependents) = self.market_dependents.get(series) {
            for study_id in dependents {
                self.merge_invalidated_range(&mut dirty, *study_id, range)?;
            }
        }
        self.propagate_dirty(dirty)
    }

    /// Builds the downstream recalculation plan for one specific study output mutation.
    ///
    /// # Errors
    /// Returns an error for an unknown output or an overflowing trailing-window expansion.
    pub fn plan_output_change(
        &self,
        output: StudyOutputId,
        range: StudyDirtyRange,
    ) -> Result<Vec<StudyCalculation>, StudyRuntimeError> {
        self.validate_output(output)?;
        let mut dirty = BTreeMap::new();
        if let Some(dependents) = self.output_dependents.get(&output) {
            for study_id in dependents {
                self.merge_invalidated_range(&mut dirty, *study_id, range)?;
            }
        }
        self.propagate_dirty(dirty)
    }

    fn validate_definition(
        &self,
        owner: Option<ConsumerId>,
        definition: &StudyDefinition,
    ) -> Result<(), StudyRuntimeError> {
        let identifier = definition.identifier.trim();
        if identifier.is_empty() {
            return Err(StudyRuntimeError::InvalidIdentifier);
        }
        if identifier.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES {
            return Err(StudyRuntimeError::IdentifierTooLong {
                maximum: MAXIMUM_STUDY_IDENTIFIER_BYTES,
            });
        }
        if definition.dependencies.is_empty() {
            return Err(StudyRuntimeError::MissingDependency);
        }
        if definition.dependencies.len() > self.config.maximum_dependencies_per_study.get() {
            return Err(StudyRuntimeError::TooManyDependencies {
                maximum: self.config.maximum_dependencies_per_study.get(),
            });
        }
        if definition.outputs.is_empty() {
            return Err(StudyRuntimeError::MissingOutput);
        }
        if definition.outputs.len() > self.config.maximum_outputs_per_study.get() {
            return Err(StudyRuntimeError::TooManyOutputs {
                maximum: self.config.maximum_outputs_per_study.get(),
            });
        }
        let mut output_identifiers = BTreeSet::new();
        for output in &definition.outputs {
            let identifier = output.identifier.trim();
            if identifier.is_empty() {
                return Err(StudyRuntimeError::InvalidOutputIdentifier);
            }
            if output.title.trim().is_empty() {
                return Err(StudyRuntimeError::InvalidOutputTitle);
            }
            if identifier.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
                || output.title.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
            {
                return Err(StudyRuntimeError::OutputMetadataTooLong {
                    maximum: MAXIMUM_STUDY_IDENTIFIER_BYTES,
                });
            }
            if !output_identifiers.insert(identifier) {
                return Err(StudyRuntimeError::DuplicateOutputIdentifier);
            }
        }
        validate_setting_specs(&definition.settings)?;

        for dependency in &definition.dependencies {
            match dependency {
                StudyDependency::Market(input) => {
                    input
                        .series
                        .validate()
                        .map_err(|_| StudyRuntimeError::InvalidMarketSeries)?;
                    if input.streams.is_empty() {
                        return Err(StudyRuntimeError::EmptyMarketStreams);
                    }
                }
                StudyDependency::Output(output) => {
                    self.validate_output(*output)?;
                    let dependency_owner = self
                        .studies
                        .get(&output.study_id)
                        .map(|node| node.owner)
                        .ok_or(StudyRuntimeError::UnknownOutput(*output))?;
                    if dependency_owner != owner {
                        return Err(StudyRuntimeError::CrossConsumerDependency {
                            study_id: StudyInstanceId(
                                NonZeroU64::new(self.next_id)
                                    .ok_or(StudyRuntimeError::IdentifierExhausted)?,
                            ),
                            dependency: *output,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_output(&self, output: StudyOutputId) -> Result<(), StudyRuntimeError> {
        let Some(node) = self.studies.get(&output.study_id) else {
            return Err(StudyRuntimeError::UnknownOutput(output));
        };
        if output.output_index >= node.definition.outputs.len() {
            return Err(StudyRuntimeError::UnknownOutput(output));
        }
        Ok(())
    }

    fn merge_invalidated_range(
        &self,
        dirty: &mut BTreeMap<StudyInstanceId, StudyDirtyRange>,
        study_id: StudyInstanceId,
        source_range: StudyDirtyRange,
    ) -> Result<(), StudyRuntimeError> {
        let node = self
            .studies
            .get(&study_id)
            .ok_or(StudyRuntimeError::UnknownStudy(study_id))?;
        let invalidated = source_range.invalidated_by(node.definition.invalidation)?;
        dirty
            .entry(study_id)
            .and_modify(|range| *range = range.merge(invalidated))
            .or_insert(invalidated);
        Ok(())
    }

    fn propagate_dirty(
        &self,
        mut dirty: BTreeMap<StudyInstanceId, StudyDirtyRange>,
    ) -> Result<Vec<StudyCalculation>, StudyRuntimeError> {
        for (study_id, node) in &self.studies {
            let Some(source_range) = dirty.get(study_id).copied() else {
                continue;
            };
            for output_index in 0..node.definition.outputs.len() {
                let output = StudyOutputId {
                    study_id: *study_id,
                    output_index,
                };
                let Some(dependents) = self.output_dependents.get(&output) else {
                    continue;
                };
                for dependent in dependents {
                    self.merge_invalidated_range(&mut dirty, *dependent, source_range)?;
                }
            }
        }

        Ok(dirty
            .into_iter()
            .map(|(study_id, range)| StudyCalculation { study_id, range })
            .collect())
    }

    fn rebuild_dependency_indexes(&mut self) {
        self.market_dependents.clear();
        self.output_dependents.clear();
        for (study_id, node) in &self.studies {
            for dependency in &node.definition.dependencies {
                match dependency {
                    StudyDependency::Market(input) => {
                        self.market_dependents
                            .entry(input.series.clone())
                            .or_default()
                            .insert(*study_id);
                    }
                    StudyDependency::Output(output) => {
                        self.output_dependents
                            .entry(*output)
                            .or_default()
                            .insert(*study_id);
                    }
                }
            }
        }
    }

    fn remove_outputs_for_study(&mut self, study_id: StudyInstanceId) {
        let removed_points = self
            .outputs
            .iter()
            .filter(|(output, _)| output.study_id == study_id)
            .map(|(_, series)| series.len())
            .sum::<usize>();
        self.outputs.retain(|output, _| output.study_id != study_id);
        self.output_points = self.output_points.saturating_sub(removed_points);
    }
}

fn primary_timestamps(input: &ResolvedStudyInput) -> Arc<[i64]> {
    match input {
        ResolvedStudyInput::Market(series) => series
            .bars()
            .iter()
            .map(|bar| bar.exchange_timestamp_unix_nanos)
            .collect::<Vec<_>>()
            .into(),
        ResolvedStudyInput::Output(series) => Arc::clone(&series.timestamps),
    }
}

fn clamp_dirty_range(range: StudyDirtyRange, length: usize) -> Option<StudyDirtyRange> {
    if range.start >= length {
        return None;
    }
    let end = range.end_exclusive.unwrap_or(length).min(length);
    (range.start < end).then_some(StudyDirtyRange {
        start: range.start,
        end_exclusive: Some(end),
    })
}

fn bounded_execution_detail(mut detail: String) -> String {
    if detail.len() <= MAXIMUM_STUDY_EXECUTION_ERROR_BYTES {
        return detail;
    }
    let mut end = MAXIMUM_STUDY_EXECUTION_ERROR_BYTES;
    while !detail.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    detail.truncate(end);
    detail
}

#[cfg(test)]
mod tests {
    use super::*;
    use axiusflow_market_data::{BarPeriod, DepthSnapshot, EventMetadata, QualifiedTimestamp};
    use axiusflow_market_engine::{
        MarketEngineConfig, MarketStream, ProviderCapabilities, ProviderConfig, ProviderGeneration,
    };
    use std::time::Duration;

    fn bound(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test bounds are non-zero")
    }

    fn config(maximum_studies: usize) -> StudyRuntimeConfig {
        StudyRuntimeConfig {
            maximum_studies: bound(maximum_studies),
            maximum_dependencies_per_study: bound(8),
            maximum_outputs_per_study: bound(8),
            maximum_points_per_output: bound(64),
            maximum_total_output_points: bound(1_024),
            maximum_state_bytes_per_study: bound(1_024),
            maximum_total_state_bytes: bound(8_192),
        }
    }

    fn series(instrument: &str) -> BarSeriesKey {
        series_period(instrument, 60)
    }

    fn series_period(instrument: &str, seconds: u32) -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "provider".to_string(),
            instrument_id: instrument.to_string(),
            entitlement_id: "entitlement".to_string(),
            period: BarPeriod::Time { seconds },
            definition_version: 1,
        }
    }

    fn engine(maximum_series: usize) -> MarketEngine {
        let mut engine = MarketEngine::new(MarketEngineConfig {
            maximum_consumers: bound(4),
            maximum_series: bound(maximum_series),
            maximum_bars: bound(maximum_series.saturating_mul(64).max(1)),
        });
        engine
            .register_provider(
                "provider".to_string(),
                ProviderConfig {
                    account_id: "provider:test".to_string(),
                    capabilities: ProviderCapabilities {
                        historical_bars: true,
                        realtime_bars: true,
                        streams: StreamRequirements::BARS
                            .with(MarketStream::Trades)
                            .with(MarketStream::Quotes)
                            .with(MarketStream::Depth),
                    },
                    reconnect_delay: Duration::from_millis(250),
                },
            )
            .expect("test provider registers");
        engine
            .begin_provider_session("provider", ProviderGeneration(NonZeroU64::MIN))
            .expect("test provider session begins");
        engine
    }

    fn bars() -> Vec<MarketBar> {
        vec![
            MarketBar {
                source_sequence: 1,
                exchange_timestamp_seconds: 1_700_000_000,
                exchange_timestamp_unix_nanos: 1_700_000_000_000_000_000,
                open: 10_000,
                high: 11_000,
                low: 9_000,
                close: 10_500,
                volume: 1_250,
            },
            MarketBar {
                source_sequence: 2,
                exchange_timestamp_seconds: 1_700_000_060,
                exchange_timestamp_unix_nanos: 1_700_000_060_000_000_000,
                open: 10_500,
                high: 11_500,
                low: 10_000,
                close: 11_000,
                volume: 1_500,
            },
        ]
    }

    fn bar(source_sequence: u64, offset_seconds: i64, close: i64) -> MarketBar {
        let exchange_timestamp_seconds = 1_700_000_000 + offset_seconds;
        MarketBar {
            source_sequence,
            exchange_timestamp_seconds,
            exchange_timestamp_unix_nanos: exchange_timestamp_seconds * 1_000_000_000,
            open: close,
            high: close + 100,
            low: close - 100,
            close,
            volume: 1_000,
        }
    }

    fn event_metadata(source_sequence: u64, offset_seconds: i64) -> EventMetadata {
        let observed_unix_nanos = (1_700_000_000 + offset_seconds) * 1_000_000_000;
        EventMetadata {
            provider_id: "provider".to_string(),
            instrument_id: "ES".to_string(),
            entitlement_id: "entitlement".to_string(),
            source_sequence,
            session_generation: 1,
            timestamps: QualifiedTimestamp {
                exchange_unix_nanos: Some(observed_unix_nanos),
                provider_unix_nanos: None,
                received_unix_nanos: observed_unix_nanos + 1,
            },
        }
    }

    fn market(series: BarSeriesKey, streams: StreamRequirements) -> StudyDependency {
        StudyDependency::Market(StudyMarketInput { series, streams })
    }

    fn outputs(count: usize) -> Vec<StudyOutputSpec> {
        (0..count)
            .map(|index| StudyOutputSpec {
                identifier: format!("output_{index}"),
                title: format!("Output {}", index + 1),
                plot: StudyPlotKind::Line,
                pane: StudyPaneTarget::Price,
                scale: StudyScaleTarget::Primary,
            })
            .collect()
    }

    fn definition(
        identifier: &str,
        dependencies: Vec<StudyDependency>,
        output_count: usize,
        invalidation: StudyInvalidationPolicy,
    ) -> StudyDefinition {
        StudyDefinition {
            identifier: identifier.to_string(),
            dependencies,
            settings: Vec::new(),
            outputs: outputs(output_count),
            invalidation,
        }
    }

    fn calculate_scaled_close(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let (inputs, outputs) = context.split();
        let multiplier = match inputs.settings().get("multiplier") {
            Some(StudySettingValue::Integer(value)) => *value,
            _ => return Err("multiplier setting is unavailable".to_string()),
        };
        let close = match inputs.input(0) {
            Some(StudyInputSeries::Market(series)) => series.field(StudyBarField::Close),
            _ => return Err("primary market input is unavailable".to_string()),
        };
        let output = outputs
            .get_mut(0)
            .ok_or_else(|| "primary output is unavailable".to_string())?;
        let multiplier = f64::from(
            i32::try_from(multiplier).map_err(|_| "test multiplier is out of range".to_string())?,
        );
        let dirty = inputs.dirty_range();
        let end = dirty
            .end_exclusive
            .unwrap_or(output.len())
            .min(output.len());
        for index in dirty.start..end {
            let value = close
                .value(index)
                .ok_or_else(|| "close row is unavailable".to_string())?;
            let value = f64::from(
                i32::try_from(value).map_err(|_| "test close is out of range".to_string())?,
            );
            output.set(index, Some(value * multiplier))?;
        }
        Ok(())
    }

    fn calculate_double_output(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let (inputs, outputs) = context.split();
        let Some(StudyInputSeries::Output(source)) = inputs.input(0) else {
            return Err("upstream study output is unavailable".to_string());
        };
        let output = outputs
            .get_mut(0)
            .ok_or_else(|| "downstream output is unavailable".to_string())?;
        let dirty = inputs.dirty_range();
        let end = dirty
            .end_exclusive
            .unwrap_or(output.len())
            .min(output.len());
        for index in dirty.start..end {
            output.set(
                index,
                source.value(index).flatten().map(|value| value * 2.0),
            )?;
        }
        Ok(())
    }

    fn calculate_then_fail(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let (_, outputs) = context.split();
        let output = outputs
            .get_mut(0)
            .ok_or_else(|| "failure-test output is unavailable".to_string())?;
        if !output.is_empty() {
            output.set(0, Some(999.0))?;
        }
        Err("intentional calculation rejection".to_string())
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct TestNativeState {
        executions: u32,
        accounted_bytes: usize,
    }

    fn test_native_state_bytes(state: &TestNativeState) -> usize {
        state.accounted_bytes
    }

    fn panicking_test_native_state_bytes(_state: &TestNativeState) -> usize {
        panic!("intentional state accounting panic");
    }

    fn create_test_native_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
        let accounted_bytes = match settings.get("state_bytes") {
            Some(StudySettingValue::Integer(value)) => usize::try_from(*value)
                .map_err(|_| "test state byte setting is out of range".to_string())?,
            _ => return Err("test state byte setting is unavailable".to_string()),
        };
        Ok(NativeStudyState::new(
            TestNativeState {
                executions: 0,
                accounted_bytes,
            },
            test_native_state_bytes,
        ))
    }

    fn create_panicking_accounted_state(
        settings: &StudySettings,
    ) -> Result<NativeStudyState, String> {
        if settings.get("state_bytes").is_none() {
            return Err("test state byte setting is unavailable".to_string());
        }
        Ok(NativeStudyState::new(
            TestNativeState {
                executions: 0,
                accounted_bytes: 8,
            },
            panicking_test_native_state_bytes,
        ))
    }

    fn calculate_stateful_counter(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let (inputs, state, outputs) = context
            .split_with_state::<TestNativeState>()
            .ok_or_else(|| "test native state is unavailable".to_string())?;
        state.executions = state.executions.saturating_add(1);
        let output = outputs
            .get_mut(0)
            .ok_or_else(|| "stateful test output is unavailable".to_string())?;
        let dirty = inputs.dirty_range();
        let end = dirty
            .end_exclusive
            .unwrap_or(output.len())
            .min(output.len());
        for index in dirty.start..end {
            output.set(index, Some(f64::from(state.executions)))?;
        }
        Ok(())
    }

    fn calculate_stateful_then_fail(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let (_, state, outputs) = context
            .split_with_state::<TestNativeState>()
            .ok_or_else(|| "test native state is unavailable".to_string())?;
        state.executions = state.executions.saturating_add(100);
        if let Some(output) = outputs.first_mut()
            && !output.is_empty()
        {
            output.set(0, Some(999.0))?;
        }
        Err("intentional stateful rejection".to_string())
    }

    fn calculate_stateful_then_panic(
        context: &mut StudyExecutionContext<'_>,
    ) -> Result<(), String> {
        let state = context
            .state_mut::<TestNativeState>()
            .ok_or_else(|| "test native state is unavailable".to_string())?;
        state.executions = state.executions.saturating_add(100);
        panic!("intentional stateful panic");
    }

    fn calculate_stateful_growth(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let (_, state, outputs) = context
            .split_with_state::<TestNativeState>()
            .ok_or_else(|| "test native state is unavailable".to_string())?;
        state.executions = state.executions.saturating_add(1);
        state.accounted_bytes = 2_048;
        if let Some(output) = outputs.first_mut()
            && !output.is_empty()
        {
            output.set(0, Some(999.0))?;
        }
        Ok(())
    }

    fn stateful_registration(
        source: BarSeriesKey,
        accounted_bytes: usize,
    ) -> NativeStudyRegistration {
        stateful_registration_with_dependency(
            market(source, StreamRequirements::BARS),
            accounted_bytes,
        )
    }

    fn stateful_registration_with_dependency(
        dependency: StudyDependency,
        accounted_bytes: usize,
    ) -> NativeStudyRegistration {
        let definition = StudyDefinition {
            identifier: "stateful_counter".to_string(),
            dependencies: vec![dependency],
            settings: vec![StudySettingSpec {
                identifier: "state_bytes".to_string(),
                default: StudySettingValue::Integer(1),
            }],
            outputs: outputs(1),
            invalidation: StudyInvalidationPolicy::SameRange,
        };
        let settings = StudySettings::with_overrides(
            &definition.settings,
            BTreeMap::from([(
                "state_bytes".to_string(),
                StudySettingValue::Integer(
                    i64::try_from(accounted_bytes).expect("test state size fits i64"),
                ),
            )]),
        )
        .expect("stateful settings are valid");
        NativeStudyRegistration {
            settings,
            definition,
            program: NativeStudyProgram {
                calculate: calculate_stateful_counter,
                state_factory: Some(create_test_native_state),
            },
        }
    }

    fn committed_test_state(
        runtime: &mut StudyRuntime,
        study_id: StudyInstanceId,
    ) -> TestNativeState {
        runtime
            .studies
            .get_mut(&study_id)
            .and_then(|node| node.state.as_mut())
            .and_then(NativeStudyState::value_mut::<TestNativeState>)
            .cloned()
            .expect("test native state is committed")
    }

    fn calculate_secondary_asof(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let (inputs, outputs) = context.split();
        let Some(StudyInputSeries::Market(_primary)) = inputs.input(0) else {
            return Err("primary market input is unavailable".to_string());
        };
        let Some(StudyInputSeries::Market(secondary)) = inputs.input(1) else {
            return Err("secondary market input is unavailable".to_string());
        };
        let output = outputs
            .get_mut(0)
            .ok_or_else(|| "MTF output is unavailable".to_string())?;
        let dirty = inputs.dirty_range();
        let end = dirty
            .end_exclusive
            .unwrap_or(output.len())
            .min(output.len());
        for index in dirty.start..end {
            let timestamp = *inputs
                .timestamps()
                .get(index)
                .ok_or_else(|| "primary timestamp is unavailable".to_string())?;
            let insertion = secondary
                .bars()
                .partition_point(|bar| bar.exchange_timestamp_unix_nanos <= timestamp);
            let value = insertion
                .checked_sub(1)
                .and_then(|secondary_index| secondary.bars().get(secondary_index))
                .map(|bar| bar.close)
                .map(i32::try_from)
                .transpose()
                .map_err(|_| "secondary close is out of test range".to_string())?
                .map(f64::from);
            output.set(index, value)?;
        }
        Ok(())
    }

    fn calculate_live_microstructure(
        context: &mut StudyExecutionContext<'_>,
    ) -> Result<(), String> {
        let (inputs, outputs) = context.split();
        let live = inputs
            .live_market(0)
            .ok_or_else(|| "live market state is unavailable".to_string())?;
        let quote = live
            .quote()
            .ok_or_else(|| "quote is unavailable".to_string())?;
        let trades = live
            .trades()
            .ok_or_else(|| "trades are unavailable".to_string())?;
        let depth = live
            .depth()
            .ok_or_else(|| "depth is unavailable".to_string())?;
        let bid = quote
            .bid()
            .ok_or_else(|| "quote bid is unavailable".to_string())?
            .price;
        let ask = quote
            .ask()
            .ok_or_else(|| "quote ask is unavailable".to_string())?
            .price;
        let trade_quantity = trades
            .iter()
            .last()
            .ok_or_else(|| "retained trade is unavailable".to_string())?
            .quantity;
        let depth_bid = depth
            .bids()
            .next()
            .ok_or_else(|| "depth bid is unavailable".to_string())?
            .price;
        let value = i32::try_from((ask - bid) + trade_quantity + depth_bid)
            .map(f64::from)
            .map_err(|_| "microstructure test value is out of range".to_string())?;
        let output = outputs
            .first_mut()
            .ok_or_else(|| "microstructure output is unavailable".to_string())?;
        let dirty = inputs.dirty_range();
        let end = dirty
            .end_exclusive
            .unwrap_or(output.len())
            .min(output.len());
        for row in dirty.start..end {
            output.set(row, Some(value))?;
        }
        Ok(())
    }

    struct LiveMicrostructureFixture {
        book: OrderBook,
        trades: VecDeque<StudyTradeSample>,
        quote_one: TopOfBookQuote,
        quote_two: TopOfBookQuote,
    }

    fn live_microstructure_fixture() -> LiveMicrostructureFixture {
        let mut book = OrderBook::new(bound(8));
        book.install_snapshot(&DepthSnapshot {
            metadata: event_metadata(1, 90),
            bids: vec![DepthLevel {
                price: 95,
                quantity: 4,
                order_count: Some(1),
            }],
            asks: vec![DepthLevel {
                price: 140,
                quantity: 5,
                order_count: Some(1),
            }],
        })
        .expect("depth installs");
        let trades = VecDeque::from([StudyTradeSample {
            observed_unix_nanos: event_metadata(2, 90).timestamps.received_unix_nanos,
            price: 105,
            quantity: 3,
            aggressor: AggressorSide::Buy,
        }]);
        let quote_one = TopOfBookQuote {
            metadata: event_metadata(3, 90),
            bid: Some(DepthLevel {
                price: 100,
                quantity: 2,
                order_count: None,
            }),
            ask: Some(DepthLevel {
                price: 110,
                quantity: 2,
                order_count: None,
            }),
        };
        let quote_two = TopOfBookQuote {
            metadata: event_metadata(4, 90),
            ask: Some(DepthLevel {
                price: 130,
                quantity: 2,
                order_count: None,
            }),
            ..quote_one.clone()
        };
        LiveMicrostructureFixture {
            book,
            trades,
            quote_one,
            quote_two,
        }
    }

    fn scaled_close_registration(source: BarSeriesKey, multiplier: i64) -> NativeStudyRegistration {
        let definition = StudyDefinition {
            identifier: "scaled_close".to_string(),
            dependencies: vec![market(source, StreamRequirements::BARS)],
            settings: vec![StudySettingSpec {
                identifier: "multiplier".to_string(),
                default: StudySettingValue::Integer(1),
            }],
            outputs: outputs(1),
            invalidation: StudyInvalidationPolicy::SameRange,
        };
        let settings = StudySettings::with_overrides(
            &definition.settings,
            BTreeMap::from([(
                "multiplier".to_string(),
                StudySettingValue::Integer(multiplier),
            )]),
        )
        .expect("scaled close settings are valid");
        NativeStudyRegistration {
            definition,
            settings,
            program: NativeStudyProgram {
                calculate: calculate_scaled_close,
                state_factory: None,
            },
        }
    }

    struct NativeChainFixture {
        runtime: StudyRuntime,
        engine: MarketEngine,
        source: BarSeriesKey,
        provider_generation: ProviderGeneration,
        producer: StudyInstanceId,
        consumer: StudyInstanceId,
    }

    fn native_chain(multiplier: i64) -> NativeChainFixture {
        let mut runtime = StudyRuntime::new(config(8));
        let mut engine = engine(2);
        let source = series("ES");
        let provider_generation = ProviderGeneration(NonZeroU64::MIN);
        engine
            .install_history(provider_generation, &source, 2, 3, bars())
            .expect("canonical history installs");
        let owner = ConsumerId(NonZeroU64::MIN);
        let producer = runtime
            .register_native_for_consumer(
                owner,
                scaled_close_registration(source.clone(), multiplier),
            )
            .expect("producer registers");
        let consumer_definition = StudyDefinition {
            identifier: "double_scaled_close".to_string(),
            dependencies: vec![StudyDependency::Output(StudyOutputId {
                study_id: producer,
                output_index: 0,
            })],
            settings: Vec::new(),
            outputs: outputs(1),
            invalidation: StudyInvalidationPolicy::SameRange,
        };
        let consumer = runtime
            .register_native_for_consumer(
                owner,
                NativeStudyRegistration {
                    settings: StudySettings::defaults(&consumer_definition.settings)
                        .expect("consumer defaults"),
                    definition: consumer_definition,
                    program: NativeStudyProgram {
                        calculate: calculate_double_output,
                        state_factory: None,
                    },
                },
            )
            .expect("consumer registers");
        runtime
            .execute_ready_for_market(&engine, &source)
            .expect("initial chain executes");
        NativeChainFixture {
            runtime,
            engine,
            source,
            provider_generation,
            producer,
            consumer,
        }
    }

    struct MtfFixture {
        runtime: StudyRuntime,
        engine: MarketEngine,
        secondary: BarSeriesKey,
        provider_generation: ProviderGeneration,
        study: StudyInstanceId,
        output: StudyOutputId,
    }

    fn mtf_fixture() -> MtfFixture {
        let mut runtime = StudyRuntime::new(config(4));
        let mut engine = engine(2);
        let provider_generation = ProviderGeneration(NonZeroU64::MIN);
        let primary = series_period("ES-1m", 60);
        let secondary = series_period("ES-5m", 300);
        engine
            .install_history(
                provider_generation,
                &primary,
                2,
                3,
                (0..7)
                    .map(|index| {
                        bar(
                            u64::try_from(index + 1).expect("small sequence"),
                            i64::from(index) * 60,
                            10_000 + i64::from(index) * 100,
                        )
                    })
                    .collect(),
            )
            .expect("primary history installs");
        engine
            .install_history(
                provider_generation,
                &secondary,
                2,
                3,
                vec![bar(1, 0, 20_000)],
            )
            .expect("secondary history installs");
        let definition = StudyDefinition {
            identifier: "mtf_asof".to_string(),
            dependencies: vec![
                market(primary, StreamRequirements::BARS),
                market(secondary.clone(), StreamRequirements::BARS),
            ],
            settings: Vec::new(),
            outputs: outputs(1),
            invalidation: StudyInvalidationPolicy::SameRange,
        };
        let study = runtime
            .register_native_for_consumer(
                ConsumerId(NonZeroU64::MIN),
                NativeStudyRegistration {
                    settings: StudySettings::defaults(&definition.settings).expect("defaults"),
                    definition,
                    program: NativeStudyProgram {
                        calculate: calculate_secondary_asof,
                        state_factory: None,
                    },
                },
            )
            .expect("MTF study registers");
        assert!(
            runtime
                .execute_ready(&engine, study)
                .expect("initial MTF run")
        );
        MtfFixture {
            runtime,
            engine,
            secondary,
            provider_generation,
            study,
            output: StudyOutputId {
                study_id: study,
                output_index: 0,
            },
        }
    }

    #[test]
    fn typed_settings_apply_only_declared_same_type_overrides() {
        let specs = vec![
            StudySettingSpec {
                identifier: "length".to_string(),
                default: StudySettingValue::Integer(20),
            },
            StudySettingSpec {
                identifier: "threshold".to_string(),
                default: StudySettingValue::Decimal(StudyDecimal {
                    mantissa: 125,
                    scale: 2,
                }),
            },
            StudySettingSpec {
                identifier: "enabled".to_string(),
                default: StudySettingValue::Boolean(true),
            },
        ];
        let settings = StudySettings::with_overrides(
            &specs,
            BTreeMap::from([("length".to_string(), StudySettingValue::Integer(50))]),
        )
        .expect("typed override is valid");
        assert_eq!(
            settings.get("length"),
            Some(&StudySettingValue::Integer(50))
        );
        assert_eq!(
            settings.get("threshold"),
            Some(&StudySettingValue::Decimal(StudyDecimal {
                mantissa: 125,
                scale: 2,
            }))
        );
        assert_eq!(
            settings.get("enabled"),
            Some(&StudySettingValue::Boolean(true))
        );

        assert_eq!(
            StudySettings::with_overrides(
                &specs,
                BTreeMap::from([("length".to_string(), StudySettingValue::Boolean(false),)]),
            )
            .expect_err("type mismatch must fail"),
            StudyRuntimeError::SettingTypeMismatch
        );
        assert_eq!(
            StudySettings::with_overrides(
                &specs,
                BTreeMap::from([("unknown".to_string(), StudySettingValue::Integer(1))]),
            )
            .expect_err("unknown setting must fail"),
            StudyRuntimeError::UnknownSetting
        );
    }

    #[test]
    fn native_execution_composes_market_and_study_outputs_in_dependency_order() {
        let fixture = native_chain(3);
        let producer_output = fixture
            .runtime
            .output_series(StudyOutputId {
                study_id: fixture.producer,
                output_index: 0,
            })
            .expect("producer output");
        assert_eq!(producer_output.values(), &[Some(31_500.0), Some(33_000.0)]);
        assert_eq!(producer_output.generation(), 1);
        assert_eq!(
            producer_output.timestamps(),
            &[1_700_000_000_000_000_000, 1_700_000_060_000_000_000,]
        );
        let consumer_output = fixture
            .runtime
            .output_series(StudyOutputId {
                study_id: fixture.consumer,
                output_index: 0,
            })
            .expect("consumer output");
        assert_eq!(consumer_output.values(), &[Some(63_000.0), Some(66_000.0)]);
        assert_eq!(consumer_output.generation(), 2);
        assert_eq!(consumer_output.timestamps(), producer_output.timestamps());
    }

    #[test]
    fn non_bar_live_change_recomputes_the_containing_tail_row_from_borrowed_market_state() {
        let mut runtime = StudyRuntime::new(config(4));
        let mut engine = engine(1);
        let source = series("ES");
        engine
            .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 0, bars())
            .expect("canonical history installs");
        let streams = StreamRequirements::BARS
            .with(MarketStream::Trades)
            .with(MarketStream::Quotes)
            .with(MarketStream::Depth);
        let definition = definition(
            "live_microstructure",
            vec![market(source.clone(), streams)],
            1,
            StudyInvalidationPolicy::FromFirstChanged,
        );
        let study = runtime
            .register_native_for_consumer(
                ConsumerId(NonZeroU64::MIN),
                NativeStudyRegistration {
                    settings: StudySettings::defaults(&definition.settings).expect("defaults"),
                    definition,
                    program: NativeStudyProgram::stateless(calculate_live_microstructure),
                },
            )
            .expect("study registers");

        let live = live_microstructure_fixture();

        let first_live = StudyLiveMarketData::new(
            Some(StudyQuoteView::new(&live.quote_one, 2, 0)),
            Some(StudyTradeWindow::new(&live.trades, 1, 2, 2, 0)),
            Some(StudyDepthView::new(&live.book, 2, 0)),
        );
        let mut first_lookup = |_input: &StudyMarketInput| Some(first_live);
        assert!(
            runtime
                .execute_ready_with_live(&engine, study, &mut first_lookup)
                .expect("initial execution")
        );
        assert_eq!(
            runtime
                .output_series(study.output(0))
                .expect("initial output")
                .values(),
            &[Some(108.0), Some(108.0)]
        );

        let second_live = StudyLiveMarketData::new(
            Some(StudyQuoteView::new(&live.quote_two, 2, 0)),
            Some(StudyTradeWindow::new(&live.trades, 1, 2, 2, 0)),
            Some(StudyDepthView::new(&live.book, 2, 0)),
        );
        let mut second_lookup = |_input: &StudyMarketInput| Some(second_live);
        assert_eq!(
            runtime
                .execute_live_non_bar_change_with_live(
                    &engine,
                    StudyNonBarChange {
                        provider_id: "provider",
                        instrument_id: "ES",
                        entitlement_id: "entitlement",
                        stream: MarketStream::Quotes,
                        observed_unix_nanos: event_metadata(4, 90)
                            .timestamps
                            .exchange_unix_nanos
                            .expect("exchange timestamp"),
                    },
                    &mut second_lookup,
                )
                .expect("quote change recalculates"),
            vec![study]
        );
        assert_eq!(
            runtime
                .output_series(study.output(0))
                .expect("updated output")
                .values(),
            &[Some(108.0), Some(128.0)]
        );
    }

    #[test]
    fn native_reinitialization_preserves_identity_invalidates_downstream_and_reexecutes() {
        let mut fixture = native_chain(3);
        let owner = ConsumerId(NonZeroU64::MIN);

        let affected = fixture
            .runtime
            .reinitialize_native_for_consumer(
                owner,
                fixture.producer,
                scaled_close_registration(fixture.source.clone(), 5),
            )
            .expect("producer reinitializes in place");
        assert_eq!(affected, vec![fixture.producer, fixture.consumer]);
        assert!(
            fixture
                .runtime
                .output_series(fixture.producer.output(0))
                .is_none()
        );
        assert!(
            fixture
                .runtime
                .output_series(fixture.consumer.output(0))
                .is_none()
        );

        assert_eq!(
            fixture
                .runtime
                .execute_ready_subtree(&fixture.engine, fixture.producer)
                .expect("reinitialized subtree executes"),
            vec![fixture.producer, fixture.consumer]
        );
        assert_eq!(
            fixture
                .runtime
                .output_series(fixture.producer.output(0))
                .expect("producer output")
                .values(),
            &[Some(52_500.0), Some(55_000.0)]
        );
        assert_eq!(
            fixture
                .runtime
                .output_series(fixture.consumer.output(0))
                .expect("consumer output")
                .values(),
            &[Some(105_000.0), Some(110_000.0)]
        );
    }

    #[test]
    fn native_reinitialization_rejects_output_interface_changes_and_dependency_cycles() {
        let mut fixture = native_chain(3);
        let owner = ConsumerId(NonZeroU64::MIN);
        let mut changed_interface = scaled_close_registration(fixture.source.clone(), 5);
        changed_interface.definition.outputs[0].identifier = "replacement".to_string();
        assert_eq!(
            fixture
                .runtime
                .reinitialize_native_for_consumer(owner, fixture.producer, changed_interface)
                .expect_err("output identities stay stable"),
            StudyRuntimeError::OutputInterfaceChanged(fixture.producer)
        );
        assert!(
            fixture
                .runtime
                .output_series(fixture.producer.output(0))
                .is_some()
        );

        let mut self_cycle = fixture
            .runtime
            .native_registration(fixture.consumer)
            .expect("consumer registration is retained");
        self_cycle.definition.dependencies =
            vec![StudyDependency::Output(fixture.consumer.output(0))];
        assert_eq!(
            fixture
                .runtime
                .reinitialize_native_for_consumer(owner, fixture.consumer, self_cycle)
                .expect_err("self dependency is a cycle"),
            StudyRuntimeError::DependencyOrderViolation {
                study_id: fixture.consumer,
                dependency: fixture.consumer.output(0),
            }
        );
    }

    #[test]
    fn live_append_and_revision_recompute_only_timestamp_mapped_rows_through_the_dag() {
        let mut fixture = native_chain(3);

        let appended = MarketBar {
            source_sequence: 3,
            exchange_timestamp_seconds: 1_700_000_120,
            exchange_timestamp_unix_nanos: 1_700_000_120_000_000_000,
            open: 11_000,
            high: 12_000,
            low: 10_500,
            close: 11_500,
            volume: 1_750,
        };
        fixture
            .engine
            .install_realtime_tail(
                fixture.provider_generation,
                &fixture.source,
                2,
                3,
                appended,
                true,
            )
            .expect("canonical append installs");
        assert_eq!(
            fixture
                .runtime
                .execute_live_market_change(
                    &fixture.engine,
                    &fixture.source,
                    appended.exchange_timestamp_unix_nanos,
                )
                .expect("append recalculates"),
            vec![fixture.producer, fixture.consumer]
        );
        let producer_id = StudyOutputId {
            study_id: fixture.producer,
            output_index: 0,
        };
        let consumer_id = StudyOutputId {
            study_id: fixture.consumer,
            output_index: 0,
        };
        assert_eq!(
            fixture
                .runtime
                .output_series(producer_id)
                .expect("producer")
                .values(),
            &[Some(31_500.0), Some(33_000.0), Some(34_500.0)]
        );
        assert_eq!(
            fixture
                .runtime
                .output_series(consumer_id)
                .expect("consumer")
                .values(),
            &[Some(63_000.0), Some(66_000.0), Some(69_000.0)]
        );

        let revised = MarketBar {
            close: 12_000,
            high: 12_250,
            ..appended
        };
        fixture
            .engine
            .install_realtime_tail(
                fixture.provider_generation,
                &fixture.source,
                2,
                3,
                revised,
                true,
            )
            .expect("canonical revision installs");
        assert_eq!(
            fixture
                .runtime
                .execute_live_market_change(
                    &fixture.engine,
                    &fixture.source,
                    revised.exchange_timestamp_unix_nanos,
                )
                .expect("revision recalculates"),
            vec![fixture.producer, fixture.consumer]
        );
        assert_eq!(
            fixture
                .runtime
                .output_series(producer_id)
                .expect("producer")
                .values(),
            &[Some(31_500.0), Some(33_000.0), Some(36_000.0)]
        );
        assert_eq!(
            fixture
                .runtime
                .output_series(consumer_id)
                .expect("consumer")
                .values(),
            &[Some(63_000.0), Some(66_000.0), Some(72_000.0)]
        );
    }

    #[test]
    fn secondary_timeframe_changes_map_to_primary_rows_by_timestamp_not_row_index() {
        let mut fixture = mtf_fixture();
        assert_eq!(
            fixture
                .runtime
                .output_series(fixture.output)
                .expect("MTF output")
                .values(),
            &[Some(20_000.0); 7]
        );

        let secondary_append = bar(2, 300, 21_000);
        fixture
            .engine
            .install_realtime_tail(
                fixture.provider_generation,
                &fixture.secondary,
                2,
                3,
                secondary_append,
                true,
            )
            .expect("secondary append installs");
        assert_eq!(
            fixture
                .runtime
                .execute_live_market_change(
                    &fixture.engine,
                    &fixture.secondary,
                    secondary_append.exchange_timestamp_unix_nanos,
                )
                .expect("secondary append recalculates"),
            vec![fixture.study]
        );
        assert_eq!(
            fixture
                .runtime
                .output_series(fixture.output)
                .expect("MTF output")
                .values(),
            &[
                Some(20_000.0),
                Some(20_000.0),
                Some(20_000.0),
                Some(20_000.0),
                Some(20_000.0),
                Some(21_000.0),
                Some(21_000.0),
            ]
        );

        let secondary_revision = MarketBar {
            close: 22_000,
            high: 22_100,
            low: 21_900,
            open: 22_000,
            ..secondary_append
        };
        fixture
            .engine
            .install_realtime_tail(
                fixture.provider_generation,
                &fixture.secondary,
                2,
                3,
                secondary_revision,
                true,
            )
            .expect("secondary revision installs");
        fixture
            .runtime
            .execute_live_market_change(
                &fixture.engine,
                &fixture.secondary,
                secondary_revision.exchange_timestamp_unix_nanos,
            )
            .expect("secondary revision recalculates");
        assert_eq!(
            fixture
                .runtime
                .output_series(fixture.output)
                .expect("MTF output")
                .values(),
            &[
                Some(20_000.0),
                Some(20_000.0),
                Some(20_000.0),
                Some(20_000.0),
                Some(20_000.0),
                Some(22_000.0),
                Some(22_000.0),
            ]
        );
    }

    #[test]
    fn rejected_native_execution_preserves_the_last_committed_output() {
        let mut runtime = StudyRuntime::new(config(4));
        let mut engine = engine(1);
        let source = series("ES");
        engine
            .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
            .expect("canonical history installs");
        let study = runtime
            .register_native_for_consumer(
                ConsumerId(NonZeroU64::MIN),
                scaled_close_registration(source, 2),
            )
            .expect("study registers");
        assert!(
            runtime
                .execute_ready(&engine, study)
                .expect("first execution")
        );
        let output_id = StudyOutputId {
            study_id: study,
            output_index: 0,
        };
        let committed = runtime
            .output_series(output_id)
            .expect("first output")
            .clone();
        runtime
            .studies
            .get_mut(&study)
            .expect("study remains live")
            .program = Some(NativeStudyProgram {
            calculate: calculate_then_fail,
            state_factory: None,
        });

        assert!(matches!(
            runtime.execute_ready(&engine, study),
            Err(StudyRuntimeError::ExecutionRejected { study_id, .. }) if study_id == study
        ));
        assert_eq!(runtime.output_series(output_id), Some(&committed));
    }

    #[test]
    fn stateful_execution_commits_incrementally_and_covering_execution_resets_state() {
        let mut runtime = StudyRuntime::new(config(4));
        let mut engine = engine(1);
        let source = series("ES");
        engine
            .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
            .expect("canonical history installs");
        let study = runtime
            .register_native_for_consumer(
                ConsumerId(NonZeroU64::MIN),
                stateful_registration(source, 8),
            )
            .expect("stateful study registers");

        assert!(
            runtime
                .execute_ready(&engine, study)
                .expect("full execution")
        );
        assert_eq!(committed_test_state(&mut runtime, study).executions, 1);
        assert_eq!(
            runtime
                .output_series(study.output(0))
                .expect("stateful output")
                .values(),
            &[Some(1.0), Some(1.0)]
        );

        assert!(
            runtime
                .execute_ready_range(
                    &engine,
                    study,
                    Some(StudyDirtyRange::bounded(1, 2).expect("dirty row")),
                )
                .expect("incremental execution")
        );
        assert_eq!(committed_test_state(&mut runtime, study).executions, 2);
        assert_eq!(
            runtime
                .output_series(study.output(0))
                .expect("incremental output")
                .values(),
            &[Some(1.0), Some(2.0)]
        );

        assert!(
            runtime
                .execute_ready_range(
                    &engine,
                    study,
                    Some(StudyDirtyRange::bounded(0, 2).expect("covering dirty range")),
                )
                .expect("explicit covering execution")
        );
        assert_eq!(committed_test_state(&mut runtime, study).executions, 1);
        assert_eq!(
            runtime
                .output_series(study.output(0))
                .expect("rebuilt output")
                .values(),
            &[Some(1.0), Some(1.0)]
        );
    }

    #[test]
    fn rejected_and_panicking_stateful_execution_preserve_state_output_and_generation() {
        let mut runtime = StudyRuntime::new(config(4));
        let mut engine = engine(1);
        let source = series("ES");
        engine
            .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
            .expect("canonical history installs");
        let study = runtime
            .register_native_for_consumer(
                ConsumerId(NonZeroU64::MIN),
                stateful_registration(source, 8),
            )
            .expect("stateful study registers");
        runtime
            .execute_ready(&engine, study)
            .expect("initial state commits");
        let committed_output = runtime
            .output_series(study.output(0))
            .expect("stateful output")
            .clone();
        let committed_generation = committed_output.generation();

        runtime
            .studies
            .get_mut(&study)
            .and_then(|node| node.program.as_mut())
            .expect("native program")
            .calculate = calculate_stateful_then_fail;
        assert!(matches!(
            runtime.execute_ready_range(
                &engine,
                study,
                Some(StudyDirtyRange::bounded(0, 1).expect("dirty row")),
            ),
            Err(StudyRuntimeError::ExecutionRejected { study_id, .. }) if study_id == study
        ));
        assert_eq!(committed_test_state(&mut runtime, study).executions, 1);
        assert_eq!(
            runtime.output_series(study.output(0)),
            Some(&committed_output)
        );
        assert_eq!(
            runtime
                .output_series(study.output(0))
                .expect("committed output")
                .generation(),
            committed_generation
        );

        runtime
            .studies
            .get_mut(&study)
            .and_then(|node| node.program.as_mut())
            .expect("native program")
            .calculate = calculate_stateful_then_panic;
        assert!(matches!(
            runtime.execute_ready_range(
                &engine,
                study,
                Some(StudyDirtyRange::bounded(0, 1).expect("dirty row")),
            ),
            Err(StudyRuntimeError::ExecutionRejected { study_id, .. }) if study_id == study
        ));
        assert_eq!(committed_test_state(&mut runtime, study).executions, 1);
        assert_eq!(
            runtime.output_series(study.output(0)),
            Some(&committed_output)
        );
    }

    #[test]
    fn state_growth_beyond_limit_is_transactional() {
        let mut runtime = StudyRuntime::new(config(4));
        let mut engine = engine(1);
        let source = series("ES");
        engine
            .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
            .expect("canonical history installs");
        let study = runtime
            .register_native_for_consumer(
                ConsumerId(NonZeroU64::MIN),
                stateful_registration(source, 8),
            )
            .expect("stateful study registers");
        runtime
            .execute_ready(&engine, study)
            .expect("initial state commits");
        let committed_output = runtime
            .output_series(study.output(0))
            .expect("stateful output")
            .clone();
        runtime
            .studies
            .get_mut(&study)
            .and_then(|node| node.program.as_mut())
            .expect("native program")
            .calculate = calculate_stateful_growth;

        assert_eq!(
            runtime
                .execute_ready_range(
                    &engine,
                    study,
                    Some(StudyDirtyRange::bounded(0, 1).expect("dirty row")),
                )
                .expect_err("oversized candidate state must not commit"),
            StudyRuntimeError::StateMemoryLimitExceeded {
                maximum: 1_024,
                requested: 2_048,
            }
        );
        assert_eq!(committed_test_state(&mut runtime, study).executions, 1);
        assert_eq!(committed_test_state(&mut runtime, study).accounted_bytes, 8);
        assert_eq!(
            runtime.output_series(study.output(0)),
            Some(&committed_output)
        );
        assert_eq!(runtime.state_bytes, 8);
    }

    #[test]
    fn state_memory_bounds_and_removal_accounting_are_enforced() {
        let mut runtime = StudyRuntime::new(StudyRuntimeConfig {
            maximum_studies: bound(4),
            maximum_dependencies_per_study: bound(2),
            maximum_outputs_per_study: bound(2),
            maximum_points_per_output: bound(64),
            maximum_total_output_points: bound(256),
            maximum_state_bytes_per_study: bound(8),
            maximum_total_state_bytes: bound(12),
        });
        let owner = ConsumerId(NonZeroU64::MIN);
        assert_eq!(
            runtime
                .register_native_for_consumer(owner, stateful_registration(series("ES"), 9))
                .expect_err("per-study state bound"),
            StudyRuntimeError::StateMemoryLimitExceeded {
                maximum: 8,
                requested: 9,
            }
        );
        let es = runtime
            .register_native_for_consumer(owner, stateful_registration(series("ES"), 4))
            .expect("first state fits");
        runtime
            .register_native_for_consumer(owner, stateful_registration(series("NQ"), 8))
            .expect("second state exactly fills total budget");
        assert_eq!(runtime.state_bytes, 12);
        assert_eq!(
            runtime
                .register_native_for_consumer(owner, stateful_registration(series("YM"), 1))
                .expect_err("global state bound"),
            StudyRuntimeError::TotalStateMemoryLimitExceeded {
                maximum: 12,
                requested: 13,
            }
        );

        assert_eq!(
            runtime
                .remove_subtree(es)
                .expect("stateful subtree removes"),
            vec![es]
        );
        assert_eq!(runtime.state_bytes, 8);
        runtime
            .register_native_for_consumer(owner, stateful_registration(series("YM"), 1))
            .expect("released state budget is reusable");
        assert_eq!(runtime.state_bytes, 9);
    }

    #[test]
    fn panicking_state_memory_accounting_is_rejected_without_unwinding() {
        let mut runtime = StudyRuntime::new(config(4));
        let mut registration = stateful_registration(series("ES"), 8);
        registration.program.state_factory = Some(create_panicking_accounted_state);

        assert_eq!(
            runtime
                .register_native_for_consumer(ConsumerId(NonZeroU64::MIN), registration)
                .expect_err("accounting panic must be contained"),
            StudyRuntimeError::StateInitializationRejected {
                detail: "native study state accounting panicked".to_string(),
            }
        );
        assert!(runtime.is_empty());
        assert_eq!(runtime.state_bytes, 0);
    }

    #[test]
    fn reinitialization_resets_state_for_the_entire_dependent_subtree() {
        let mut runtime = StudyRuntime::new(config(4));
        let mut engine = engine(1);
        let source = series("ES");
        engine
            .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
            .expect("canonical history installs");
        let owner = ConsumerId(NonZeroU64::MIN);
        let root = runtime
            .register_native_for_consumer(owner, stateful_registration(source.clone(), 8))
            .expect("root registers");
        let dependent = runtime
            .register_native_for_consumer(
                owner,
                stateful_registration_with_dependency(StudyDependency::Output(root.output(0)), 8),
            )
            .expect("dependent registers");
        runtime
            .execute_ready_subtree(&engine, root)
            .expect("initial subtree execution");
        assert_eq!(committed_test_state(&mut runtime, root).executions, 1);
        assert_eq!(committed_test_state(&mut runtime, dependent).executions, 1);

        runtime
            .execute_ready_range(
                &engine,
                dependent,
                Some(StudyDirtyRange::bounded(0, 1).expect("dirty row")),
            )
            .expect("dependent incremental execution");
        assert_eq!(committed_test_state(&mut runtime, dependent).executions, 2);

        let affected = runtime
            .reinitialize_native_for_consumer(owner, root, stateful_registration(source, 8))
            .expect("root reinitializes");
        assert_eq!(affected, vec![root, dependent]);
        assert_eq!(committed_test_state(&mut runtime, root).executions, 0);
        assert_eq!(committed_test_state(&mut runtime, dependent).executions, 0);
        assert!(runtime.output_series(root.output(0)).is_none());
        assert!(runtime.output_series(dependent.output(0)).is_none());

        runtime
            .execute_ready_subtree(&engine, root)
            .expect("reinitialized subtree executes");
        assert_eq!(committed_test_state(&mut runtime, root).executions, 1);
        assert_eq!(committed_test_state(&mut runtime, dependent).executions, 1);
    }

    #[test]
    fn subtree_checkpoint_restores_state_outputs_without_rewinding_generation() {
        let mut runtime = StudyRuntime::new(config(4));
        let mut engine = engine(1);
        let source = series("ES");
        engine
            .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
            .expect("canonical history installs");
        let owner = ConsumerId(NonZeroU64::MIN);
        let root = runtime
            .register_native_for_consumer(owner, stateful_registration(source, 8))
            .expect("root registers");
        let dependent = runtime
            .register_native_for_consumer(
                owner,
                stateful_registration_with_dependency(StudyDependency::Output(root.output(0)), 8),
            )
            .expect("dependent registers");
        runtime
            .execute_ready_subtree(&engine, root)
            .expect("initial subtree execution");
        let root_output = runtime
            .output_series(root.output(0))
            .expect("root output")
            .clone();
        let dependent_output = runtime
            .output_series(dependent.output(0))
            .expect("dependent output")
            .clone();
        let checkpoint = runtime
            .checkpoint_subtree(root)
            .expect("subtree checkpoint");
        let generation_before_mutation = runtime.next_output_generation;

        runtime
            .execute_ready_range(
                &engine,
                root,
                Some(StudyDirtyRange::bounded(0, 1).expect("dirty row")),
            )
            .expect("root incremental execution");
        runtime
            .execute_ready_range(
                &engine,
                dependent,
                Some(StudyDirtyRange::bounded(0, 1).expect("dirty row")),
            )
            .expect("dependent incremental execution");
        let generation_after_mutation = runtime.next_output_generation;
        assert!(generation_after_mutation > generation_before_mutation);
        assert_eq!(committed_test_state(&mut runtime, root).executions, 2);
        assert_eq!(committed_test_state(&mut runtime, dependent).executions, 2);

        runtime.restore_subtree_checkpoint(checkpoint);
        assert_eq!(committed_test_state(&mut runtime, root).executions, 1);
        assert_eq!(committed_test_state(&mut runtime, dependent).executions, 1);
        assert_eq!(runtime.output_series(root.output(0)), Some(&root_output));
        assert_eq!(
            runtime.output_series(dependent.output(0)),
            Some(&dependent_output)
        );
        assert_eq!(runtime.next_output_generation, generation_after_mutation);
    }

    #[test]
    fn native_outputs_obey_per_output_and_global_point_bounds() {
        let mut runtime = StudyRuntime::new(StudyRuntimeConfig {
            maximum_studies: bound(2),
            maximum_dependencies_per_study: bound(2),
            maximum_outputs_per_study: bound(2),
            maximum_points_per_output: bound(1),
            maximum_total_output_points: bound(2),
            maximum_state_bytes_per_study: bound(1_024),
            maximum_total_state_bytes: bound(2_048),
        });
        let mut engine = engine(1);
        let source = series("ES");
        engine
            .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
            .expect("canonical history installs");
        let study = runtime
            .register_native_for_consumer(
                ConsumerId(NonZeroU64::MIN),
                scaled_close_registration(source, 1),
            )
            .expect("study registers");
        assert_eq!(
            runtime
                .execute_ready(&engine, study)
                .expect_err("two canonical rows exceed one-row output bound"),
            StudyRuntimeError::OutputPointLimitExceeded {
                maximum: 1,
                requested: 2,
            }
        );
        assert!(
            runtime
                .output_series(StudyOutputId {
                    study_id: study,
                    output_index: 0,
                })
                .is_none()
        );
    }

    #[test]
    fn shared_market_requirements_union_without_owning_provider_work() {
        let mut runtime = StudyRuntime::new(config(8));
        let source = series("ES");
        runtime
            .register(definition(
                "close_sma",
                vec![market(source.clone(), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::TrailingWindow { bars: bound(20) },
            ))
            .expect("first study registers");
        runtime
            .register(definition(
                "trade_pressure",
                vec![market(
                    source.clone(),
                    StreamRequirements::BARS.with(MarketStream::Trades),
                )],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("second study registers");

        let requirements = runtime.market_requirements();
        assert_eq!(requirements.len(), 1);
        let streams = requirements.get(&source).expect("shared market request");
        assert!(streams.contains(MarketStream::Bars));
        assert!(streams.contains(MarketStream::Trades));
        assert!(!streams.contains(MarketStream::Depth));
    }

    #[test]
    fn study_market_dependencies_reconcile_to_one_engine_lease_per_series() {
        let mut runtime = StudyRuntime::new(config(8));
        let mut engine = engine(4);
        let source = series("ES");
        let bars = runtime
            .register(definition(
                "bars",
                vec![market(source.clone(), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("bars study registers");
        let pressure = runtime
            .register(definition(
                "pressure",
                vec![market(
                    source.clone(),
                    StreamRequirements::BARS.with(MarketStream::Trades),
                )],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("pressure study registers");

        let changes = runtime
            .reconcile_market_leases(&mut engine)
            .expect("study demand reconciles");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, StudyMarketLeaseChangeKind::Acquired);
        let lease_id = changes[0].lease_id;
        let shared = StreamRequirements::BARS.with(MarketStream::Trades);
        assert_eq!(engine.data_lease_count(), 1);
        assert_eq!(engine.data_lease(lease_id), Some((&source, shared)));
        assert_eq!(
            engine
                .subscription_status(&source)
                .map(|status| status.consumer_count),
            Some(0)
        );
        assert_eq!(
            engine
                .subscription_status(&source)
                .map(|status| status.streams),
            Some(shared)
        );

        assert_eq!(
            runtime.remove_subtree(pressure).expect("pressure removes"),
            vec![pressure]
        );
        let changes = runtime
            .reconcile_market_leases(&mut engine)
            .expect("streams narrow in place");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].lease_id, lease_id);
        assert_eq!(changes[0].kind, StudyMarketLeaseChangeKind::Updated);
        assert_eq!(
            engine.data_lease(lease_id),
            Some((&source, StreamRequirements::BARS))
        );

        assert_eq!(
            runtime.remove_subtree(bars).expect("bars removes"),
            vec![bars]
        );
        let changes = runtime
            .reconcile_market_leases(&mut engine)
            .expect("last study releases demand");
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].lease_id, lease_id);
        assert_eq!(changes[0].kind, StudyMarketLeaseChangeKind::Released);
        assert_eq!(engine.data_lease_count(), 0);
        assert!(!engine.has_subscription(&source));
    }

    #[test]
    fn study_market_lease_capacity_preflight_does_not_partially_mutate_engine() {
        let mut runtime = StudyRuntime::new(config(8));
        let mut engine = engine(1);
        runtime
            .register(definition(
                "es",
                vec![market(series("ES"), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("ES registers");
        runtime
            .register(definition(
                "nq",
                vec![market(series("NQ"), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("NQ registers");

        assert!(matches!(
            runtime.reconcile_market_leases(&mut engine),
            Err(StudyRuntimeError::MarketDemand(
                EngineError::DataLeaseLimitExceeded { .. }
            ))
        ));
        assert_eq!(engine.data_lease_count(), 0);
        assert!(engine.subscriptions().is_empty());
    }

    #[test]
    fn native_market_input_is_a_zero_copy_fixed_point_view_of_canonical_state() {
        let mut runtime = StudyRuntime::new(config(4));
        let mut engine = engine(2);
        let source = series("ES");
        let study = runtime
            .register(definition(
                "native_input",
                vec![market(source.clone(), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("study registers");
        runtime
            .reconcile_market_leases(&mut engine)
            .expect("study lease installs");
        assert!(
            runtime
                .market_input(&engine, study, 0)
                .expect("dependency is valid")
                .is_none(),
            "valid studies must observe loading instead of fabricated rows"
        );

        engine
            .install_history(ProviderGeneration(NonZeroU64::MIN), &source, 2, 3, bars())
            .expect("canonical history installs");
        let canonical = engine.series_snapshot(&source).expect("canonical snapshot");
        let input = runtime
            .market_input(&engine, study, 0)
            .expect("dependency is valid")
            .expect("canonical data is ready");
        assert_eq!(input.series(), &source);
        assert!(!input.forming());
        assert_eq!(
            input.publication_generation(),
            canonical.publication_generation
        );
        assert!(std::ptr::eq(input.bars().as_ptr(), canonical.bars.as_ptr()));

        let close = input.field(StudyBarField::Close);
        assert_eq!(close.len(), 2);
        assert_eq!(close.scale(), 2);
        assert_eq!(close.value(0), Some(10_500));
        assert_eq!(
            close.exchange_timestamp_unix_nanos(1),
            Some(1_700_000_060_000_000_000)
        );
        let volume = input.field(StudyBarField::Volume);
        assert_eq!(volume.scale(), 3);
        assert_eq!(volume.value(1), Some(1_500));
    }

    #[test]
    fn native_market_input_rejects_output_dependencies_without_guessing_alignment() {
        let mut runtime = StudyRuntime::new(config(4));
        let engine = engine(2);
        let producer = runtime
            .register(definition(
                "producer",
                vec![market(series("ES"), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("producer registers");
        let consumer = runtime
            .register(definition(
                "consumer",
                vec![StudyDependency::Output(StudyOutputId {
                    study_id: producer,
                    output_index: 0,
                })],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("consumer registers");

        assert_eq!(
            runtime
                .market_input(&engine, consumer, 0)
                .expect_err("output dependency is not market data"),
            StudyRuntimeError::DependencyIsNotMarket {
                study_id: consumer,
                dependency_index: 0,
            }
        );
    }

    #[test]
    fn production_studies_cannot_depend_on_another_consumers_outputs() {
        let mut runtime = StudyRuntime::new(config(4));
        let first_owner = ConsumerId(NonZeroU64::new(1).expect("owner"));
        let second_owner = ConsumerId(NonZeroU64::new(2).expect("owner"));
        let producer = runtime
            .register_for_consumer(
                first_owner,
                definition(
                    "producer",
                    vec![market(series("ES"), StreamRequirements::BARS)],
                    1,
                    StudyInvalidationPolicy::SameRange,
                ),
            )
            .expect("producer registers");
        let result = runtime.register_for_consumer(
            second_owner,
            definition(
                "consumer",
                vec![StudyDependency::Output(StudyOutputId {
                    study_id: producer,
                    output_index: 0,
                })],
                1,
                StudyInvalidationPolicy::SameRange,
            ),
        );

        assert!(matches!(
            result,
            Err(StudyRuntimeError::CrossConsumerDependency { dependency, .. })
                if dependency.study_id == producer
        ));
        assert_eq!(runtime.owner(producer), Some(first_owner));
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime.remove_consumer(first_owner), vec![producer]);
        assert!(runtime.is_empty());
    }

    #[test]
    fn dirty_ranges_propagate_in_dependency_order() {
        let mut runtime = StudyRuntime::new(config(8));
        let source = series("ES");
        let price = runtime
            .register(definition(
                "price_transform",
                vec![market(source.clone(), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("source study registers");
        let rolling = runtime
            .register(definition(
                "rolling",
                vec![StudyDependency::Output(StudyOutputId {
                    study_id: price,
                    output_index: 0,
                })],
                1,
                StudyInvalidationPolicy::TrailingWindow { bars: bound(3) },
            ))
            .expect("rolling study registers");
        let recursive = runtime
            .register(definition(
                "recursive",
                vec![StudyDependency::Output(StudyOutputId {
                    study_id: rolling,
                    output_index: 0,
                })],
                1,
                StudyInvalidationPolicy::FromFirstChanged,
            ))
            .expect("recursive study registers");

        let plan = runtime
            .plan_market_change(
                &source,
                StudyDirtyRange::bounded(10, 11).expect("dirty range"),
            )
            .expect("plan");
        assert_eq!(
            plan,
            vec![
                StudyCalculation {
                    study_id: price,
                    range: StudyDirtyRange::bounded(10, 11).expect("range"),
                },
                StudyCalculation {
                    study_id: rolling,
                    range: StudyDirtyRange::bounded(10, 13).expect("range"),
                },
                StudyCalculation {
                    study_id: recursive,
                    range: StudyDirtyRange::to_tail(10),
                },
            ]
        );
    }

    #[test]
    fn output_changes_only_schedule_actual_downstream_dependencies() {
        let mut runtime = StudyRuntime::new(config(8));
        let source = series("ES");
        let producer = runtime
            .register(definition(
                "two_outputs",
                vec![market(source, StreamRequirements::BARS)],
                2,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("producer registers");
        let first = runtime
            .register(definition(
                "first_consumer",
                vec![StudyDependency::Output(StudyOutputId {
                    study_id: producer,
                    output_index: 0,
                })],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("first consumer registers");
        let second = runtime
            .register(definition(
                "second_consumer",
                vec![StudyDependency::Output(StudyOutputId {
                    study_id: producer,
                    output_index: 1,
                })],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("second consumer registers");

        let plan = runtime
            .plan_output_change(
                StudyOutputId {
                    study_id: producer,
                    output_index: 0,
                },
                StudyDirtyRange::bounded(4, 5).expect("range"),
            )
            .expect("downstream plan");
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].study_id, first);
        assert_ne!(plan[0].study_id, second);
    }

    #[test]
    fn forward_and_missing_output_dependencies_are_rejected() {
        let mut runtime = StudyRuntime::new(config(8));
        let unknown = StudyInstanceId(NonZeroU64::new(99).expect("non-zero"));
        let error = runtime
            .register(definition(
                "invalid",
                vec![StudyDependency::Output(StudyOutputId {
                    study_id: unknown,
                    output_index: 0,
                })],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect_err("forward dependency is rejected");
        assert_eq!(
            error,
            StudyRuntimeError::UnknownOutput(StudyOutputId {
                study_id: unknown,
                output_index: 0,
            })
        );

        let producer = runtime
            .register(definition(
                "producer",
                vec![market(series("ES"), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("producer registers");
        assert_eq!(
            runtime
                .register(definition(
                    "bad_output",
                    vec![StudyDependency::Output(StudyOutputId {
                        study_id: producer,
                        output_index: 1,
                    })],
                    1,
                    StudyInvalidationPolicy::SameRange,
                ))
                .expect_err("missing output is rejected"),
            StudyRuntimeError::UnknownOutput(StudyOutputId {
                study_id: producer,
                output_index: 1,
            })
        );
    }

    #[test]
    fn removing_a_source_removes_its_downstream_subtree_and_releases_demand() {
        let mut runtime = StudyRuntime::new(config(8));
        let es = series("ES");
        let nq = series("NQ");
        let source = runtime
            .register(definition(
                "source",
                vec![market(es.clone(), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("source registers");
        let dependent = runtime
            .register(definition(
                "dependent",
                vec![StudyDependency::Output(StudyOutputId {
                    study_id: source,
                    output_index: 0,
                })],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("dependent registers");
        let independent = runtime
            .register(definition(
                "independent",
                vec![market(nq.clone(), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("independent registers");

        let removed = runtime.remove_subtree(source).expect("source is live");
        assert_eq!(removed, vec![source, dependent]);
        assert_eq!(runtime.len(), 1);
        assert!(runtime.definition(independent).is_some());
        let requirements = runtime.market_requirements();
        assert!(!requirements.contains_key(&es));
        assert!(requirements.contains_key(&nq));
    }

    #[test]
    fn runtime_enforces_instance_dependency_and_output_bounds() {
        let mut runtime = StudyRuntime::new(StudyRuntimeConfig {
            maximum_studies: bound(1),
            maximum_dependencies_per_study: bound(1),
            maximum_outputs_per_study: bound(1),
            maximum_points_per_output: bound(64),
            maximum_total_output_points: bound(64),
            maximum_state_bytes_per_study: bound(64),
            maximum_total_state_bytes: bound(64),
        });
        runtime
            .register(definition(
                "one",
                vec![market(series("ES"), StreamRequirements::BARS)],
                1,
                StudyInvalidationPolicy::SameRange,
            ))
            .expect("first study fits");
        assert_eq!(
            runtime
                .register(definition(
                    "two",
                    vec![market(series("NQ"), StreamRequirements::BARS)],
                    1,
                    StudyInvalidationPolicy::SameRange,
                ))
                .expect_err("instance bound"),
            StudyRuntimeError::StudyLimitExceeded { maximum: 1 }
        );

        let runtime = StudyRuntime::new(StudyRuntimeConfig {
            maximum_studies: bound(4),
            maximum_dependencies_per_study: bound(1),
            maximum_outputs_per_study: bound(1),
            maximum_points_per_output: bound(64),
            maximum_total_output_points: bound(64),
            maximum_state_bytes_per_study: bound(64),
            maximum_total_state_bytes: bound(64),
        });
        assert_eq!(
            runtime
                .validate_definition(
                    None,
                    &definition(
                        "too_many_dependencies",
                        vec![
                            market(series("ES"), StreamRequirements::BARS),
                            market(series("NQ"), StreamRequirements::BARS),
                        ],
                        1,
                        StudyInvalidationPolicy::SameRange,
                    )
                )
                .expect_err("dependency bound"),
            StudyRuntimeError::TooManyDependencies { maximum: 1 }
        );
        assert_eq!(
            runtime
                .validate_definition(
                    None,
                    &definition(
                        "too_many_outputs",
                        vec![market(series("ES"), StreamRequirements::BARS)],
                        2,
                        StudyInvalidationPolicy::SameRange,
                    )
                )
                .expect_err("output bound"),
            StudyRuntimeError::TooManyOutputs { maximum: 1 }
        );
    }

    #[test]
    fn output_presentation_metadata_is_bounded_nonempty_and_uniquely_identified() {
        let runtime = StudyRuntime::new(config(4));
        let mut missing = definition(
            "missing_output",
            vec![market(series("ES"), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::SameRange,
        );
        missing.outputs.clear();
        assert_eq!(
            runtime
                .validate_definition(None, &missing)
                .expect_err("at least one output is required"),
            StudyRuntimeError::MissingOutput
        );

        let mut duplicate = definition(
            "duplicate_output",
            vec![market(series("ES"), StreamRequirements::BARS)],
            2,
            StudyInvalidationPolicy::SameRange,
        );
        duplicate.outputs[1].identifier = duplicate.outputs[0].identifier.clone();
        assert_eq!(
            runtime
                .validate_definition(None, &duplicate)
                .expect_err("output identifiers are stable unique keys"),
            StudyRuntimeError::DuplicateOutputIdentifier
        );

        let mut empty_title = definition(
            "empty_title",
            vec![market(series("ES"), StreamRequirements::BARS)],
            1,
            StudyInvalidationPolicy::SameRange,
        );
        empty_title.outputs[0].title.clear();
        assert_eq!(
            runtime
                .validate_definition(None, &empty_title)
                .expect_err("visible output title is required"),
            StudyRuntimeError::InvalidOutputTitle
        );
    }
}
