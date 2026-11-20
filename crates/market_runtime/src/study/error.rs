use super::{StudyInstanceId, StudyOutputId};
use aeris_market_engine::EngineError;
use std::{error::Error, fmt};

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
