use super::{
    StudyDefinition, StudyExecutionContext, StudyInstanceId, StudyInvalidationPolicy,
    StudyRuntimeError, StudySettings,
};
use aeris_market_data::BarSeriesKey;
use aeris_market_engine::{MarketDataLeaseId, StreamRequirements};
use std::any::Any;

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

/// One opaque, cloneable native-study state value owned by [`super::StudyRuntime`].
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

    pub(super) fn value_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.value
            .downcast_mut::<NativeStudyStateValue<T>>()
            .map(|state| &mut state.value)
    }

    #[cfg(test)]
    pub(super) fn value<T: 'static>(&self) -> Option<&T> {
        self.value
            .downcast_ref::<NativeStudyStateValue<T>>()
            .map(|state| &state.value)
    }

    pub(super) fn runtime_bytes(&self) -> usize {
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
    /// Optional factory for explicit intrabar-persistent state (Pine `varip` equivalent).
    /// Persists across intrabar updates of a forming bar; resets on covering rebuild or reinit.
    pub intrabar_state_factory: Option<NativeStudyStateFactory>,
    /// When `true`, the study opts into read-only [`super::StudyChartContext`] updates.
    pub observes_chart_context: bool,
}

impl NativeStudyProgram {
    /// Builds a trusted native program that carries no mutable per-instance state.
    #[must_use]
    pub const fn stateless(calculate: NativeStudyCalculate) -> Self {
        Self {
            calculate,
            state_factory: None,
            intrabar_state_factory: None,
            observes_chart_context: false,
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
            intrabar_state_factory: None,
            observes_chart_context: false,
        }
    }

    /// Attaches an optional intrabar-persistent state factory (Pine `varip` equivalent).
    #[must_use]
    pub const fn with_intrabar_state(mut self, factory: NativeStudyStateFactory) -> Self {
        self.intrabar_state_factory = Some(factory);
        self
    }

    /// Opts this study into read-only visible-range chart context recalculations.
    #[must_use]
    pub const fn with_chart_context(mut self) -> Self {
        self.observes_chart_context = true;
        self
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

    pub(super) fn merge(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end_exclusive: match (self.end_exclusive, other.end_exclusive) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (None, _) | (_, None) => None,
            },
        }
    }

    pub(super) fn invalidated_by(
        self,
        policy: StudyInvalidationPolicy,
    ) -> Result<Self, StudyRuntimeError> {
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
