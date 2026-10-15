//! Bounded command-response identity for one consumer and provider.

use super::ConsumerId;
use aeris_market_engine::fence::{CommandTag, Epoch, EpochSequence};
use std::collections::BTreeMap;

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
    ) -> Epoch<CommandTag> {
        self.record_operation(consumer, provider, kind, wire_generation, false)
    }

    pub(super) fn record_operation(
        &mut self,
        consumer: ConsumerId,
        provider: String,
        kind: CommandKind,
        wire_generation: u64,
        selection: bool,
    ) -> Epoch<CommandTag> {
        let epoch = self.sequence.next();
        self.pending.insert(
            (consumer, provider, kind),
            Pending {
                wire_generation,
                epoch,
                selection,
                completed: false,
            },
        );
        epoch
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
        let old = fences.record(consumer, "rithmic".into(), CommandKind::StartupResolve, 17);
        let current = fences.record(consumer, "rithmic".into(), CommandKind::StartupResolve, 18);
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
            fences.record(consumer, "rithmic".into(), kind, 1);
        }
        assert_eq!(fences.count_for(consumer), 5);
        fences.remove_consumer(consumer);
        assert_eq!(fences.count_for(consumer), 0);
    }
}
