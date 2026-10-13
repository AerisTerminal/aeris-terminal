//! One retirement path for all coordinator-owned consumer resources.

use super::{ConsumerId, Coordinator};

pub(super) struct ConsumerRegistry<'a, 'b> {
    coordinator: &'a mut Coordinator<'b>,
}

impl<'a, 'b> ConsumerRegistry<'a, 'b> {
    pub(super) fn new(coordinator: &'a mut Coordinator<'b>) -> Self {
        Self { coordinator }
    }

    /// Detach already retired the engine lease; explicit removal still owns it.
    pub(super) fn remove_consumer(
        &mut self,
        consumer: ConsumerId,
        remove_engine_lease: bool,
    ) -> bool {
        let owner = &mut *self.coordinator;
        let removed_studies = !owner.studies.remove_consumer(consumer).is_empty();
        owner.events.remove(&consumer);
        owner.consumer_clients.remove(&consumer);
        owner.command_fences.remove_consumer(consumer);
        owner.price_alerts.remove_consumer(consumer);
        owner.remove_waiter(consumer);
        if remove_engine_lease {
            owner.engine.remove_consumer(consumer);
        }
        removed_studies
    }
}
