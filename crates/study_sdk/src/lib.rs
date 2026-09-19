//! `TradingPlot` native Study SDK.
//!
//! This crate is the stable Axius-owned surface above the in-process Study
//! Runtime. Technical-analysis math delegates to the exact pinned Nucleus pure
//! indicator crate so built-ins and SDK studies do not fork formula behavior.

use num_traits::ToPrimitive;
use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    num::NonZeroUsize,
    panic::{AssertUnwindSafe, catch_unwind},
};
pub use tradingplot_market_data::{
    AggressorSide, BarPeriod, BarSeriesKey, DepthLevel, OrderBookState,
};
pub use tradingplot_market_runtime::{
    MarketStream, StreamRequirements,
    study::{
        MAXIMUM_STUDY_IDENTIFIER_BYTES, NativeStudyCalculate, NativeStudyProgram,
        NativeStudyRegistration, NativeStudyState, NativeStudyStateFactory, StudyBarField,
        StudyDecimal, StudyDefinition, StudyDependency, StudyDepthView, StudyDirtyRange,
        StudyExecutionContext, StudyExecutionInputs, StudyInputSeries, StudyInstanceId,
        StudyInvalidationPolicy, StudyLiveMarketData, StudyMarketInput, StudyMarketSeries,
        StudyOutputBuffer, StudyOutputId, StudyOutputSpec, StudyPaneTarget, StudyPlotKind,
        StudyPointStyle, StudyQuoteView, StudyRuntimeError, StudyScaleTarget,
        StudySettingChoiceOption, StudySettingCondition, StudySettingControl,
        StudySettingPresentation, StudySettingSpec, StudySettingValue, StudySettings,
        StudyThresholdRegion, StudyTradeSample, StudyTradeWindow,
    },
};

/// Converts one fixed-point market value into the floating-point representation used by study
/// formulas without requiring authors to depend on `TradingPlot`'s internal conversion crate.
///
/// The conversion is intentionally explicit because canonical market storage remains fixed-point.
/// `None` is reserved for a conversion failure; ordinary `i64` market values and the runtime's
/// bounded decimal scales are representable as finite `f64` values.
#[must_use]
pub fn fixed_point_to_f64(value: i64, scale: u8) -> Option<f64> {
    let value = value.to_f64()?;
    let divisor = 10_f64.powi(i32::from(scale));
    (value.is_finite() && divisor.is_finite() && divisor != 0.0).then_some(value / divisor)
}

/// Stable implementation revision for the built-in Simple Moving Average.
pub const BUILTIN_SMA_IMPLEMENTATION_REVISION: u32 = 1;
/// Stable implementation identifier for the built-in Simple Moving Average.
pub const BUILTIN_SMA_IDENTIFIER: &str = "builtin.sma";
/// Stable durable setting identifier for the built-in SMA period.
pub const BUILTIN_SMA_PERIOD_SETTING: &str = "period";
/// Default built-in SMA period used by legacy workspace migration and product UI.
pub const BUILTIN_SMA_DEFAULT_PERIOD: i64 = 20;
/// Stable output identifier exposed by the built-in SMA.
pub const BUILTIN_SMA_OUTPUT_IDENTIFIER: &str = "sma";
/// Stable implementation revision for the built-in Exponential Moving Average.
pub const BUILTIN_EMA_IMPLEMENTATION_REVISION: u32 = 1;
/// Stable implementation identifier for the built-in Exponential Moving Average.
pub const BUILTIN_EMA_IDENTIFIER: &str = "builtin.ema";
/// Stable durable setting identifier for the built-in EMA period.
pub const BUILTIN_EMA_PERIOD_SETTING: &str = "period";
/// Default built-in EMA period used by legacy workspace migration and product UI.
pub const BUILTIN_EMA_DEFAULT_PERIOD: i64 = 20;
/// Stable output identifier exposed by the built-in EMA.
pub const BUILTIN_EMA_OUTPUT_IDENTIFIER: &str = "ema";
/// Stable implementation revision for the built-in EMA Ribbon.
pub const BUILTIN_EMA_RIBBON_IMPLEMENTATION_REVISION: u32 = 1;
/// Stable implementation identifier for the built-in EMA Ribbon.
pub const BUILTIN_EMA_RIBBON_IDENTIFIER: &str = "builtin.ema_ribbon";
/// Stable durable setting identifiers for the five ribbon periods.
pub const BUILTIN_EMA_RIBBON_PERIOD_SETTINGS: [&str; 5] =
    ["period_1", "period_2", "period_3", "period_4", "period_5"];
/// Default EMA Ribbon periods used by legacy workspace migration and product UI.
pub const BUILTIN_EMA_RIBBON_DEFAULT_PERIODS: [i64; 5] = [5, 10, 20, 50, 200];
/// Stable EMA Ribbon output identifiers. Output identity is independent of edited periods.
pub const BUILTIN_EMA_RIBBON_OUTPUT_IDENTIFIERS: [&str; 5] =
    ["ema_1", "ema_2", "ema_3", "ema_4", "ema_5"];
/// Stable implementation revision for the built-in Weighted Moving Average.
pub const BUILTIN_WMA_IMPLEMENTATION_REVISION: u32 = 1;
/// Stable implementation identifier for the built-in Weighted Moving Average.
pub const BUILTIN_WMA_IDENTIFIER: &str = "builtin.wma";
/// Stable durable setting identifier for the built-in WMA period.
pub const BUILTIN_WMA_PERIOD_SETTING: &str = "period";
/// Default built-in WMA period used by legacy workspace migration and product UI.
pub const BUILTIN_WMA_DEFAULT_PERIOD: i64 = 20;
/// Stable output identifier exposed by the built-in WMA.
pub const BUILTIN_WMA_OUTPUT_IDENTIFIER: &str = "wma";
/// Stable implementation revision for the built-in Bollinger Bands study.
pub const BUILTIN_BOLLINGER_IMPLEMENTATION_REVISION: u32 = 1;
/// Stable implementation identifier for the built-in Bollinger Bands study.
pub const BUILTIN_BOLLINGER_IDENTIFIER: &str = "builtin.bollinger";
/// Stable durable setting identifier for the Bollinger period.
pub const BUILTIN_BOLLINGER_PERIOD_SETTING: &str = "period";
/// Stable durable setting identifier for the Bollinger deviation multiplier.
pub const BUILTIN_BOLLINGER_DEVIATION_SETTING: &str = "deviation";
/// Default built-in Bollinger period.
pub const BUILTIN_BOLLINGER_DEFAULT_PERIOD: i64 = 20;
/// Default built-in Bollinger deviation multiplier (2.0 exactly).
pub const BUILTIN_BOLLINGER_DEFAULT_DEVIATION: StudyDecimal = StudyDecimal {
    mantissa: 2,
    scale: 0,
};
/// Stable upper-band output identifier.
pub const BUILTIN_BOLLINGER_UPPER_OUTPUT_IDENTIFIER: &str = "upper";
/// Stable center-line output identifier.
pub const BUILTIN_BOLLINGER_MIDDLE_OUTPUT_IDENTIFIER: &str = "middle";
/// Stable lower-band output identifier.
pub const BUILTIN_BOLLINGER_LOWER_OUTPUT_IDENTIFIER: &str = "lower";
/// Stable implementation revision for the built-in Average True Range.
pub const BUILTIN_ATR_IMPLEMENTATION_REVISION: u32 = 1;
/// Stable implementation identifier for the built-in Average True Range.
pub const BUILTIN_ATR_IDENTIFIER: &str = "builtin.atr";
/// Stable durable setting identifier for the ATR period.
pub const BUILTIN_ATR_PERIOD_SETTING: &str = "period";
/// Default built-in ATR period used by legacy workspace migration and product UI.
pub const BUILTIN_ATR_DEFAULT_PERIOD: i64 = 14;
/// Stable output identifier exposed by the built-in ATR.
pub const BUILTIN_ATR_OUTPUT_IDENTIFIER: &str = "atr";
/// Stable implementation revision for the built-in session VWAP.
pub const BUILTIN_VWAP_IMPLEMENTATION_REVISION: u32 = 1;
/// Stable implementation identifier for the built-in session VWAP.
pub const BUILTIN_VWAP_IDENTIFIER: &str = "builtin.vwap";
/// Stable output identifier exposed by the built-in session VWAP.
pub const BUILTIN_VWAP_OUTPUT_IDENTIFIER: &str = "vwap";
/// Stable implementation revision for the built-in Relative Strength Index.
pub const BUILTIN_RSI_IMPLEMENTATION_REVISION: u32 = 1;
/// Stable implementation identifier for the built-in Relative Strength Index.
pub const BUILTIN_RSI_IDENTIFIER: &str = "builtin.rsi";
/// Stable durable setting identifier for the RSI period.
pub const BUILTIN_RSI_PERIOD_SETTING: &str = "period";
/// Default RSI period used by legacy workspace migration and product UI.
pub const BUILTIN_RSI_DEFAULT_PERIOD: i64 = 14;
/// Stable RSI output identifier.
pub const BUILTIN_RSI_OUTPUT_IDENTIFIER: &str = "rsi";
/// Stable implementation revision for the built-in MACD study.
pub const BUILTIN_MACD_IMPLEMENTATION_REVISION: u32 = 1;
/// Stable implementation identifier for the built-in MACD study.
pub const BUILTIN_MACD_IDENTIFIER: &str = "builtin.macd";
pub const BUILTIN_MACD_FAST_PERIOD_SETTING: &str = "fast_period";
pub const BUILTIN_MACD_SLOW_PERIOD_SETTING: &str = "slow_period";
pub const BUILTIN_MACD_SIGNAL_PERIOD_SETTING: &str = "signal_period";
pub const BUILTIN_MACD_DEFAULT_FAST_PERIOD: i64 = 12;
pub const BUILTIN_MACD_DEFAULT_SLOW_PERIOD: i64 = 26;
pub const BUILTIN_MACD_DEFAULT_SIGNAL_PERIOD: i64 = 9;
pub const BUILTIN_MACD_LINE_OUTPUT_IDENTIFIER: &str = "macd";
pub const BUILTIN_MACD_SIGNAL_OUTPUT_IDENTIFIER: &str = "signal";
pub const BUILTIN_MACD_HISTOGRAM_OUTPUT_IDENTIFIER: &str = "histogram";
/// Stable implementation revision for the built-in Stochastic oscillator.
pub const BUILTIN_STOCHASTIC_IMPLEMENTATION_REVISION: u32 = 1;
/// Stable implementation identifier for the built-in Stochastic oscillator.
pub const BUILTIN_STOCHASTIC_IDENTIFIER: &str = "builtin.stochastic";
pub const BUILTIN_STOCHASTIC_K_PERIOD_SETTING: &str = "k_period";
pub const BUILTIN_STOCHASTIC_D_PERIOD_SETTING: &str = "d_period";
pub const BUILTIN_STOCHASTIC_DEFAULT_K_PERIOD: i64 = 14;
pub const BUILTIN_STOCHASTIC_DEFAULT_D_PERIOD: i64 = 3;
pub const BUILTIN_STOCHASTIC_K_OUTPUT_IDENTIFIER: &str = "k";
pub const BUILTIN_STOCHASTIC_D_OUTPUT_IDENTIFIER: &str = "d";
/// Built-in period ceiling aligned with the production study runtime's per-series point bound.
/// Periods larger than the retained source window cannot produce additional useful warm-up state
/// and would otherwise make windowed formulas perform needlessly large bounded scans.
pub const BUILTIN_MAXIMUM_PERIOD: i64 = 16_384;
/// Durable compatibility epoch for statically linked trusted-native Study SDK packages.
///
/// Cargo semver governs source compatibility. This epoch separately fences durable package
/// descriptors when an incompatible host/package contract would make persisted reconstruction
/// unsafe.
pub const STUDY_SDK_COMPATIBILITY_EPOCH: u32 = 1;
/// Source API version of the Study SDK crate linked into the current product build.
pub const STUDY_SDK_API_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Maximum number of statically approved external study packages in one product build.
pub const MAXIMUM_TRUSTED_STUDY_PACKAGES: usize = 128;
const RESERVED_BUILTIN_STUDY_PREFIX: &str = "builtin.";

/// Failure while resolving durable study data to trusted native code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StudySdkError {
    UnknownStudyIdentifier(String),
    UnsupportedImplementationRevision { identifier: String, revision: u32 },
    InvalidDependencyContract(String),
    PackageIdentityMismatch { expected: String, actual: String },
    PackageDependencyMismatch(String),
    PackageSettingsMismatch(String),
    PackageRestorePanicked(String),
    Runtime(StudyRuntimeError),
}

impl fmt::Display for StudySdkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownStudyIdentifier(identifier) => {
                write!(
                    formatter,
                    "unknown native study implementation {identifier}"
                )
            }
            Self::UnsupportedImplementationRevision {
                identifier,
                revision,
            } => write!(
                formatter,
                "native study {identifier} revision {revision} is unsupported"
            ),
            Self::InvalidDependencyContract(identifier) => {
                write!(
                    formatter,
                    "native study {identifier} has an invalid dependency contract"
                )
            }
            Self::PackageIdentityMismatch { expected, actual } => write!(
                formatter,
                "trusted native study package {expected} returned registration identity {actual}"
            ),
            Self::PackageDependencyMismatch(identifier) => write!(
                formatter,
                "trusted native study package {identifier} changed the durable dependency graph"
            ),
            Self::PackageSettingsMismatch(identifier) => write!(
                formatter,
                "trusted native study package {identifier} changed the durable settings during restore"
            ),
            Self::PackageRestorePanicked(identifier) => write!(
                formatter,
                "trusted native study package {identifier} panicked while restoring durable state"
            ),
            Self::Runtime(error) => error.fmt(formatter),
        }
    }
}

impl Error for StudySdkError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Runtime(error) => Some(error),
            _ => None,
        }
    }
}

impl From<StudyRuntimeError> for StudySdkError {
    fn from(error: StudyRuntimeError) -> Self {
        Self::Runtime(error)
    }
}

/// Restore callback exported by one statically linked trusted-native study package.
///
/// The callback receives only durable study configuration. Provider/account credentials,
/// `MarketEngine`, GPUI, Nucleus render owners, and desktop state are intentionally absent.
pub type TrustedStudyRestore = fn(
    implementation_revision: u32,
    dependencies: Vec<StudyDependency>,
    settings: BTreeMap<String, StudySettingValue>,
) -> Result<NativeStudyRegistration, StudySdkError>;

/// Build-time descriptor for one reviewed trusted-native study package.
///
/// `TradingPlot` does not load arbitrary native libraries at runtime. Product builds statically link
/// approved study crates and list their descriptors at the desktop packaging boundary. The signed
/// application therefore defines the trust set.
#[derive(Clone, Copy)]
pub struct TrustedStudyPackage {
    identifier: &'static str,
    compatibility_epoch: u32,
    current_implementation_revision: u32,
    restore: TrustedStudyRestore,
}

impl TrustedStudyPackage {
    /// Creates one statically linked trusted-native package descriptor.
    #[must_use]
    pub const fn new(
        identifier: &'static str,
        compatibility_epoch: u32,
        current_implementation_revision: u32,
        restore: TrustedStudyRestore,
    ) -> Self {
        Self {
            identifier,
            compatibility_epoch,
            current_implementation_revision,
            restore,
        }
    }

    /// Returns the durable study identifier owned by this package.
    #[must_use]
    pub const fn identifier(self) -> &'static str {
        self.identifier
    }

    /// Returns the host/package compatibility epoch declared by this build.
    #[must_use]
    pub const fn compatibility_epoch(self) -> u32 {
        self.compatibility_epoch
    }

    /// Returns the newest durable implementation revision this package can create.
    #[must_use]
    pub const fn current_implementation_revision(self) -> u32 {
        self.current_implementation_revision
    }
}

/// Failure while constructing the immutable trusted-native package registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrustedStudyRegistryError {
    TooManyPackages {
        maximum: usize,
    },
    InvalidIdentifier(String),
    ReservedIdentifier(String),
    DuplicateIdentifier(String),
    IncompatibleSdkEpoch {
        identifier: String,
        package_epoch: u32,
        host_epoch: u32,
    },
    InvalidImplementationRevision(String),
}

impl fmt::Display for TrustedStudyRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyPackages { maximum } => {
                write!(formatter, "trusted study package count exceeds {maximum}")
            }
            Self::InvalidIdentifier(identifier) => {
                write!(
                    formatter,
                    "invalid trusted study package identifier {identifier}"
                )
            }
            Self::ReservedIdentifier(identifier) => write!(
                formatter,
                "trusted study package identifier {identifier} uses the reserved built-in namespace"
            ),
            Self::DuplicateIdentifier(identifier) => {
                write!(
                    formatter,
                    "duplicate trusted study package identifier {identifier}"
                )
            }
            Self::IncompatibleSdkEpoch {
                identifier,
                package_epoch,
                host_epoch,
            } => write!(
                formatter,
                "trusted study package {identifier} targets SDK epoch {package_epoch}, host uses {host_epoch}"
            ),
            Self::InvalidImplementationRevision(identifier) => write!(
                formatter,
                "trusted study package {identifier} declares implementation revision zero"
            ),
        }
    }
}

impl Error for TrustedStudyRegistryError {}

/// Immutable resolver for built-ins plus statically linked trusted-native study packages.
#[derive(Clone, Copy)]
pub struct TrustedStudyRegistry<'a> {
    packages: &'a [TrustedStudyPackage],
}

impl TrustedStudyRegistry<'static> {
    /// Creates a registry containing only built-in studies.
    #[must_use]
    pub const fn new() -> Self {
        Self { packages: &[] }
    }
}

impl Default for TrustedStudyRegistry<'static> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> TrustedStudyRegistry<'a> {
    /// Validates and installs the product build's statically approved package descriptors.
    ///
    /// # Errors
    /// Returns an error for an excessive package count, invalid/reserved/duplicate identity,
    /// incompatible SDK epoch, or zero current implementation revision.
    pub fn from_packages(
        packages: &'a [TrustedStudyPackage],
    ) -> Result<Self, TrustedStudyRegistryError> {
        if packages.len() > MAXIMUM_TRUSTED_STUDY_PACKAGES {
            return Err(TrustedStudyRegistryError::TooManyPackages {
                maximum: MAXIMUM_TRUSTED_STUDY_PACKAGES,
            });
        }
        for (index, package) in packages.iter().copied().enumerate() {
            validate_trusted_package(package)?;
            if packages[..index]
                .iter()
                .any(|candidate| candidate.identifier == package.identifier)
            {
                return Err(TrustedStudyRegistryError::DuplicateIdentifier(
                    package.identifier.to_string(),
                ));
            }
        }
        Ok(Self { packages })
    }

    /// Returns the number of statically approved external packages.
    #[must_use]
    pub fn len(&self) -> usize {
        self.packages.len()
    }

    /// Returns whether the product build contains no external trusted-native packages.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// Returns one approved external package descriptor by durable identifier.
    #[must_use]
    pub fn package(&self, identifier: &str) -> Option<TrustedStudyPackage> {
        self.packages
            .iter()
            .copied()
            .find(|package| package.identifier == identifier)
    }

    /// Restores a built-in or statically approved trusted-native study registration.
    ///
    /// # Errors
    /// Returns the package/built-in restore error, rejects future implementation revisions, catches
    /// package restore panics, and rejects a package that changes durable identity, dependencies,
    /// or settings while reconstructing the registration.
    pub fn restore(
        &self,
        identifier: &str,
        implementation_revision: u32,
        dependencies: Vec<StudyDependency>,
        settings: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        let Some(package) = self.package(identifier) else {
            return restore_native_registration(
                identifier,
                implementation_revision,
                dependencies,
                settings,
            );
        };
        if implementation_revision == 0
            || implementation_revision > package.current_implementation_revision
        {
            return Err(StudySdkError::UnsupportedImplementationRevision {
                identifier: identifier.to_string(),
                revision: implementation_revision,
            });
        }
        let durable_dependencies = dependencies.clone();
        let durable_settings = settings.clone();
        let registration = catch_unwind(AssertUnwindSafe(|| {
            (package.restore)(implementation_revision, dependencies, settings)
        }))
        .map_err(|_| StudySdkError::PackageRestorePanicked(identifier.to_string()))??;
        if registration.definition.identifier != identifier {
            return Err(StudySdkError::PackageIdentityMismatch {
                expected: identifier.to_string(),
                actual: registration.definition.identifier,
            });
        }
        if registration.definition.dependencies != durable_dependencies {
            return Err(StudySdkError::PackageDependencyMismatch(
                identifier.to_string(),
            ));
        }
        if registration.definition.settings.len() != durable_settings.len()
            || registration
                .definition
                .settings
                .iter()
                .any(|spec| !durable_settings.contains_key(&spec.identifier))
        {
            return Err(StudySdkError::PackageSettingsMismatch(
                identifier.to_string(),
            ));
        }
        let expected_settings =
            StudySettings::with_overrides(&registration.definition.settings, durable_settings)?;
        if registration.settings != expected_settings {
            return Err(StudySdkError::PackageSettingsMismatch(
                identifier.to_string(),
            ));
        }
        Ok(registration)
    }
}

fn validate_trusted_package(package: TrustedStudyPackage) -> Result<(), TrustedStudyRegistryError> {
    let identifier = package.identifier;
    if identifier.trim().is_empty()
        || identifier.trim() != identifier
        || identifier.len() > MAXIMUM_STUDY_IDENTIFIER_BYTES
    {
        return Err(TrustedStudyRegistryError::InvalidIdentifier(
            identifier.to_string(),
        ));
    }
    if identifier.starts_with(RESERVED_BUILTIN_STUDY_PREFIX) {
        return Err(TrustedStudyRegistryError::ReservedIdentifier(
            identifier.to_string(),
        ));
    }
    if package.compatibility_epoch != STUDY_SDK_COMPATIBILITY_EPOCH {
        return Err(TrustedStudyRegistryError::IncompatibleSdkEpoch {
            identifier: identifier.to_string(),
            package_epoch: package.compatibility_epoch,
            host_epoch: STUDY_SDK_COMPATIBILITY_EPOCH,
        });
    }
    if package.current_implementation_revision == 0 {
        return Err(TrustedStudyRegistryError::InvalidImplementationRevision(
            identifier.to_string(),
        ));
    }
    Ok(())
}

/// Resolves one durable study definition to trusted native executable code.
///
/// Dependencies are already resolved by the host against its authoritative
/// market/runtime graph. This function never opens provider sessions or owns
/// market state; it only validates the implementation contract and binds code.
///
/// # Errors
///
/// Returns an error when the implementation identifier or revision is unknown,
/// the dependency shape is incompatible with the implementation, or persisted
/// settings fail the implementation's typed setting contract.
pub fn restore_native_registration(
    identifier: &str,
    implementation_revision: u32,
    dependencies: Vec<StudyDependency>,
    settings: BTreeMap<String, StudySettingValue>,
) -> Result<NativeStudyRegistration, StudySdkError> {
    match identifier {
        BUILTIN_SMA_IDENTIFIER => {
            if implementation_revision != BUILTIN_SMA_IMPLEMENTATION_REVISION {
                return Err(StudySdkError::UnsupportedImplementationRevision {
                    identifier: identifier.to_string(),
                    revision: implementation_revision,
                });
            }
            builtins::sma_registration(dependencies, settings)
        }
        BUILTIN_EMA_IDENTIFIER => {
            if implementation_revision != BUILTIN_EMA_IMPLEMENTATION_REVISION {
                return Err(StudySdkError::UnsupportedImplementationRevision {
                    identifier: identifier.to_string(),
                    revision: implementation_revision,
                });
            }
            builtins::ema_registration(dependencies, settings)
        }
        BUILTIN_EMA_RIBBON_IDENTIFIER => {
            if implementation_revision != BUILTIN_EMA_RIBBON_IMPLEMENTATION_REVISION {
                return Err(StudySdkError::UnsupportedImplementationRevision {
                    identifier: identifier.to_string(),
                    revision: implementation_revision,
                });
            }
            builtins::ema_ribbon_registration(dependencies, settings)
        }
        BUILTIN_WMA_IDENTIFIER => {
            if implementation_revision != BUILTIN_WMA_IMPLEMENTATION_REVISION {
                return Err(StudySdkError::UnsupportedImplementationRevision {
                    identifier: identifier.to_string(),
                    revision: implementation_revision,
                });
            }
            builtins::wma_registration(dependencies, settings)
        }
        BUILTIN_BOLLINGER_IDENTIFIER => {
            if implementation_revision != BUILTIN_BOLLINGER_IMPLEMENTATION_REVISION {
                return Err(StudySdkError::UnsupportedImplementationRevision {
                    identifier: identifier.to_string(),
                    revision: implementation_revision,
                });
            }
            builtins::bollinger_registration(dependencies, settings)
        }
        BUILTIN_ATR_IDENTIFIER => {
            if implementation_revision != BUILTIN_ATR_IMPLEMENTATION_REVISION {
                return Err(StudySdkError::UnsupportedImplementationRevision {
                    identifier: identifier.to_string(),
                    revision: implementation_revision,
                });
            }
            builtins::atr_registration(dependencies, settings)
        }
        BUILTIN_VWAP_IDENTIFIER => {
            if implementation_revision != BUILTIN_VWAP_IMPLEMENTATION_REVISION {
                return Err(StudySdkError::UnsupportedImplementationRevision {
                    identifier: identifier.to_string(),
                    revision: implementation_revision,
                });
            }
            builtins::vwap_registration(dependencies, settings)
        }
        BUILTIN_RSI_IDENTIFIER => {
            if implementation_revision != BUILTIN_RSI_IMPLEMENTATION_REVISION {
                return Err(StudySdkError::UnsupportedImplementationRevision {
                    identifier: identifier.to_string(),
                    revision: implementation_revision,
                });
            }
            builtins::rsi_registration(dependencies, settings)
        }
        BUILTIN_MACD_IDENTIFIER => {
            if implementation_revision != BUILTIN_MACD_IMPLEMENTATION_REVISION {
                return Err(StudySdkError::UnsupportedImplementationRevision {
                    identifier: identifier.to_string(),
                    revision: implementation_revision,
                });
            }
            builtins::macd_registration(dependencies, settings)
        }
        BUILTIN_STOCHASTIC_IDENTIFIER => {
            if implementation_revision != BUILTIN_STOCHASTIC_IMPLEMENTATION_REVISION {
                return Err(StudySdkError::UnsupportedImplementationRevision {
                    identifier: identifier.to_string(),
                    revision: implementation_revision,
                });
            }
            builtins::stochastic_registration(dependencies, settings)
        }
        _ => Err(StudySdkError::UnknownStudyIdentifier(
            identifier.to_string(),
        )),
    }
}

/// Built-in studies expressed through the same native registration contract
/// available to trusted SDK consumers.
pub mod builtins {
    use super::{
        BTreeMap, BUILTIN_ATR_DEFAULT_PERIOD, BUILTIN_ATR_IDENTIFIER,
        BUILTIN_ATR_OUTPUT_IDENTIFIER, BUILTIN_ATR_PERIOD_SETTING,
        BUILTIN_BOLLINGER_DEFAULT_DEVIATION, BUILTIN_BOLLINGER_DEFAULT_PERIOD,
        BUILTIN_BOLLINGER_DEVIATION_SETTING, BUILTIN_BOLLINGER_IDENTIFIER,
        BUILTIN_BOLLINGER_LOWER_OUTPUT_IDENTIFIER, BUILTIN_BOLLINGER_MIDDLE_OUTPUT_IDENTIFIER,
        BUILTIN_BOLLINGER_PERIOD_SETTING, BUILTIN_BOLLINGER_UPPER_OUTPUT_IDENTIFIER,
        BUILTIN_EMA_DEFAULT_PERIOD, BUILTIN_EMA_IDENTIFIER, BUILTIN_EMA_OUTPUT_IDENTIFIER,
        BUILTIN_EMA_PERIOD_SETTING, BUILTIN_EMA_RIBBON_DEFAULT_PERIODS,
        BUILTIN_EMA_RIBBON_IDENTIFIER, BUILTIN_EMA_RIBBON_OUTPUT_IDENTIFIERS,
        BUILTIN_EMA_RIBBON_PERIOD_SETTINGS, BUILTIN_MACD_DEFAULT_FAST_PERIOD,
        BUILTIN_MACD_DEFAULT_SIGNAL_PERIOD, BUILTIN_MACD_DEFAULT_SLOW_PERIOD,
        BUILTIN_MACD_FAST_PERIOD_SETTING, BUILTIN_MACD_HISTOGRAM_OUTPUT_IDENTIFIER,
        BUILTIN_MACD_IDENTIFIER, BUILTIN_MACD_LINE_OUTPUT_IDENTIFIER,
        BUILTIN_MACD_SIGNAL_OUTPUT_IDENTIFIER, BUILTIN_MACD_SIGNAL_PERIOD_SETTING,
        BUILTIN_MACD_SLOW_PERIOD_SETTING, BUILTIN_MAXIMUM_PERIOD, BUILTIN_RSI_DEFAULT_PERIOD,
        BUILTIN_RSI_IDENTIFIER, BUILTIN_RSI_OUTPUT_IDENTIFIER, BUILTIN_RSI_PERIOD_SETTING,
        BUILTIN_SMA_DEFAULT_PERIOD, BUILTIN_SMA_IDENTIFIER, BUILTIN_SMA_OUTPUT_IDENTIFIER,
        BUILTIN_SMA_PERIOD_SETTING, BUILTIN_STOCHASTIC_D_OUTPUT_IDENTIFIER,
        BUILTIN_STOCHASTIC_D_PERIOD_SETTING, BUILTIN_STOCHASTIC_DEFAULT_D_PERIOD,
        BUILTIN_STOCHASTIC_DEFAULT_K_PERIOD, BUILTIN_STOCHASTIC_IDENTIFIER,
        BUILTIN_STOCHASTIC_K_OUTPUT_IDENTIFIER, BUILTIN_STOCHASTIC_K_PERIOD_SETTING,
        BUILTIN_VWAP_IDENTIFIER, BUILTIN_VWAP_OUTPUT_IDENTIFIER, BUILTIN_WMA_DEFAULT_PERIOD,
        BUILTIN_WMA_IDENTIFIER, BUILTIN_WMA_OUTPUT_IDENTIFIER, BUILTIN_WMA_PERIOD_SETTING,
        BarSeriesKey, NativeStudyProgram, NativeStudyRegistration, NativeStudyState, NonZeroUsize,
        StreamRequirements, StudyBarField, StudyDecimal, StudyDefinition, StudyDependency,
        StudyExecutionContext, StudyInputSeries, StudyInvalidationPolicy, StudyMarketInput,
        StudyOutputBuffer, StudyOutputSpec, StudyPaneTarget, StudyPlotKind, StudyPointStyle,
        StudyRuntimeError, StudyScaleTarget, StudySdkError, StudySettingControl,
        StudySettingPresentation, StudySettingSpec, StudySettingValue, StudySettings,
        StudyThresholdRegion, ToPrimitive,
    };

    fn period_setting_spec(identifier: &str, default: i64) -> StudySettingSpec {
        StudySettingSpec::new(identifier, StudySettingValue::Integer(default)).with_presentation(
            StudySettingPresentation {
                label: "Period".to_string(),
                description: Some("Number of input bars used by the calculation.".to_string()),
                group: Some("Inputs".to_string()),
                control: StudySettingControl::Integer {
                    minimum: Some(1),
                    maximum: Some(BUILTIN_MAXIMUM_PERIOD),
                    step: Some(1),
                },
                visible_when: None,
                enabled_when: None,
            },
        )
    }

    fn named_period_setting_spec(
        identifier: &str,
        default: i64,
        label: &str,
        description: &str,
    ) -> StudySettingSpec {
        StudySettingSpec::new(identifier, StudySettingValue::Integer(default)).with_presentation(
            StudySettingPresentation {
                label: label.to_string(),
                description: Some(description.to_string()),
                group: Some("Inputs".to_string()),
                control: StudySettingControl::Integer {
                    minimum: Some(1),
                    maximum: Some(BUILTIN_MAXIMUM_PERIOD),
                    step: Some(1),
                },
                visible_when: None,
                enabled_when: None,
            },
        )
    }

    fn ribbon_period_setting_spec(index: usize) -> StudySettingSpec {
        StudySettingSpec::new(
            BUILTIN_EMA_RIBBON_PERIOD_SETTINGS[index],
            StudySettingValue::Integer(BUILTIN_EMA_RIBBON_DEFAULT_PERIODS[index]),
        )
        .with_presentation(StudySettingPresentation {
            label: format!("EMA {} period", index + 1),
            description: Some("Period for this ribbon line.".to_string()),
            group: Some("Periods".to_string()),
            control: StudySettingControl::Integer {
                minimum: Some(1),
                maximum: Some(BUILTIN_MAXIMUM_PERIOD),
                step: Some(1),
            },
            visible_when: None,
            enabled_when: None,
        })
    }

    fn bollinger_deviation_setting_spec() -> StudySettingSpec {
        StudySettingSpec::new(
            BUILTIN_BOLLINGER_DEVIATION_SETTING,
            StudySettingValue::Decimal(BUILTIN_BOLLINGER_DEFAULT_DEVIATION),
        )
        .with_presentation(StudySettingPresentation {
            label: "Deviation".to_string(),
            description: Some("Standard-deviation multiplier applied to the bands.".to_string()),
            group: Some("Inputs".to_string()),
            control: StudySettingControl::Decimal {
                minimum: None,
                maximum: None,
                step: Some(StudyDecimal {
                    mantissa: 1,
                    scale: 1,
                }),
            },
            visible_when: None,
            enabled_when: None,
        })
    }

    /// Builds a Simple Moving Average registration over one canonical bar series.
    ///
    /// # Errors
    /// Returns a study validation error when the requested period cannot be
    /// represented by the durable integer setting contract.
    pub fn sma(
        series: BarSeriesKey,
        period: NonZeroUsize,
    ) -> Result<NativeStudyRegistration, StudyRuntimeError> {
        let period_value =
            i64::try_from(period.get()).map_err(|_| StudyRuntimeError::InvalidSettingValue)?;
        sma_registration(
            vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams: StreamRequirements::BARS,
            })],
            BTreeMap::from([(
                BUILTIN_SMA_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(period_value),
            )]),
        )
        .map_err(|error| match error {
            StudySdkError::Runtime(error) => error,
            StudySdkError::InvalidDependencyContract(_) => StudyRuntimeError::MissingDependency,
            StudySdkError::UnknownStudyIdentifier(_)
            | StudySdkError::UnsupportedImplementationRevision { .. }
            | StudySdkError::PackageIdentityMismatch { .. }
            | StudySdkError::PackageDependencyMismatch(_)
            | StudySdkError::PackageSettingsMismatch(_)
            | StudySdkError::PackageRestorePanicked(_) => StudyRuntimeError::InvalidIdentifier,
        })
    }

    /// Builds an Exponential Moving Average registration over one canonical bar series.
    ///
    /// # Errors
    /// Returns a study validation error when the requested period cannot be
    /// represented by the durable integer setting contract.
    pub fn ema(
        series: BarSeriesKey,
        period: NonZeroUsize,
    ) -> Result<NativeStudyRegistration, StudyRuntimeError> {
        let period_value =
            i64::try_from(period.get()).map_err(|_| StudyRuntimeError::InvalidSettingValue)?;
        ema_registration(
            vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams: StreamRequirements::BARS,
            })],
            BTreeMap::from([(
                BUILTIN_EMA_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(period_value),
            )]),
        )
        .map_err(sdk_error_to_runtime)
    }

    /// Builds a five-line EMA Ribbon registration over one canonical bar series.
    ///
    /// # Errors
    /// Returns a study validation error when any requested period cannot be
    /// represented by the durable integer setting contract.
    pub fn ema_ribbon(
        series: BarSeriesKey,
        periods: [NonZeroUsize; 5],
    ) -> Result<NativeStudyRegistration, StudyRuntimeError> {
        let mut overrides = BTreeMap::new();
        for (identifier, period) in BUILTIN_EMA_RIBBON_PERIOD_SETTINGS.iter().zip(periods) {
            let value =
                i64::try_from(period.get()).map_err(|_| StudyRuntimeError::InvalidSettingValue)?;
            overrides.insert((*identifier).to_string(), StudySettingValue::Integer(value));
        }
        ema_ribbon_registration(
            vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams: StreamRequirements::BARS,
            })],
            overrides,
        )
        .map_err(sdk_error_to_runtime)
    }

    /// Builds a Weighted Moving Average registration over one canonical bar series.
    ///
    /// # Errors
    /// Returns a study validation error when the requested period cannot be
    /// represented by the durable integer setting contract.
    pub fn wma(
        series: BarSeriesKey,
        period: NonZeroUsize,
    ) -> Result<NativeStudyRegistration, StudyRuntimeError> {
        let period_value =
            i64::try_from(period.get()).map_err(|_| StudyRuntimeError::InvalidSettingValue)?;
        wma_registration(
            vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams: StreamRequirements::BARS,
            })],
            BTreeMap::from([(
                BUILTIN_WMA_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(period_value),
            )]),
        )
        .map_err(sdk_error_to_runtime)
    }

    /// Builds a Bollinger Bands registration over one canonical bar series.
    ///
    /// # Errors
    /// Returns a study validation error when the requested period or exact
    /// decimal deviation is outside the typed settings contract.
    pub fn bollinger(
        series: BarSeriesKey,
        period: NonZeroUsize,
        deviation: StudyDecimal,
    ) -> Result<NativeStudyRegistration, StudyRuntimeError> {
        let period_value =
            i64::try_from(period.get()).map_err(|_| StudyRuntimeError::InvalidSettingValue)?;
        bollinger_registration(
            vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams: StreamRequirements::BARS,
            })],
            BTreeMap::from([
                (
                    BUILTIN_BOLLINGER_PERIOD_SETTING.to_string(),
                    StudySettingValue::Integer(period_value),
                ),
                (
                    BUILTIN_BOLLINGER_DEVIATION_SETTING.to_string(),
                    StudySettingValue::Decimal(deviation),
                ),
            ]),
        )
        .map_err(sdk_error_to_runtime)
    }

    /// Builds an Average True Range registration over one canonical bar series.
    ///
    /// # Errors
    /// Returns a study validation error when the requested period cannot be
    /// represented by the durable integer setting contract.
    pub fn atr(
        series: BarSeriesKey,
        period: NonZeroUsize,
    ) -> Result<NativeStudyRegistration, StudyRuntimeError> {
        let period_value =
            i64::try_from(period.get()).map_err(|_| StudyRuntimeError::InvalidSettingValue)?;
        atr_registration(
            vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams: StreamRequirements::BARS,
            })],
            BTreeMap::from([(
                BUILTIN_ATR_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(period_value),
            )]),
        )
        .map_err(sdk_error_to_runtime)
    }

    /// Builds a UTC-session VWAP registration over one canonical bar series.
    ///
    /// # Errors
    /// Returns a study validation error when the market dependency contract is invalid.
    pub fn vwap(series: BarSeriesKey) -> Result<NativeStudyRegistration, StudyRuntimeError> {
        vwap_registration(
            vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams: StreamRequirements::BARS,
            })],
            BTreeMap::new(),
        )
        .map_err(sdk_error_to_runtime)
    }

    /// Builds a Relative Strength Index registration over one canonical bar series.
    ///
    /// # Errors
    /// Returns a study validation error when the period or dependency contract is invalid.
    pub fn rsi(
        series: BarSeriesKey,
        period: NonZeroUsize,
    ) -> Result<NativeStudyRegistration, StudyRuntimeError> {
        let period =
            i64::try_from(period.get()).map_err(|_| StudyRuntimeError::InvalidSettingValue)?;
        rsi_registration(
            vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams: StreamRequirements::BARS,
            })],
            BTreeMap::from([(
                BUILTIN_RSI_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(period),
            )]),
        )
        .map_err(sdk_error_to_runtime)
    }

    /// Builds a MACD registration over one canonical bar series.
    ///
    /// # Errors
    /// Returns a study validation error when any period or the dependency contract is invalid.
    pub fn macd(
        series: BarSeriesKey,
        fast: NonZeroUsize,
        slow: NonZeroUsize,
        signal: NonZeroUsize,
    ) -> Result<NativeStudyRegistration, StudyRuntimeError> {
        let integer = |value: NonZeroUsize| {
            i64::try_from(value.get()).map_err(|_| StudyRuntimeError::InvalidSettingValue)
        };
        macd_registration(
            vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams: StreamRequirements::BARS,
            })],
            BTreeMap::from([
                (
                    BUILTIN_MACD_FAST_PERIOD_SETTING.to_string(),
                    StudySettingValue::Integer(integer(fast)?),
                ),
                (
                    BUILTIN_MACD_SLOW_PERIOD_SETTING.to_string(),
                    StudySettingValue::Integer(integer(slow)?),
                ),
                (
                    BUILTIN_MACD_SIGNAL_PERIOD_SETTING.to_string(),
                    StudySettingValue::Integer(integer(signal)?),
                ),
            ]),
        )
        .map_err(sdk_error_to_runtime)
    }

    /// Builds a Stochastic oscillator registration over one canonical bar series.
    ///
    /// # Errors
    /// Returns a study validation error when either period or the bar dependency is invalid.
    pub fn stochastic(
        series: BarSeriesKey,
        k_period: NonZeroUsize,
        d_period: NonZeroUsize,
    ) -> Result<NativeStudyRegistration, StudyRuntimeError> {
        let integer = |value: NonZeroUsize| {
            i64::try_from(value.get()).map_err(|_| StudyRuntimeError::InvalidSettingValue)
        };
        stochastic_registration(
            vec![StudyDependency::Market(StudyMarketInput {
                series,
                streams: StreamRequirements::BARS,
            })],
            BTreeMap::from([
                (
                    BUILTIN_STOCHASTIC_K_PERIOD_SETTING.to_string(),
                    StudySettingValue::Integer(integer(k_period)?),
                ),
                (
                    BUILTIN_STOCHASTIC_D_PERIOD_SETTING.to_string(),
                    StudySettingValue::Integer(integer(d_period)?),
                ),
            ]),
        )
        .map_err(sdk_error_to_runtime)
    }

    fn sdk_error_to_runtime(error: StudySdkError) -> StudyRuntimeError {
        match error {
            StudySdkError::Runtime(error) => error,
            StudySdkError::InvalidDependencyContract(_) => StudyRuntimeError::MissingDependency,
            StudySdkError::UnknownStudyIdentifier(_)
            | StudySdkError::UnsupportedImplementationRevision { .. }
            | StudySdkError::PackageIdentityMismatch { .. }
            | StudySdkError::PackageDependencyMismatch(_)
            | StudySdkError::PackageSettingsMismatch(_)
            | StudySdkError::PackageRestorePanicked(_) => StudyRuntimeError::InvalidIdentifier,
        }
    }

    pub(super) fn sma_registration(
        dependencies: Vec<StudyDependency>,
        overrides: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        if dependencies.len() != 1
            || matches!(
                dependencies.first(),
                Some(StudyDependency::Market(input))
                    if input.streams != StreamRequirements::BARS
            )
        {
            return Err(StudySdkError::InvalidDependencyContract(
                BUILTIN_SMA_IDENTIFIER.to_string(),
            ));
        }
        let settings_spec = vec![period_setting_spec(
            BUILTIN_SMA_PERIOD_SETTING,
            BUILTIN_SMA_DEFAULT_PERIOD,
        )];
        let settings = StudySettings::with_overrides(&settings_spec, overrides)?;
        let period = match settings.get(BUILTIN_SMA_PERIOD_SETTING) {
            Some(StudySettingValue::Integer(value)) if *value > 0 => usize::try_from(*value)
                .ok()
                .and_then(NonZeroUsize::new)
                .ok_or(StudyRuntimeError::InvalidSettingValue)?,
            _ => return Err(StudyRuntimeError::InvalidSettingValue.into()),
        };
        Ok(NativeStudyRegistration {
            definition: StudyDefinition {
                identifier: BUILTIN_SMA_IDENTIFIER.to_string(),
                dependencies,
                settings: settings_spec,
                outputs: vec![StudyOutputSpec {
                    identifier: BUILTIN_SMA_OUTPUT_IDENTIFIER.to_string(),
                    title: format!("SMA {}", period.get()),
                    legend_label: None,
                    plot: StudyPlotKind::Line,
                    pane: StudyPaneTarget::Price,
                    scale: StudyScaleTarget::Primary,
                    threshold_region: None,
                    point_style: StudyPointStyle::Uniform,
                }],
                invalidation: StudyInvalidationPolicy::TrailingWindow { bars: period },
            },
            settings,
            program: NativeStudyProgram::stateless(calculate_sma),
        })
    }

    pub(super) fn wma_registration(
        dependencies: Vec<StudyDependency>,
        overrides: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        validate_one_numeric_dependency(BUILTIN_WMA_IDENTIFIER, &dependencies)?;
        let settings_spec = vec![period_setting_spec(
            BUILTIN_WMA_PERIOD_SETTING,
            BUILTIN_WMA_DEFAULT_PERIOD,
        )];
        let settings = StudySettings::with_overrides(&settings_spec, overrides)?;
        let period = wma_period(&settings)?;
        Ok(NativeStudyRegistration {
            definition: StudyDefinition {
                identifier: BUILTIN_WMA_IDENTIFIER.to_string(),
                dependencies,
                settings: settings_spec,
                outputs: vec![StudyOutputSpec {
                    identifier: BUILTIN_WMA_OUTPUT_IDENTIFIER.to_string(),
                    title: format!("WMA {}", period.get()),
                    legend_label: None,
                    plot: StudyPlotKind::Line,
                    pane: StudyPaneTarget::Price,
                    scale: StudyScaleTarget::Primary,
                    threshold_region: None,
                    point_style: StudyPointStyle::Uniform,
                }],
                invalidation: StudyInvalidationPolicy::TrailingWindow { bars: period },
            },
            settings,
            program: NativeStudyProgram::stateless(calculate_wma),
        })
    }

    pub(super) fn ema_registration(
        dependencies: Vec<StudyDependency>,
        overrides: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        validate_one_numeric_dependency(BUILTIN_EMA_IDENTIFIER, &dependencies)?;
        let settings_spec = vec![period_setting_spec(
            BUILTIN_EMA_PERIOD_SETTING,
            BUILTIN_EMA_DEFAULT_PERIOD,
        )];
        let settings = StudySettings::with_overrides(&settings_spec, overrides)?;
        let period = positive_period(&settings, BUILTIN_EMA_PERIOD_SETTING)?;
        Ok(NativeStudyRegistration {
            definition: StudyDefinition {
                identifier: BUILTIN_EMA_IDENTIFIER.to_string(),
                dependencies,
                settings: settings_spec,
                outputs: vec![StudyOutputSpec {
                    identifier: BUILTIN_EMA_OUTPUT_IDENTIFIER.to_string(),
                    title: format!("EMA {}", period.get()),
                    legend_label: None,
                    plot: StudyPlotKind::Line,
                    pane: StudyPaneTarget::Price,
                    scale: StudyScaleTarget::Primary,
                    threshold_region: None,
                    point_style: StudyPointStyle::Uniform,
                }],
                invalidation: StudyInvalidationPolicy::FromFirstChanged,
            },
            settings,
            program: NativeStudyProgram::stateful(calculate_ema, create_ema_state),
        })
    }

    pub(super) fn ema_ribbon_registration(
        dependencies: Vec<StudyDependency>,
        overrides: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        validate_one_numeric_dependency(BUILTIN_EMA_RIBBON_IDENTIFIER, &dependencies)?;
        let settings_spec = (0..BUILTIN_EMA_RIBBON_PERIOD_SETTINGS.len())
            .map(ribbon_period_setting_spec)
            .collect::<Vec<_>>();
        let settings = StudySettings::with_overrides(&settings_spec, overrides)?;
        let periods = ema_ribbon_periods(&settings)?;
        let outputs = BUILTIN_EMA_RIBBON_OUTPUT_IDENTIFIERS
            .iter()
            .zip(periods)
            .map(|(identifier, period)| StudyOutputSpec {
                identifier: (*identifier).to_string(),
                title: format!("EMA {}", period.get()),
                legend_label: None,
                plot: StudyPlotKind::Line,
                pane: StudyPaneTarget::Price,
                scale: StudyScaleTarget::Primary,
                threshold_region: None,
                point_style: StudyPointStyle::Uniform,
            })
            .collect();
        Ok(NativeStudyRegistration {
            definition: StudyDefinition {
                identifier: BUILTIN_EMA_RIBBON_IDENTIFIER.to_string(),
                dependencies,
                settings: settings_spec,
                outputs,
                invalidation: StudyInvalidationPolicy::FromFirstChanged,
            },
            settings,
            program: NativeStudyProgram::stateful(calculate_ema_ribbon, create_ema_ribbon_state),
        })
    }

    pub(super) fn bollinger_registration(
        dependencies: Vec<StudyDependency>,
        overrides: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        validate_one_numeric_dependency(BUILTIN_BOLLINGER_IDENTIFIER, &dependencies)?;
        let settings_spec = vec![
            period_setting_spec(
                BUILTIN_BOLLINGER_PERIOD_SETTING,
                BUILTIN_BOLLINGER_DEFAULT_PERIOD,
            ),
            bollinger_deviation_setting_spec(),
        ];
        let settings = StudySettings::with_overrides(&settings_spec, overrides)?;
        let period = positive_period(&settings, BUILTIN_BOLLINGER_PERIOD_SETTING)?;
        let deviation = decimal_setting(&settings, BUILTIN_BOLLINGER_DEVIATION_SETTING)?;
        let deviation_title = deviation.to_string();
        let title = format!("Bollinger {} {deviation_title}", period.get());
        Ok(NativeStudyRegistration {
            definition: StudyDefinition {
                identifier: BUILTIN_BOLLINGER_IDENTIFIER.to_string(),
                dependencies,
                settings: settings_spec,
                outputs: vec![
                    StudyOutputSpec {
                        identifier: BUILTIN_BOLLINGER_UPPER_OUTPUT_IDENTIFIER.to_string(),
                        title: title.clone(),
                        legend_label: Some("Upper".to_string()),
                        plot: StudyPlotKind::Line,
                        pane: StudyPaneTarget::Price,
                        scale: StudyScaleTarget::Primary,
                        threshold_region: None,
                        point_style: StudyPointStyle::Uniform,
                    },
                    StudyOutputSpec {
                        identifier: BUILTIN_BOLLINGER_MIDDLE_OUTPUT_IDENTIFIER.to_string(),
                        title: title.clone(),
                        legend_label: Some("Basis".to_string()),
                        plot: StudyPlotKind::Line,
                        pane: StudyPaneTarget::Price,
                        scale: StudyScaleTarget::Primary,
                        threshold_region: None,
                        point_style: StudyPointStyle::Uniform,
                    },
                    StudyOutputSpec {
                        identifier: BUILTIN_BOLLINGER_LOWER_OUTPUT_IDENTIFIER.to_string(),
                        title,
                        legend_label: Some("Lower".to_string()),
                        plot: StudyPlotKind::Line,
                        pane: StudyPaneTarget::Price,
                        scale: StudyScaleTarget::Primary,
                        threshold_region: None,
                        point_style: StudyPointStyle::Uniform,
                    },
                ],
                invalidation: StudyInvalidationPolicy::TrailingWindow { bars: period },
            },
            settings,
            program: NativeStudyProgram::stateless(calculate_bollinger),
        })
    }

    pub(super) fn atr_registration(
        dependencies: Vec<StudyDependency>,
        overrides: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        validate_one_bar_market_dependency(BUILTIN_ATR_IDENTIFIER, &dependencies)?;
        let settings_spec = vec![period_setting_spec(
            BUILTIN_ATR_PERIOD_SETTING,
            BUILTIN_ATR_DEFAULT_PERIOD,
        )];
        let settings = StudySettings::with_overrides(&settings_spec, overrides)?;
        let period = positive_period(&settings, BUILTIN_ATR_PERIOD_SETTING)?;
        Ok(NativeStudyRegistration {
            definition: StudyDefinition {
                identifier: BUILTIN_ATR_IDENTIFIER.to_string(),
                dependencies,
                settings: settings_spec,
                outputs: vec![StudyOutputSpec {
                    identifier: BUILTIN_ATR_OUTPUT_IDENTIFIER.to_string(),
                    title: format!("ATR {}", period.get()),
                    legend_label: None,
                    plot: StudyPlotKind::Line,
                    pane: StudyPaneTarget::Dedicated { group: 0 },
                    scale: StudyScaleTarget::Primary,
                    threshold_region: None,
                    point_style: StudyPointStyle::Uniform,
                }],
                invalidation: StudyInvalidationPolicy::FromFirstChanged,
            },
            settings,
            program: NativeStudyProgram::stateful(calculate_atr, create_atr_state),
        })
    }

    pub(super) fn vwap_registration(
        dependencies: Vec<StudyDependency>,
        overrides: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        validate_one_bar_market_dependency(BUILTIN_VWAP_IDENTIFIER, &dependencies)?;
        let settings_spec = Vec::new();
        let settings = StudySettings::with_overrides(&settings_spec, overrides)?;
        Ok(NativeStudyRegistration {
            definition: StudyDefinition {
                identifier: BUILTIN_VWAP_IDENTIFIER.to_string(),
                dependencies,
                settings: settings_spec,
                outputs: vec![StudyOutputSpec {
                    identifier: BUILTIN_VWAP_OUTPUT_IDENTIFIER.to_string(),
                    title: "VWAP".to_string(),
                    legend_label: None,
                    plot: StudyPlotKind::Line,
                    pane: StudyPaneTarget::Price,
                    scale: StudyScaleTarget::Primary,
                    threshold_region: None,
                    point_style: StudyPointStyle::Uniform,
                }],
                invalidation: StudyInvalidationPolicy::FromFirstChanged,
            },
            settings,
            program: NativeStudyProgram::stateful(calculate_vwap, create_vwap_state),
        })
    }

    pub(super) fn rsi_registration(
        dependencies: Vec<StudyDependency>,
        overrides: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        validate_one_numeric_dependency(BUILTIN_RSI_IDENTIFIER, &dependencies)?;
        let settings_spec = vec![period_setting_spec(
            BUILTIN_RSI_PERIOD_SETTING,
            BUILTIN_RSI_DEFAULT_PERIOD,
        )];
        let settings = StudySettings::with_overrides(&settings_spec, overrides)?;
        let period = positive_period(&settings, BUILTIN_RSI_PERIOD_SETTING)?;
        Ok(NativeStudyRegistration {
            definition: StudyDefinition {
                identifier: BUILTIN_RSI_IDENTIFIER.to_string(),
                dependencies,
                settings: settings_spec,
                outputs: vec![StudyOutputSpec {
                    identifier: BUILTIN_RSI_OUTPUT_IDENTIFIER.to_string(),
                    title: format!("RSI {}", period.get()),
                    legend_label: None,
                    plot: StudyPlotKind::Line,
                    pane: StudyPaneTarget::Dedicated { group: 0 },
                    scale: StudyScaleTarget::Primary,
                    threshold_region: Some(StudyThresholdRegion {
                        lower: StudyDecimal {
                            mantissa: 30,
                            scale: 0,
                        },
                        upper: StudyDecimal {
                            mantissa: 70,
                            scale: 0,
                        },
                    }),
                    point_style: StudyPointStyle::Uniform,
                }],
                invalidation: StudyInvalidationPolicy::FromFirstChanged,
            },
            settings,
            program: NativeStudyProgram::stateful(calculate_rsi, create_rsi_state),
        })
    }

    pub(super) fn macd_registration(
        dependencies: Vec<StudyDependency>,
        overrides: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        validate_one_numeric_dependency(BUILTIN_MACD_IDENTIFIER, &dependencies)?;
        let settings_spec = vec![
            named_period_setting_spec(
                BUILTIN_MACD_FAST_PERIOD_SETTING,
                BUILTIN_MACD_DEFAULT_FAST_PERIOD,
                "Fast period",
                "Fast EMA period.",
            ),
            named_period_setting_spec(
                BUILTIN_MACD_SLOW_PERIOD_SETTING,
                BUILTIN_MACD_DEFAULT_SLOW_PERIOD,
                "Slow period",
                "Slow EMA period.",
            ),
            named_period_setting_spec(
                BUILTIN_MACD_SIGNAL_PERIOD_SETTING,
                BUILTIN_MACD_DEFAULT_SIGNAL_PERIOD,
                "Signal period",
                "Signal EMA period.",
            ),
        ];
        let settings = StudySettings::with_overrides(&settings_spec, overrides)?;
        let (fast, slow, signal) = macd_periods(&settings)?;
        let pane = StudyPaneTarget::Dedicated { group: 0 };
        let title = format!("MACD {} {} {}", fast.get(), slow.get(), signal.get());
        Ok(NativeStudyRegistration {
            definition: StudyDefinition {
                identifier: BUILTIN_MACD_IDENTIFIER.to_string(),
                dependencies,
                settings: settings_spec,
                outputs: vec![
                    StudyOutputSpec {
                        identifier: BUILTIN_MACD_LINE_OUTPUT_IDENTIFIER.to_string(),
                        title: title.clone(),
                        legend_label: Some("MACD".to_string()),
                        plot: StudyPlotKind::Line,
                        pane,
                        scale: StudyScaleTarget::Primary,
                        threshold_region: None,
                        point_style: StudyPointStyle::Uniform,
                    },
                    StudyOutputSpec {
                        identifier: BUILTIN_MACD_SIGNAL_OUTPUT_IDENTIFIER.to_string(),
                        title: title.clone(),
                        legend_label: Some("Signal".to_string()),
                        plot: StudyPlotKind::Line,
                        pane,
                        scale: StudyScaleTarget::Primary,
                        threshold_region: None,
                        point_style: StudyPointStyle::Uniform,
                    },
                    StudyOutputSpec {
                        identifier: BUILTIN_MACD_HISTOGRAM_OUTPUT_IDENTIFIER.to_string(),
                        title,
                        legend_label: Some("Histogram".to_string()),
                        plot: StudyPlotKind::Histogram,
                        pane,
                        scale: StudyScaleTarget::Primary,
                        threshold_region: None,
                        point_style: StudyPointStyle::MomentumHistogram,
                    },
                ],
                invalidation: StudyInvalidationPolicy::FromFirstChanged,
            },
            settings,
            program: NativeStudyProgram::stateful(calculate_macd, create_macd_state),
        })
    }

    pub(super) fn stochastic_registration(
        dependencies: Vec<StudyDependency>,
        overrides: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        validate_one_bar_market_dependency(BUILTIN_STOCHASTIC_IDENTIFIER, &dependencies)?;
        let settings_spec = vec![
            named_period_setting_spec(
                BUILTIN_STOCHASTIC_K_PERIOD_SETTING,
                BUILTIN_STOCHASTIC_DEFAULT_K_PERIOD,
                "%K period",
                "Lookback window for the fast oscillator.",
            ),
            named_period_setting_spec(
                BUILTIN_STOCHASTIC_D_PERIOD_SETTING,
                BUILTIN_STOCHASTIC_DEFAULT_D_PERIOD,
                "%D period",
                "Smoothing window for the signal line.",
            ),
        ];
        let settings = StudySettings::with_overrides(&settings_spec, overrides)?;
        let (k_period, d_period) = stochastic_periods(&settings)?;
        let pane = StudyPaneTarget::Dedicated { group: 0 };
        let title = format!("Stochastic {} {}", k_period.get(), d_period.get());
        Ok(NativeStudyRegistration {
            definition: StudyDefinition {
                identifier: BUILTIN_STOCHASTIC_IDENTIFIER.to_string(),
                dependencies,
                settings: settings_spec,
                outputs: vec![
                    StudyOutputSpec {
                        identifier: BUILTIN_STOCHASTIC_K_OUTPUT_IDENTIFIER.to_string(),
                        title: title.clone(),
                        legend_label: Some("%K".to_string()),
                        plot: StudyPlotKind::Line,
                        pane,
                        scale: StudyScaleTarget::Primary,
                        threshold_region: Some(StudyThresholdRegion {
                            lower: StudyDecimal {
                                mantissa: 20,
                                scale: 0,
                            },
                            upper: StudyDecimal {
                                mantissa: 80,
                                scale: 0,
                            },
                        }),
                        point_style: StudyPointStyle::Uniform,
                    },
                    StudyOutputSpec {
                        identifier: BUILTIN_STOCHASTIC_D_OUTPUT_IDENTIFIER.to_string(),
                        title,
                        legend_label: Some("%D".to_string()),
                        plot: StudyPlotKind::Line,
                        pane,
                        scale: StudyScaleTarget::Primary,
                        threshold_region: None,
                        point_style: StudyPointStyle::Uniform,
                    },
                ],
                invalidation: StudyInvalidationPolicy::FromFirstChanged,
            },
            settings,
            program: NativeStudyProgram::stateful(calculate_stochastic, create_stochastic_state),
        })
    }

    fn validate_one_numeric_dependency(
        identifier: &str,
        dependencies: &[StudyDependency],
    ) -> Result<(), StudySdkError> {
        if dependencies.len() != 1
            || matches!(
                dependencies.first(),
                Some(StudyDependency::Market(input))
                    if input.streams != StreamRequirements::BARS
            )
        {
            return Err(StudySdkError::InvalidDependencyContract(
                identifier.to_string(),
            ));
        }
        Ok(())
    }

    fn validate_one_bar_market_dependency(
        identifier: &str,
        dependencies: &[StudyDependency],
    ) -> Result<(), StudySdkError> {
        if !matches!(
            dependencies,
            [StudyDependency::Market(input)] if input.streams == StreamRequirements::BARS
        ) {
            return Err(StudySdkError::InvalidDependencyContract(
                identifier.to_string(),
            ));
        }
        Ok(())
    }

    fn positive_period(
        settings: &StudySettings,
        identifier: &str,
    ) -> Result<NonZeroUsize, StudyRuntimeError> {
        match settings.get(identifier) {
            Some(StudySettingValue::Integer(value)) if *value > 0 => usize::try_from(*value)
                .ok()
                .and_then(NonZeroUsize::new)
                .ok_or(StudyRuntimeError::InvalidSettingValue),
            _ => Err(StudyRuntimeError::InvalidSettingValue),
        }
    }

    fn ema_ribbon_periods(
        settings: &StudySettings,
    ) -> Result<[NonZeroUsize; 5], StudyRuntimeError> {
        let periods = BUILTIN_EMA_RIBBON_PERIOD_SETTINGS
            .iter()
            .map(|identifier| positive_period(settings, identifier))
            .collect::<Result<Vec<_>, _>>()?;
        periods
            .try_into()
            .map_err(|_| StudyRuntimeError::InvalidSettingValue)
    }

    fn macd_periods(
        settings: &StudySettings,
    ) -> Result<(NonZeroUsize, NonZeroUsize, NonZeroUsize), StudyRuntimeError> {
        Ok((
            positive_period(settings, BUILTIN_MACD_FAST_PERIOD_SETTING)?,
            positive_period(settings, BUILTIN_MACD_SLOW_PERIOD_SETTING)?,
            positive_period(settings, BUILTIN_MACD_SIGNAL_PERIOD_SETTING)?,
        ))
    }

    fn stochastic_periods(
        settings: &StudySettings,
    ) -> Result<(NonZeroUsize, NonZeroUsize), StudyRuntimeError> {
        Ok((
            positive_period(settings, BUILTIN_STOCHASTIC_K_PERIOD_SETTING)?,
            positive_period(settings, BUILTIN_STOCHASTIC_D_PERIOD_SETTING)?,
        ))
    }

    fn wma_period(settings: &StudySettings) -> Result<NonZeroUsize, StudyRuntimeError> {
        let period = positive_period(settings, BUILTIN_WMA_PERIOD_SETTING)?;
        let value = period.get();
        value
            .checked_add(1)
            .and_then(|next| value.checked_mul(next))
            .ok_or(StudyRuntimeError::InvalidSettingValue)?;
        Ok(period)
    }

    fn decimal_setting(
        settings: &StudySettings,
        identifier: &str,
    ) -> Result<f64, StudyRuntimeError> {
        let Some(StudySettingValue::Decimal(value)) = settings.get(identifier) else {
            return Err(StudyRuntimeError::InvalidSettingValue);
        };
        if value.scale > 18 {
            return Err(StudyRuntimeError::InvalidSettingValue);
        }
        value
            .mantissa
            .to_f64()
            .map(|mantissa| mantissa / 10_f64.powi(i32::from(value.scale)))
            .filter(|value| value.is_finite())
            .ok_or(StudyRuntimeError::InvalidSettingValue)
    }

    fn calculate_sma(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let (inputs, outputs) = context.split();
        let period = match inputs.settings().get(BUILTIN_SMA_PERIOD_SETTING) {
            Some(StudySettingValue::Integer(value)) if *value > 0 => usize::try_from(*value)
                .map_err(|_| "SMA period is outside the native range".to_string())?,
            _ => return Err("SMA period is unavailable".to_string()),
        };
        let output = outputs
            .get_mut(0)
            .ok_or_else(|| "SMA output is unavailable".to_string())?;
        let dirty = inputs.dirty_range();
        let end = dirty
            .end_exclusive
            .unwrap_or(output.len())
            .min(output.len());
        if dirty.start >= end {
            return Ok(());
        }
        let source_start = dirty.start.saturating_sub(period.saturating_sub(1));
        let source = numeric_input_values(inputs.input(0), source_start, end, "SMA")?;
        let calculated = sma_with_gaps(&source, period);
        for index in dirty.start..end {
            let local = index.saturating_sub(source_start);
            output.set(index, calculated.get(local).copied().flatten())?;
        }
        Ok(())
    }

    fn create_ema_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
        let period = positive_period(settings, BUILTIN_EMA_PERIOD_SETTING)
            .map_err(|_| "EMA period is unavailable".to_string())?;
        Ok(NativeStudyState::new_transactional(
            nucleuscharts_indicators::IncrementalEmaState::new(period),
            ema_state_runtime_bytes,
            transactional_clone,
        ))
    }

    fn transactional_clone<T: Clone>(state: &T) -> T {
        // The pinned Nucleus incremental states used below provide mutation-isolated
        // clone/COW semantics for their private checkpoint storage.
        state.clone()
    }

    #[derive(Clone)]
    struct EmaRibbonState {
        states: [nucleuscharts_indicators::IncrementalEmaState; 5],
    }

    fn create_ema_ribbon_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
        let periods =
            ema_ribbon_periods(settings).map_err(|_| "EMA Ribbon periods are unavailable")?;
        Ok(NativeStudyState::new_transactional(
            EmaRibbonState {
                states: periods.map(nucleuscharts_indicators::IncrementalEmaState::new),
            },
            ema_ribbon_state_runtime_bytes,
            transactional_clone,
        ))
    }

    fn ema_ribbon_state_runtime_bytes(state: &EmaRibbonState) -> usize {
        std::mem::size_of::<EmaRibbonState>().saturating_add(
            state
                .states
                .iter()
                .map(nucleuscharts_indicators::IncrementalEmaState::runtime_bytes)
                .sum::<usize>(),
        )
    }

    fn create_atr_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
        let period = positive_period(settings, BUILTIN_ATR_PERIOD_SETTING)
            .map_err(|_| "ATR period is unavailable".to_string())?;
        Ok(NativeStudyState::new_transactional(
            nucleuscharts_indicators::IncrementalAtrState::new(period),
            atr_state_runtime_bytes,
            transactional_clone,
        ))
    }

    fn atr_state_runtime_bytes(state: &nucleuscharts_indicators::IncrementalAtrState) -> usize {
        std::mem::size_of::<nucleuscharts_indicators::IncrementalAtrState>()
            .saturating_add(state.runtime_bytes())
    }

    fn create_vwap_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
        if !settings.is_empty() {
            return Err("VWAP does not accept settings".to_string());
        }
        Ok(NativeStudyState::new_transactional(
            nucleuscharts_indicators::IncrementalVwapState::new(),
            vwap_state_runtime_bytes,
            transactional_clone,
        ))
    }

    fn vwap_state_runtime_bytes(state: &nucleuscharts_indicators::IncrementalVwapState) -> usize {
        std::mem::size_of::<nucleuscharts_indicators::IncrementalVwapState>()
            .saturating_add(state.runtime_bytes())
    }

    fn create_rsi_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
        let period = positive_period(settings, BUILTIN_RSI_PERIOD_SETTING)
            .map_err(|_| "RSI period is unavailable".to_string())?;
        Ok(NativeStudyState::new_transactional(
            nucleuscharts_indicators::IncrementalRsiState::new(period),
            rsi_state_runtime_bytes,
            transactional_clone,
        ))
    }

    fn rsi_state_runtime_bytes(state: &nucleuscharts_indicators::IncrementalRsiState) -> usize {
        std::mem::size_of::<nucleuscharts_indicators::IncrementalRsiState>()
            .saturating_add(state.runtime_bytes())
    }

    fn create_macd_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
        let (fast, slow, signal) =
            macd_periods(settings).map_err(|_| "MACD periods are unavailable".to_string())?;
        Ok(NativeStudyState::new_transactional(
            nucleuscharts_indicators::IncrementalMacdState::new(fast, slow, signal),
            macd_state_runtime_bytes,
            transactional_clone,
        ))
    }

    fn macd_state_runtime_bytes(state: &nucleuscharts_indicators::IncrementalMacdState) -> usize {
        std::mem::size_of::<nucleuscharts_indicators::IncrementalMacdState>()
            .saturating_add(state.runtime_bytes())
    }

    fn create_stochastic_state(settings: &StudySettings) -> Result<NativeStudyState, String> {
        let (k_period, d_period) = stochastic_periods(settings)
            .map_err(|_| "Stochastic periods are unavailable".to_string())?;
        Ok(NativeStudyState::new_transactional(
            nucleuscharts_indicators::IncrementalStochasticState::new(k_period, d_period),
            stochastic_state_runtime_bytes,
            transactional_clone,
        ))
    }

    fn stochastic_state_runtime_bytes(
        state: &nucleuscharts_indicators::IncrementalStochasticState,
    ) -> usize {
        std::mem::size_of::<nucleuscharts_indicators::IncrementalStochasticState>()
            .saturating_add(state.runtime_bytes())
    }

    fn ema_state_runtime_bytes(state: &nucleuscharts_indicators::IncrementalEmaState) -> usize {
        std::mem::size_of::<nucleuscharts_indicators::IncrementalEmaState>()
            .saturating_add(state.runtime_bytes())
    }

    fn calculate_ema(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let Some((inputs, state, outputs)) =
            context.split_with_state::<nucleuscharts_indicators::IncrementalEmaState>()
        else {
            return Err("EMA runtime state is unavailable".to_string());
        };
        let output = outputs
            .get_mut(0)
            .ok_or_else(|| "EMA output is unavailable".to_string())?;
        let from = inputs.dirty_range().start.min(output.len());
        if from >= output.len() {
            return Ok(());
        }
        let input = inputs
            .input(0)
            .ok_or_else(|| "EMA requires one numeric study input".to_string())?;
        match input {
            StudyInputSeries::Market(series) => {
                let close = series.field(StudyBarField::Close);
                let divisor = 10_f64.powi(i32::from(close.scale()));
                rebuild_ema_output(
                    state,
                    output.len(),
                    from,
                    |index| fixed_point_sample(close.value(index), divisor),
                    |index, value| output.set(index, value),
                )?;
            }
            StudyInputSeries::Output(series) => {
                rebuild_ema_output(
                    state,
                    output.len(),
                    from,
                    |index| series.value(index).flatten(),
                    |index, value| output.set(index, value),
                )?;
            }
        }
        Ok(())
    }

    fn calculate_ema_ribbon(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let Some((inputs, state, outputs)) = context.split_with_state::<EmaRibbonState>() else {
            return Err("EMA Ribbon runtime state is unavailable".to_string());
        };
        if outputs.len() != state.states.len() {
            return Err("EMA Ribbon outputs are unavailable".to_string());
        }
        let from = inputs
            .dirty_range()
            .start
            .min(outputs.first().map_or(0, StudyOutputBuffer::len));
        if outputs.first().is_none_or(|output| from >= output.len()) {
            return Ok(());
        }
        let input = inputs
            .input(0)
            .ok_or_else(|| "EMA Ribbon requires one numeric study input".to_string())?;
        match input {
            StudyInputSeries::Market(series) => {
                let close = series.field(StudyBarField::Close);
                let divisor = 10_f64.powi(i32::from(close.scale()));
                for (ema_state, output) in state.states.iter_mut().zip(outputs.iter_mut()) {
                    rebuild_ema_output(
                        ema_state,
                        output.len(),
                        from,
                        |index| fixed_point_sample(close.value(index), divisor),
                        |index, value| output.set(index, value),
                    )?;
                }
            }
            StudyInputSeries::Output(series) => {
                for (ema_state, output) in state.states.iter_mut().zip(outputs.iter_mut()) {
                    rebuild_ema_output(
                        ema_state,
                        output.len(),
                        from,
                        |index| series.value(index).flatten(),
                        |index, value| output.set(index, value),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn calculate_atr(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let Some((inputs, state, outputs)) =
            context.split_with_state::<nucleuscharts_indicators::IncrementalAtrState>()
        else {
            return Err("ATR runtime state is unavailable".to_string());
        };
        let output = outputs
            .get_mut(0)
            .ok_or_else(|| "ATR output is unavailable".to_string())?;
        let from = inputs.dirty_range().start.min(output.len());
        if from >= output.len() {
            return Ok(());
        }
        let Some(StudyInputSeries::Market(series)) = inputs.input(0) else {
            return Err("ATR requires one canonical bar input".to_string());
        };
        let high = series.field(StudyBarField::High);
        let low = series.field(StudyBarField::Low);
        let close = series.field(StudyBarField::Close);
        let divisor = 10_f64.powi(i32::from(close.scale()));
        let mut write_error = None;
        state.rebuild_from_indexed(
            output.len(),
            from,
            |index| {
                Some(nucleuscharts_indicators::AtrSample {
                    high: fixed_point_sample(high.value(index), divisor)?,
                    low: fixed_point_sample(low.value(index), divisor)?,
                    close: fixed_point_sample(close.value(index), divisor)?,
                })
            },
            |index, value| {
                if write_error.is_none() {
                    write_error = output.set(index, value).err();
                }
            },
        );
        write_error.map_or(Ok(()), Err)
    }

    fn calculate_vwap(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let Some((inputs, state, outputs)) =
            context.split_with_state::<nucleuscharts_indicators::IncrementalVwapState>()
        else {
            return Err("VWAP runtime state is unavailable".to_string());
        };
        let output = outputs
            .get_mut(0)
            .ok_or_else(|| "VWAP output is unavailable".to_string())?;
        let from = inputs.dirty_range().start.min(output.len());
        if from >= output.len() {
            return Ok(());
        }
        let Some(StudyInputSeries::Market(series)) = inputs.input(0) else {
            return Err("VWAP requires one canonical bar input".to_string());
        };
        let high = series.field(StudyBarField::High);
        let low = series.field(StudyBarField::Low);
        let close = series.field(StudyBarField::Close);
        let volume = series.field(StudyBarField::Volume);
        let price_divisor = 10_f64.powi(i32::from(close.scale()));
        let volume_divisor = 10_f64.powi(i32::from(volume.scale()));
        let mut write_error = None;
        state.rebuild_from_indexed(
            output.len(),
            from,
            |index| {
                Some(nucleuscharts_indicators::VwapSample {
                    time_unix_seconds: close
                        .exchange_timestamp_unix_nanos(index)?
                        .div_euclid(1_000_000_000),
                    high: fixed_point_sample(high.value(index), price_divisor)?,
                    low: fixed_point_sample(low.value(index), price_divisor)?,
                    close: fixed_point_sample(close.value(index), price_divisor)?,
                    volume: fixed_point_sample(volume.value(index), volume_divisor),
                })
            },
            |index, value| {
                if write_error.is_none() {
                    write_error = output.set(index, value).err();
                }
            },
        );
        write_error.map_or(Ok(()), Err)
    }

    fn calculate_rsi(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let Some((inputs, state, outputs)) =
            context.split_with_state::<nucleuscharts_indicators::IncrementalRsiState>()
        else {
            return Err("RSI runtime state is unavailable".to_string());
        };
        let output = outputs
            .get_mut(0)
            .ok_or_else(|| "RSI output is unavailable".to_string())?;
        let from = inputs.dirty_range().start.min(output.len());
        if from >= output.len() {
            return Ok(());
        }
        let input = inputs
            .input(0)
            .ok_or_else(|| "RSI requires one numeric study input".to_string())?;
        let mut write_error = None;
        match input {
            StudyInputSeries::Market(series) => {
                let close = series.field(StudyBarField::Close);
                let divisor = 10_f64.powi(i32::from(close.scale()));
                state.rebuild_from_indexed(
                    output.len(),
                    from,
                    |index| fixed_point_sample(close.value(index), divisor),
                    |index, value| {
                        if write_error.is_none() {
                            write_error = output.set(index, value).err();
                        }
                    },
                );
            }
            StudyInputSeries::Output(series) => state.rebuild_from_indexed(
                output.len(),
                from,
                |index| series.value(index).flatten(),
                |index, value| {
                    if write_error.is_none() {
                        write_error = output.set(index, value).err();
                    }
                },
            ),
        }
        write_error.map_or(Ok(()), Err)
    }

    fn calculate_macd(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let Some((inputs, state, outputs)) =
            context.split_with_state::<nucleuscharts_indicators::IncrementalMacdState>()
        else {
            return Err("MACD runtime state is unavailable".to_string());
        };
        let [line, signal, histogram, ..] = outputs else {
            return Err("MACD outputs are unavailable".to_string());
        };
        let len = line.len().min(signal.len()).min(histogram.len());
        let from = inputs.dirty_range().start.min(len);
        if from >= len {
            return Ok(());
        }
        let input = inputs
            .input(0)
            .ok_or_else(|| "MACD requires one numeric study input".to_string())?;
        let mut write_error = None;
        let mut write = |index: usize, point: nucleuscharts_indicators::MacdPoint| {
            if write_error.is_some() {
                return;
            }
            write_error = line
                .set(index, point.macd)
                .and_then(|()| signal.set(index, point.signal))
                .and_then(|()| histogram.set(index, point.histogram))
                .err();
        };
        match input {
            StudyInputSeries::Market(series) => {
                let close = series.field(StudyBarField::Close);
                let divisor = 10_f64.powi(i32::from(close.scale()));
                state.rebuild_from_indexed(
                    len,
                    from,
                    |index| fixed_point_sample(close.value(index), divisor),
                    &mut write,
                );
            }
            StudyInputSeries::Output(series) => state.rebuild_from_indexed(
                len,
                from,
                |index| series.value(index).flatten(),
                &mut write,
            ),
        }
        write_error.map_or(Ok(()), Err)
    }

    fn calculate_stochastic(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let Some((inputs, state, outputs)) =
            context.split_with_state::<nucleuscharts_indicators::IncrementalStochasticState>()
        else {
            return Err("Stochastic runtime state is unavailable".to_string());
        };
        let [k_output, d_output, ..] = outputs else {
            return Err("Stochastic outputs are unavailable".to_string());
        };
        let len = k_output.len().min(d_output.len());
        let from = inputs.dirty_range().start.min(len);
        if from >= len {
            return Ok(());
        }
        let Some(StudyInputSeries::Market(series)) = inputs.input(0) else {
            return Err("Stochastic requires one canonical bar input".to_string());
        };
        let high = series.field(StudyBarField::High);
        let low = series.field(StudyBarField::Low);
        let close = series.field(StudyBarField::Close);
        let divisor = 10_f64.powi(i32::from(close.scale()));
        let mut write_error = None;
        state.rebuild_from_indexed(
            len,
            from,
            |index| {
                Some(nucleuscharts_indicators::StochasticSample {
                    high: fixed_point_sample(high.value(index), divisor)?,
                    low: fixed_point_sample(low.value(index), divisor)?,
                    close: fixed_point_sample(close.value(index), divisor)?,
                })
            },
            |index, point| {
                if write_error.is_none() {
                    write_error = k_output
                        .set(index, point.k)
                        .and_then(|()| d_output.set(index, point.d))
                        .err();
                }
            },
        );
        write_error.map_or(Ok(()), Err)
    }

    fn rebuild_ema_output<S, W>(
        state: &mut nucleuscharts_indicators::IncrementalEmaState,
        len: usize,
        from: usize,
        sample_at: S,
        mut write: W,
    ) -> Result<(), String>
    where
        S: FnMut(usize) -> Option<f64>,
        W: FnMut(usize, Option<f64>) -> Result<(), String>,
    {
        let mut write_error = None;
        state.rebuild_from_indexed(len, from, sample_at, |index, value| {
            if write_error.is_none() {
                write_error = write(index, value).err();
            }
        });
        write_error.map_or(Ok(()), Err)
    }

    fn fixed_point_sample(value: Option<i64>, divisor: f64) -> Option<f64> {
        value
            .and_then(|value| value.to_f64())
            .map(|value| value / divisor)
    }

    fn calculate_wma(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let (inputs, outputs) = context.split();
        let period = wma_period(inputs.settings())
            .map_err(|_| "WMA period is unavailable".to_string())?
            .get();
        let output = outputs
            .get_mut(0)
            .ok_or_else(|| "WMA output is unavailable".to_string())?;
        let dirty = inputs.dirty_range();
        let end = dirty
            .end_exclusive
            .unwrap_or(output.len())
            .min(output.len());
        if dirty.start >= end {
            return Ok(());
        }
        let source_start = dirty.start.saturating_sub(period.saturating_sub(1));
        let source = numeric_input_values(inputs.input(0), source_start, end, "WMA")?;
        let calculated = wma_with_gaps(&source, period);
        for index in dirty.start..end {
            let local = index.saturating_sub(source_start);
            output.set(index, calculated.get(local).copied().flatten())?;
        }
        Ok(())
    }

    fn calculate_bollinger(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        let (inputs, outputs) = context.split();
        let period = positive_period(inputs.settings(), BUILTIN_BOLLINGER_PERIOD_SETTING)
            .map_err(|_| "Bollinger period is unavailable".to_string())?
            .get();
        let deviation = decimal_setting(inputs.settings(), BUILTIN_BOLLINGER_DEVIATION_SETTING)
            .map_err(|_| "Bollinger deviation is unavailable".to_string())?;
        let [upper, middle, lower, ..] = outputs else {
            return Err("Bollinger outputs are unavailable".to_string());
        };
        let dirty = inputs.dirty_range();
        let end = dirty
            .end_exclusive
            .unwrap_or(upper.len())
            .min(upper.len())
            .min(middle.len())
            .min(lower.len());
        if dirty.start >= end {
            return Ok(());
        }
        let source_start = dirty.start.saturating_sub(period.saturating_sub(1));
        let source = numeric_input_values(inputs.input(0), source_start, end, "Bollinger")?;
        let calculated = bollinger_with_gaps(&source, period, deviation);
        for index in dirty.start..end {
            let local = index.saturating_sub(source_start);
            let point = calculated
                .get(local)
                .copied()
                .ok_or_else(|| "Bollinger output row is unavailable".to_string())?;
            upper.set(index, point.upper)?;
            middle.set(index, point.middle)?;
            lower.set(index, point.lower)?;
        }
        Ok(())
    }

    fn numeric_input_values(
        input: Option<StudyInputSeries<'_>>,
        start: usize,
        end: usize,
        study_name: &str,
    ) -> Result<Vec<Option<f64>>, String> {
        match input {
            Some(StudyInputSeries::Market(series)) => {
                let close = series.field(StudyBarField::Close);
                let end = end.min(close.len());
                let scale = 10_f64.powi(i32::from(close.scale()));
                (start..end)
                    .map(|index| {
                        close
                            .value(index)
                            .and_then(|value| value.to_f64())
                            .map(|value| Some(value / scale))
                            .ok_or_else(|| format!("{study_name} close value is unavailable"))
                    })
                    .collect()
            }
            Some(StudyInputSeries::Output(series)) => Ok((start..end)
                .map(|index| series.value(index).flatten())
                .collect()),
            None => Err(format!("{study_name} requires one numeric study input")),
        }
    }

    fn sma_with_gaps(source: &[Option<f64>], period: usize) -> Vec<Option<f64>> {
        let mut result = vec![None; source.len()];
        let mut start = 0;
        while start < source.len() {
            while start < source.len() && source[start].is_none() {
                start += 1;
            }
            if start == source.len() {
                break;
            }
            let mut end = start;
            while end < source.len() && source[end].is_some() {
                end += 1;
            }
            let dense = source[start..end]
                .iter()
                .filter_map(|value| *value)
                .collect::<Vec<_>>();
            let calculated = nucleuscharts_indicators::sma(&dense, period);
            result[start..end].copy_from_slice(&calculated);
            start = end;
        }
        result
    }

    fn wma_with_gaps(source: &[Option<f64>], period: usize) -> Vec<Option<f64>> {
        map_dense_segments(source, |dense| nucleuscharts_indicators::wma(dense, period))
    }

    fn bollinger_with_gaps(
        source: &[Option<f64>],
        period: usize,
        deviation: f64,
    ) -> Vec<nucleuscharts_indicators::BollingerPoint> {
        let empty = nucleuscharts_indicators::BollingerPoint {
            middle: None,
            upper: None,
            lower: None,
        };
        let mut result = vec![empty; source.len()];
        for (start, end, dense) in dense_segments(source) {
            let calculated = nucleuscharts_indicators::bollinger(&dense, period, deviation);
            result[start..end].copy_from_slice(&calculated);
        }
        result
    }

    fn map_dense_segments(
        source: &[Option<f64>],
        calculate: impl Fn(&[f64]) -> Vec<Option<f64>>,
    ) -> Vec<Option<f64>> {
        let mut result = vec![None; source.len()];
        for (start, end, dense) in dense_segments(source) {
            let calculated = calculate(&dense);
            result[start..end].copy_from_slice(&calculated);
        }
        result
    }

    fn dense_segments(source: &[Option<f64>]) -> Vec<(usize, usize, Vec<f64>)> {
        let mut segments = Vec::new();
        let mut start = 0;
        while start < source.len() {
            while start < source.len() && source[start].is_none() {
                start += 1;
            }
            if start == source.len() {
                break;
            }
            let mut end = start;
            while end < source.len() && source[end].is_some() {
                end += 1;
            }
            segments.push((
                start,
                end,
                source[start..end]
                    .iter()
                    .filter_map(|value| *value)
                    .collect(),
            ));
            start = end;
        }
        segments
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn bounded_window_adapters_match_nucleus_formulas() {
            let dense = vec![10.0, 11.0, 9.0, 13.0, 12.0, 15.0];
            let source = dense.iter().copied().map(Some).collect::<Vec<_>>();

            assert_eq!(
                wma_with_gaps(&source, 3),
                nucleuscharts_indicators::wma(&dense, 3)
            );
            assert_eq!(
                bollinger_with_gaps(&source, 3, 2.0),
                nucleuscharts_indicators::bollinger(&dense, 3, 2.0)
            );
        }

        #[test]
        fn bounded_window_adapters_restart_after_explicit_gaps() {
            let source = vec![Some(1.0), Some(2.0), None, Some(4.0), Some(5.0), Some(6.0)];

            assert_eq!(
                wma_with_gaps(&source, 2),
                vec![
                    None,
                    Some(5.0 / 3.0),
                    None,
                    None,
                    Some(14.0 / 3.0),
                    Some(17.0 / 3.0),
                ]
            );
            let bollinger = bollinger_with_gaps(&source, 2, 2.0);
            assert_eq!(bollinger[2].middle, None);
            assert_eq!(bollinger[3].middle, None);
            assert_eq!(bollinger[4].middle, Some(4.5));
        }

        #[test]
        fn ema_adapter_visits_only_checkpoint_bounded_fixed_point_rows() {
            let mut source = (0..5_000)
                .map(|index| 10_000_i64 + i64::from(index))
                .collect::<Vec<_>>();
            let mut output = vec![None; source.len()];
            let mut state = nucleuscharts_indicators::IncrementalEmaState::new(
                NonZeroUsize::new(20).expect("period"),
            );
            let divisor = 100.0;
            let mut visited = 0usize;

            rebuild_ema_output(
                &mut state,
                source.len(),
                0,
                |index| {
                    visited += 1;
                    fixed_point_sample(source.get(index).copied(), divisor)
                },
                |index, value| {
                    output[index] = value;
                    Ok(())
                },
            )
            .expect("full EMA rebuild");
            assert_eq!(visited, source.len());
            let dense = source
                .iter()
                .map(|value| value.to_f64().expect("test fixed-point converts") / 100.0)
                .collect::<Vec<_>>();
            assert_eq!(output, nucleuscharts_indicators::ema(&dense, 20));

            let last = source.len() - 1;
            source[last] += 250;
            visited = 0;
            rebuild_ema_output(
                &mut state,
                source.len(),
                last,
                |index| {
                    visited += 1;
                    fixed_point_sample(source.get(index).copied(), divisor)
                },
                |index, value| {
                    output[index] = value;
                    Ok(())
                },
            )
            .expect("tail EMA revision");
            assert_eq!(visited, 1);
            assert_eq!(state.last_work_rows(), 1);

            source.push(25_000);
            output.push(None);
            visited = 0;
            let appended = source.len() - 1;
            rebuild_ema_output(
                &mut state,
                source.len(),
                appended,
                |index| {
                    visited += 1;
                    fixed_point_sample(source.get(index).copied(), divisor)
                },
                |index, value| {
                    output[index] = value;
                    Ok(())
                },
            )
            .expect("EMA append");
            assert_eq!(visited, 1);
            assert_eq!(state.last_work_rows(), 1);

            let repaired = 2_500;
            source[repaired] -= 375;
            visited = 0;
            rebuild_ema_output(
                &mut state,
                source.len(),
                repaired,
                |index| {
                    visited += 1;
                    fixed_point_sample(source.get(index).copied(), divisor)
                },
                |index, value| {
                    output[index] = value;
                    Ok(())
                },
            )
            .expect("historical EMA repair");
            assert_eq!(visited, state.last_work_rows());
            assert!(visited >= source.len() - repaired);
            assert!(visited < source.len() - repaired + 1_024);
            assert!(state.runtime_bytes() < 4 * 1024);
        }

        #[test]
        #[ignore = "explicit release soak qualification for sustained recursive EMA tail work"]
        fn ema_live_tail_release_soak_measures_recursive_work() {
            const HISTORY_ROWS: usize = 5_000;
            const TAIL_REVISIONS: u32 = 10_000_000;

            let mut source = (0..HISTORY_ROWS)
                .map(|index| 10_000_i64 + i64::try_from(index).expect("small history index"))
                .collect::<Vec<_>>();
            let mut output = vec![None; source.len()];
            let mut state = nucleuscharts_indicators::IncrementalEmaState::new(
                NonZeroUsize::new(20).expect("period"),
            );
            let divisor = 100.0;
            rebuild_ema_output(
                &mut state,
                source.len(),
                0,
                |index| fixed_point_sample(source.get(index).copied(), divisor),
                |index, value| {
                    output[index] = value;
                    Ok(())
                },
            )
            .expect("initial EMA history builds");
            let expected_runtime_bytes = state.runtime_bytes();
            let last = source.len() - 1;
            let mut visited_rows = 0_u64;
            let started = std::time::Instant::now();

            for _ in 0..TAIL_REVISIONS {
                source[last] += 1;
                let mut visited = 0_u64;
                rebuild_ema_output(
                    &mut state,
                    source.len(),
                    last,
                    |index| {
                        visited += 1;
                        fixed_point_sample(source.get(index).copied(), divisor)
                    },
                    |index, value| {
                        output[index] = value;
                        Ok(())
                    },
                )
                .expect("EMA tail revision succeeds");
                visited_rows += visited;
            }

            let elapsed = started.elapsed();
            let revisions_per_second =
                f64::from(TAIL_REVISIONS) / elapsed.as_secs_f64().max(f64::EPSILON);
            eprintln!(
                "study_ema_tail_soak revisions={TAIL_REVISIONS} elapsed_us={} revisions_per_second={revisions_per_second:.0}",
                elapsed.as_micros()
            );
            assert_eq!(visited_rows, u64::from(TAIL_REVISIONS));
            assert_eq!(state.last_work_rows(), 1);
            assert_eq!(state.runtime_bytes(), expected_runtime_bytes);
            assert!(output[last].is_some());
        }

        #[test]
        fn ema_adapter_preserves_output_backed_hard_gaps() {
            let source = [
                Some(1.0),
                Some(2.0),
                Some(3.0),
                None,
                Some(10.0),
                Some(20.0),
                Some(30.0),
            ];
            let mut output = vec![None; source.len()];
            let mut state = nucleuscharts_indicators::IncrementalEmaState::new(
                NonZeroUsize::new(3).expect("period"),
            );

            rebuild_ema_output(
                &mut state,
                source.len(),
                0,
                |index| source[index],
                |index, value| {
                    output[index] = value;
                    Ok(())
                },
            )
            .expect("output-backed EMA");

            assert_eq!(
                output,
                vec![None, None, Some(2.0), None, None, None, Some(20.0)]
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tradingplot_market_data::BarPeriod;

    fn test_series() -> BarSeriesKey {
        BarSeriesKey {
            provider_id: "provider".to_string(),
            instrument_id: "instrument".to_string(),
            entitlement_id: "entitlement".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        }
    }

    fn test_market_dependency() -> StudyDependency {
        StudyDependency::Market(StudyMarketInput {
            series: test_series(),
            streams: StreamRequirements::BARS,
        })
    }

    fn ribbon_settings() -> BTreeMap<String, StudySettingValue> {
        BUILTIN_EMA_RIBBON_PERIOD_SETTINGS
            .iter()
            .zip(BUILTIN_EMA_RIBBON_DEFAULT_PERIODS)
            .map(|(identifier, period)| {
                (
                    (*identifier).to_string(),
                    StudySettingValue::Integer(period),
                )
            })
            .collect()
    }

    fn macd_settings() -> BTreeMap<String, StudySettingValue> {
        BTreeMap::from([
            (
                BUILTIN_MACD_FAST_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(BUILTIN_MACD_DEFAULT_FAST_PERIOD),
            ),
            (
                BUILTIN_MACD_SLOW_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(BUILTIN_MACD_DEFAULT_SLOW_PERIOD),
            ),
            (
                BUILTIN_MACD_SIGNAL_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(BUILTIN_MACD_DEFAULT_SIGNAL_PERIOD),
            ),
        ])
    }

    fn stochastic_settings() -> BTreeMap<String, StudySettingValue> {
        BTreeMap::from([
            (
                BUILTIN_STOCHASTIC_K_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(BUILTIN_STOCHASTIC_DEFAULT_K_PERIOD),
            ),
            (
                BUILTIN_STOCHASTIC_D_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(BUILTIN_STOCHASTIC_DEFAULT_D_PERIOD),
            ),
        ])
    }

    fn calculate_trusted_package(context: &mut StudyExecutionContext<'_>) -> Result<(), String> {
        context
            .output(0)
            .map(|_| ())
            .ok_or_else(|| "trusted-package test output is unavailable".to_string())
    }

    fn trusted_package_restore(
        implementation_revision: u32,
        dependencies: Vec<StudyDependency>,
        settings: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        if implementation_revision != 1 {
            return Err(StudySdkError::UnsupportedImplementationRevision {
                identifier: "example.trusted".to_string(),
                revision: implementation_revision,
            });
        }
        if settings.into_iter().next().is_some() {
            return Err(StudySdkError::InvalidDependencyContract(
                "example.trusted".to_string(),
            ));
        }
        let definition = StudyDefinition {
            identifier: "example.trusted".to_string(),
            dependencies,
            settings: Vec::new(),
            outputs: vec![StudyOutputSpec {
                identifier: "value".to_string(),
                title: "Trusted Example".to_string(),
                legend_label: None,
                plot: StudyPlotKind::Line,
                pane: StudyPaneTarget::Price,
                scale: StudyScaleTarget::Primary,
                threshold_region: None,
                point_style: StudyPointStyle::Uniform,
            }],
            invalidation: StudyInvalidationPolicy::SameRange,
        };
        Ok(NativeStudyRegistration {
            settings: StudySettings::defaults(&definition.settings)?,
            definition,
            program: NativeStudyProgram::stateless(calculate_trusted_package),
        })
    }

    fn mismatched_trusted_package_restore(
        revision: u32,
        dependencies: Vec<StudyDependency>,
        settings: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        let mut registration = trusted_package_restore(revision, dependencies, settings)?;
        registration.definition.identifier = "example.wrong".to_string();
        Ok(registration)
    }

    fn dependency_mismatch_trusted_package_restore(
        revision: u32,
        dependencies: Vec<StudyDependency>,
        settings: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        let mut registration = trusted_package_restore(revision, dependencies, settings)?;
        registration.definition.dependencies.clear();
        Ok(registration)
    }

    fn settings_mismatch_trusted_package_restore(
        revision: u32,
        dependencies: Vec<StudyDependency>,
        settings: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        if revision != 1 {
            return Err(StudySdkError::UnsupportedImplementationRevision {
                identifier: "example.trusted".to_string(),
                revision,
            });
        }
        let _ = settings.into_iter().next();
        let setting_specs = vec![StudySettingSpec::new(
            "period",
            StudySettingValue::Integer(1),
        )];
        let settings = StudySettings::with_overrides(
            &setting_specs,
            BTreeMap::from([("period".to_string(), StudySettingValue::Integer(2))]),
        )?;
        Ok(NativeStudyRegistration {
            definition: StudyDefinition {
                identifier: "example.trusted".to_string(),
                dependencies,
                settings: setting_specs,
                outputs: vec![StudyOutputSpec {
                    identifier: "value".to_string(),
                    title: "Trusted Example".to_string(),
                    legend_label: None,
                    plot: StudyPlotKind::Line,
                    pane: StudyPaneTarget::Price,
                    scale: StudyScaleTarget::Primary,
                    threshold_region: None,
                    point_style: StudyPointStyle::Uniform,
                }],
                invalidation: StudyInvalidationPolicy::SameRange,
            },
            settings,
            program: NativeStudyProgram::stateless(calculate_trusted_package),
        })
    }

    fn panicking_trusted_package_restore(
        _revision: u32,
        _dependencies: Vec<StudyDependency>,
        _settings: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        panic!("intentional trusted package restore panic");
    }

    fn added_default_trusted_package_restore(
        revision: u32,
        dependencies: Vec<StudyDependency>,
        settings: BTreeMap<String, StudySettingValue>,
    ) -> Result<NativeStudyRegistration, StudySdkError> {
        let has_settings = settings.into_iter().next().is_some();
        if revision != 1 || has_settings {
            return Err(StudySdkError::InvalidDependencyContract(
                "example.trusted".to_string(),
            ));
        }
        let setting_specs = vec![StudySettingSpec::new(
            "period",
            StudySettingValue::Integer(20),
        )];
        Ok(NativeStudyRegistration {
            settings: StudySettings::defaults(&setting_specs)?,
            definition: StudyDefinition {
                identifier: "example.trusted".to_string(),
                dependencies,
                settings: setting_specs,
                outputs: vec![StudyOutputSpec {
                    identifier: "value".to_string(),
                    title: "Trusted Example".to_string(),
                    legend_label: None,
                    plot: StudyPlotKind::Line,
                    pane: StudyPaneTarget::Price,
                    scale: StudyScaleTarget::Primary,
                    threshold_region: None,
                    point_style: StudyPointStyle::Uniform,
                }],
                invalidation: StudyInvalidationPolicy::SameRange,
            },
            program: NativeStudyProgram::stateless(calculate_trusted_package),
        })
    }

    #[test]
    fn trusted_registry_validates_static_package_identity_epoch_and_revision() {
        let package = TrustedStudyPackage::new(
            "example.trusted",
            STUDY_SDK_COMPATIBILITY_EPOCH,
            1,
            trusted_package_restore,
        );
        let packages = [package];
        let registry = TrustedStudyRegistry::from_packages(&packages).expect("trusted registry");
        assert_eq!(registry.len(), 1);
        let resolved = registry
            .package("example.trusted")
            .expect("package resolves");
        assert_eq!(resolved.identifier(), "example.trusted");
        assert_eq!(
            resolved.compatibility_epoch(),
            STUDY_SDK_COMPATIBILITY_EPOCH
        );
        assert_eq!(resolved.current_implementation_revision(), 1);
        assert!(matches!(
            TrustedStudyRegistry::from_packages(&[package, package]),
            Err(TrustedStudyRegistryError::DuplicateIdentifier(identifier))
                if identifier == "example.trusted"
        ));
        assert!(matches!(
            TrustedStudyRegistry::from_packages(&[TrustedStudyPackage::new(
                "builtin.shadow",
                STUDY_SDK_COMPATIBILITY_EPOCH,
                1,
                trusted_package_restore,
            )]),
            Err(TrustedStudyRegistryError::ReservedIdentifier(identifier))
                if identifier == "builtin.shadow"
        ));
        assert!(matches!(
            TrustedStudyRegistry::from_packages(&[TrustedStudyPackage::new(
                "example.old",
                STUDY_SDK_COMPATIBILITY_EPOCH + 1,
                1,
                trusted_package_restore,
            )]),
            Err(TrustedStudyRegistryError::IncompatibleSdkEpoch { .. })
        ));
    }

    #[test]
    fn trusted_registry_restores_external_and_builtin_studies_without_identity_escape() {
        let packages = [TrustedStudyPackage::new(
            "example.trusted",
            STUDY_SDK_COMPATIBILITY_EPOCH,
            1,
            trusted_package_restore,
        )];
        let registry = TrustedStudyRegistry::from_packages(&packages).expect("trusted registry");
        let custom = registry
            .restore(
                "example.trusted",
                1,
                vec![test_market_dependency()],
                BTreeMap::new(),
            )
            .expect("custom study restores");
        assert_eq!(custom.definition.identifier, "example.trusted");
        assert!(
            registry
                .restore(
                    BUILTIN_SMA_IDENTIFIER,
                    BUILTIN_SMA_IMPLEMENTATION_REVISION,
                    vec![test_market_dependency()],
                    BTreeMap::from([(
                        BUILTIN_SMA_PERIOD_SETTING.to_string(),
                        StudySettingValue::Integer(BUILTIN_SMA_DEFAULT_PERIOD),
                    )]),
                )
                .is_ok()
        );
        assert!(matches!(
            registry.restore(
                "example.trusted",
                2,
                vec![test_market_dependency()],
                BTreeMap::new(),
            ),
            Err(StudySdkError::UnsupportedImplementationRevision { revision: 2, .. })
        ));

        let mismatched_packages = [TrustedStudyPackage::new(
            "example.trusted",
            STUDY_SDK_COMPATIBILITY_EPOCH,
            1,
            mismatched_trusted_package_restore,
        )];
        let mismatched = TrustedStudyRegistry::from_packages(&mismatched_packages)
            .expect("mismatched package descriptor itself is valid");
        assert!(matches!(
            mismatched.restore(
                "example.trusted",
                1,
                vec![test_market_dependency()],
                BTreeMap::new(),
            ),
            Err(StudySdkError::PackageIdentityMismatch { .. })
        ));

        let dependency_mismatch_packages = [TrustedStudyPackage::new(
            "example.trusted",
            STUDY_SDK_COMPATIBILITY_EPOCH,
            1,
            dependency_mismatch_trusted_package_restore,
        )];
        let dependency_mismatch =
            TrustedStudyRegistry::from_packages(&dependency_mismatch_packages)
                .expect("dependency-mismatch descriptor itself is valid");
        assert!(matches!(
            dependency_mismatch.restore(
                "example.trusted",
                1,
                vec![test_market_dependency()],
                BTreeMap::new(),
            ),
            Err(StudySdkError::PackageDependencyMismatch(identifier))
                if identifier == "example.trusted"
        ));

        let settings_mismatch_packages = [TrustedStudyPackage::new(
            "example.trusted",
            STUDY_SDK_COMPATIBILITY_EPOCH,
            1,
            settings_mismatch_trusted_package_restore,
        )];
        let settings_mismatch = TrustedStudyRegistry::from_packages(&settings_mismatch_packages)
            .expect("settings-mismatch descriptor itself is valid");
        assert!(matches!(
            settings_mismatch.restore(
                "example.trusted",
                1,
                vec![test_market_dependency()],
                BTreeMap::from([("period".to_string(), StudySettingValue::Integer(1))]),
            ),
            Err(StudySdkError::PackageSettingsMismatch(identifier))
                if identifier == "example.trusted"
        ));
    }

    #[test]
    fn trusted_registry_contains_package_restore_panics() {
        let packages = [TrustedStudyPackage::new(
            "example.trusted",
            STUDY_SDK_COMPATIBILITY_EPOCH,
            1,
            panicking_trusted_package_restore,
        )];
        let registry = TrustedStudyRegistry::from_packages(&packages)
            .expect("panicking package descriptor itself is valid");
        assert!(matches!(
            registry.restore(
                "example.trusted",
                1,
                vec![test_market_dependency()],
                BTreeMap::new(),
            ),
            Err(StudySdkError::PackageRestorePanicked(identifier))
                if identifier == "example.trusted"
        ));
    }

    #[test]
    fn trusted_registry_rejects_silent_setting_schema_growth_within_one_revision() {
        let packages = [TrustedStudyPackage::new(
            "example.trusted",
            STUDY_SDK_COMPATIBILITY_EPOCH,
            1,
            added_default_trusted_package_restore,
        )];
        let registry = TrustedStudyRegistry::from_packages(&packages)
            .expect("schema-growth package descriptor itself is valid");
        assert!(matches!(
            registry.restore(
                "example.trusted",
                1,
                vec![test_market_dependency()],
                BTreeMap::new(),
            ),
            Err(StudySdkError::PackageSettingsMismatch(identifier))
                if identifier == "example.trusted"
        ));
    }

    #[test]
    fn builtin_sma_uses_the_shared_ta_contract_and_declares_price_pane_output() {
        let registration = builtins::sma(
            BarSeriesKey {
                provider_id: "provider".to_string(),
                instrument_id: "instrument".to_string(),
                entitlement_id: "entitlement".to_string(),
                period: BarPeriod::time(60).expect("minute period"),
                definition_version: 1,
            },
            NonZeroUsize::new(20).expect("period"),
        )
        .expect("SMA registration");

        assert_eq!(registration.definition.identifier, "builtin.sma");
        assert_eq!(registration.definition.outputs.len(), 1);
        assert_eq!(registration.definition.outputs[0].title, "SMA 20");
        assert_eq!(registration.definition.outputs[0].plot, StudyPlotKind::Line);
        assert_eq!(
            registration.definition.outputs[0].pane,
            StudyPaneTarget::Price
        );
        assert_eq!(
            registration.definition.invalidation,
            StudyInvalidationPolicy::TrailingWindow {
                bars: NonZeroUsize::new(20).expect("period")
            }
        );
    }

    #[test]
    fn builtin_ema_uses_recursive_state_and_from_first_changed_invalidation() {
        let registration = builtins::ema(
            BarSeriesKey {
                provider_id: "provider".to_string(),
                instrument_id: "instrument".to_string(),
                entitlement_id: "entitlement".to_string(),
                period: BarPeriod::time(60).expect("minute period"),
                definition_version: 1,
            },
            NonZeroUsize::new(20).expect("period"),
        )
        .expect("EMA registration");

        assert_eq!(registration.definition.identifier, BUILTIN_EMA_IDENTIFIER);
        assert_eq!(registration.definition.outputs.len(), 1);
        assert_eq!(
            registration.definition.outputs[0].identifier,
            BUILTIN_EMA_OUTPUT_IDENTIFIER
        );
        assert_eq!(registration.definition.outputs[0].title, "EMA 20");
        assert_eq!(
            registration.definition.invalidation,
            StudyInvalidationPolicy::FromFirstChanged
        );
        assert!(registration.program.state_factory.is_some());
    }

    #[test]
    fn durable_sma_restore_binds_versioned_code_and_typed_settings() {
        let source = BarSeriesKey {
            provider_id: "provider".to_string(),
            instrument_id: "instrument".to_string(),
            entitlement_id: "entitlement".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        };
        let registration = restore_native_registration(
            BUILTIN_SMA_IDENTIFIER,
            BUILTIN_SMA_IMPLEMENTATION_REVISION,
            vec![StudyDependency::Market(StudyMarketInput {
                series: source,
                streams: StreamRequirements::BARS,
            })],
            BTreeMap::from([("period".to_string(), StudySettingValue::Integer(50))]),
        )
        .expect("durable SMA resolves");

        assert_eq!(registration.definition.identifier, BUILTIN_SMA_IDENTIFIER);
        assert_eq!(registration.definition.outputs[0].identifier, "sma");
        assert_eq!(registration.definition.outputs[0].title, "SMA 50");
        assert_eq!(
            registration.settings.get("period"),
            Some(&StudySettingValue::Integer(50))
        );
        assert!(matches!(
            restore_native_registration(
                BUILTIN_SMA_IDENTIFIER,
                2,
                registration.definition.dependencies,
                BTreeMap::new(),
            ),
            Err(StudySdkError::UnsupportedImplementationRevision { revision: 2, .. })
        ));
    }

    #[test]
    fn durable_ema_restore_supports_market_and_output_dependencies() {
        let source = BarSeriesKey {
            provider_id: "provider".to_string(),
            instrument_id: "instrument".to_string(),
            entitlement_id: "entitlement".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        };
        let market = restore_native_registration(
            BUILTIN_EMA_IDENTIFIER,
            BUILTIN_EMA_IMPLEMENTATION_REVISION,
            vec![StudyDependency::Market(StudyMarketInput {
                series: source,
                streams: StreamRequirements::BARS,
            })],
            BTreeMap::from([(
                BUILTIN_EMA_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(34),
            )]),
        )
        .expect("durable market EMA resolves");
        assert_eq!(
            market.settings.get(BUILTIN_EMA_PERIOD_SETTING),
            Some(&StudySettingValue::Integer(34))
        );
        assert!(market.program.state_factory.is_some());

        let upstream = StudyInstanceId::try_from_u64(7).expect("upstream study id");
        let output = restore_native_registration(
            BUILTIN_EMA_IDENTIFIER,
            BUILTIN_EMA_IMPLEMENTATION_REVISION,
            vec![StudyDependency::Output(upstream.output(0))],
            BTreeMap::from([(
                BUILTIN_EMA_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(5),
            )]),
        )
        .expect("durable output-backed EMA resolves");
        assert_eq!(
            output.definition.dependencies,
            vec![StudyDependency::Output(upstream.output(0))]
        );
        assert_eq!(
            output.definition.invalidation,
            StudyInvalidationPolicy::FromFirstChanged
        );
        assert!(matches!(
            restore_native_registration(
                BUILTIN_EMA_IDENTIFIER,
                2,
                output.definition.dependencies,
                BTreeMap::new(),
            ),
            Err(StudySdkError::UnsupportedImplementationRevision { revision: 2, .. })
        ));
    }

    #[test]
    fn wma_and_bollinger_use_bounded_window_invalidation_and_stable_outputs() {
        let source = BarSeriesKey {
            provider_id: "provider".to_string(),
            instrument_id: "instrument".to_string(),
            entitlement_id: "entitlement".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        };
        let wma = builtins::wma(source.clone(), NonZeroUsize::new(20).expect("WMA period"))
            .expect("WMA registration");
        let bollinger = builtins::bollinger(
            source,
            NonZeroUsize::new(20).expect("Bollinger period"),
            BUILTIN_BOLLINGER_DEFAULT_DEVIATION,
        )
        .expect("Bollinger registration");

        assert_eq!(wma.definition.identifier, BUILTIN_WMA_IDENTIFIER);
        assert_eq!(
            wma.definition.outputs[0].identifier,
            BUILTIN_WMA_OUTPUT_IDENTIFIER
        );
        assert_eq!(
            wma.definition.invalidation,
            StudyInvalidationPolicy::TrailingWindow {
                bars: NonZeroUsize::new(20).expect("period")
            }
        );
        assert_eq!(
            bollinger.definition.identifier,
            BUILTIN_BOLLINGER_IDENTIFIER
        );
        assert_eq!(
            bollinger
                .definition
                .outputs
                .iter()
                .map(|output| output.identifier.as_str())
                .collect::<Vec<_>>(),
            vec!["upper", "middle", "lower"]
        );
        assert!(
            bollinger
                .definition
                .outputs
                .iter()
                .all(|output| output.pane == StudyPaneTarget::Price)
        );
        let wma_period = &wma.definition.settings[0];
        assert_eq!(wma_period.presentation.label, "Period");
        assert_eq!(wma_period.presentation.group.as_deref(), Some("Inputs"));
        assert!(matches!(
            wma_period.presentation.control,
            StudySettingControl::Integer {
                minimum: Some(1),
                maximum: Some(BUILTIN_MAXIMUM_PERIOD),
                step: Some(1),
            }
        ));
        assert!(matches!(
            bollinger.definition.settings[1].presentation.control,
            StudySettingControl::Decimal {
                minimum: None,
                maximum: None,
                step: Some(StudyDecimal {
                    mantissa: 1,
                    scale: 1,
                }),
            }
        ));
    }

    #[test]
    fn durable_wma_and_bollinger_restore_versioned_settings_and_outputs() {
        let source = BarSeriesKey {
            provider_id: "provider".to_string(),
            instrument_id: "instrument".to_string(),
            entitlement_id: "entitlement".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        };
        let dependency = StudyDependency::Market(StudyMarketInput {
            series: source,
            streams: StreamRequirements::BARS,
        });
        let wma = restore_native_registration(
            BUILTIN_WMA_IDENTIFIER,
            BUILTIN_WMA_IMPLEMENTATION_REVISION,
            vec![dependency.clone()],
            BTreeMap::from([(
                BUILTIN_WMA_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(34),
            )]),
        )
        .expect("durable WMA resolves");
        let bollinger_deviation = StudyDecimal {
            mantissa: 25,
            scale: 1,
        };
        let bollinger = restore_native_registration(
            BUILTIN_BOLLINGER_IDENTIFIER,
            BUILTIN_BOLLINGER_IMPLEMENTATION_REVISION,
            vec![dependency.clone()],
            BTreeMap::from([
                (
                    BUILTIN_BOLLINGER_PERIOD_SETTING.to_string(),
                    StudySettingValue::Integer(21),
                ),
                (
                    BUILTIN_BOLLINGER_DEVIATION_SETTING.to_string(),
                    StudySettingValue::Decimal(bollinger_deviation),
                ),
            ]),
        )
        .expect("durable Bollinger resolves");

        assert_eq!(
            wma.definition.outputs[0].identifier,
            BUILTIN_WMA_OUTPUT_IDENTIFIER
        );
        assert_eq!(
            wma.settings.get(BUILTIN_WMA_PERIOD_SETTING),
            Some(&StudySettingValue::Integer(34))
        );
        assert_eq!(
            bollinger
                .definition
                .outputs
                .iter()
                .map(|output| output.identifier.as_str())
                .collect::<Vec<_>>(),
            vec![
                BUILTIN_BOLLINGER_UPPER_OUTPUT_IDENTIFIER,
                BUILTIN_BOLLINGER_MIDDLE_OUTPUT_IDENTIFIER,
                BUILTIN_BOLLINGER_LOWER_OUTPUT_IDENTIFIER,
            ]
        );
        assert_eq!(
            bollinger.settings.get(BUILTIN_BOLLINGER_DEVIATION_SETTING),
            Some(&StudySettingValue::Decimal(bollinger_deviation))
        );
        assert!(matches!(
            restore_native_registration(
                BUILTIN_WMA_IDENTIFIER,
                2,
                vec![dependency.clone()],
                BTreeMap::new(),
            ),
            Err(StudySdkError::UnsupportedImplementationRevision { revision: 2, .. })
        ));
        assert!(matches!(
            restore_native_registration(
                BUILTIN_BOLLINGER_IDENTIFIER,
                2,
                vec![dependency],
                BTreeMap::new(),
            ),
            Err(StudySdkError::UnsupportedImplementationRevision { revision: 2, .. })
        ));
    }

    #[test]
    fn phase_c_scalar_builtins_preserve_recursive_state_panes_and_stable_outputs() {
        let source = test_series();
        let ribbon = builtins::ema_ribbon(
            source.clone(),
            [5, 10, 20, 50, 200].map(|period| NonZeroUsize::new(period).expect("period")),
        )
        .expect("EMA Ribbon registration");
        let atr = builtins::atr(source.clone(), NonZeroUsize::new(14).expect("period"))
            .expect("ATR registration");
        let vwap = builtins::vwap(source.clone()).expect("VWAP registration");

        assert_eq!(ribbon.definition.identifier, BUILTIN_EMA_RIBBON_IDENTIFIER);
        assert_eq!(
            ribbon
                .definition
                .outputs
                .iter()
                .map(|output| output.identifier.as_str())
                .collect::<Vec<_>>(),
            BUILTIN_EMA_RIBBON_OUTPUT_IDENTIFIERS
        );
        assert!(
            ribbon
                .definition
                .outputs
                .iter()
                .all(|output| output.pane == StudyPaneTarget::Price)
        );
        assert_eq!(
            ribbon.definition.invalidation,
            StudyInvalidationPolicy::FromFirstChanged
        );
        assert!(ribbon.program.state_factory.is_some());

        assert_eq!(atr.definition.identifier, BUILTIN_ATR_IDENTIFIER);
        assert_eq!(
            atr.definition.outputs[0].identifier,
            BUILTIN_ATR_OUTPUT_IDENTIFIER
        );
        assert_eq!(
            atr.definition.outputs[0].pane,
            StudyPaneTarget::Dedicated { group: 0 }
        );
        assert_eq!(atr.definition.outputs[0].scale, StudyScaleTarget::Primary);
        assert!(atr.program.state_factory.is_some());

        assert_eq!(vwap.definition.identifier, BUILTIN_VWAP_IDENTIFIER);
        assert_eq!(
            vwap.definition.outputs[0].identifier,
            BUILTIN_VWAP_OUTPUT_IDENTIFIER
        );
        assert_eq!(vwap.definition.outputs[0].pane, StudyPaneTarget::Price);
        assert!(vwap.program.state_factory.is_some());
    }

    #[test]
    fn richer_builtins_preserve_panes_semantic_presentation_and_stable_outputs() {
        let source = test_series();
        let rsi = builtins::rsi(source.clone(), NonZeroUsize::new(14).expect("period"))
            .expect("RSI registration");
        let macd = builtins::macd(
            source.clone(),
            NonZeroUsize::new(12).expect("fast"),
            NonZeroUsize::new(26).expect("slow"),
            NonZeroUsize::new(9).expect("signal"),
        )
        .expect("MACD registration");
        let stochastic = builtins::stochastic(
            source,
            NonZeroUsize::new(14).expect("k"),
            NonZeroUsize::new(3).expect("d"),
        )
        .expect("Stochastic registration");

        assert_eq!(rsi.definition.identifier, BUILTIN_RSI_IDENTIFIER);
        assert_eq!(
            rsi.definition.outputs[0].threshold_region,
            Some(StudyThresholdRegion {
                lower: StudyDecimal {
                    mantissa: 30,
                    scale: 0,
                },
                upper: StudyDecimal {
                    mantissa: 70,
                    scale: 0,
                },
            })
        );
        assert_eq!(
            rsi.definition.outputs[0].point_style,
            StudyPointStyle::Uniform
        );
        assert!(rsi.program.state_factory.is_some());

        assert_eq!(macd.definition.identifier, BUILTIN_MACD_IDENTIFIER);
        assert_eq!(macd.definition.outputs.len(), 3);
        assert!(
            macd.definition
                .outputs
                .iter()
                .all(|output| output.title == "MACD 12 26 9")
        );
        assert_eq!(
            macd.definition
                .outputs
                .iter()
                .map(|output| output.legend_label.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("MACD"), Some("Signal"), Some("Histogram")]
        );
        assert_eq!(macd.definition.outputs[2].plot, StudyPlotKind::Histogram);
        assert_eq!(
            macd.definition.outputs[2].point_style,
            StudyPointStyle::MomentumHistogram
        );
        assert!(macd.program.state_factory.is_some());

        assert_eq!(
            stochastic.definition.identifier,
            BUILTIN_STOCHASTIC_IDENTIFIER
        );
        assert_eq!(stochastic.definition.outputs.len(), 2);
        assert!(
            stochastic
                .definition
                .outputs
                .iter()
                .all(|output| output.title == "Stochastic 14 3")
        );
        assert_eq!(
            stochastic
                .definition
                .outputs
                .iter()
                .map(|output| output.legend_label.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("%K"), Some("%D")]
        );
        assert_eq!(
            stochastic.definition.outputs[0].threshold_region,
            Some(StudyThresholdRegion {
                lower: StudyDecimal {
                    mantissa: 20,
                    scale: 0,
                },
                upper: StudyDecimal {
                    mantissa: 80,
                    scale: 0,
                },
            })
        );
        assert!(stochastic.program.state_factory.is_some());
    }

    #[test]
    fn phase_c_scalar_builtins_restore_exact_revisions() {
        let market = test_market_dependency();
        let ribbon = restore_native_registration(
            BUILTIN_EMA_RIBBON_IDENTIFIER,
            BUILTIN_EMA_RIBBON_IMPLEMENTATION_REVISION,
            vec![market.clone()],
            ribbon_settings(),
        )
        .expect("durable EMA Ribbon resolves");
        let atr = restore_native_registration(
            BUILTIN_ATR_IDENTIFIER,
            BUILTIN_ATR_IMPLEMENTATION_REVISION,
            vec![market.clone()],
            BTreeMap::from([(
                BUILTIN_ATR_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(BUILTIN_ATR_DEFAULT_PERIOD),
            )]),
        )
        .expect("durable ATR resolves");
        let vwap = restore_native_registration(
            BUILTIN_VWAP_IDENTIFIER,
            BUILTIN_VWAP_IMPLEMENTATION_REVISION,
            vec![market.clone()],
            BTreeMap::new(),
        )
        .expect("durable VWAP resolves");
        assert_eq!(ribbon.definition.outputs.len(), 5);
        assert_eq!(atr.definition.outputs.len(), 1);
        assert_eq!(vwap.definition.outputs.len(), 1);

        for (identifier, revision) in [
            (
                BUILTIN_EMA_RIBBON_IDENTIFIER,
                BUILTIN_EMA_RIBBON_IMPLEMENTATION_REVISION,
            ),
            (BUILTIN_ATR_IDENTIFIER, BUILTIN_ATR_IMPLEMENTATION_REVISION),
            (
                BUILTIN_VWAP_IDENTIFIER,
                BUILTIN_VWAP_IMPLEMENTATION_REVISION,
            ),
        ] {
            assert!(matches!(
                restore_native_registration(
                    identifier,
                    revision + 1,
                    vec![market.clone()],
                    BTreeMap::new(),
                ),
                Err(StudySdkError::UnsupportedImplementationRevision { .. })
            ));
        }
    }

    #[test]
    fn richer_builtins_restore_exact_revisions() {
        let market = test_market_dependency();
        let rsi = restore_native_registration(
            BUILTIN_RSI_IDENTIFIER,
            BUILTIN_RSI_IMPLEMENTATION_REVISION,
            vec![market.clone()],
            BTreeMap::from([(
                BUILTIN_RSI_PERIOD_SETTING.to_string(),
                StudySettingValue::Integer(BUILTIN_RSI_DEFAULT_PERIOD),
            )]),
        )
        .expect("durable RSI resolves");
        let macd = restore_native_registration(
            BUILTIN_MACD_IDENTIFIER,
            BUILTIN_MACD_IMPLEMENTATION_REVISION,
            vec![market.clone()],
            macd_settings(),
        )
        .expect("durable MACD resolves");
        let stochastic = restore_native_registration(
            BUILTIN_STOCHASTIC_IDENTIFIER,
            BUILTIN_STOCHASTIC_IMPLEMENTATION_REVISION,
            vec![market.clone()],
            stochastic_settings(),
        )
        .expect("durable Stochastic resolves");
        assert_eq!(rsi.definition.outputs.len(), 1);
        assert_eq!(macd.definition.outputs.len(), 3);
        assert_eq!(stochastic.definition.outputs.len(), 2);

        for (identifier, revision) in [
            (BUILTIN_RSI_IDENTIFIER, BUILTIN_RSI_IMPLEMENTATION_REVISION),
            (
                BUILTIN_MACD_IDENTIFIER,
                BUILTIN_MACD_IMPLEMENTATION_REVISION,
            ),
            (
                BUILTIN_STOCHASTIC_IDENTIFIER,
                BUILTIN_STOCHASTIC_IMPLEMENTATION_REVISION,
            ),
        ] {
            assert!(matches!(
                restore_native_registration(
                    identifier,
                    revision + 1,
                    vec![market.clone()],
                    BTreeMap::new(),
                ),
                Err(StudySdkError::UnsupportedImplementationRevision { .. })
            ));
        }
    }

    #[test]
    fn builtin_dependency_contracts_distinguish_numeric_outputs_from_bar_only_studies() {
        let upstream = StudyInstanceId::try_from_u64(9).expect("upstream study id");
        let output = StudyDependency::Output(upstream.output(0));
        assert!(
            restore_native_registration(
                BUILTIN_EMA_RIBBON_IDENTIFIER,
                BUILTIN_EMA_RIBBON_IMPLEMENTATION_REVISION,
                vec![output.clone()],
                ribbon_settings(),
            )
            .is_ok()
        );
        assert!(
            restore_native_registration(
                BUILTIN_RSI_IDENTIFIER,
                BUILTIN_RSI_IMPLEMENTATION_REVISION,
                vec![output.clone()],
                BTreeMap::from([(
                    BUILTIN_RSI_PERIOD_SETTING.to_string(),
                    StudySettingValue::Integer(BUILTIN_RSI_DEFAULT_PERIOD),
                )]),
            )
            .is_ok()
        );
        assert!(
            restore_native_registration(
                BUILTIN_MACD_IDENTIFIER,
                BUILTIN_MACD_IMPLEMENTATION_REVISION,
                vec![output.clone()],
                macd_settings(),
            )
            .is_ok()
        );
        assert!(matches!(
            restore_native_registration(
                BUILTIN_ATR_IDENTIFIER,
                BUILTIN_ATR_IMPLEMENTATION_REVISION,
                vec![output.clone()],
                BTreeMap::from([(
                    BUILTIN_ATR_PERIOD_SETTING.to_string(),
                    StudySettingValue::Integer(BUILTIN_ATR_DEFAULT_PERIOD),
                )]),
            ),
            Err(StudySdkError::InvalidDependencyContract(_))
        ));
        assert!(matches!(
            restore_native_registration(
                BUILTIN_STOCHASTIC_IDENTIFIER,
                BUILTIN_STOCHASTIC_IMPLEMENTATION_REVISION,
                vec![output],
                stochastic_settings(),
            ),
            Err(StudySdkError::InvalidDependencyContract(_))
        ));
    }

    #[test]
    fn wma_rejects_a_period_that_would_overflow_the_shared_formula() {
        let source = BarSeriesKey {
            provider_id: "provider".to_string(),
            instrument_id: "instrument".to_string(),
            entitlement_id: "entitlement".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        };

        assert!(matches!(
            restore_native_registration(
                BUILTIN_WMA_IDENTIFIER,
                BUILTIN_WMA_IMPLEMENTATION_REVISION,
                vec![StudyDependency::Market(StudyMarketInput {
                    series: source,
                    streams: StreamRequirements::BARS,
                })],
                BTreeMap::from([(
                    BUILTIN_WMA_PERIOD_SETTING.to_string(),
                    StudySettingValue::Integer(i64::MAX),
                )]),
            ),
            Err(StudySdkError::Runtime(
                StudyRuntimeError::InvalidSettingValue
            ))
        ));
    }

    #[test]
    fn built_in_period_settings_reject_values_beyond_the_runtime_source_ceiling() {
        let source = BarSeriesKey {
            provider_id: "provider".to_string(),
            instrument_id: "instrument".to_string(),
            entitlement_id: "entitlement".to_string(),
            period: BarPeriod::time(60).expect("minute period"),
            definition_version: 1,
        };
        let dependency = StudyDependency::Market(StudyMarketInput {
            series: source,
            streams: StreamRequirements::BARS,
        });
        let oversized = BUILTIN_MAXIMUM_PERIOD + 1;

        assert!(matches!(
            restore_native_registration(
                BUILTIN_RSI_IDENTIFIER,
                BUILTIN_RSI_IMPLEMENTATION_REVISION,
                vec![dependency],
                BTreeMap::from([(
                    BUILTIN_RSI_PERIOD_SETTING.to_string(),
                    StudySettingValue::Integer(oversized),
                )]),
            ),
            Err(StudySdkError::Runtime(
                StudyRuntimeError::InvalidSettingValue
            ))
        ));
    }
}
