use super::{
    MAXIMUM_STUDY_SETTING_CHOICE_OPTIONS, MAXIMUM_STUDY_SETTING_DECIMAL_SCALE,
    MAXIMUM_STUDY_SETTING_IDENTIFIER_BYTES, MAXIMUM_STUDY_SETTING_TEXT_BYTES,
    MAXIMUM_STUDY_SETTINGS, StudyRuntimeError,
};
use std::collections::{BTreeMap, BTreeSet};

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
