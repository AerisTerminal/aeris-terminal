//! Bounded command-response identity for one consumer and provider.

use super::ConsumerId;
use aeris_market_engine::fence::{CommandTag, Epoch, EpochSequence};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

/// Longest a search or selection may wait for its provider callback. Adapters
/// answer or reject every command they accept; this bounds a lost callback so a
/// consumer never waits forever, while leaving room for the provider login a
/// first command waits behind.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum CommandKind {
    Search,
    Select,
    Selection,
    Screen,
    StartupResolve,
}

#[derive(Clone, Copy)]
struct Pending {
    wire_generation: u64,
    epoch: Epoch<CommandTag>,
    selection: bool,
    completed: bool,
    deadline: Instant,
}

/// A catalog command whose provider callback did not arrive in time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ExpiredCommand {
    pub(super) consumer: ConsumerId,
    pub(super) provider: String,
    pub(super) kind: CommandKind,
    pub(super) wire_generation: u64,
    pub(super) selection: bool,
}

#[derive(Default)]
pub(super) struct CommandFences {
    sequence: EpochSequence<CommandTag>,
    pending: BTreeMap<(ConsumerId, String, CommandKind), Pending>,
}

impl CommandFences {
    pub(super) fn record(
        &mut self,
        consumer: ConsumerId,
        provider: String,
        kind: CommandKind,
        wire_generation: u64,
        now: Instant,
    ) -> Epoch<CommandTag> {
        self.record_operation(consumer, provider, kind, wire_generation, false, now)
    }

    pub(super) fn record_operation(
        &mut self,
        consumer: ConsumerId,
        provider: String,
        kind: CommandKind,
        wire_generation: u64,
        selection: bool,
        now: Instant,
    ) -> Epoch<CommandTag> {
        let epoch = self.sequence.next();
        self.pending.insert(
            (consumer, provider, kind),
            Pending {
                wire_generation,
                epoch,
                selection,
                completed: false,
                deadline: now + RESPONSE_TIMEOUT,
            },
        );
        epoch
    }

    /// Earliest response deadline among commands still awaiting a callback.
    /// Screens and completed searches are not answered by a later callback.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.awaiting_callback()
            .map(|(_, pending)| pending.deadline)
            .min()
    }

    /// Commands whose response deadline has passed. They stay recorded until
    /// the caller rejects them through the normal rejection path.
    pub(super) fn expired(&self, now: Instant) -> Vec<ExpiredCommand> {
        self.awaiting_callback()
            .filter(|(_, pending)| pending.deadline <= now)
            .map(|((consumer, provider, kind), pending)| ExpiredCommand {
                consumer: *consumer,
                provider: provider.clone(),
                kind: *kind,
                wire_generation: pending.wire_generation,
                selection: pending.selection,
            })
            .collect()
    }

    fn awaiting_callback(
        &self,
    ) -> impl Iterator<Item = (&(ConsumerId, String, CommandKind), &Pending)> {
        self.pending
            .iter()
            .filter(|((_, _, kind), pending)| *kind != CommandKind::Screen && !pending.completed)
    }

    pub(super) fn accepts(
        &self,
        consumer: ConsumerId,
        provider: &str,
        kind: CommandKind,
        wire_generation: u64,
    ) -> bool {
        self.pending
            .get(&(consumer, provider.to_owned(), kind))
            .is_some_and(|pending| pending.wire_generation == wire_generation)
    }

    pub(super) fn kind_for(
        &self,
        consumer: ConsumerId,
        provider: &str,
        generation: u64,
        selection: bool,
    ) -> Option<CommandKind> {
        let kinds = if selection {
            [
                CommandKind::StartupResolve,
                CommandKind::Selection,
                CommandKind::Select,
            ]
        } else {
            [
                CommandKind::StartupResolve,
                CommandKind::Selection,
                CommandKind::Search,
            ]
        };
        kinds.into_iter().find(|kind| {
            self.pending
                .get(&(consumer, provider.to_owned(), *kind))
                .is_some_and(|pending| {
                    pending.wire_generation == generation
                        && pending.selection == selection
                        && !pending.completed
                })
        })
    }

    pub(super) fn pending_for(
        &self,
        provider: &str,
        kind: CommandKind,
    ) -> Vec<(ConsumerId, u64, bool)> {
        self.pending
            .iter()
            .filter(|((_, candidate, lane), _)| candidate == provider && *lane == kind)
            .map(|((consumer, _, _), pending)| {
                (*consumer, pending.wire_generation, pending.selection)
            })
            .collect()
    }

    pub(super) fn take(
        &mut self,
        consumer: ConsumerId,
        provider: &str,
        kind: CommandKind,
        wire_generation: u64,
    ) -> Option<Epoch<CommandTag>> {
        if !self.accepts(consumer, provider, kind, wire_generation) {
            return None;
        }
        self.pending
            .remove(&(consumer, provider.to_owned(), kind))
            .map(|pending| pending.epoch)
    }

    pub(super) fn complete_search(
        &mut self,
        consumer: ConsumerId,
        provider: &str,
        wire_generation: u64,
    ) {
        if let Some(pending) =
            self.pending
                .get_mut(&(consumer, provider.to_owned(), CommandKind::Search))
            && pending.wire_generation == wire_generation
        {
            pending.completed = true;
        }
    }

    pub(super) fn remove_consumer(&mut self, consumer: ConsumerId) {
        self.pending
            .retain(|(candidate, _, _), _| *candidate != consumer);
    }

    #[cfg(test)]
    pub(super) fn count_for(&self, consumer: ConsumerId) -> usize {
        self.pending
            .keys()
            .filter(|(candidate, _, _)| *candidate == consumer)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    #[test]
    fn superseded_startup_search_does_not_accept_old_result() {
        let consumer = ConsumerId(NonZeroU64::MIN);
        let mut fences = CommandFences::default();
        let now = Instant::now();
        let old = fences.record(
            consumer,
            "rithmic".into(),
            CommandKind::StartupResolve,
            17,
            now,
        );
        let current = fences.record(
            consumer,
            "rithmic".into(),
            CommandKind::StartupResolve,
            18,
            now,
        );
        assert_ne!(old, current);
        assert!(
            fences
                .take(consumer, "rithmic", CommandKind::StartupResolve, 17)
                .is_none()
        );
        assert_eq!(
            fences.take(consumer, "rithmic", CommandKind::StartupResolve, 18),
            Some(current)
        );
        assert_eq!(fences.count_for(consumer), 0);
    }

    #[test]
    fn removing_a_consumer_clears_every_command_kind() {
        let consumer = ConsumerId(NonZeroU64::MIN);
        let mut fences = CommandFences::default();
        for kind in [
            CommandKind::Search,
            CommandKind::Select,
            CommandKind::Selection,
            CommandKind::Screen,
            CommandKind::StartupResolve,
        ] {
            fences.record(consumer, "rithmic".into(), kind, 1, Instant::now());
        }
        assert_eq!(fences.count_for(consumer), 5);
        fences.remove_consumer(consumer);
        assert_eq!(fences.count_for(consumer), 0);
    }

    #[test]
    fn unanswered_commands_expire_but_screens_and_completed_searches_do_not() {
        let consumer = ConsumerId(NonZeroU64::MIN);
        let mut fences = CommandFences::default();
        let start = Instant::now();
        fences.record(consumer, "rithmic".into(), CommandKind::Search, 4, start);
        fences.record(consumer, "rithmic".into(), CommandKind::Screen, 5, start);
        fences.record_operation(
            consumer,
            "rithmic".into(),
            CommandKind::Select,
            6,
            true,
            start + Duration::from_secs(1),
        );
        assert_eq!(fences.next_deadline(), Some(start + RESPONSE_TIMEOUT));
        assert_eq!(fences.expired(start + Duration::from_secs(44)), []);
        assert_eq!(
            fences.expired(start + RESPONSE_TIMEOUT),
            [ExpiredCommand {
                consumer,
                provider: "rithmic".into(),
                kind: CommandKind::Search,
                wire_generation: 4,
                selection: false,
            }]
        );

        fences.complete_search(consumer, "rithmic", 4);
        assert_eq!(
            fences.next_deadline(),
            Some(start + Duration::from_secs(1) + RESPONSE_TIMEOUT)
        );
        assert_eq!(
            fences.expired(start + Duration::from_secs(1) + RESPONSE_TIMEOUT),
            [ExpiredCommand {
                consumer,
                provider: "rithmic".into(),
                kind: CommandKind::Select,
                wire_generation: 6,
                selection: true,
            }]
        );
    }
}
