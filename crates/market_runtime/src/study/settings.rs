use super::{
    MAXIMUM_STUDY_IDENTIFIER_BYTES, MAXIMUM_STUDY_SETTING_CHOICE_OPTIONS,
    MAXIMUM_STUDY_SETTING_DECIMAL_SCALE, MAXIMUM_STUDY_SETTING_IDENTIFIER_BYTES,
    MAXIMUM_STUDY_SETTING_TEXT_BYTES, MAXIMUM_STUDY_SETTINGS, StudyMarketInput, StudyRuntimeError,
};
use aeris_market_data::{BarPeriod, BarSeriesKey};
use aeris_market_engine::StreamRequirements;
use std::collections::{BTreeMap, BTreeSet};

/// Exact 8-bit RGBA color shared across semantic study settings, outputs, and scene objects.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StudyColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl StudyColor {
    /// Creates an opaque RGB color (`a = 255`).
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Creates an RGBA color with explicit opacity.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Packs this color as `0xRRGGBBAA` for compact durable storage.
    #[must_use]
    pub const fn to_rgba_u32(self) -> u32 {
        u32::from_be_bytes([self.r, self.g, self.b, self.a])
    }

    /// Unpacks a `0xRRGGBBAA` integer into a color.
    #[must_use]
    pub const fn from_rgba_u32(packed: u32) -> Self {
        let [r, g, b, a] = packed.to_be_bytes();
        Self { r, g, b, a }
    }

    /// Parses `#RRGGBB` or `#RRGGBBAA` hex color strings.
    #[must_use]
    pub fn parse_hex(input: &str) -> Option<Self> {
        let hex = input.trim().strip_prefix('#')?;
        let bytes = hex.as_bytes();
        let parse_pair = |start: usize| -> Option<u8> {
            let text = std::str::from_utf8(bytes.get(start..start + 2)?).ok()?;
            u8::from_str_radix(text, 16).ok()
        };
        match bytes.len() {
            6 => Some(Self::rgb(parse_pair(0)?, parse_pair(2)?, parse_pair(4)?)),
            8 => Some(Self::rgba(
                parse_pair(0)?,
                parse_pair(2)?,
                parse_pair(4)?,
                parse_pair(6)?,
            )),
            _ => None,
        }
    }

    /// Formats this color as `#rrggbb` when opaque or `#rrggbbaa` when translucent.
    #[must_use]
    pub fn to_hex(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }
}

/// Canonical provider-neutral instrument reference for semantic Symbol settings.
///
/// Both fields empty means "follow the current chart instrument" when allowed by the control.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StudySymbolRef {
    pub provider_id: String,
    pub instrument_id: String,
}

impl StudySymbolRef {
    /// Sentinel value representing the current chart's instrument.
    #[must_use]
    pub const fn current_chart() -> Self {
        Self {
            provider_id: String::new(),
            instrument_id: String::new(),
        }
    }

    /// Creates an explicit canonical instrument reference.
    #[must_use]
    pub fn new(provider_id: impl Into<String>, instrument_id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            instrument_id: instrument_id.into(),
        }
    }

    /// Returns whether this reference follows the current chart's instrument.
    #[must_use]
    pub fn is_current_chart(&self) -> bool {
        self.provider_id.is_empty() && self.instrument_id.is_empty()
    }

    /// Parses `""` (current chart) or `"provider_id/instrument_id"`.
    #[must_use]
    pub fn parse_label(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Some(Self::current_chart());
        }
        let (provider, instrument) = trimmed.split_once('/')?;
        let provider = provider.trim();
        let instrument = instrument.trim();
        if provider.is_empty()
            || instrument.is_empty()
            || provider.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
            || instrument.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
        {
            return None;
        }
        Some(Self::new(provider, instrument))
    }

    /// Formats `""` for current chart or `"provider_id/instrument_id"`.
    #[must_use]
    pub fn to_label(&self) -> String {
        if self.is_current_chart() {
            String::new()
        } else {
            format!("{}/{}", self.provider_id, self.instrument_id)
        }
    }

    fn validate(&self) -> Result<(), StudyRuntimeError> {
        if self.is_current_chart() {
            return Ok(());
        }
        if self.provider_id.trim().is_empty()
            || self.provider_id.trim() != self.provider_id
            || self.provider_id.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
            || self.instrument_id.trim().is_empty()
            || self.instrument_id.trim() != self.instrument_id
            || self.instrument_id.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
        {
            return Err(StudyRuntimeError::InvalidSettingValue);
        }
        Ok(())
    }
}

/// Semantic timeframe setting: either the current chart's period or an explicit canonical period.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StudyTimeframe {
    #[default]
    CurrentChart,
    Period(BarPeriod),
}

impl StudyTimeframe {
    /// Resolves this timeframe against the chart's current `BarPeriod`.
    #[must_use]
    pub const fn resolve(self, current: BarPeriod) -> BarPeriod {
        match self {
            Self::CurrentChart => current,
            Self::Period(period) => period,
        }
    }

    /// Parses canonical interval labels (`""`, `"1s"`, `"1m"`, `"15m"`, `"1h"`, `"1D"`, `"1W"`, `"1M"`, `"100t"`).
    #[must_use]
    pub fn parse_label(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("chart") {
            return Some(Self::CurrentChart);
        }
        let (digits, suffix) = trimmed.split_at(trimmed.len().checked_sub(1)?);
        let value = digits.parse::<u32>().ok()?;
        let period = match suffix {
            "t" | "T" => BarPeriod::Tick { trades: value },
            "s" | "S" => BarPeriod::Time { seconds: value },
            "m" => BarPeriod::Time {
                seconds: value.checked_mul(60)?,
            },
            "h" | "H" => BarPeriod::Time {
                seconds: value.checked_mul(3_600)?,
            },
            "d" | "D" => BarPeriod::Session { days: value },
            "w" | "W" => BarPeriod::Week { weeks: value },
            "M" => BarPeriod::Month { months: value },
            _ => return None,
        };
        period.validate().ok()?;
        Some(Self::Period(period))
    }

    /// Formats this timeframe as a canonical interval label (`""` for `CurrentChart`).
    #[must_use]
    pub fn to_label(self) -> String {
        match self {
            Self::CurrentChart => String::new(),
            Self::Period(BarPeriod::Tick { trades }) => format!("{trades}t"),
            Self::Period(BarPeriod::Time { seconds }) => {
                if seconds >= 86_400 && seconds % 86_400 == 0 {
                    format!("{}D", seconds / 86_400)
                } else if seconds >= 3_600 && seconds % 3_600 == 0 {
                    format!("{}h", seconds / 3_600)
                } else if seconds >= 60 && seconds % 60 == 0 {
                    format!("{}m", seconds / 60)
                } else {
                    format!("{seconds}s")
                }
            }
            Self::Period(BarPeriod::Session { days }) => format!("{days}D"),
            Self::Period(BarPeriod::Week { weeks }) => format!("{weeks}W"),
            Self::Period(BarPeriod::Month { months }) => format!("{months}M"),
        }
    }

    fn validate(self) -> Result<(), StudyRuntimeError> {
        match self {
            Self::CurrentChart => Ok(()),
            Self::Period(period) => period
                .validate()
                .map_err(|_| StudyRuntimeError::InvalidSettingValue),
        }
    }
}

/// Semantic intraday session window (minutes from midnight in `time_zone`, or exchange zone if empty).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StudySession {
    pub start_minute: u16,
    pub end_minute: u16,
    /// Optional IANA time zone name (e.g. `"America/New_York"`). Empty uses the exchange time zone.
    pub time_zone: String,
}

impl StudySession {
    /// Creates a session window in the given optional IANA time zone.
    #[must_use]
    pub fn new(start_minute: u16, end_minute: u16, time_zone: impl Into<String>) -> Self {
        Self {
            start_minute,
            end_minute,
            time_zone: time_zone.into(),
        }
    }

    /// Parses `"HH:MM-HH:MM"` or `"HH:MM-HH:MM Area/Location"`.
    #[must_use]
    pub fn parse_label(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        let (window, tz) = match trimmed.split_once(char::is_whitespace) {
            Some((w, t)) => (w.trim(), t.trim()),
            None => (trimmed, ""),
        };
        let (start_text, end_text) = window.split_once('-')?;
        let parse_hhmm = |text: &str, allow_2400: bool| -> Option<u16> {
            let (hh, mm) = text.split_once(':')?;
            if hh.len() != 2 || mm.len() != 2 {
                return None;
            }
            let h = hh.parse::<u16>().ok()?;
            let m = mm.parse::<u16>().ok()?;
            if h == 24 && m == 0 && allow_2400 {
                return Some(1_440);
            }
            (h < 24 && m < 60).then_some(h * 60 + m)
        };
        let start_minute = parse_hhmm(start_text, false)?;
        let end_minute = parse_hhmm(end_text, true)?;
        let session = Self::new(start_minute, end_minute, tz);
        session.validate().ok()?;
        Some(session)
    }

    /// Formats as `"HH:MM-HH:MM"` or `"HH:MM-HH:MM Area/Location"`.
    #[must_use]
    pub fn to_label(&self) -> String {
        let sh = self.start_minute / 60;
        let sm = self.start_minute % 60;
        let eh = self.end_minute / 60;
        let em = self.end_minute % 60;
        if self.time_zone.is_empty() {
            format!("{sh:02}:{sm:02}-{eh:02}:{em:02}")
        } else {
            format!("{sh:02}:{sm:02}-{eh:02}:{em:02} {}", self.time_zone)
        }
    }

    /// Returns whether `minute_of_day` (`0..1440`) falls inside this session window (supports overnight wrap).
    #[must_use]
    pub const fn contains_minute(&self, minute_of_day: u16) -> bool {
        if self.start_minute < self.end_minute {
            minute_of_day >= self.start_minute && minute_of_day < self.end_minute
        } else {
            minute_of_day >= self.start_minute || minute_of_day < self.end_minute
        }
    }

    fn validate(&self) -> Result<(), StudyRuntimeError> {
        if self.start_minute >= 1_440
            || self.end_minute > 1_440
            || self.start_minute == self.end_minute
            || self.time_zone.trim() != self.time_zone
            || self.time_zone.len() > MAXIMUM_STUDY_SETTING_IDENTIFIER_BYTES
        {
            return Err(StudyRuntimeError::InvalidSettingValue);
        }
        if !self.time_zone.is_empty() && self.time_zone.parse::<chrono_tz::Tz>().is_err() {
            return Err(StudyRuntimeError::InvalidSettingValue);
        }
        Ok(())
    }
}

/// Semantic source input: either a canonical bar calculation or a prior study's durable output.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StudySource {
    Open,
    High,
    Low,
    #[default]
    Close,
    Hl2,
    Hlc3,
    Ohlc4,
    Volume,
    /// Durable reference to another study's output using workspace `local_id` (never transient runtime IDs).
    StudyOutput {
        study_local_id: u64,
        output_identifier: String,
    },
}

impl StudySource {
    /// Parses canonical source labels (`"open"`, `"high"`, `"low"`, `"close"`, `"hl2"`, `"hlc3"`, `"ohlc4"`, `"volume"`, or `"study:<local_id>:<output>"`).
    #[must_use]
    pub fn parse_label(input: &str) -> Option<Self> {
        match input.trim() {
            "open" | "Open" => Some(Self::Open),
            "high" | "High" => Some(Self::High),
            "low" | "Low" => Some(Self::Low),
            "close" | "Close" => Some(Self::Close),
            "hl2" | "HL2" => Some(Self::Hl2),
            "hlc3" | "HLC3" => Some(Self::Hlc3),
            "ohlc4" | "OHLC4" => Some(Self::Ohlc4),
            "volume" | "Volume" => Some(Self::Volume),
            other => {
                let rest = other.strip_prefix("study:")?;
                let (id_str, output_id) = rest.split_once(':')?;
                let study_local_id = id_str.parse::<u64>().ok()?;
                let source = Self::StudyOutput {
                    study_local_id,
                    output_identifier: output_id.trim().to_string(),
                };
                source.validate().ok()?;
                Some(source)
            }
        }
    }

    /// Formats this source as a canonical label.
    #[must_use]
    pub fn to_label(&self) -> String {
        match self {
            Self::Open => "open".to_string(),
            Self::High => "high".to_string(),
            Self::Low => "low".to_string(),
            Self::Close => "close".to_string(),
            Self::Hl2 => "hl2".to_string(),
            Self::Hlc3 => "hlc3".to_string(),
            Self::Ohlc4 => "ohlc4".to_string(),
            Self::Volume => "volume".to_string(),
            Self::StudyOutput {
                study_local_id,
                output_identifier,
            } => format!("study:{study_local_id}:{output_identifier}"),
        }
    }

    fn validate(&self) -> Result<(), StudyRuntimeError> {
        if let Self::StudyOutput {
            study_local_id,
            output_identifier,
        } = self
            && (*study_local_id == 0
                || output_identifier.trim().is_empty()
                || output_identifier.trim() != output_identifier
                || output_identifier.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES)
        {
            return Err(StudyRuntimeError::InvalidSettingValue);
        }
        Ok(())
    }
}

/// Resolves semantic Symbol and Timeframe settings against a chart's base `BarSeriesKey` into
/// one canonical `StudyMarketInput` suitable for `MarketEngine` lease reconciliation.
///
/// # Errors
/// Returns [`StudyRuntimeError::InvalidMarketSeries`] or [`StudyRuntimeError::EmptyMarketStreams`]
/// if the resolved series key or stream set fails canonical validation.
pub fn resolve_market_input(
    base: &BarSeriesKey,
    symbol: Option<&StudySymbolRef>,
    timeframe: Option<StudyTimeframe>,
    streams: StreamRequirements,
) -> Result<StudyMarketInput, StudyRuntimeError> {
    if streams.is_empty() {
        return Err(StudyRuntimeError::EmptyMarketStreams);
    }
    let mut series = base.clone();
    if let Some(symbol) = symbol
        && !symbol.is_current_chart()
    {
        symbol.validate()?;
        series.provider_id.clone_from(&symbol.provider_id);
        series.instrument_id.clone_from(&symbol.instrument_id);
    }
    if let Some(timeframe) = timeframe {
        timeframe.validate()?;
        series.period = timeframe.resolve(base.period);
    }
    series
        .validate()
        .map_err(|_| StudyRuntimeError::InvalidMarketSeries)?;
    Ok(StudyMarketInput { series, streams })
}

/// Exact base-10 decimal used by study settings without introducing binary
/// floating-point ambiguity into durable configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StudyDecimal {
    pub mantissa: i64,
    pub scale: u8,
}

impl StudyDecimal {
    pub(super) fn scaled_mantissa(self, scale: u8) -> Option<i128> {
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
    Color(StudyColor),
    Symbol(StudySymbolRef),
    Timeframe(StudyTimeframe),
    Session(StudySession),
    Timestamp(i64),
    Price(StudyDecimal),
    Source(StudySource),
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
                | (Self::Color(_), Self::Color(_))
                | (Self::Symbol(_), Self::Symbol(_))
                | (Self::Timeframe(_), Self::Timeframe(_))
                | (Self::Session(_), Self::Session(_))
                | (Self::Timestamp(_), Self::Timestamp(_))
                | (Self::Price(_), Self::Price(_))
                | (Self::Source(_), Self::Source(_))
        )
    }

    fn validate(&self) -> Result<(), StudyRuntimeError> {
        match self {
            Self::Decimal(decimal) | Self::Price(decimal)
                if decimal.scale > MAXIMUM_STUDY_SETTING_DECIMAL_SCALE =>
            {
                Err(StudyRuntimeError::InvalidSettingValue)
            }
            Self::Text(value) | Self::Choice(value)
                if value.len() > MAXIMUM_STUDY_SETTING_TEXT_BYTES =>
            {
                Err(StudyRuntimeError::InvalidSettingValue)
            }
            Self::Symbol(symbol) => symbol.validate(),
            Self::Timeframe(timeframe) => timeframe.validate(),
            Self::Session(session) => session.validate(),
            Self::Source(source) => source.validate(),
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
    Color,
    Symbol {
        allow_current_chart: bool,
    },
    Timeframe {
        allow_current_chart: bool,
    },
    Session,
    Timestamp,
    Price {
        minimum: Option<StudyDecimal>,
        maximum: Option<StudyDecimal>,
        step: Option<StudyDecimal>,
    },
    Source {
        allow_study_output: bool,
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
            StudySettingValue::Color(_) => StudySettingControl::Color,
            StudySettingValue::Symbol(symbol) => StudySettingControl::Symbol {
                allow_current_chart: symbol.is_current_chart(),
            },
            StudySettingValue::Timeframe(timeframe) => StudySettingControl::Timeframe {
                allow_current_chart: matches!(timeframe, StudyTimeframe::CurrentChart),
            },
            StudySettingValue::Session(_) => StudySettingControl::Session,
            StudySettingValue::Timestamp(_) => StudySettingControl::Timestamp,
            StudySettingValue::Price(_) => StudySettingControl::Price {
                minimum: None,
                maximum: None,
                step: None,
            },
            StudySettingValue::Source(source) => StudySettingControl::Source {
                allow_study_output: matches!(source, StudySource::StudyOutput { .. }),
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

    /// Returns one boolean setting value.
    #[must_use]
    pub fn bool(&self, identifier: &str) -> Option<bool> {
        match self.get(identifier)? {
            StudySettingValue::Boolean(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns one integer setting value.
    #[must_use]
    pub fn integer(&self, identifier: &str) -> Option<i64> {
        match self.get(identifier)? {
            StudySettingValue::Integer(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns one exact decimal setting value.
    #[must_use]
    pub fn decimal(&self, identifier: &str) -> Option<StudyDecimal> {
        match self.get(identifier)? {
            StudySettingValue::Decimal(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns one textual setting value.
    #[must_use]
    pub fn text(&self, identifier: &str) -> Option<&str> {
        match self.get(identifier)? {
            StudySettingValue::Text(value) => Some(value.as_str()),
            _ => None,
        }
    }

    /// Returns one choice setting identifier.
    #[must_use]
    pub fn choice(&self, identifier: &str) -> Option<&str> {
        match self.get(identifier)? {
            StudySettingValue::Choice(value) => Some(value.as_str()),
            _ => None,
        }
    }

    /// Returns one semantic color setting value.
    #[must_use]
    pub fn color(&self, identifier: &str) -> Option<StudyColor> {
        match self.get(identifier)? {
            StudySettingValue::Color(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns one semantic symbol reference setting value.
    #[must_use]
    pub fn symbol(&self, identifier: &str) -> Option<&StudySymbolRef> {
        match self.get(identifier)? {
            StudySettingValue::Symbol(value) => Some(value),
            _ => None,
        }
    }

    /// Returns one semantic timeframe setting value.
    #[must_use]
    pub fn timeframe(&self, identifier: &str) -> Option<StudyTimeframe> {
        match self.get(identifier)? {
            StudySettingValue::Timeframe(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns one semantic session setting value.
    #[must_use]
    pub fn session(&self, identifier: &str) -> Option<&StudySession> {
        match self.get(identifier)? {
            StudySettingValue::Session(value) => Some(value),
            _ => None,
        }
    }

    /// Returns one semantic Unix-nanosecond timestamp setting value.
    #[must_use]
    pub fn timestamp(&self, identifier: &str) -> Option<i64> {
        match self.get(identifier)? {
            StudySettingValue::Timestamp(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns one semantic price setting value.
    #[must_use]
    pub fn price(&self, identifier: &str) -> Option<StudyDecimal> {
        match self.get(identifier)? {
            StudySettingValue::Price(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns one semantic source setting value.
    #[must_use]
    pub fn source(&self, identifier: &str) -> Option<&StudySource> {
        match self.get(identifier)? {
            StudySettingValue::Source(value) => Some(value),
            _ => None,
        }
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

pub(super) fn validate_setting_specs(specs: &[StudySettingSpec]) -> Result<(), StudyRuntimeError> {
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
        | (StudySettingValue::Text(_), StudySettingControl::Text)
        | (StudySettingValue::Color(_), StudySettingControl::Color)
        | (StudySettingValue::Session(_), StudySettingControl::Session)
        | (StudySettingValue::Timestamp(_), StudySettingControl::Timestamp)
        | (StudySettingValue::Symbol(_), StudySettingControl::Symbol { .. })
        | (StudySettingValue::Timeframe(_), StudySettingControl::Timeframe { .. })
        | (StudySettingValue::Source(_), StudySettingControl::Source { .. }) => Ok(()),
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
        )
        | (
            StudySettingValue::Price(_),
            StudySettingControl::Price {
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
        | (StudySettingValue::Text(_), StudySettingControl::Text)
        | (StudySettingValue::Color(_), StudySettingControl::Color)
        | (StudySettingValue::Session(_), StudySettingControl::Session)
        | (StudySettingValue::Timestamp(_), StudySettingControl::Timestamp) => Ok(()),
        (
            StudySettingValue::Symbol(symbol),
            StudySettingControl::Symbol {
                allow_current_chart,
            },
        ) => {
            if !allow_current_chart && symbol.is_current_chart() {
                return Err(StudyRuntimeError::InvalidSettingValue);
            }
            Ok(())
        }
        (
            StudySettingValue::Timeframe(timeframe),
            StudySettingControl::Timeframe {
                allow_current_chart,
            },
        ) => {
            if !allow_current_chart && matches!(timeframe, StudyTimeframe::CurrentChart) {
                return Err(StudyRuntimeError::InvalidSettingValue);
            }
            Ok(())
        }
        (StudySettingValue::Source(source), StudySettingControl::Source { allow_study_output }) => {
            if !allow_study_output && matches!(source, StudySource::StudyOutput { .. }) {
                return Err(StudyRuntimeError::InvalidSettingValue);
            }
            Ok(())
        }
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
        )
        | (
            StudySettingValue::Price(value),
            StudySettingControl::Price {
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

pub(super) fn validate_settings(
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
