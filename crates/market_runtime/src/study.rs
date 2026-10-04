//! Bounded native study dependency runtime.
//!
//! This module deliberately stops at the market-runtime ownership boundary. Studies declare
//! dependencies on already-resolved canonical market series or earlier study outputs; provider
//! sessions, history, live handoff, and canonical market state remain owned by `MarketEngine` and
//! `market_service`. The runtime produces deterministic recalculation plans and shared upstream
//! stream requirements without creating provider work itself.

use crate::RetainedMarketTrade;
use aeris_market_data::{
    AggressorSide, BarSeriesKey, DepthLevel, MarketBar, OrderBook, OrderBookState, TopOfBookQuote,
};
use aeris_market_engine::{
    ConsumerId, EngineError, MarketDataLeaseId, MarketEngine, MarketStream, SeriesSnapshot,
    StreamRequirements,
};
use std::{
    any::Any,
    array,
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
    num::{NonZeroU64, NonZeroUsize},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, OnceLock},
};

mod execution;

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
/// Maximum stable choice options declared by one setting.
pub const MAXIMUM_STUDY_SETTING_CHOICE_OPTIONS: usize = 64;
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
    trades: &'a VecDeque<RetainedMarketTrade>,
    session_generation: u64,
    source_watermark: u64,
    price_scale: u8,
    quantity_scale: u8,
}

impl<'a> StudyTradeWindow<'a> {
    pub(crate) const fn new(
        trades: &'a VecDeque<RetainedMarketTrade>,
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
        self.trades.get(index).map(study_trade_sample)
    }

    /// Iterates retained trade samples from oldest to newest without allocating.
    pub fn iter(self) -> impl ExactSizeIterator<Item = StudyTradeSample> + 'a {
        self.trades.iter().map(study_trade_sample)
    }
}

fn study_trade_sample(trade: &RetainedMarketTrade) -> StudyTradeSample {
    StudyTradeSample {
        observed_unix_nanos: trade.observed_unix_nanos,
        price: trade.trade.price,
        quantity: trade.trade.quantity,
        aggressor: trade.trade.aggressor,
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
/// Level iteration reads an immutable book image. Worker execution owns a
/// bounded snapshot so calculations never borrow mutable canonical state.
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
    pub fn bids(self) -> impl ExactSizeIterator<Item = DepthLevel> + 'a {
        self.book.bid_levels()
    }

    /// Returns canonical ask levels from best to worst without allocating.
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

impl StudyDecimal {
    fn scaled_mantissa(self, scale: u8) -> Option<i128> {
        let exponent = scale.checked_sub(self.scale)?;
        i128::from(self.mantissa).checked_mul(10_i128.checked_pow(u32::from(exponent))?)
    }
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

/// One stable choice identifier and its human-readable product label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudySettingChoiceOption {
    pub identifier: String,
    pub label: String,
}

/// Product-owned editor control for one typed study setting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StudySettingControl {
    Boolean,
    Integer {
        minimum: Option<i64>,
        maximum: Option<i64>,
        step: Option<i64>,
    },
    Decimal {
        minimum: Option<StudyDecimal>,
        maximum: Option<StudyDecimal>,
        step: Option<StudyDecimal>,
    },
    Text,
    Choice {
        options: Vec<StudySettingChoiceOption>,
    },
}

/// Simple raw-value predicate used by product presentation. It never changes
/// formula execution or dependency ownership; it only controls whether another
/// declared setting is shown or enabled by a generic editor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudySettingCondition {
    pub setting_identifier: String,
    pub equals: StudySettingValue,
}

/// Human-readable setting metadata consumed by product-owned generic editors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudySettingPresentation {
    pub label: String,
    pub description: Option<String>,
    pub group: Option<String>,
    pub control: StudySettingControl,
    pub visible_when: Option<StudySettingCondition>,
    pub enabled_when: Option<StudySettingCondition>,
}

impl StudySettingPresentation {
    fn inferred(label: String, default: &StudySettingValue) -> Self {
        let control = match default {
            StudySettingValue::Boolean(_) => StudySettingControl::Boolean,
            StudySettingValue::Integer(_) => StudySettingControl::Integer {
                minimum: None,
                maximum: None,
                step: None,
            },
            StudySettingValue::Decimal(_) => StudySettingControl::Decimal {
                minimum: None,
                maximum: None,
                step: None,
            },
            StudySettingValue::Text(_) => StudySettingControl::Text,
            StudySettingValue::Choice(value) => StudySettingControl::Choice {
                options: vec![StudySettingChoiceOption {
                    identifier: value.clone(),
                    label: value.clone(),
                }],
            },
        };
        Self {
            label,
            description: None,
            group: None,
            control,
            visible_when: None,
            enabled_when: None,
        }
    }
}

/// One durable typed setting declaration plus presentation metadata. Formula
/// code receives only the validated `StudySettings` values; presentation code
/// can render a generic editor without knowing native implementation details.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudySettingSpec {
    pub identifier: String,
    pub default: StudySettingValue,
    pub presentation: StudySettingPresentation,
}

impl StudySettingSpec {
    /// Creates a typed setting with conservative presentation defaults. SDK
    /// authors can replace the inferred metadata with [`Self::with_presentation`].
    #[must_use]
    pub fn new(identifier: impl Into<String>, default: StudySettingValue) -> Self {
        let identifier = identifier.into();
        let presentation = StudySettingPresentation::inferred(identifier.clone(), &default);
        Self {
            identifier,
            default,
            presentation,
        }
    }

    /// Replaces the generic presentation metadata while preserving the stable
    /// setting identifier and typed default.
    #[must_use]
    pub fn with_presentation(mut self, presentation: StudySettingPresentation) -> Self {
        self.presentation = presentation;
        self
    }
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
            validate_setting_value_against_control(&value, &spec.presentation.control)?;
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
        validate_setting_presentation(spec, specs)?;
    }
    Ok(())
}

fn validate_setting_presentation(
    spec: &StudySettingSpec,
    specs: &[StudySettingSpec],
) -> Result<(), StudyRuntimeError> {
    let presentation = &spec.presentation;
    if presentation.label.trim().is_empty()
        || presentation.label.len() > MAXIMUM_STUDY_SETTING_TEXT_BYTES
        || presentation
            .description
            .as_ref()
            .is_some_and(|value| value.len() > MAXIMUM_STUDY_SETTING_TEXT_BYTES)
        || presentation.group.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAXIMUM_STUDY_SETTING_TEXT_BYTES
        })
    {
        return Err(StudyRuntimeError::InvalidSettingPresentation);
    }
    validate_setting_control(&spec.default, &presentation.control)?;
    for condition in [
        presentation.visible_when.as_ref(),
        presentation.enabled_when.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        let referenced = specs
            .iter()
            .find(|candidate| candidate.identifier == condition.setting_identifier)
            .ok_or(StudyRuntimeError::InvalidSettingPresentation)?;
        condition.equals.validate()?;
        if !condition.equals.same_type(&referenced.default) {
            return Err(StudyRuntimeError::InvalidSettingPresentation);
        }
        validate_setting_value_against_control(
            &condition.equals,
            &referenced.presentation.control,
        )?;
    }
    validate_setting_value_against_control(&spec.default, &presentation.control)
}

fn validate_setting_control(
    default: &StudySettingValue,
    control: &StudySettingControl,
) -> Result<(), StudyRuntimeError> {
    match (default, control) {
        (StudySettingValue::Boolean(_), StudySettingControl::Boolean)
        | (StudySettingValue::Text(_), StudySettingControl::Text) => Ok(()),
        (
            StudySettingValue::Integer(_),
            StudySettingControl::Integer {
                minimum,
                maximum,
                step,
            },
        ) => {
            if step.is_some_and(|step| step <= 0)
                || minimum.zip(*maximum).is_some_and(|(min, max)| min > max)
            {
                return Err(StudyRuntimeError::InvalidSettingPresentation);
            }
            Ok(())
        }
        (
            StudySettingValue::Decimal(_),
            StudySettingControl::Decimal {
                minimum,
                maximum,
                step,
            },
        ) => {
            if step.is_some_and(|step| {
                step.scale > MAXIMUM_STUDY_SETTING_DECIMAL_SCALE || step.mantissa <= 0
            }) {
                return Err(StudyRuntimeError::InvalidSettingPresentation);
            }
            if minimum.is_some_and(|value| value.scale > MAXIMUM_STUDY_SETTING_DECIMAL_SCALE)
                || maximum.is_some_and(|value| value.scale > MAXIMUM_STUDY_SETTING_DECIMAL_SCALE)
                || decimal_bounds_invalid(*minimum, *maximum)
            {
                return Err(StudyRuntimeError::InvalidSettingPresentation);
            }
            Ok(())
        }
        (StudySettingValue::Choice(_), StudySettingControl::Choice { options }) => {
            if options.is_empty() || options.len() > MAXIMUM_STUDY_SETTING_CHOICE_OPTIONS {
                return Err(StudyRuntimeError::InvalidSettingPresentation);
            }
            let mut identifiers = BTreeSet::new();
            for option in options {
                if option.identifier.trim().is_empty()
                    || option.identifier.len() > MAXIMUM_STUDY_SETTING_TEXT_BYTES
                    || option.label.trim().is_empty()
                    || option.label.len() > MAXIMUM_STUDY_SETTING_TEXT_BYTES
                    || !identifiers.insert(option.identifier.as_str())
                {
                    return Err(StudyRuntimeError::InvalidSettingPresentation);
                }
            }
            Ok(())
        }
        _ => Err(StudyRuntimeError::InvalidSettingPresentation),
    }
}

fn decimal_bounds_invalid(minimum: Option<StudyDecimal>, maximum: Option<StudyDecimal>) -> bool {
    let (Some(minimum), Some(maximum)) = (minimum, maximum) else {
        return false;
    };
    let scale = minimum.scale.max(maximum.scale);
    let Some(minimum) = minimum.scaled_mantissa(scale) else {
        return true;
    };
    let Some(maximum) = maximum.scaled_mantissa(scale) else {
        return true;
    };
    minimum > maximum
}

fn validate_setting_value_against_control(
    value: &StudySettingValue,
    control: &StudySettingControl,
) -> Result<(), StudyRuntimeError> {
    match (value, control) {
        (StudySettingValue::Boolean(_), StudySettingControl::Boolean)
        | (StudySettingValue::Text(_), StudySettingControl::Text) => Ok(()),
        (
            StudySettingValue::Integer(value),
            StudySettingControl::Integer {
                minimum,
                maximum,
                step,
            },
        ) => {
            if minimum.is_some_and(|minimum| *value < minimum)
                || maximum.is_some_and(|maximum| *value > maximum)
            {
                return Err(StudyRuntimeError::InvalidSettingValue);
            }
            if let Some(step) = step {
                let origin = minimum.unwrap_or(0);
                if value
                    .checked_sub(origin)
                    .is_none_or(|delta| delta % step != 0)
                {
                    return Err(StudyRuntimeError::InvalidSettingValue);
                }
            }
            Ok(())
        }
        (
            StudySettingValue::Decimal(value),
            StudySettingControl::Decimal {
                minimum,
                maximum,
                step,
            },
        ) => validate_decimal_setting_value(*value, *minimum, *maximum, *step),
        (StudySettingValue::Choice(value), StudySettingControl::Choice { options }) => options
            .iter()
            .any(|option| option.identifier == *value)
            .then_some(())
            .ok_or(StudyRuntimeError::InvalidSettingValue),
        _ => Err(StudyRuntimeError::SettingTypeMismatch),
    }
}

fn validate_decimal_setting_value(
    value: StudyDecimal,
    minimum: Option<StudyDecimal>,
    maximum: Option<StudyDecimal>,
    step: Option<StudyDecimal>,
) -> Result<(), StudyRuntimeError> {
    let scale = value
        .scale
        .max(minimum.map_or(0, |value| value.scale))
        .max(maximum.map_or(0, |value| value.scale))
        .max(step.map_or(0, |value| value.scale));
    let value = value
        .scaled_mantissa(scale)
        .ok_or(StudyRuntimeError::InvalidSettingValue)?;
    let minimum = match minimum {
        Some(value) => Some(
            value
                .scaled_mantissa(scale)
                .ok_or(StudyRuntimeError::InvalidSettingValue)?,
        ),
        None => None,
    };
    let maximum = match maximum {
        Some(value) => Some(
            value
                .scaled_mantissa(scale)
                .ok_or(StudyRuntimeError::InvalidSettingValue)?,
        ),
        None => None,
    };
    if minimum.is_some_and(|minimum| value < minimum)
        || maximum.is_some_and(|maximum| value > maximum)
    {
        return Err(StudyRuntimeError::InvalidSettingValue);
    }
    if let Some(step) = step {
        let step = step
            .scaled_mantissa(scale)
            .filter(|step| *step > 0)
            .ok_or(StudyRuntimeError::InvalidSettingValue)?;
        let origin = minimum.unwrap_or(0);
        if (value - origin) % step != 0 {
            return Err(StudyRuntimeError::InvalidSettingValue);
        }
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
        validate_setting_value_against_control(value, &spec.presentation.control)?;
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
/// chart pane; actual Aeris Charts pane identities remain presentation-owned.
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

/// One fixed-value background channel requested behind a scalar output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudyThresholdRegion {
    pub lower: StudyDecimal,
    pub upper: StudyDecimal,
}

/// Semantic per-point styling policy for one scalar output.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StudyPointStyle {
    /// One ordinary series color for every row.
    #[default]
    Uniform,
    /// Four-state histogram coloring based on sign and movement toward/away from zero.
    MomentumHistogram,
}

/// Declarative presentation metadata for one scalar study output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudyOutputSpec {
    pub identifier: String,
    pub title: String,
    /// Optional short label used when a host groups multiple outputs into one legend row.
    /// `None` leaves the value unlabeled, which is useful for visually grouped families such as
    /// EMA ribbons.
    pub legend_label: Option<String>,
    pub plot: StudyPlotKind,
    pub pane: StudyPaneTarget,
    pub scale: StudyScaleTarget,
    pub threshold_region: Option<StudyThresholdRegion>,
    pub point_style: StudyPointStyle,
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

const STUDY_OUTPUT_VALUE_LEAF_LEN: usize = 64;
const STUDY_OUTPUT_VALUE_BRANCH_FACTOR: usize = 32;

#[derive(Clone)]
struct StudyOutputTimeline(Arc<StudyOutputTimelineInner>);

struct StudyOutputTimelineInner {
    snapshot: Arc<SeriesSnapshot>,
    materialized: OnceLock<Arc<[i64]>>,
}

impl StudyOutputTimeline {
    fn market(snapshot: Arc<SeriesSnapshot>) -> Self {
        Self(Arc::new(StudyOutputTimelineInner {
            snapshot,
            materialized: OnceLock::new(),
        }))
    }

    fn len(&self) -> usize {
        self.0.snapshot.bars.len()
    }

    fn timestamp(&self, index: usize) -> Option<i64> {
        self.0
            .snapshot
            .bars
            .get(index)
            .map(|bar| bar.exchange_timestamp_unix_nanos)
    }

    fn lower_bound(&self, timestamp_unix_nanos: i64) -> usize {
        self.0
            .snapshot
            .bars
            .partition_point(|bar| bar.exchange_timestamp_unix_nanos < timestamp_unix_nanos)
    }

    fn upper_bound(&self, timestamp_unix_nanos: i64) -> usize {
        self.0
            .snapshot
            .bars
            .partition_point(|bar| bar.exchange_timestamp_unix_nanos <= timestamp_unix_nanos)
    }

    fn timestamps(&self) -> &[i64] {
        self.0
            .materialized
            .get_or_init(|| {
                self.0
                    .snapshot
                    .bars
                    .iter()
                    .map(|bar| bar.exchange_timestamp_unix_nanos)
                    .collect::<Vec<_>>()
                    .into()
            })
            .as_ref()
    }
}

enum StudyOutputValueNode {
    Branch(Box<[Option<Arc<StudyOutputValueNode>>; STUDY_OUTPUT_VALUE_BRANCH_FACTOR]>),
    Leaf(Box<[Option<f64>; STUDY_OUTPUT_VALUE_LEAF_LEN]>),
}

#[derive(Clone)]
struct StudyOutputValues {
    root: Option<Arc<StudyOutputValueNode>>,
    depth: u8,
    len: usize,
}

impl StudyOutputValues {
    fn empty(len: usize) -> Self {
        Self {
            root: None,
            depth: 0,
            len,
        }
    }

    fn from_dense(values: &[Option<f64>]) -> Self {
        let len = values.len();
        let mut output = Self::empty(len);
        for (chunk_index, chunk) in values.chunks(STUDY_OUTPUT_VALUE_LEAF_LEN).enumerate() {
            if chunk.iter().all(Option::is_none) {
                continue;
            }
            let mut leaf = [None; STUDY_OUTPUT_VALUE_LEAF_LEN];
            leaf[..chunk.len()].copy_from_slice(chunk);
            output.insert_leaf(chunk_index, Box::new(leaf));
        }
        output
    }

    fn value_at(&self, index: usize) -> Option<f64> {
        debug_assert!(index < self.len);
        let chunk_index = index / STUDY_OUTPUT_VALUE_LEAF_LEN;
        let offset = index % STUDY_OUTPUT_VALUE_LEAF_LEN;
        let mut node = self.root.as_ref()?;
        let mut depth = self.depth;
        let mut remaining = chunk_index;
        while depth > 0 {
            let StudyOutputValueNode::Branch(children) = node.as_ref() else {
                return None;
            };
            let child_capacity = Self::chunk_capacity(depth - 1);
            let child_index = remaining / child_capacity;
            remaining %= child_capacity;
            node = children.get(child_index).and_then(Option::as_ref)?;
            depth -= 1;
        }
        let StudyOutputValueNode::Leaf(values) = node.as_ref() else {
            return None;
        };
        values[offset]
    }

    fn set(&mut self, index: usize, value: Option<f64>) -> Result<(), String> {
        if index >= self.len {
            return Err("study output row is outside the calculation range".to_string());
        }
        let chunk_index = index / STUDY_OUTPUT_VALUE_LEAF_LEN;
        self.ensure_depth(chunk_index);
        self.root = Some(Self::updated_value(
            self.root.as_ref(),
            self.depth,
            chunk_index,
            index % STUDY_OUTPUT_VALUE_LEAF_LEN,
            value,
        ));
        Ok(())
    }

    fn resized(mut self, len: usize) -> Self {
        self.len = len;
        self
    }

    fn to_vec(&self) -> Vec<Option<f64>> {
        (0..self.len).map(|index| self.value_at(index)).collect()
    }

    fn chunk_capacity(depth: u8) -> usize {
        let mut capacity = 1usize;
        for _ in 0..depth {
            capacity = capacity.saturating_mul(STUDY_OUTPUT_VALUE_BRANCH_FACTOR);
        }
        capacity
    }

    fn ensure_depth(&mut self, chunk_index: usize) {
        while chunk_index >= Self::chunk_capacity(self.depth) {
            let mut children = array::from_fn(|_| None);
            children[0] = self.root.take();
            self.root = Some(Arc::new(StudyOutputValueNode::Branch(Box::new(children))));
            self.depth = self.depth.saturating_add(1);
        }
    }

    fn insert_leaf(
        &mut self,
        chunk_index: usize,
        values: Box<[Option<f64>; STUDY_OUTPUT_VALUE_LEAF_LEN]>,
    ) {
        self.ensure_depth(chunk_index);
        self.root = Some(Self::updated_leaf(
            self.root.as_ref(),
            self.depth,
            chunk_index,
            Arc::new(StudyOutputValueNode::Leaf(values)),
        ));
    }

    fn updated_leaf(
        node: Option<&Arc<StudyOutputValueNode>>,
        depth: u8,
        chunk_index: usize,
        leaf: Arc<StudyOutputValueNode>,
    ) -> Arc<StudyOutputValueNode> {
        if depth == 0 {
            return leaf;
        }
        let child_capacity = Self::chunk_capacity(depth - 1);
        let child_index = chunk_index / child_capacity;
        let remainder = chunk_index % child_capacity;
        let mut children = match node.map(Arc::as_ref) {
            Some(StudyOutputValueNode::Branch(children)) => (**children).clone(),
            _ => array::from_fn(|_| None),
        };
        children[child_index] = Some(Self::updated_leaf(
            children[child_index].as_ref(),
            depth - 1,
            remainder,
            leaf,
        ));
        Arc::new(StudyOutputValueNode::Branch(Box::new(children)))
    }

    fn updated_value(
        node: Option<&Arc<StudyOutputValueNode>>,
        depth: u8,
        chunk_index: usize,
        offset: usize,
        value: Option<f64>,
    ) -> Arc<StudyOutputValueNode> {
        if depth == 0 {
            let mut values = match node.map(Arc::as_ref) {
                Some(StudyOutputValueNode::Leaf(values)) => **values,
                _ => [None; STUDY_OUTPUT_VALUE_LEAF_LEN],
            };
            values[offset] = value;
            return Arc::new(StudyOutputValueNode::Leaf(Box::new(values)));
        }
        let child_capacity = Self::chunk_capacity(depth - 1);
        let child_index = chunk_index / child_capacity;
        let remainder = chunk_index % child_capacity;
        let mut children = match node.map(Arc::as_ref) {
            Some(StudyOutputValueNode::Branch(children)) => (**children).clone(),
            _ => array::from_fn(|_| None),
        };
        children[child_index] = Some(Self::updated_value(
            children[child_index].as_ref(),
            depth - 1,
            remainder,
            offset,
            value,
        ));
        Arc::new(StudyOutputValueNode::Branch(Box::new(children)))
    }
}

/// Immutable scalar output series produced by one native study output.
///
/// Timestamps are inherited from the study's first dependency and shared by all
/// outputs from the same calculation. `None` represents a deliberate gap such
/// as an indicator warm-up period. Incremental generations structurally share
/// unchanged value blocks; full contiguous slices are materialized only when a
/// publication consumer explicitly requests them.
#[derive(Clone)]
pub struct StudyOutputSeries {
    timeline: StudyOutputTimeline,
    values: StudyOutputValues,
    materialized_values: Arc<OnceLock<Arc<[Option<f64>]>>>,
    generation: u64,
}

impl fmt::Debug for StudyOutputSeries {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StudyOutputSeries")
            .field("len", &self.len())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl PartialEq for StudyOutputSeries {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.timestamps() == other.timestamps()
            && self.values() == other.values()
    }
}

impl StudyOutputSeries {
    /// Returns the number of output rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len
    }

    /// Returns whether no output rows are available.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.len == 0
    }

    /// Returns the output generation committed by the runtime.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns the exact row timestamp.
    #[must_use]
    pub fn timestamp_unix_nanos(&self, index: usize) -> Option<i64> {
        self.timeline.timestamp(index)
    }

    /// Returns one finite scalar value or a deliberate gap.
    #[must_use]
    pub fn value(&self, index: usize) -> Option<Option<f64>> {
        (index < self.len()).then(|| self.values.value_at(index))
    }

    /// Returns all timestamps for binary-search alignment by secondary inputs.
    #[must_use]
    pub fn timestamps(&self) -> &[i64] {
        self.timeline.timestamps()
    }

    /// Returns all output values.
    #[must_use]
    pub fn values(&self) -> &[Option<f64>] {
        self.materialized_values
            .get_or_init(|| self.values.to_vec().into())
            .as_ref()
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
    values: StudyOutputBufferValues,
}

enum StudyOutputBufferValues {
    Dense(Vec<Option<f64>>),
    Persistent(StudyOutputValues),
}

impl StudyOutputBuffer {
    fn new(len: usize) -> Self {
        Self {
            values: StudyOutputBufferValues::Dense(vec![None; len]),
        }
    }

    fn from_previous(
        timeline: &StudyOutputTimeline,
        previous: Option<&StudyOutputSeries>,
        dirty: StudyDirtyRange,
    ) -> (Self, usize) {
        let Some(previous) = previous else {
            return (Self::new(timeline.len()), timeline.len());
        };
        if Self::can_reuse_previous(timeline, previous, dirty) {
            let mut output = Self {
                values: StudyOutputBufferValues::Persistent(
                    previous.values.clone().resized(timeline.len()),
                ),
            };
            let cleared = output.clear(dirty);
            return (output, cleared);
        }

        let mut output = Self::new(timeline.len());
        let mut previous_index = 0;
        let mut current_index = 0;
        while previous_index < previous.len() && current_index < timeline.len() {
            let previous_timestamp = previous
                .timestamp_unix_nanos(previous_index)
                .expect("bounded previous output timestamp");
            let current_timestamp = timeline
                .timestamp(current_index)
                .expect("bounded current output timestamp");
            match previous_timestamp.cmp(&current_timestamp) {
                std::cmp::Ordering::Less => previous_index += 1,
                std::cmp::Ordering::Greater => current_index += 1,
                std::cmp::Ordering::Equal => {
                    if let StudyOutputBufferValues::Dense(values) = &mut output.values {
                        values[current_index] = previous.value(previous_index).flatten();
                    }
                    previous_index += 1;
                    current_index += 1;
                }
            }
        }
        let scanned = previous_index.saturating_add(current_index);
        output.clear(dirty);
        (output, scanned)
    }

    fn can_reuse_previous(
        timeline: &StudyOutputTimeline,
        previous: &StudyOutputSeries,
        dirty: StudyDirtyRange,
    ) -> bool {
        let previous_len = previous.len();
        let current_len = timeline.len();
        if dirty.start == 0
            || previous_len == 0
            || current_len < previous_len
            || dirty.start > previous_len
        {
            return false;
        }
        if timeline.timestamp(0) != previous.timestamp_unix_nanos(0)
            || timeline.timestamp(dirty.start - 1) != previous.timestamp_unix_nanos(dirty.start - 1)
        {
            return false;
        }
        if dirty.start < previous_len
            && timeline.timestamp(dirty.start) != previous.timestamp_unix_nanos(dirty.start)
        {
            return false;
        }
        current_len == previous_len
            || timeline.timestamp(previous_len - 1)
                == previous.timestamp_unix_nanos(previous_len - 1)
    }

    fn clear(&mut self, range: StudyDirtyRange) -> usize {
        let end = range.end_exclusive.unwrap_or(self.len()).min(self.len());
        if range.start >= end {
            return 0;
        }
        match &mut self.values {
            StudyOutputBufferValues::Dense(values) => values[range.start..end].fill(None),
            StudyOutputBufferValues::Persistent(values) => {
                for index in range.start..end {
                    values.set(index, None).expect("bounded output clear range");
                }
            }
        }
        end - range.start
    }

    /// Returns the output row count inherited from the first dependency.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.values {
            StudyOutputBufferValues::Dense(values) => values.len(),
            StudyOutputBufferValues::Persistent(values) => values.len,
        }
    }

    /// Returns whether this output has no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Writes one finite scalar value or an explicit gap.
    ///
    /// # Errors
    /// Returns an error for an out-of-range row or non-finite value.
    pub fn set(&mut self, index: usize, value: Option<f64>) -> Result<(), String> {
        if value.is_some_and(|value| !value.is_finite()) {
            return Err("study output must be finite".to_string());
        }
        match &mut self.values {
            StudyOutputBufferValues::Dense(values) => {
                let slot = values.get_mut(index).ok_or_else(|| {
                    "study output row is outside the calculation range".to_string()
                })?;
                *slot = value;
                Ok(())
            }
            StudyOutputBufferValues::Persistent(values) => values.set(index, value),
        }
    }

    fn into_values(self) -> StudyOutputValues {
        match self.values {
            StudyOutputBufferValues::Dense(values) => StudyOutputValues::from_dense(&values),
            StudyOutputBufferValues::Persistent(values) => values,
        }
    }
}

/// In-process execution context for one trusted native Rust study calculation.
///
/// Inputs are immutable snapshots. Outputs are bounded buffers owned by the
/// runtime. Studies receive timestamps rather than pixel coordinates and cannot
/// reach provider sessions, `MarketEngine`, Aeris Charts, GPUI, or GPU state.
pub struct StudyExecutionContext<'a> {
    settings: &'a StudySettings,
    inputs: &'a [ResolvedStudyInput],
    live_inputs: &'a [Option<StudyLiveMarketData<'a>>],
    timeline: &'a StudyOutputTimeline,
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
    timeline: &'a StudyOutputTimeline,
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
    pub fn timestamps(self) -> &'a [i64] {
        self.timeline.timestamps()
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
    pub fn timestamps(&self) -> &[i64] {
        self.timeline.timestamps()
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
                timeline: self.timeline,
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
                timeline: self.timeline,
                dirty: self.dirty,
            },
            state,
            self.outputs,
        ))
    }
}

struct NativeStudyStateValue<T> {
    value: T,
    runtime_bytes: fn(&T) -> usize,
    clone_for_transaction: fn(&T) -> T,
}

type ErasedNativeStudyStateValue = dyn Any + Send;
type CloneNativeStudyStateValue =
    fn(&ErasedNativeStudyStateValue) -> Box<ErasedNativeStudyStateValue>;
type NativeStudyStateBytes = fn(&ErasedNativeStudyStateValue) -> usize;

fn clone_native_study_state_value<T: Send + 'static>(
    value: &ErasedNativeStudyStateValue,
) -> Box<ErasedNativeStudyStateValue> {
    let state = value
        .downcast_ref::<NativeStudyStateValue<T>>()
        .expect("native study state clone type");
    Box::new(NativeStudyStateValue {
        value: (state.clone_for_transaction)(&state.value),
        runtime_bytes: state.runtime_bytes,
        clone_for_transaction: state.clone_for_transaction,
    })
}

fn native_study_state_value_bytes<T: Send + 'static>(value: &ErasedNativeStudyStateValue) -> usize {
    let state = value
        .downcast_ref::<NativeStudyStateValue<T>>()
        .expect("native study state accounting type");
    (state.runtime_bytes)(&state.value)
}

fn copy_native_study_state_value<T: Copy>(value: &T) -> T {
    *value
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
    /// Wraps one trivially copyable runtime state value and its exact
    /// memory-accounting callback.
    ///
    /// `Copy` is deliberately required here: a shallow `Clone` of shared
    /// interior-mutable storage can let a rejected calculation mutate the
    /// previously committed state. Stateful implementations that own heap data
    /// must use [`Self::new_transactional`] and provide an explicit
    /// mutation-isolated candidate clone.
    #[must_use]
    pub fn new<T: Copy + Send + 'static>(value: T, runtime_bytes: fn(&T) -> usize) -> Self {
        Self::new_transactional(value, runtime_bytes, copy_native_study_state_value::<T>)
    }

    /// Wraps one runtime state value whose candidate clone is explicitly
    /// isolated from the committed value.
    ///
    /// The supplied clone callback is part of the trusted static-native study
    /// contract. It must return state that can be mutated independently: no
    /// `Arc<Mutex<_>>`, `Rc<RefCell<_>>`, or similar mutable storage may remain
    /// shared with the source state after the callback returns. This explicit
    /// boundary prevents the runtime from treating ordinary shallow `Clone`
    /// semantics as transactional rollback semantics.
    #[must_use]
    pub fn new_transactional<T: Send + 'static>(
        value: T,
        runtime_bytes: fn(&T) -> usize,
        clone_for_transaction: fn(&T) -> T,
    ) -> Self {
        Self {
            value: Box::new(NativeStudyStateValue {
                value,
                runtime_bytes,
                clone_for_transaction,
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

    #[cfg(test)]
    fn value<T: 'static>(&self) -> Option<&T> {
        self.value
            .downcast_ref::<NativeStudyStateValue<T>>()
            .map(|state| &state.value)
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

/// Result of one dependency-ordered execution wave.
///
/// Successful independent studies remain publishable even when another study
/// rejects execution. Failures are retained in deterministic study order;
/// dependents of a failed study are skipped for the wave so they cannot consume
/// stale upstream output as though it belonged to the new market mutation.
#[derive(Default)]
pub(crate) struct StudyExecutionBatch {
    pub(crate) executed: Vec<StudyInstanceId>,
    pub(crate) errors: Vec<StudyRuntimeError>,
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
    InvalidOutputPresentation,
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
    InvalidSettingPresentation,
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
            | Self::InvalidOutputPresentation
            | Self::DuplicateOutputIdentifier
            | Self::OutputMetadataTooLong { .. } => fmt_output_metadata_error(self, formatter),
            Self::TooManySettings { .. }
            | Self::InvalidSettingIdentifier
            | Self::InvalidSettingPresentation
            | Self::InvalidSettingValue
            | Self::UnknownSetting
            | Self::SettingTypeMismatch => fmt_setting_error(self, formatter),
            Self::StudyLimitExceeded { maximum } => {
                write!(formatter, "study runtime exceeds {maximum} instances")
            }
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

fn fmt_setting_error(error: &StudyRuntimeError, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    match error {
        StudyRuntimeError::TooManySettings { maximum } => {
            write!(formatter, "study setting count exceeds {maximum}")
        }
        StudyRuntimeError::InvalidSettingIdentifier => {
            formatter.write_str("study setting identifier is invalid")
        }
        StudyRuntimeError::InvalidSettingPresentation => {
            formatter.write_str("study setting presentation metadata is invalid")
        }
        StudyRuntimeError::InvalidSettingValue => {
            formatter.write_str("study setting value is invalid")
        }
        StudyRuntimeError::UnknownSetting => formatter.write_str("study setting is not declared"),
        StudyRuntimeError::SettingTypeMismatch => {
            formatter.write_str("study setting type does not match")
        }
        _ => formatter.write_str("study setting failed"),
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
        StudyRuntimeError::InvalidOutputPresentation => {
            formatter.write_str("study output presentation metadata is invalid")
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
    timeline: &'a StudyOutputTimeline,
    dirty: StudyDirtyRange,
}

struct CalculatedStudyOutputs {
    outputs: Vec<StudyOutputBuffer>,
    #[cfg(test)]
    preparation_work_rows: usize,
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
    executor: OnceLock<Result<execution::Executor, String>>,
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
    #[cfg(test)]
    last_output_preparation_work_rows: usize,
}

fn validate_study_outputs(outputs: &[StudyOutputSpec]) -> Result<(), StudyRuntimeError> {
    let mut identifiers = BTreeSet::new();
    for output in outputs {
        let identifier = output.identifier.trim();
        if identifier.is_empty() {
            return Err(StudyRuntimeError::InvalidOutputIdentifier);
        }
        if output.title.trim().is_empty()
            || output
                .legend_label
                .as_ref()
                .is_some_and(|label| label.trim().is_empty())
        {
            return Err(StudyRuntimeError::InvalidOutputTitle);
        }
        if identifier.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
            || output.title.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
            || output
                .legend_label
                .as_ref()
                .is_some_and(|label| label.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES)
        {
            return Err(StudyRuntimeError::OutputMetadataTooLong {
                maximum: MAXIMUM_STUDY_IDENTIFIER_BYTES,
            });
        }
        if !identifiers.insert(identifier) {
            return Err(StudyRuntimeError::DuplicateOutputIdentifier);
        }
        if let Some(region) = output.threshold_region {
            let scale = region.lower.scale.max(region.upper.scale);
            if scale > MAXIMUM_STUDY_SETTING_DECIMAL_SCALE {
                return Err(StudyRuntimeError::InvalidOutputPresentation);
            }
            let lower = region
                .lower
                .scaled_mantissa(scale)
                .ok_or(StudyRuntimeError::InvalidOutputPresentation)?;
            let upper = region
                .upper
                .scaled_mantissa(scale)
                .ok_or(StudyRuntimeError::InvalidOutputPresentation)?;
            if lower >= upper || !matches!(output.plot, StudyPlotKind::Line | StudyPlotKind::Area) {
                return Err(StudyRuntimeError::InvalidOutputPresentation);
            }
        }
        if output.point_style == StudyPointStyle::MomentumHistogram
            && output.plot != StudyPlotKind::Histogram
        {
            return Err(StudyRuntimeError::InvalidOutputPresentation);
        }
    }
    Ok(())
}

impl StudyRuntime {
    /// Creates an empty bounded runtime.
    #[must_use]
    pub fn new(config: StudyRuntimeConfig) -> Self {
        Self {
            executor: OnceLock::new(),
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
            #[cfg(test)]
            last_output_preparation_work_rows: 0,
        }
    }

    pub(crate) fn begin_turn(&self) {
        if let Ok(executor) = self.executor.get_or_init(execution::Executor::start) {
            executor.begin_turn();
        }
    }

    pub(crate) fn execution_failed(&self) -> bool {
        self.executor.get().is_some_and(|executor| match executor {
            Ok(executor) => executor.is_failed(),
            Err(_) => true,
        })
    }

    fn ensure_execution_available(
        &self,
        study_id: StudyInstanceId,
    ) -> Result<(), StudyRuntimeError> {
        if self.execution_failed() {
            return Err(StudyRuntimeError::ExecutionRejected {
                study_id,
                detail: "study execution disabled after worker deadline; restart required".into(),
            });
        }
        Ok(())
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

    /// Returns the transitive market streams required by one study.
    ///
    /// Output dependencies inherit the requirements of their upstream study;
    /// this keeps a downstream scalar publication honest when its calculation
    /// is fed by a trade- or depth-backed study output.
    #[must_use]
    pub fn input_stream_requirements(
        &self,
        study_id: StudyInstanceId,
    ) -> Option<StreamRequirements> {
        fn collect(
            runtime: &StudyRuntime,
            study_id: StudyInstanceId,
            visiting: &mut BTreeSet<StudyInstanceId>,
        ) -> Option<StreamRequirements> {
            if !visiting.insert(study_id) {
                return None;
            }
            let node = runtime.studies.get(&study_id)?;
            let mut streams = StreamRequirements::NONE;
            for dependency in &node.definition.dependencies {
                match dependency {
                    StudyDependency::Market(input) => {
                        streams = streams.union(input.streams);
                    }
                    StudyDependency::Output(output) => {
                        streams = streams.union(collect(runtime, output.study_id, visiting)?);
                    }
                }
            }
            visiting.remove(&study_id);
            Some(streams)
        }

        collect(self, study_id, &mut BTreeSet::new())
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
        self.ensure_execution_available(study_id)?;
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
        let timeline =
            primary_timeline(inputs.first().ok_or(StudyRuntimeError::MissingDependency)?);
        if timeline.len() == 0 {
            return Ok(false);
        }
        if timeline.len() > self.config.maximum_points_per_output.get() {
            return Err(StudyRuntimeError::OutputPointLimitExceeded {
                maximum: self.config.maximum_points_per_output.get(),
                requested: timeline.len(),
            });
        }
        let output_count = definition.outputs.len();
        let total_points =
            self.output_points_after_replacement(study_id, timeline.len(), output_count)?;
        let next_generation = self
            .next_output_generation
            .checked_add(1)
            .ok_or(StudyRuntimeError::OutputGenerationExhausted)?;
        let generation = self.next_output_generation;
        let dirty = if let Some(requested) = requested_dirty {
            let Some(clamped) = clamp_dirty_range(requested, timeline.len()) else {
                return Ok(false);
            };
            clamped
        } else {
            StudyDirtyRange {
                start: 0,
                end_exclusive: Some(timeline.len()),
            }
        };
        let covering_execution = dirty.start == 0 && dirty.end_exclusive == Some(timeline.len());
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
        let calculated = self.calculate_outputs(
            &NativeStudyCalculation {
                study_id,
                settings: &settings,
                program,
                output_count,
                inputs: &inputs,
                live_inputs: &live_inputs,
                timeline: &timeline,
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
        self.commit_outputs(
            study_id,
            &timeline,
            calculated.outputs,
            generation,
            total_points,
        );
        #[cfg(test)]
        {
            self.last_output_preparation_work_rows = calculated.preparation_work_rows;
        }
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
    ) -> Result<CalculatedStudyOutputs, StudyRuntimeError> {
        let prepared = (0..calculation.output_count)
            .map(|output_index| {
                let output = StudyOutputId {
                    study_id: calculation.study_id,
                    output_index,
                };
                StudyOutputBuffer::from_previous(
                    calculation.timeline,
                    self.outputs.get(&output),
                    calculation.dirty,
                )
            })
            .collect::<Vec<_>>();
        #[cfg(test)]
        let preparation_work_rows = prepared
            .iter()
            .map(|(_, work_rows)| *work_rows)
            .sum::<usize>();
        let mut outputs = prepared
            .into_iter()
            .map(|(output, _)| output)
            .collect::<Vec<_>>();
        let executor = self
            .executor
            .get_or_init(execution::Executor::start)
            .as_ref()
            .map_err(|detail| StudyRuntimeError::ExecutionRejected {
                study_id: calculation.study_id,
                detail: detail.clone(),
            })?;
        let result = executor
            .calculate(calculation, outputs, state.take())
            .map_err(|detail| StudyRuntimeError::ExecutionRejected {
                study_id: calculation.study_id,
                detail,
            })?;
        outputs = result.0;
        *state = result.1;
        Ok(CalculatedStudyOutputs {
            outputs,
            #[cfg(test)]
            preparation_work_rows,
        })
    }

    fn commit_outputs(
        &mut self,
        study_id: StudyInstanceId,
        timeline: &StudyOutputTimeline,
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
                    timeline: timeline.clone(),
                    values: output.into_values(),
                    materialized_values: Arc::new(OnceLock::new()),
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
        let batch = self.execute_live_market_change_with_live(
            engine,
            series,
            exchange_timestamp_unix_nanos,
            &mut no_live_market,
        )?;
        if let Some(error) = batch.errors.into_iter().next() {
            Err(error)
        } else {
            Ok(batch.executed)
        }
    }

    pub(crate) fn execute_live_market_change_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        series: &BarSeriesKey,
        exchange_timestamp_unix_nanos: i64,
        live_market: &mut F,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError>
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

        self.execute_pending_ranges_with_live(engine, pending, live_market, None)
    }

    pub(crate) fn execute_live_non_bar_change_with_live<'a, F, R>(
        &mut self,
        engine: &MarketEngine,
        change: StudyNonBarChange<'_>,
        live_market: &mut F,
        market_ready: &mut R,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
        R: FnMut(&StudyMarketInput) -> bool,
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
        self.execute_pending_ranges_with_live(engine, pending, live_market, Some(market_ready))
    }

    /// Incrementally executes studies affected by one successful ranged
    /// historical repair. The supplied bounds are the actual provider-returned
    /// bar timestamps, not merely the requested viewport, so unchanged retained
    /// history stays outside the dirty range.
    pub(crate) fn execute_history_range_change_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        series: &BarSeriesKey,
        first_changed_unix_nanos: i64,
        last_changed_unix_nanos: i64,
        live_market: &mut F,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        if first_changed_unix_nanos > last_changed_unix_nanos {
            return Ok(StudyExecutionBatch::default());
        }
        let direct = self
            .market_dependents
            .get(series)
            .cloned()
            .unwrap_or_default();
        let mut pending = BTreeMap::<StudyInstanceId, StudyDirtyRange>::new();
        for study_id in direct {
            let Some(range) = self.market_history_change_range(
                engine,
                study_id,
                series,
                first_changed_unix_nanos,
                last_changed_unix_nanos,
            )?
            else {
                continue;
            };
            pending
                .entry(study_id)
                .and_modify(|current| *current = current.merge(range))
                .or_insert(range);
        }
        self.execute_pending_ranges_with_live(engine, pending, live_market, None)
    }

    fn execute_pending_ranges_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        mut pending: BTreeMap<StudyInstanceId, StudyDirtyRange>,
        live_market: &mut F,
        mut market_ready: Option<&mut dyn FnMut(&StudyMarketInput) -> bool>,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError>
    where
        F: FnMut(&StudyMarketInput) -> Option<StudyLiveMarketData<'a>>,
    {
        let ordered = self.studies.keys().copied().collect::<Vec<_>>();
        let mut batch = StudyExecutionBatch::default();
        let mut blocked = BTreeSet::new();
        for study_id in ordered {
            let Some(range) = pending.remove(&study_id) else {
                continue;
            };
            if blocked.contains(&study_id) {
                continue;
            }
            if let Some(readiness) = market_ready.as_deref_mut() {
                let ready = self.study_market_ancestry_ready(study_id, readiness)?;
                if !ready {
                    blocked.extend(self.dependent_subtree(study_id)?);
                    continue;
                }
            }
            match self.execute_ready_range_with_live(engine, study_id, Some(range), live_market) {
                Ok(true) => {
                    batch.executed.push(study_id);
                    if let Err(error) =
                        self.propagate_output_change(engine, study_id, range, &mut pending)
                    {
                        batch.errors.push(error);
                        blocked.extend(self.dependent_subtree(study_id)?);
                    }
                }
                Ok(false) | Err(StudyRuntimeError::StudyNotExecutable(_)) => {
                    blocked.extend(self.dependent_subtree(study_id)?);
                }
                Err(error) => {
                    batch.errors.push(error);
                    blocked.extend(self.dependent_subtree(study_id)?);
                }
            }
        }
        Ok(batch)
    }

    fn study_market_ancestry_ready(
        &self,
        study_id: StudyInstanceId,
        readiness: &mut dyn FnMut(&StudyMarketInput) -> bool,
    ) -> Result<bool, StudyRuntimeError> {
        let mut pending = vec![study_id];
        let mut visited = BTreeSet::new();
        while let Some(candidate) = pending.pop() {
            if !visited.insert(candidate) {
                continue;
            }
            let node = self
                .studies
                .get(&candidate)
                .ok_or(StudyRuntimeError::UnknownStudy(candidate))?;
            for dependency in &node.definition.dependencies {
                match dependency {
                    StudyDependency::Market(input) => {
                        if !readiness(input) {
                            return Ok(false);
                        }
                    }
                    StudyDependency::Output(output) => pending.push(output.study_id),
                }
            }
        }
        Ok(true)
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
                    self.primary_row_containing(engine, &node.definition, observed_unix_nanos)
                else {
                    continue;
                };
                start
            };
            if start >= length {
                continue;
            }
            let candidate =
                if !matches!(changed_stream, MarketStream::Bars) || dependency_index == 0 {
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

    fn market_history_change_range(
        &self,
        engine: &MarketEngine,
        study_id: StudyInstanceId,
        changed_series: &BarSeriesKey,
        first_changed_unix_nanos: i64,
        last_changed_unix_nanos: i64,
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
            if input.series != *changed_series || !input.streams.contains(MarketStream::Bars) {
                continue;
            }
            let Some(length) = self.primary_timeline_len(engine, &node.definition) else {
                continue;
            };
            let Some(start) =
                self.primary_lower_bound(engine, &node.definition, first_changed_unix_nanos)
            else {
                continue;
            };
            if start >= length {
                continue;
            }
            let candidate = if dependency_index == 0 {
                let Some(end_exclusive) =
                    self.primary_upper_bound(engine, &node.definition, last_changed_unix_nanos)
                else {
                    continue;
                };
                let end_exclusive = end_exclusive.min(length);
                if start >= end_exclusive {
                    continue;
                }
                StudyDirtyRange::bounded(start, end_exclusive)?
                    .invalidated_by(node.definition.invalidation)?
            } else {
                // Historical changes in a secondary time-aligned input can alter
                // every later primary row that resolves that input as-of time.
                StudyDirtyRange::to_tail(start)
            };
            dirty = Some(dirty.map_or(candidate, |current: StudyDirtyRange| {
                current.merge(candidate)
            }));
        }
        Ok(dirty)
    }

    fn primary_row_containing(
        &self,
        engine: &MarketEngine,
        definition: &StudyDefinition,
        observed_unix_nanos: i64,
    ) -> Option<usize> {
        let snapshot = match definition.dependencies.first()? {
            StudyDependency::Market(input) => engine.series_snapshot(&input.series)?,
            StudyDependency::Output(output) => {
                Arc::clone(&self.outputs.get(output)?.timeline.0.snapshot)
            }
        };
        let upper = snapshot
            .bars
            .partition_point(|bar| bar.exchange_timestamp_unix_nanos <= observed_unix_nanos);
        if upper == 0 {
            return None;
        }
        let index = upper - 1;
        let bar = snapshot.bars.get(index)?;
        if let Some(duration) = snapshot.series.period.duration_nanos() {
            return bar
                .exchange_timestamp_unix_nanos
                .checked_add(duration)
                .is_some_and(|end_exclusive| observed_unix_nanos < end_exclusive)
                .then_some(index);
        }
        if let Some(next) = snapshot.bars.get(index + 1) {
            return (observed_unix_nanos < next.exchange_timestamp_unix_nanos).then_some(index);
        }
        // Tick/calendar periods do not expose a fixed nanosecond duration. The
        // newest row can own later live non-bar events only while it is explicitly
        // the forming canonical tail; a completed last row must not absorb an
        // arbitrary future event.
        (snapshot.forming || observed_unix_nanos == bar.exchange_timestamp_unix_nanos)
            .then_some(index)
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
                    .timeline
                    .lower_bound(exchange_timestamp_unix_nanos),
            ),
        }
    }

    fn primary_upper_bound(
        &self,
        engine: &MarketEngine,
        definition: &StudyDefinition,
        exchange_timestamp_unix_nanos: i64,
    ) -> Option<usize> {
        match definition.dependencies.first()? {
            StudyDependency::Market(input) => {
                let snapshot = engine.series_snapshot(&input.series)?;
                Some(snapshot.bars.partition_point(|bar| {
                    bar.exchange_timestamp_unix_nanos <= exchange_timestamp_unix_nanos
                }))
            }
            StudyDependency::Output(output) => Some(
                self.outputs
                    .get(output)?
                    .timeline
                    .upper_bound(exchange_timestamp_unix_nanos),
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
        let batch = self.execute_ready_for_market_with_live(engine, series, &mut no_live_market)?;
        if let Some(error) = batch.errors.into_iter().next() {
            Err(error)
        } else {
            Ok(batch.executed)
        }
    }

    pub(crate) fn execute_ready_for_market_with_live<'a, F>(
        &mut self,
        engine: &MarketEngine,
        series: &BarSeriesKey,
        live_market: &mut F,
    ) -> Result<StudyExecutionBatch, StudyRuntimeError>
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

        let mut batch = StudyExecutionBatch::default();
        let mut blocked = BTreeSet::new();
        for study_id in affected {
            if blocked.contains(&study_id) {
                continue;
            }
            match self.execute_ready_with_live(engine, study_id, live_market) {
                Ok(true) => batch.executed.push(study_id),
                Ok(false) | Err(StudyRuntimeError::StudyNotExecutable(_)) => {
                    blocked.extend(self.dependent_subtree(study_id)?);
                }
                Err(error) => {
                    batch.errors.push(error);
                    blocked.extend(self.dependent_subtree(study_id)?);
                }
            }
        }
        Ok(batch)
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
        validate_study_outputs(&definition.outputs)?;
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

fn primary_timeline(input: &ResolvedStudyInput) -> StudyOutputTimeline {
    match input {
        ResolvedStudyInput::Market(series) => {
            StudyOutputTimeline::market(Arc::clone(&series.snapshot))
        }
        ResolvedStudyInput::Output(series) => series.timeline.clone(),
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
mod tests;
