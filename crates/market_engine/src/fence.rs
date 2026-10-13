//! Typed identities and equality-only retirement fences for market messages.

use std::{marker::PhantomData, num::NonZeroU64};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ProviderTag {}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DemandTag {}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CommandTag {}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ConsumerTag {}

/// An identity in a single domain. Only a sequence can create one.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Epoch<Tag> {
    value: NonZeroU64,
    tag: PhantomData<fn() -> Tag>,
}

impl<Tag> Epoch<Tag> {
    #[must_use]
    pub const fn get(&self) -> NonZeroU64 {
        self.value
    }
}

/// The sole mint for an epoch domain; exhaustion cannot wrap and revive retired work.
#[derive(Debug)]
pub struct EpochSequence<Tag> {
    next: Option<NonZeroU64>,
    tag: PhantomData<fn() -> Tag>,
}

impl<Tag> Default for EpochSequence<Tag> {
    fn default() -> Self {
        Self {
            next: Some(NonZeroU64::MIN),
            tag: PhantomData,
        }
    }
}

impl<Tag> EpochSequence<Tag> {
    /// Returns the next identity without wrapping into a previously used identity.
    ///
    /// # Panics
    /// Panics if all nonzero 64-bit identities have been consumed.
    #[expect(
        clippy::should_implement_trait,
        reason = "an exhausted epoch must panic rather than terminate a stream"
    )]
    pub fn next(&mut self) -> Epoch<Tag> {
        let value = self.next.expect("market epoch space exhausted");
        self.next = value.checked_add(1);
        Epoch {
            value,
            tag: PhantomData,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fenced<Tag, T> {
    pub epoch: Epoch<Tag>,
    pub value: T,
}

#[derive(Clone, Debug)]
pub struct Fence<Tag> {
    current: Option<Epoch<Tag>>,
    dropped: u64,
}

impl<Tag> Default for Fence<Tag> {
    fn default() -> Self {
        Self {
            current: None,
            dropped: 0,
        }
    }
}

impl<Tag> Fence<Tag> {
    pub fn advance(&mut self, epoch: Epoch<Tag>) {
        self.current = Some(epoch);
    }

    pub fn accept<'a, T>(&mut self, message: &'a Fenced<Tag, T>) -> Option<&'a T> {
        if self.current.as_ref().map(Epoch::get) == Some(message.epoch.get()) {
            Some(&message.value)
        } else {
            self.dropped = self.dropped.saturating_add(1);
            None
        }
    }

    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_rejects_older_and_unadvanced_newer_epoch() {
        let mut sequence = EpochSequence::<ProviderTag>::default();
        let old = sequence.next();
        let current = sequence.next();
        let future = sequence.next();
        let mut fence = Fence::default();
        fence.advance(current);
        assert_eq!(
            fence.accept(&Fenced {
                epoch: old,
                value: 1
            }),
            None
        );
        assert_eq!(fence.dropped(), 1);
        assert_eq!(
            fence.accept(&Fenced {
                epoch: future,
                value: 2
            }),
            None
        );
        assert_eq!(fence.dropped(), 2);
        assert_eq!(
            fence.accept(&Fenced {
                epoch: current,
                value: 3
            }),
            Some(&3)
        );
        assert_eq!(fence.dropped(), 2);
    }

    #[test]
    fn epoch_sequence_issues_last_identity_before_exhaustion() {
        let mut sequence = EpochSequence::<ProviderTag> {
            next: Some(NonZeroU64::MAX),
            ..Default::default()
        };
        assert_eq!(sequence.next().get(), NonZeroU64::MAX);
        assert!(std::panic::catch_unwind(move || sequence.next()).is_err());
    }
}
