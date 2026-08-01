//! Fallible current-thread priority and affinity hints.

use std::{error::Error, fmt};

use crate::CapabilityAvailability;

#[cfg(target_os = "linux")]
use core_affinity::CoreId;
#[cfg(target_os = "linux")]
use thread_priority::{ThreadPriority, ThreadPriorityValue};

/// An operating-system affinity target discovered for the current process.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AffinityTarget {
    native_id: usize,
}

impl AffinityTarget {
    #[must_use]
    pub const fn native_id(self) -> usize {
        self.native_id
    }
}

/// Portable current-thread priority hints that avoid real-time scheduling classes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThreadPriorityHint {
    #[default]
    Balanced,
    Responsive,
    LatencySensitive,
}

impl ThreadPriorityHint {
    /// Returns the backend value, or `None` when the hint preserves the thread's
    /// existing normal priority. `Balanced` never lowers niceness, so it stays
    /// usable on a thread already started at a positive nice value.
    const fn cross_platform_value(self) -> Option<u8> {
        match self {
            Self::Balanced => None,
            Self::Responsive => Some(65),
            Self::LatencySensitive => Some(80),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
trait ThreadSchedulingBackend: fmt::Debug + Send + Sync {
    fn affinity_targets(&self) -> Option<Vec<usize>>;
    fn reject_realtime_policy(&self) -> Result<(), ThreadSchedulingError>;
    fn set_current_priority(&self, value: u8) -> Result<(), ThreadSchedulingError>;
    fn apply_current_affinity(&self, native_id: usize) -> bool;
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Debug)]
struct OperatingSystemThreadSchedulingBackend;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl ThreadSchedulingBackend for OperatingSystemThreadSchedulingBackend {
    fn affinity_targets(&self) -> Option<Vec<usize>> {
        #[cfg(target_os = "linux")]
        {
            core_affinity::get_core_ids().map(|processors| {
                processors
                    .into_iter()
                    .map(|processor| processor.id)
                    .collect()
            })
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        None
    }

    fn reject_realtime_policy(&self) -> Result<(), ThreadSchedulingError> {
        #[cfg(target_os = "linux")]
        {
            use thread_priority::{NormalThreadSchedulePolicy, ThreadSchedulePolicy};

            match thread_priority::thread_schedule_policy()
                .map_err(ThreadSchedulingError::Priority)?
            {
                ThreadSchedulePolicy::Realtime(_) => {
                    return Err(ThreadSchedulingError::RealtimePolicyActive);
                }
                ThreadSchedulePolicy::Normal(NormalThreadSchedulePolicy::Idle) => {
                    return Err(ThreadSchedulingError::IdlePolicyActive);
                }
                ThreadSchedulePolicy::Normal(_) => {}
            }
        }
        Ok(())
    }

    fn set_current_priority(&self, value: u8) -> Result<(), ThreadSchedulingError> {
        self.reject_realtime_policy()?;
        #[cfg(target_os = "linux")]
        {
            let value = ThreadPriorityValue::try_from(value)
                .map_err(|_| ThreadSchedulingError::PriorityValueOutOfRange)?;
            ThreadPriority::Crossplatform(value)
                .set_for_current()
                .map_err(ThreadSchedulingError::Priority)
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let _ = value;
            Err(ThreadSchedulingError::PriorityUnavailable)
        }
    }

    fn apply_current_affinity(&self, native_id: usize) -> bool {
        #[cfg(target_os = "linux")]
        {
            core_affinity::set_for_current(CoreId { id: native_id })
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let _ = native_id;
            false
        }
    }
}

/// Native current-thread scheduler. Affinity targets are discovered per calling
/// thread because a thread's allowed set is not shared process-wide.
pub struct NativeThreadScheduler {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    backend: Box<dyn ThreadSchedulingBackend>,
}

impl NativeThreadScheduler {
    /// Returns whether this target has a native current-thread priority backend.
    #[must_use]
    pub const fn priority_availability() -> CapabilityAvailability {
        if cfg!(target_os = "linux") {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    }

    /// Returns whether this target has a native current-thread affinity backend.
    #[must_use]
    pub const fn affinity_availability() -> CapabilityAvailability {
        if cfg!(target_os = "linux") {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable
        }
    }

    /// Creates the native scheduler for this platform.
    ///
    /// # Errors
    ///
    /// Returns an error only on unsupported platforms. Priority and affinity remain
    /// independently fallible per call.
    pub fn discover() -> Result<Self, ThreadSchedulingError> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            Ok(Self::with_backend(OperatingSystemThreadSchedulingBackend))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        Err(ThreadSchedulingError::UnsupportedPlatform)
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    fn with_backend(backend: impl ThreadSchedulingBackend + 'static) -> Self {
        Self {
            backend: Box::new(backend),
        }
    }

    /// Returns the affinity targets allowed for the calling thread.
    #[must_use]
    pub fn current_affinity_targets(&self) -> Vec<AffinityTarget> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            let mut target_ids = self.backend.affinity_targets().unwrap_or_default();
            target_ids.sort_unstable();
            target_ids.dedup();
            target_ids
                .into_iter()
                .map(|native_id| AffinityTarget { native_id })
                .collect()
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        Vec::new()
    }

    /// Applies a non-real-time priority hint to the calling thread.
    ///
    /// `Balanced` preserves the thread's existing normal priority and needs no
    /// privileges. `Responsive` and `LatencySensitive` raise priority, so an
    /// unprivileged Linux process without `CAP_SYS_NICE` or a raised `RLIMIT_NICE`
    /// receives an operating-system error. A Linux thread already running under a
    /// real-time policy is rejected rather than having these values reinterpreted as
    /// real-time priorities, and a thread under the Linux idle policy is refused
    /// because that policy ignores niceness. Windows and macOS report priority unavailable: neither
    /// exposes a real-time-class check reachable without `unsafe`, which this
    /// workspace forbids, so raising priority there could silently produce a
    /// real-time base priority.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error when the hint cannot be applied.
    pub fn apply_current_priority(
        &self,
        hint: ThreadPriorityHint,
    ) -> Result<(), ThreadSchedulingError> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            match hint.cross_platform_value() {
                None if Self::priority_availability() == CapabilityAvailability::Available => {
                    self.backend.reject_realtime_policy()
                }
                None => Err(ThreadSchedulingError::PriorityUnavailable),
                Some(value) => self.backend.set_current_priority(value),
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            let _ = hint;
            Err(ThreadSchedulingError::UnsupportedPlatform)
        }
    }

    /// Applies one affinity target to the calling thread.
    ///
    /// The operating system validates the target, so a thread already narrowed to a
    /// single processor can still move to another target the kernel permits.
    ///
    /// # Errors
    ///
    /// Returns an error when affinity is unavailable or the operating system rejects
    /// the target.
    pub fn apply_current_affinity(
        &self,
        target: AffinityTarget,
    ) -> Result<(), ThreadSchedulingError> {
        if Self::affinity_availability() == CapabilityAvailability::Unavailable {
            return Err(ThreadSchedulingError::AffinityUnavailable);
        }
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            if self.backend.apply_current_affinity(target.native_id) {
                return Ok(());
            }
            Err(ThreadSchedulingError::AffinityRejected)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        Err(ThreadSchedulingError::UnsupportedPlatform)
    }
}

impl fmt::Debug for NativeThreadScheduler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeThreadScheduler")
            .field("priority", &Self::priority_availability())
            .field("affinity", &Self::affinity_availability())
            .finish_non_exhaustive()
    }
}

/// Current-thread priority and affinity failures.
#[derive(Debug)]
pub enum ThreadSchedulingError {
    UnsupportedPlatform,
    AffinityUnavailable,
    AffinityRejected,
    PriorityUnavailable,
    PriorityValueOutOfRange,
    RealtimePolicyActive,
    IdlePolicyActive,
    #[cfg(target_os = "linux")]
    Priority(thread_priority::Error),
}

impl fmt::Display for ThreadSchedulingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                formatter.write_str("native thread scheduling is unsupported on this platform")
            }
            Self::AffinityUnavailable => formatter
                .write_str("current-thread affinity hints are unavailable on this platform"),
            Self::AffinityRejected => {
                formatter.write_str("operating system rejected the thread affinity hint")
            }
            Self::PriorityUnavailable => formatter
                .write_str("current-thread priority hints are unavailable on this platform"),
            Self::PriorityValueOutOfRange => {
                formatter.write_str("thread priority hint is out of range")
            }
            Self::RealtimePolicyActive => formatter.write_str(
                "thread uses a real-time scheduling policy, so non-real-time hints are refused",
            ),
            Self::IdlePolicyActive => formatter.write_str(
                "thread uses the idle scheduling policy, which ignores niceness, so the hint is refused",
            ),
            #[cfg(target_os = "linux")]
            Self::Priority(error) => write!(formatter, "thread priority hint failed: {error}"),
        }
    }
}

impl Error for ThreadSchedulingError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Priority(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "macos", target_os = "windows")
))]
mod tests {
    use super::{
        AffinityTarget, NativeThreadScheduler, ThreadPriorityHint, ThreadSchedulingBackend,
        ThreadSchedulingError,
    };
    use crate::CapabilityAvailability;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct RecordingState {
        priorities: Mutex<Vec<u8>>,
        affinities: Mutex<Vec<usize>>,
    }

    #[derive(Debug)]
    struct RecordingBackend {
        targets: Option<Vec<usize>>,
        state: Arc<RecordingState>,
        affinity_accepted: bool,
    }

    impl ThreadSchedulingBackend for RecordingBackend {
        fn affinity_targets(&self) -> Option<Vec<usize>> {
            self.targets.clone()
        }

        fn reject_realtime_policy(&self) -> Result<(), ThreadSchedulingError> {
            Ok(())
        }

        fn set_current_priority(&self, value: u8) -> Result<(), ThreadSchedulingError> {
            self.state
                .priorities
                .lock()
                .expect("priority recording lock is available")
                .push(value);
            Ok(())
        }

        fn apply_current_affinity(&self, native_id: usize) -> bool {
            self.state
                .affinities
                .lock()
                .expect("affinity recording lock is available")
                .push(native_id);
            self.affinity_accepted
        }
    }

    fn backend(targets: Option<Vec<usize>>) -> (RecordingBackend, Arc<RecordingState>) {
        let state = Arc::new(RecordingState::default());
        (
            RecordingBackend {
                targets,
                state: Arc::clone(&state),
                affinity_accepted: true,
            },
            state,
        )
    }

    #[test]
    fn discovery_sorts_and_deduplicates_native_affinity_targets() {
        let scheduler = NativeThreadScheduler::with_backend(backend(Some(vec![7, 2, 7, 4])).0);
        assert_eq!(
            scheduler.current_affinity_targets(),
            vec![
                AffinityTarget { native_id: 2 },
                AffinityTarget { native_id: 4 },
                AffinityTarget { native_id: 7 }
            ]
        );
        let empty = NativeThreadScheduler::with_backend(backend(None).0);
        assert!(empty.current_affinity_targets().is_empty());
        #[cfg(target_os = "linux")]
        empty
            .apply_current_priority(ThreadPriorityHint::Balanced)
            .expect("priority is independent from affinity discovery");
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        assert!(matches!(
            empty.apply_current_priority(ThreadPriorityHint::Balanced),
            Err(ThreadSchedulingError::PriorityUnavailable)
        ));
    }

    #[test]
    fn native_capabilities_and_affinity_targets_are_reported_without_mutation() {
        #[cfg(target_os = "linux")]
        assert_eq!(
            NativeThreadScheduler::affinity_availability(),
            CapabilityAvailability::Available
        );
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        assert_eq!(
            NativeThreadScheduler::affinity_availability(),
            CapabilityAvailability::Unavailable
        );
        #[cfg(target_os = "linux")]
        assert_eq!(
            NativeThreadScheduler::priority_availability(),
            CapabilityAvailability::Available
        );
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        assert_eq!(
            NativeThreadScheduler::priority_availability(),
            CapabilityAvailability::Unavailable
        );

        let scheduler =
            NativeThreadScheduler::discover().expect("supported target constructs scheduler");
        #[cfg(target_os = "linux")]
        assert!(!scheduler.current_affinity_targets().is_empty());
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        assert!(scheduler.current_affinity_targets().is_empty());
    }

    #[test]
    fn priority_hints_are_bounded_and_affinity_requires_a_discovered_target() {
        let (recording_backend, state) = backend(Some(vec![3, 8]));
        let scheduler = NativeThreadScheduler::with_backend(recording_backend);
        #[cfg(target_os = "linux")]
        scheduler
            .apply_current_priority(ThreadPriorityHint::Balanced)
            .expect("balanced priority is accepted");
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        assert!(matches!(
            scheduler.apply_current_priority(ThreadPriorityHint::Balanced),
            Err(ThreadSchedulingError::PriorityUnavailable)
        ));
        scheduler
            .apply_current_priority(ThreadPriorityHint::Responsive)
            .expect("responsive priority is accepted");
        scheduler
            .apply_current_priority(ThreadPriorityHint::LatencySensitive)
            .expect("latency-sensitive priority is accepted");
        assert_eq!(
            state
                .priorities
                .lock()
                .expect("priority recording lock is available")
                .as_slice(),
            &[65, 80]
        );
        assert!(
            scheduler
                .apply_current_affinity(scheduler.current_affinity_targets()[1])
                .is_ok()
        );
        assert_eq!(
            state
                .affinities
                .lock()
                .expect("affinity recording lock is available")
                .as_slice(),
            &[8]
        );

        let state = Arc::new(RecordingState::default());
        let rejected = NativeThreadScheduler::with_backend(RecordingBackend {
            targets: Some(vec![3]),
            state,
            affinity_accepted: false,
        });
        assert!(matches!(
            rejected.apply_current_affinity(rejected.current_affinity_targets()[0]),
            Err(ThreadSchedulingError::AffinityRejected)
        ));
    }
}
